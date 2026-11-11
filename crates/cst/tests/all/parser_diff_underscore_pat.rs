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
//! hard FS0010 parse error, member heads included. Upstream FCS has removed that
//! gate (dotnet/fsharp#20222), and we do not model it: the parse is clean at every
//! version, a recorded leniency (`docs/parser-plan.md`, "Known leniencies"). The
//! version tests below hold the trees to FCS from 4.7 up, and pin the leniency at
//! 4.6.

use std::collections::{BTreeSet, HashSet};
use std::io::Write;

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
// Language versions.
// ---------------------------------------------------------------------------

/// Each fixture holds at most one `_.`-rooted pattern (none, for the controls):
/// below 4.7 the pinned FCS's gate is a hard parse error, and its recovery skips
/// past any second one without reporting it.
const VERSION_FIXTURES: &[&str] = &[
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
    // The member self-id reaches FCS through the same production, so the pinned
    // FCS gates it too.
    "type T() =\n  member _.M = 1\n",
    "type T() =\n  member _.M x = x\n",
    "type T() =\n  member _.(+) x = x\n",
    "type T() =\n  member private _.M = 1\n",
    "let x = { new System.IDisposable with member _.Dispose() = () }\n",
    // Controls: a backtick-quoted `_` is an ordinary `pathOp` ident, not the
    // `UNDERSCORE` token, so FCS never gates it; nor is the bare wildcard.
    "match x with\n| ``_``.M -> 1\n",
    "type T() =\n  member ``_``.M = 1\n",
    "match x with\n| _ -> 1\n",
];

/// The bands at which the pinned FCS accepts `_.`-rooted patterns, with the
/// `<LangVersion>` token FCS reads.
const ACCEPTING_BANDS: &[(LanguageVersion, &str)] = &[
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

fn source_file(source: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    tmp
}

/// Every `.` that follows a bare `_` token in `source`: the spans the pinned
/// FCS's gate reports at (`rhs parseState 2`). Computed from the raw text,
/// independently of both parsers, so the leniency pin cannot pass on an FCS
/// error that is not the gate.
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

/// From F# 4.7 up, FCS accepts every fixture and the trees agree.
#[test]
fn underscore_pat_matches_fcs_from_4_7() {
    for source in VERSION_FIXTURES {
        let tmp = source_file(source);
        for &(lang, lang_text) in ACCEPTING_BANDS {
            let json = fcs_ast_batch_at(tmp.path(), lang_text);
            let ours = parse_at(source, lang);
            assert!(
                !fcs_parse_had_errors(&json),
                "FCS rejects {source:?} at {lang_text}",
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
        }
    }
}

/// The known leniency (`docs/parser-plan.md`, "Known leniencies"): at F# 4.6 the
/// pinned FCS reports FS0010 at exactly the `_.` dots, and we parse each fixture
/// clean and lossless, to the same tree as at preview.
///
/// Upstream FCS removed the gate in dotnet/fsharp#20222. Once the fcs-dump pin
/// moves past that change, FCS stops erroring here and this test fails. The
/// leniency is then gone: delete its entry in `docs/parser-plan.md` and this
/// test, and add `LanguageVersion::V4_6` to [`ACCEPTING_BANDS`].
#[test]
fn underscore_pat_below_4_7_is_lenient_lossless() {
    for source in VERSION_FIXTURES {
        let tmp = source_file(source);
        let json = fcs_ast_batch_at(tmp.path(), "4.6");
        let gated = underscore_dot_spans(source);
        let fcs_fs0010: BTreeSet<(usize, usize)> = fcs_diagnostic_spans(&json, source, FS0010)
            .into_iter()
            .map(|s| (s.start, s.end))
            .collect();
        assert_eq!(
            (fcs_fs0010, fcs_parse_had_errors(&json)),
            (gated.clone(), !gated.is_empty()),
            "at 4.6 the pinned FCS must error FS0010 at exactly the `_.` dots of {source:?}. \
             If it no longer errors, the fcs-dump pin has moved past dotnet/fsharp#20222 and \
             the leniency is gone: delete its entry in docs/parser-plan.md (\"Known \
             leniencies\") and this test, and add 4.6 to ACCEPTING_BANDS.",
        );
        let ours = parse_at(source, LanguageVersion::V4_6);
        assert!(
            ours.errors.is_empty(),
            "lenient: no error at 4.6 for {source:?}: {:?}",
            ours.errors
        );
        assert_eq!(ours.root.to_string(), *source, "lossless at 4.6");
        let preview = parse_at(source, LanguageVersion::Preview);
        assert_eq!(
            ours.root.structure(),
            preview.root.structure(),
            "the 4.6 tree of {source:?} is the preview tree",
        );
    }
}
