//! Differential test (`parser::parse` vs FCS): active-pattern *names* as binding
//! heads — `let (|Foo|Bar|) x = …`. FCS lexes/parses the parenthesised
//! pipe-delimited case list into a single-segment `SynPat.LongIdent` whose
//! `SynLongIdent` ident has `idText = "|Foo|Bar|"` (the leading/trailing pipes
//! and inner separators baked into one ident), with the curried args following
//! as `SynArgPats.Pats`. Partial active patterns spell the trailing `_` into the
//! name (`(|Foo|_|)` → `"|Foo|_|"`).
//!
//! Because the active-pattern name is an `atomicPatternLongIdent`, FCS also
//! accepts it at non-head pattern sites (`match`/`function` clause heads, nested
//! paren args); those are exercised here too.

use crate::common::{assert_asts_match, assert_asts_match_with_diagnostic};
use borzoi_cst::parser::parse;

/// FCS `parsActivePatternCaseMustBeginWithUpperCase` (`FSComp.txt:477`): an
/// active-pattern case identifier whose leading character is not "upper case"
/// per `String.isLeadingIdentifierCharacterUpperCase`.
const FS_ACTIVE_PAT_CASE_UPPERCASE: i64 = 623;

/// FCS `parsActivePatternCaseContainsPipe` (`FSComp.txt:478`): a case
/// identifier (only reachable via a backticked ident) whose `idText` contains
/// a `'|'`.
const FS_ACTIVE_PAT_CASE_PIPE: i64 = 624;

// ---- total active patterns (binding heads) ------------------------------

/// Two-case total active pattern — the canonical form. FCS:
/// `headPat = LongIdent(["|Foo|Bar|"], Pats[Named x])`.
#[test]
fn diff_ast_total_two_case() {
    assert_asts_match("let (|Foo|Bar|) x = x\n");
}

/// Single-case total active pattern (`(|Foo|)` → `"|Foo|"`).
#[test]
fn diff_ast_total_single_case() {
    assert_asts_match("let (|Foo|) x = x\n");
}

/// Three-case total active pattern — the separators all fold into the name.
#[test]
fn diff_ast_total_three_case() {
    assert_asts_match("let (|A|B|C|) x = A\n");
}

/// The motivating shape: a parenthesised, type-annotated curried argument.
#[test]
fn diff_ast_typed_arg() {
    assert_asts_match("let (|Foo|Bar|) (x : int) = x\n");
}

/// Multiple curried arguments after the name.
#[test]
fn diff_ast_multiple_args() {
    assert_asts_match("let (|Foo|Bar|) a b = a\n");
}

/// No arguments at all — `let (|Foo|Bar|) = e` is still a `LongIdent` head with
/// an empty `Pats`.
#[test]
fn diff_ast_no_args() {
    assert_asts_match("let (|Foo|Bar|) = id\n");
}

/// A realistic active-pattern body.
#[test]
fn diff_ast_realistic_body() {
    assert_asts_match("let (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n");
}

// ---- partial active patterns --------------------------------------------

/// Partial active pattern — the trailing `_` is part of the name (`"|Foo|_|"`).
#[test]
fn diff_ast_partial() {
    assert_asts_match("let (|Foo|_|) x = None\n");
}

/// Partial active pattern with a typed argument.
#[test]
fn diff_ast_partial_typed_arg() {
    assert_asts_match("let (|Parse|_|) (s : string) = None\n");
}

// ---- whitespace tolerance -----------------------------------------------

/// FCS tolerates spaces around the pipes and builds the same `idText`.
#[test]
fn diff_ast_spaced() {
    assert_asts_match("let (| Foo | Bar |) x = x\n");
}

// ---- non-head pattern sites ---------------------------------------------

/// Active-pattern name as a `match`-clause head. FCS reaches the same
/// `SynPat.LongIdent` here (it is an `atomicPattern`).
#[test]
fn diff_ast_match_clause() {
    assert_asts_match("match z with (|A|B|) -> 1 | _ -> 2\n");
}

/// Active-pattern name nested inside a paren argument.
#[test]
fn diff_ast_nested_paren_arg() {
    assert_asts_match("let f ((|Foo|Bar|)) = 1\n");
}

/// Active-pattern name as a *direct* curried argument — the active-pattern
/// parens are the name's own, so FCS yields `Pats[Named("|Foo|Bar|")]` with no
/// `Paren` wrapper.
#[test]
fn diff_ast_direct_curried_arg() {
    assert_asts_match("let f (|Foo|Bar|) = 1\n");
}

// ---- layout: nullary name must not absorb an offside neighbour ----------

/// A nullary active-pattern name followed by a layout-separated element must
/// stay nullary (`Named`), not promote to an empty-arg `LongIdent` by reading
/// past the `BlockSep` virtual. Here the list pattern has two elements —
/// `(|A|B|)` and `y` — split by the offside break, so FCS yields
/// `ArrayOrList[Named "|A|B|"; Named y]`.
#[test]
fn diff_ast_nullary_then_offside_element_is_not_promoted() {
    assert_asts_match("match x with\n| [ (|A|B|)\n    y ] -> 1\n");
}

// ---- access modifiers ---------------------------------------------------

/// `let private (|Foo|Bar|) x = …` — FCS's `access pathOp` (`pathOp` includes
/// active-pattern operator names) attaches the modifier to the
/// `SynPat.LongIdent` accessibility slot, which the normaliser elides; the head
/// is otherwise the ordinary function-form `LongIdent`.
#[test]
fn diff_ast_access_modifier_function() {
    assert_asts_match("let private (|Foo|Bar|) x = x\n");
}

