//! Differential test (`parser::parse` vs FCS): the `_`-rooted *pattern*
//! long-identifier head — FCS's `atomicPatternLongIdent` alternatives
//! `UNDERSCORE DOT pathOp` and `access UNDERSCORE DOT pathOp` (`_.M`,
//! `private _.M`). They yield `SynPat.LongIdent(SynLongIdent ids, …)` whose
//! leading segment's `idText` is `"_"` — the same shape the member self-id
//! `member _.M` produces, which reaches FCS through the same production.
//!
//! The pattern is the `global.`-rooted head's sibling (`parser_diff_global_pat.rs`),
//! and is unrelated to F# 8's `_.Member` accessor shorthand, which is an
//! *expression* (`SynExpr.DotLambda`, `parser_diff_dot_lambda.rs`).
//!
//! FCS (the `FSharp.Compiler.Service` `tools/fcs-dump` pins) gates the production
//! on `LanguageFeature.SingleUnderscorePattern` (F# 4.7): below it the `.` is a
//! hard FS0010 parse error, member heads included. We report the same error at the
//! same span and keep the maximal tree (the version is a diagnostic lens, never a
//! reshape — `docs/ast-versioning-plan.md`). The version grid below holds that to
//! FCS at every version band the gate distinguishes.

use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::ops::Range;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, Parse, ParseOptions, parse_with_options};
use tempfile::NamedTempFile;

use crate::common::{
    assert_asts_match, fcs_ast_batch_at, fcs_diagnostic_spans, fcs_parse_had_errors, normalised_ast,
};

/// A `_.`-rooted pattern in a `match` clause — the motivating shape.
#[test]
fn diff_underscore_pat_in_match_clause() {
    assert_asts_match("match x with\n| _.M -> 1\n| _ -> 2\n");
}

/// A `_.`-rooted `let` head: a nullary dotted path, so `SynPat.LongIdent`.
#[test]
fn diff_underscore_pat_let_head() {
    assert_asts_match("let _.M = 1\n");
}

/// A `_.`-rooted curried argument: FCS's `atomicPattern` reduction of the path,
/// nullary, under the enclosing function head.
#[test]
fn diff_underscore_pat_curried_arg() {
    assert_asts_match("let f _.M = 1\n");
}

/// Two `_.`-rooted curried arguments: each stays its own nullary atomic pattern
/// rather than the second being swept in as an argument of the first.
#[test]
fn diff_underscore_pat_two_curried_args() {
    assert_asts_match("let f _.M _.N = 1\n");
}

/// A `_.`-rooted head with an argument (`constrPattern`).
#[test]
fn diff_underscore_pat_with_arg() {
    assert_asts_match("match x with\n| _.M y -> 1\n");
}

/// A `_.`-rooted head with the named-field argument group.
#[test]
fn diff_underscore_pat_with_name_pat_pairs() {
    assert_asts_match("match x with\n| _.M(a = 1) -> 1\n");
}

/// A `_.`-rooted head with an adjacent paren argument.
#[test]
fn diff_underscore_pat_with_adjacent_paren_arg() {
    assert_asts_match("match x with\n| _.M(y) -> 1\n");
}

/// A `_.`-rooted `let` head carrying explicit value typars.
#[test]
fn diff_underscore_pat_with_typars() {
    assert_asts_match("let _.M<'a> = 1\n");
}

/// FCS's `access UNDERSCORE DOT pathOp`: the modifier lands in the elided
/// accessibility slot.
#[test]
fn diff_underscore_pat_access_prefixed() {
    assert_asts_match("let private _.M = 1\n");
}

/// The access-prefixed form with an argument.
#[test]
fn diff_underscore_pat_access_prefixed_with_arg() {
    assert_asts_match("let internal _.M x = 1\n");
}

/// A multi-segment tail.
#[test]
fn diff_underscore_pat_multi_segment() {
    assert_asts_match("match x with\n| _.A.B -> 1\n");
}

/// A lowercase tail: dotted, so still `LongIdent`, never `Named`.
#[test]
fn diff_underscore_pat_lowercase_tail() {
    assert_asts_match("match x with\n| _.m -> 1\n");
}

/// Whitespace between the `_` and the `.` (FCS's grammar imposes no adjacency).
#[test]
fn diff_underscore_pat_spaced_dot() {
    assert_asts_match("match x with\n| _ .M -> 1\n");
}

/// Whitespace after the `.`.
#[test]
fn diff_underscore_pat_spaced_after_dot() {
    assert_asts_match("match x with\n| _. M -> 1\n");
}

/// A backtick-quoted tail segment.
#[test]
fn diff_underscore_pat_quoted_tail() {
    assert_asts_match("match x with\n| _.``M N`` -> 1\n");
}

