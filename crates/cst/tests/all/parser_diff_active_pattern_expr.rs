//! Differential test (`parser::parse` vs FCS): active-pattern *names* in
//! *expression* position — `let f = (|Foo|_|)`. FCS's `identExpr: opName`
//! admits the active-pattern productions (`pars.fsy:6812-6819`), building a
//! single-segment `SynExpr.LongIdent` whose `idText` is the whole name
//! (`"|Foo|_|"`) with `IdentTrivia.HasParenthesis` — structurally identical to a
//! parenthesised operator-value `(+)`, differing only in trivia. The motivating
//! real-world shape is `verify q (|Call|_|) …` — an active-pattern function
//! passed as an argument.
//!
//! Because the active-pattern name is an `opName`, it folds into a long-ident
//! path exactly like `(+)`: `(|Foo|_|).Bar` → `LongIdent(["|Foo|_|"; "Bar"])`
//! and `Foo.(|Bar|_|)` → `LongIdent(["Foo"; "|Bar|_|"])`. Those are exercised
//! here too. It is, however, *excluded* from `atomicExprAfterType`, so
//! `new C(|Foo|_|)` is an FCS error (covered by `parser::parse` recovery, not a
//! diff).

use crate::common::{assert_asts_match, assert_asts_match_with_diagnostic};
use borzoi_cst::parser::parse;

/// FCS `parsActivePatternCaseMustBeginWithUpperCase` (`FSComp.txt:477`).
const FS_ACTIVE_PAT_CASE_UPPERCASE: i64 = 623;

// ---- bare values --------------------------------------------------------

/// Partial active-pattern name as a bound value (`"|Foo|_|"`).
#[test]
fn diff_ast_partial_value() {
    assert_asts_match("let f = (|Foo|_|)\n");
}

/// Total two-case active-pattern name as a bound value (`"|Foo|Bar|"`).
#[test]
fn diff_ast_total_value() {
    assert_asts_match("let f = (|Foo|Bar|)\n");
}

/// Single-case total active-pattern name (`"|Foo|"`).
#[test]
fn diff_ast_single_case_value() {
    assert_asts_match("let f = (|Foo|)\n");
}

/// Three-case total active-pattern name (`"|A|B|C|"`).
#[test]
fn diff_ast_three_case_value() {
    assert_asts_match("let f = (|A|B|C|)\n");
}

// ---- application arguments ----------------------------------------------

/// The motivating shape: an active-pattern name passed as an application
/// argument — `App(App(App(verify, q), (|Call|_|)), r)`.
#[test]
fn diff_ast_application_argument() {
    assert_asts_match("verify q (|Call|_|) r\n");
}

/// An active-pattern name as the *sole* application argument.
#[test]
fn diff_ast_sole_application_argument() {
    assert_asts_match("verify (|Call|_|)\n");
}

// ---- FS0623 in expression position --------------------------------------
//
// The same `activePatternCaseName` action backs `opName` (`pars.fsy:6812`),
// so a lowercase-led case is flagged here too — our single
// `parse_active_pat_name` chokepoint covers both pattern and expression sites.

/// Lowercase-led active-pattern name as a bound value — `(|foo|_|)`.
#[test]
fn diff_ast_lowercase_value_reports_fs623() {
    assert_asts_match_with_diagnostic("let f = (|foo|_|)\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

/// The motivating shape with a lowercase case — `verify q (|call|_|) r`.
#[test]
fn diff_ast_lowercase_application_argument_reports_fs623() {
    assert_asts_match_with_diagnostic("verify q (|call|_|) r\n", FS_ACTIVE_PAT_CASE_UPPERCASE);
}

// ---- folded `.member` qualification -------------------------------------

/// A trailing `.member` folds onto the name (FCS's `mkSynDot`), yielding a
/// single `LongIdent(["|Foo|_|"; "Bar"])` — *not* a `DotGet`.
#[test]
fn diff_ast_fold_trailing_member() {
    assert_asts_match("let f = (|Foo|_|).Bar\n");
}

// ---- dot-qualified leading path -----------------------------------------

/// A module-qualified active-pattern reference — `Foo.(|Bar|_|)` →
/// `LongIdent(["Foo"; "|Bar|_|"])`.
#[test]
fn diff_ast_dot_qualified() {
    assert_asts_match("let g = Foo.(|Bar|_|)\n");
}

/// A multi-segment leading path before the active-pattern name.
#[test]
fn diff_ast_dot_qualified_multi_segment() {
    assert_asts_match("let g = A.B.(|Bar|_|)\n");
}

/// An active-pattern-name qualification off a *non-ident* receiver — the
/// postfix-tail dot dispatch (`DotGet(Paren(App(id, 1)), ["|Bar|_|"])`), not
/// the ident-head long-ident fold.
#[test]
fn diff_ast_dot_qualified_off_paren_head() {
    assert_asts_match("let x = (id 1).(|Bar|_|)\n");
}

// ---- other expression positions -----------------------------------------

/// An active-pattern name as a tuple element.
#[test]
fn diff_ast_tuple_element() {
    assert_asts_match("let p = ((|Foo|_|), 1)\n");
}

/// An active-pattern name as a list element.
#[test]
fn diff_ast_list_element() {
    assert_asts_match("let xs = [ (|Foo|_|) ]\n");
}

/// An active-pattern name on the right of a pipe — a common real shape
/// (`expr |> (|Call|_|)`).
#[test]
fn diff_ast_pipe_argument() {
    assert_asts_match("let r = q |> (|Call|_|)\n");
}

// ---- recovery: not-an-expression-arg positions --------------------------

/// `new C(|Foo|_|)` is an FCS error (the active-pattern `opName` is excluded
/// from `atomicExprAfterType`). We must recover losslessly without panicking;
/// `parse` is invoked directly so the only assertion is that parsing
/// terminates.
#[test]
fn aftertype_arg_active_pattern_recovers() {
    let _ = parse("let x = new C(|Foo|_|)\n");
}

/// A malformed active-pattern name in expression position must not be silently
/// accepted as a valid value: FCS rejects `(|)` (no case), `(|Foo)` (no
/// trailing `|`), and `(|_|)` (no ident case), so our parser reports an error
/// while still recovering losslessly. `parse` is invoked directly to assert the
/// diagnostic is present (a complete name like `(|Foo|)` stays clean).
#[test]
fn malformed_active_pattern_value_is_an_error() {
    for src in [
        "let f = (|)\n",
        "let f = (|Foo)\n",
        "let f = (|_|)\n",
        "let f = (|A|B)\n",
        // `_` is valid only as the final partial marker — misplaced / doubled
        // markers are FCS errors.
        "let f = (|_|Foo|)\n",
        "let f = (|Foo|_|Bar|)\n",
        "let f = (|_|_|)\n",
    ] {
        let parsed = parse(src);
        assert!(
            !parsed.errors.is_empty(),
            "expected a parse error for malformed active-pattern value {src:?}, got none"
        );
    }
    // A complete name stays clean — including a final `_` after several cases.
    assert!(parse("let f = (|Foo|)\n").errors.is_empty());
    assert!(parse("let f = (|Foo|_|)\n").errors.is_empty());
    assert!(parse("let f = (|A|B|_|)\n").errors.is_empty());
}