/// A nullary active pattern with an access modifier still collapses to
/// `SynPat.Named` (the maybe-var rule is unaffected by accessibility).
#[test]
fn diff_ast_access_modifier_nullary() {
    assert_asts_match("let internal (|Foo|_|) = None\n");
}

// ---- explicit value typars ----------------------------------------------

/// `let (|Parse|_|)<'T> (s) = …` — active-pattern names are
/// `atomicPatternLongIdent`s, so FCS accepts explicit value typars and stores
/// them on `SynPat.LongIdent.typars`. (FCS emits a *warning* about spacing but
/// no parse error; the diff harness only gates on our side's errors.)
#[test]
fn diff_ast_typars_function() {
    assert_asts_match("let (|Parse|_|)<'T> (s) = None\n");
}

/// Carrying explicit typars forces the `LongIdent` form even with zero curried
/// args (mirroring `let h<'a> = …`).
#[test]
fn diff_ast_typars_nullary() {
    assert_asts_match("let (|Parse|_|)<'T> = None\n");
}

// ---- recovery: malformed names ------------------------------------------

/// A missing trailing pipe (`(|A|B)`) is an FCS parse error; we must recover
/// losslessly without panicking *and* report the error (FCS rejects it —
/// "Expected '|'"), rather than silently accepting the truncated name. `parse`
/// is invoked directly to assert the diagnostic is present.
#[test]
fn malformed_missing_trailing_bar_recovers() {
    let parsed = parse("let (|A|B) x = x\n");
    assert!(
        !parsed.errors.is_empty(),
        "expected a parse error for the truncated active-pattern name, got none"
    );
}

// ---- FS0623: case must begin with an uppercase letter -------------------
//
// FCS's `activePatternCaseName` action (`pars.fsy:6907`) reports
// `parsActivePatternCaseMustBeginWithUpperCase` per offending case, at the
// IDENT's range, while still building the name (it is recoverable). We mirror
// that via `ident_text_leads_uppercase`, the same `isLeadingIdentifierCharacterUpperCase`
// replica that drives the `SynPat.LongIdent` vs `SynPat.Named` split.

/// Single lowercase-led case — `(|foo|)`. FCS reports FS0623 at `foo`.
#[test]
fn diff_ast_lowercase_single_case_reports_fs623() {
    assert_asts_match_with_diagnostic("let (|foo|) x = x\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

/// A lowercase case among uppercase ones — `(|Foo|bar|)`. FS0623 lands on
/// `bar` only.
#[test]
fn diff_ast_lowercase_middle_case_reports_fs623() {
    assert_asts_match_with_diagnostic("let (|Foo|bar|) x = x\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

/// Lowercase case before the partial `_` marker — `(|foo|_|)`. The bare `_`
/// goes through FCS's `UNDERSCORE` production, not `activePatternCaseName`, so
/// only `foo` is flagged.
#[test]
fn diff_ast_lowercase_partial_reports_fs623() {
    assert_asts_match_with_diagnostic("let (|foo|_|) x = None\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

/// Underscore-led ident case — `(|_A|)`. `_A` lexes as one IDENT (not the
/// partial marker), and `_` is connector punctuation, so FCS reports FS0623.
#[test]
fn diff_ast_underscore_led_case_reports_fs623() {
    assert_asts_match_with_diagnostic("let (|_A|) x = x\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

/// Spaced lowercase case — `(| foo |)`. The diagnostic span is the bare `foo`,
/// not the surrounding whitespace/pipes.
#[test]
fn diff_ast_spaced_lowercase_reports_fs623() {
    assert_asts_match_with_diagnostic("let (| foo |) x = x\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

/// At a non-head pattern site (`match` clause), two lowercase cases — both
/// flagged independently.
#[test]
fn diff_ast_match_clause_lowercase_reports_fs623() {
    assert_asts_match_with_diagnostic(
        "match z with (|a|b|) -> 1 | _ -> 2\n",
        FS_ACTIVE_PAT_CASE_UPPERCASE,
    );
}

/// Quoted `Other_Alphabetic` non-letter case — `U+0345` is alphabetic under
/// Rust's derived property but is not a .NET `Char.IsLetter`, so FCS reports
/// FS0623.
#[test]
fn diff_ast_quoted_other_alphabetic_case_reports_fs623() {
    assert_asts_match_with_diagnostic("let (|``\u{0345}``|) x = x\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

// ---- FS0623 negatives: "never wrong" on case-insensitive scripts --------
//
// These are the inputs the old leniency feared a naive `char::is_uppercase`
// would wrongly reject. FCS accepts them (no FS0623), so our parser must
// report no error — `assert_asts_match` asserts exactly that.

/// Titlecase-led case — `ǅ` (U+01C5, general category `Lt`). `Char.IsUpper`
/// is `false`, but it is a letter that is not lower-case, so FCS accepts it.
#[test]
fn diff_ast_titlecase_led_case_accepted() {
    assert_asts_match("let (|\u{01C5}|) x = x\n");
}

/// Uncased-script case — `क` (U+0915, Devanagari, category `Lo`). A letter
/// with no case distinction; FCS treats it as upper and accepts it.
#[test]
fn diff_ast_uncased_script_case_accepted() {
    assert_asts_match("let (|\u{0915}|) x = x\n");
}

// ---- FS0624: '|' not permitted in a case identifier ---------------------

/// A backticked case whose `idText` contains a literal `'|'` — `` (|``A|B``|) ``.
/// Reachable only through a quoted ident (the lexer keeps `|` out of bare
/// idents). The leading `A` is upper-case, so this isolates FS0624 from FS0623.
#[test]
fn diff_ast_quoted_case_with_pipe_reports_fs624() {
    assert_asts_match_with_diagnostic("let (|``A|B``|) x = x\n", FS_ACTIVE_PAT_CASE_PIPE);
}