/// A `pathOp` ending in an operator name.
#[test]
fn diff_underscore_pat_operator_tail() {
    assert_asts_match("match x with\n| _.(+) -> 1\n");
}

/// A `pathOp` ending in an active-pattern name.
#[test]
fn diff_underscore_pat_active_pattern_tail() {
    assert_asts_match("match x with\n| _.(|A|_|) -> 1\n");
}

/// The operator-tailed form as a curried argument.
#[test]
fn diff_underscore_pat_operator_tail_curried_arg() {
    assert_asts_match("let f _.(+) y = 1\n");
}

/// Nested inside a paren / tuple pattern.
#[test]
fn diff_underscore_pat_in_tuple() {
    assert_asts_match("match x with\n| (_.M, _) -> 1\n");
}

/// As the argument of a union-case pattern.
#[test]
fn diff_underscore_pat_as_case_arg() {
    assert_asts_match("match x with\n| Some _.M -> 1\n| _ -> 2\n");
}

/// In list and array patterns.
#[test]
fn diff_underscore_pat_in_list_and_array() {
    assert_asts_match("match x with\n| [ _.M; _ ] -> 1\n| [| _.N |] -> 2\n| _ -> 3\n");
}

/// A lambda argument (FCS desugars it into a match; the normaliser compares the
/// pattern).
#[test]
fn diff_underscore_pat_lambda_arg() {
    assert_asts_match("let g = fun _.M -> 1\n");
}

/// A `function` clause.
#[test]
fn diff_underscore_pat_function_clause() {
    assert_asts_match("let g = function _.M -> 1 | _ -> 2\n");
}

/// Under `as` and `|`.
#[test]
fn diff_underscore_pat_under_as_and_or() {
    assert_asts_match("match x with\n| _.M as y -> 1\n| _.N | _.O -> 2\n");
}

/// A `try … with` handler.
#[test]
fn diff_underscore_pat_try_with() {
    assert_asts_match("try () with _.M -> ()\n");
}

/// A bare `_` stays the wildcard in every one of these positions — the new
/// head must not swallow a following token that is not a `.`.
#[test]
fn diff_bare_underscore_stays_wildcard() {
    assert_asts_match("match x with\n| _ -> 1\n");
    assert_asts_match("let f _ y = 1\n");
    assert_asts_match("match x with\n| Some _, M -> 1\n| _ -> 2\n");
}

/// A trailing `_.` with no segment after the dot is a parse error on both sides,
/// and the declarations around it survive. FCS discards the whole clause list,
/// which is worse recovery than ours, so the trees are graded outside the damage
/// rather than compared whole.
#[test]
fn recover_underscore_dot_without_segment() {
    for source in [
        "module M\nlet a = 1\nlet f x =\n    match x with\n    | _. -> 1\nlet b = 2\n",
        "module M\nlet a = 1\nlet _. = 1\nlet b = 2\n",
        "module M\nlet a = 1\nlet f _. = 1\nlet b = 2\n",
        "module M\nlet a = 1\nlet private _. = 1\nlet b = 2\n",
    ] {
        assert!(
            !borzoi_cst::parser::parse(source).errors.is_empty(),
            "`_.` with no segment must error: {source:?}"
        );
        crate::common::recovery::assert_recovered_trees_agree(source, 1);
    }
}

// ---------------------------------------------------------------------------
// The language-version gate.
// ---------------------------------------------------------------------------

/// Each fixture holds at most one `_.`-rooted pattern (none, for the controls):
/// below 4.7 FCS's gate is a hard parse error, and its recovery skips past any
/// second one without reporting it. The grid parses each at every version band
/// and holds both the tree and the gate diagnostic to FCS.
const GATE_FIXTURES: &[&str] = &[
    "match x with\n| _.M -> 1\n| _ -> 2\n",
    "let _.M = 1\n",
    "let f _.M = 1\n",
    "let private _.M = 1\n",
    "match x with\n| _.M y -> 1\n",
    "match x with\n| _.(+) -> 1\n",
    "match x with\n| _.(|A|_|) -> 1\n",
    "match x with\n| _ .M -> 1\n",
    "match x with\n| (_.M, _) -> 1\n",
    "let g = fun _.M -> 1\n",
    // The member self-id reaches FCS through the same production, so it is
    // gated too.
    "type T() =\n  member _.M = 1\n",
    "type T() =\n  member _.M x = x\n",
    "type T() =\n  member _.(+) x = x\n",
    "type T() =\n  member private _.M = 1\n",
    "let x = { new System.IDisposable with member _.Dispose() = () }\n",
    // Controls: a backtick-quoted `_` is an ordinary `pathOp` ident, not the
    // `UNDERSCORE` token, so it is never gated; nor is the bare wildcard.
    "match x with\n| ``_``.M -> 1\n",
    "type T() =\n  member ``_``.M = 1\n",
    "match x with\n| _ -> 1\n",
];

