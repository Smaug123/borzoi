//! Differential test (`parser::parse` vs FCS): FS1161 "TABs are not allowed in
//! F# code". FCS's lexer splits whitespace into `truewhite = [' ']` and
//! `offwhite = ['\t']`; a maximal run of tabs in code position (`offwhite+`,
//! `lex.fsl:705`) is a recoverable error — FCS reports FS1161 against the tab
//! run's range, then treats it as ordinary whitespace, so the tree is
//! unchanged. Tabs inside comments, strings, and char literals are consumed by
//! other lexer states and carry no diagnostic.
//!
//! Scope note: we diagnose tabs in ordinary code position only. Tabs on `#…`
//! directive lines are deliberately *not* diagnosed (FCS's per-directive-rule
//! tab handling isn't modelled here — see `parser::tab_diagnostics`), so this
//! suite has no directive-line cases; those are covered structurally in
//! `parser/tests/tabs.rs`.
//!
//! The positive cases use [`assert_asts_match_with_diagnostic`] (AST agrees +
//! we flag the same byte span FCS does); the negative cases use plain
//! [`assert_asts_match`] to confirm we don't *over*-report tabs that FCS leaves
//! alone.

use crate::common::{assert_asts_match, assert_asts_match_with_diagnostic};

/// FS1161 = 1161. Named for readability at the call sites.
const FS_TABS_NOT_ALLOWED: i64 = 1161;

// ---- positive cases: FCS reports FS1161, recovers, and our span matches ----

/// A single tab between `=` and the body. FCS: FS1161 over the one tab; the
/// binding still parses as `let x = 1`.
#[test]
fn diff_tab_between_equals_and_body() {
    assert_asts_match_with_diagnostic("let x =\t1\n", FS_TABS_NOT_ALLOWED);
}

/// A run of two consecutive tabs is *one* FS1161 spanning both (FCS's
/// `offwhite+` is maximal-munch), not two single-tab errors.
#[test]
fn diff_two_consecutive_tabs_are_one_error() {
    assert_asts_match_with_diagnostic("let x =\t\t1\n", FS_TABS_NOT_ALLOWED);
}

/// `space tab space`: FCS lexes the surrounding spaces as `truewhite` and only
/// the tab as `offwhite`, so the FS1161 range covers *just* the tab — our scan
/// must report the same maximal tab run, not the whole whitespace gap.
#[test]
fn diff_tab_flanked_by_spaces_flags_only_the_tab() {
    assert_asts_match_with_diagnostic("let x = \t 1\n", FS_TABS_NOT_ALLOWED);
}

/// Two separate tab runs on one line yield two separate FS1161 diagnostics.
#[test]
fn diff_two_separate_tab_runs() {
    assert_asts_match_with_diagnostic("let z =\ta +\tb\n", FS_TABS_NOT_ALLOWED);
}

/// A tab between an application's function and argument.
#[test]
fn diff_tab_between_app_head_and_arg() {
    assert_asts_match_with_diagnostic("let y = f\tx\n", FS_TABS_NOT_ALLOWED);
}

/// The original FCS regression fixture (`E_TABsNotAllowedIndentOff.fs`): a
/// leading tab before a block comment and a `type` definition. FCS reports
/// FS1161 over the leading tab and still parses `type T = class end`.
#[test]
fn diff_leading_tab_before_block_comment_and_type() {
    assert_asts_match_with_diagnostic(
        "\t(* <- invisible tab! *) type T = class end\n",
        FS_TABS_NOT_ALLOWED,
    );
}

// ---- negative cases: tabs FCS leaves alone, so we must not flag them --------

/// A tab inside a line comment is consumed by the comment lexer state — no
/// FS1161.
#[test]
fn diff_tab_in_line_comment_not_flagged() {
    assert_asts_match("let x = 1 //\tcomment\n");
}

/// A tab inside a block comment — no FS1161.
#[test]
fn diff_tab_in_block_comment_not_flagged() {
    assert_asts_match("let x = (*\t*) 1\n");
}

/// A literal tab inside a string is part of the string token — no FS1161.
#[test]
fn diff_tab_in_string_literal_not_flagged() {
    assert_asts_match("let x = \"a\tb\"\n");
}
