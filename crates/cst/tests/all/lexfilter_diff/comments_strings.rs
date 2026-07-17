//! Block comments and multi-line string RHSs between decls.

use crate::common::assert_filtered_streams_match;

// ---- Multi-line tokens and the offside cursor ----
//
// These pin that the line/column cursor advances correctly *through* a
// token whose source text contains embedded newlines (block comments,
// triple-quoted / verbatim strings, `\`-continuation strings). Before the
// cursor counted embedded `\n`s, tokens after such a token were reported on
// the wrong line — and, when they sat on the multi-line token's *closing*
// physical line, at a wildly inflated column (the raw byte offset from the
// file start). That corrupted the offside comparisons below.

/// Two top-level `let`s separated by a multi-line block comment. The second
/// `let` sits on a later physical line than the first; the top-level SeqBlock
/// must still emit its `OBLOCKSEP` and the LetDecl pop at the right ranges.
/// With a stranded cursor the second `let`'s line was off by the comment's
/// embedded newlines.
#[test]
fn diff_filtered_block_comment_between_top_level_lets() {
    assert_filtered_streams_match("let f x = 1\n(* a\nb\nc *)\nlet g y = 2\n");
}

/// A top-level decl that begins on the *closing* line of a multi-line block
/// comment: `*) let g …`. Here the `let`'s column is what a stranded cursor
/// corrupted (it became the raw byte offset, not the column within line 3).
/// FCS places `let` at line 3, column 5.
#[test]
fn diff_filtered_decl_on_block_comment_closing_line() {
    assert_filtered_streams_match("let f x = 1\n(* a\nb *) let g y = 2\n");
}

/// `match` clauses whose leading `|`s begin on the closing line of a
/// multi-line comment and must align with a later `|`. The first `|`'s
/// column anchors the `CtxtMatchClauses`; the second clause aligns with it.
/// A stranded (inflated) column for the first `|` would push the alignment
/// anchor far right, so the second `|` would read as offside and the match
/// would close after one clause — diverging from FCS.
#[test]
fn diff_filtered_match_clauses_after_multiline_comment() {
    assert_filtered_streams_match("match x with\n(* c\n*) | A -> 1\n   | B -> 2\n");
}

/// A triple-quoted string spanning three physical lines as a `let` RHS,
/// followed by a top-level `let` on the line after the closing `\"\"\"`. The
/// second `let` must be offside relative to the RHS block and trigger the
/// LetDecl pop + top-level `OBLOCKSEP`; its line depends on counting the
/// string's two embedded newlines.
#[test]
fn diff_filtered_triple_string_rhs_then_decl() {
    assert_filtered_streams_match("let s = \"\"\"a\nb\nc\"\"\"\nlet y = 1\n");
}

/// A verbatim string spanning two physical lines as a `let` RHS, followed by
/// a top-level `let`. Same shape as the triple-quoted case for the verbatim
/// scanner.
#[test]
fn diff_filtered_verbatim_string_rhs_then_decl() {
    assert_filtered_streams_match("let s = @\"a\nb\"\nlet y = 1\n");
}