/// The bands the gate distinguishes, with the `<LangVersion>` token FCS reads.
const GATE_BANDS: &[(LanguageVersion, &str)] = &[
    (LanguageVersion::V4_6, "4.6"),
    (LanguageVersion::V4_7, "4.7"),
    (LanguageVersion::V8_0, "8.0"),
    (LanguageVersion::V10_0, "10.0"),
    (LanguageVersion::Preview, "preview"),
];

/// FCS's "Unexpected symbol '.' in member definition" (`parsUnexpectedSymbolDot`).
const FS0010: i64 = 10;

fn parse_at(source: &str, lang: LanguageVersion) -> Parse {
    let symbols = HashSet::new();
    parse_with_options(
        source,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &symbols,
            lang,
        },
    )
}

fn spans(errors: &[borzoi_cst::parser::ParseError]) -> BTreeSet<(usize, usize)> {
    errors.iter().map(|e| (e.span.start, e.span.end)).collect()
}

fn as_set(spans: Vec<Range<usize>>) -> BTreeSet<(usize, usize)> {
    spans.into_iter().map(|s| (s.start, s.end)).collect()
}

/// Every `.` that follows a bare `_` token in `source` — the spans FCS's gate
/// reports at (`rhs parseState 2`). Computed from the raw text, independently of
/// both parsers, so the grid cannot pass on a gate that flags the wrong dot.
fn underscore_dot_spans(source: &str) -> BTreeSet<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut out = BTreeSet::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'_' {
            continue;
        }
        // A bare `_`: not part of an identifier or a backtick-quoted name.
        let prev = i.checked_sub(1).map(|j| bytes[j]);
        if prev.is_some_and(|p| p.is_ascii_alphanumeric() || p == b'_' || p == b'`') {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }
        if bytes.get(j) == Some(&b'.') {
            out.insert((j, j + 1));
        }
    }
    out
}

/// At each band: where FCS accepts, the trees agree and neither side errors;
/// where FCS rejects, its FS0010 spans are exactly the `_.` dots, ours are the
/// same spans, the gate is the *only* difference from our maximal parse, and our
/// tree is that maximal parse unchanged.
#[test]
fn underscore_pat_gate_matches_fcs_at_every_band() {
    for source in GATE_FIXTURES {
        let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
        tmp.write_all(source.as_bytes()).expect("write source");
        let expected_gate = underscore_dot_spans(source);
        let maximal = parse_at(source, LanguageVersion::Preview);
        assert!(
            maximal.errors.is_empty(),
            "the maximal parse of {source:?} must be clean: {:?}",
            maximal.errors
        );
        for &(lang, lang_text) in GATE_BANDS {
            let json = fcs_ast_batch_at(tmp.path(), lang_text);
            let ours = parse_at(source, lang);
            assert_eq!(
                format!("{:#?}", ours.root),
                format!("{:#?}", maximal.root),
                "the gate must not reshape the tree ({lang_text}, {source:?})",
            );
            let fcs_gate = as_set(fcs_diagnostic_spans(&json, source, FS0010));
            if lang.supports_single_underscore_pattern() {
                assert!(
                    !fcs_parse_had_errors(&json),
                    "FCS rejects {source:?} at {lang_text}, where the gate says it is open",
                );
                assert!(
                    ours.errors.is_empty(),
                    "we reject {source:?} at {lang_text}: {:?}",
                    ours.errors
                );
                let fcs = normalised_ast::normalise_fcs_dump(&json);
                let rust = normalised_ast::normalise_parse(&ours);
                assert_eq!(
                    rust, fcs,
                    "AST divergence at {lang_text} for {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
                );
            } else {
                assert_eq!(
                    fcs_gate, expected_gate,
                    "FCS's FS0010 spans at {lang_text} are not the `_.` dots of {source:?}",
                );
                assert_eq!(
                    fcs_parse_had_errors(&json),
                    !expected_gate.is_empty(),
                    "FCS's verdict at {lang_text} for {source:?}",
                );
                assert_eq!(
                    spans(&ours.errors),
                    expected_gate,
                    "our gate diagnostics at {lang_text} for {source:?}: {:?}",
                    ours.errors
                );
            }
        }
    }
}

/// The gate's message is FCS's `parsUnexpectedSymbolDot`.
#[test]
fn underscore_pat_gate_message_is_fcs_wording() {
    let source = "let _.M = 1\n";
    let p = parse_at(source, LanguageVersion::V4_6);
    assert_eq!(p.errors.len(), 1, "{:?}", p.errors);
    assert_eq!(
        p.errors[0].message,
        "Unexpected symbol '.' in member definition. Expected 'with', '=' or other token.",
    );
    assert_eq!(&source[p.errors[0].span.clone()], ".");
}
