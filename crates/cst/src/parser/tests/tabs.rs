//! FS1161 "TABs are not allowed in F# code".
//!
//! FCS's lexer splits whitespace into `truewhite = [' ']` and
//! `offwhite = ['\t']`; a maximal run of tabs consumed by the main token rule
//! (`offwhite+`, `lex.fsl:705`) is a recoverable error — FCS reports FS1161
//! against the tab run's range, then treats it as ordinary whitespace.
//!
//! Cross-checked against FCS by the differential suite in
//! `tests/all/parser_diff_tabs.rs`; these are the local-only structural tests (and
//! the negative / `#if`-region cases the differential helper can't express).

use super::super::*;

/// FCS's exact wording, mirrored verbatim in [`super::super::tab_diagnostics`].
const TABS_MSG: &str = "TABs are not allowed in F# code";

/// The spans of every tab diagnostic our parser emitted for `source`.
fn tab_error_spans(source: &str) -> Vec<std::ops::Range<usize>> {
    parse(source)
        .errors
        .into_iter()
        .filter(|e| e.message == TABS_MSG)
        .map(|e| e.span)
        .collect()
}

#[test]
fn leading_tab_flags_the_tab() {
    // `\ttype T = class end` — FCS: FS1161 at (1,0-1,1).
    assert_eq!(tab_error_spans("\ttype T = class end\n"), vec![0..1]);
}

#[test]
fn tab_between_equals_and_body() {
    // `let x =\t1` — the tab is byte 7.
    assert_eq!(tab_error_spans("let x =\t1\n"), vec![7..8]);
}

#[test]
fn two_consecutive_tabs_are_one_maximal_run() {
    // `let x =\t\t1` — one diagnostic spanning both tabs (bytes 7..9), not two.
    assert_eq!(tab_error_spans("let x =\t\t1\n"), vec![7..9]);
}

#[test]
fn tab_flanked_by_spaces_flags_only_the_tab() {
    // `let x = \t 1` — spaces are `truewhite`; only the tab (byte 8) is flagged.
    assert_eq!(tab_error_spans("let x = \t 1\n"), vec![8..9]);
}

#[test]
fn two_separate_tab_runs_are_two_diagnostics() {
    // `let z =\ta +\tb` — tabs at bytes 7 and 11.
    assert_eq!(tab_error_spans("let z =\ta +\tb\n"), vec![7..8, 11..12]);
}

#[test]
fn tab_in_line_comment_is_not_flagged() {
    assert!(tab_error_spans("let x = 1 //\tcomment\n").is_empty());
}

#[test]
fn tab_in_block_comment_is_not_flagged() {
    assert!(tab_error_spans("let x = (*\t*) 1\n").is_empty());
}

#[test]
fn tab_in_string_literal_is_not_flagged() {
    assert!(tab_error_spans("let x = \"a\tb\"\n").is_empty());
}

#[test]
fn tabs_on_directive_lines_are_not_diagnosed() {
    // Whether a tab on a `#…` line is `offwhite+` or `anywhite` depends on the
    // exact directive rule FCS matched (see `tab_diagnostics`); this LSP doesn't
    // model that grammar, so we don't diagnose tabs on any line whose first
    // non-blank byte is `#`. This covers leading indents, internal whitespace,
    // recognised directives, `#light`/`#indent`, and malformed forms alike.
    for src in [
        "\t#if FOO\nlet x = 1\n#endif\nlet y = 2\n", // leading tab, `#if`
        "\t#nowarn \"57\"\nlet x = 1\n",             // `#nowarn` (anywhite* prefix)
        "\t#line 1 \"foo.fs\"\nlet x = 1\n",         // leading tab before `#line`
        "#line\t1 \"foo.fs\"\nlet x = 1\n",          // internal tab in `#line`
        "\t#light \"on\"\nlet x = 1\n",              // leading tab before `#light`
        "#light\t\"on\"\nlet x = 1\n",               // internal tab in `#light`
        "#indent\tfoo\n",                            // malformed `#indent`
        "#ifdef\tFOO\n",                             // invalid conditional
        "\t#r \"nuget: X\"\n",                       // unsupported `#r`
    ] {
        assert!(
            tab_error_spans(src).is_empty(),
            "expected no tab diagnostics on directive line {src:?}, got {:?}",
            tab_error_spans(src),
        );
    }
}

#[test]
fn tab_before_a_non_hash_line_is_still_flagged() {
    // The leading byte being `(` (a block comment), not `#`, keeps the line in
    // scope — this is the original `E_TABsNotAllowedIndentOff.fs` shape.
    assert_eq!(
        tab_error_spans("\t(* c *) type T = class end\n"),
        vec![0..1]
    );
}

#[test]
fn tab_in_an_eliminated_if_region_is_not_flagged() {
    // With `FOO` undefined the branch is dead; FCS lexes it under `skip`
    // (no FS1161) and we keep it as `InactiveCode` trivia, never `Whitespace`.
    assert!(tab_error_spans("#if FOO\n\tlet y = 2\n#endif\nlet z = 3\n").is_empty());
}

#[test]
fn tab_free_source_reports_no_tab_diagnostics() {
    assert!(tab_error_spans("let x = 1\nlet y = 2\n").is_empty());
}

mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every tab diagnostic we emit must (a) point at a *maximal run of tab
        /// bytes* — non-empty, all `\t`, bounded by a non-tab on each side (or
        /// the source edge), the structural invariant of FCS's `offwhite+` rule
        /// — and (b) not sit on a `#…` directive line, the line class we
        /// deliberately don't diagnose. Generated from a small alphabet that
        /// mixes tabs with code, strings, comments and `#`-lines so the scan is
        /// exercised in every context.
        #[test]
        fn tab_diagnostic_spans_are_maximal_tab_runs_off_directive_lines(
            src in proptest::collection::vec(
                prop_oneof![
                    Just("\t"), Just(" "), Just("\n"), Just("x"), Just("="),
                    Just("\""), Just("(*"), Just("*)"), Just("//"), Just("1"),
                    Just("#if"), Just("#line"), Just("#nowarn"), Just("#light"),
                ],
                0..40,
            ).prop_map(|parts| parts.concat()),
        ) {
            let bytes = src.as_bytes();
            for span in tab_error_spans(&src) {
                prop_assert!(span.start < span.end, "empty tab span {span:?} in {src:?}");
                prop_assert!(
                    bytes[span.clone()].iter().all(|&b| b == b'\t'),
                    "non-tab byte in tab span {span:?} of {src:?}",
                );
                prop_assert!(
                    span.start == 0 || bytes[span.start - 1] != b'\t',
                    "tab span {span:?} not left-maximal in {src:?}",
                );
                prop_assert!(
                    span.end == bytes.len() || bytes[span.end] != b'\t',
                    "tab span {span:?} not right-maximal in {src:?}",
                );
                let line_start = bytes[..span.start]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |p| p + 1);
                let first_non_blank = bytes[line_start..]
                    .iter()
                    .find(|&&b| !matches!(b, b' ' | b'\t'));
                prop_assert!(
                    first_non_blank != Some(&b'#'),
                    "tab span {span:?} is on a `#` directive line in {src:?}",
                );
            }
        }
    }
}
