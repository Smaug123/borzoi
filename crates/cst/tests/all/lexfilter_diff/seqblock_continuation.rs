//! `OffsideBlockSep` suppression for infix continuators in parens.

use crate::common::assert_filtered_streams_match;

/// Infix operator aligned with the SeqBlock column inside parens. FCS suppresses
/// `OffsideBlockSep` before infix continuators via `isSeqBlockElementContinuator`
/// (LexFilter.fs:1898). Without this, `+` at the same column fires an extra OBLOCKSEP.
#[test]
fn diff_filtered_infix_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    1\n    + 2\n)\n");
}

/// Swallowed closer (RPAREN) followed by an aligned statement. The `last_real_end`
/// tracker must advance past the swallowed `)` so the OffsideBlockSep before `2`
/// gets the correct span start (after `)`, not after `1`).
#[test]
fn diff_filtered_block_sep_after_swallowed_closer() {
    assert_filtered_streams_match("let x =\n    (1)\n    2\n");
}

/// `or` keyword aligned with the SeqBlock column inside parens. FCS's `isInfix`
/// includes `OR`, so OBLOCKSEP must be suppressed before `or` just like any
/// other infix operator.
#[test]
fn diff_filtered_or_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    true\n    or false\n)\n");
}

/// `??` aligned with the SeqBlock column. `??` lexes to `Token::QMarkQMark`
/// (a dedicated token, not `Op(_)`), so it must be listed explicitly in the
/// continuator predicate; FCS `isInfix` includes `QMARK_QMARK`.
#[test]
fn diff_filtered_qmark_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    ?? b\n)\n");
}

/// `<` aligned with the SeqBlock column. FCS explicitly excludes single `<`
/// (LESS) from `isInfix`, so an aligned `<` starts a new statement and
/// `OffsideBlockSep` must fire. `Token::Op("<")` must not be caught by the
/// catch-all `Op(_)` arm.
#[test]
fn diff_filtered_less_than_new_statement_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    < b\n)\n");
}

/// `.` (DOT) aligned with the SeqBlock column. FCS does not list DOT in
/// `isSeqBlockElementContinuator`, so an aligned `.property` starts a new
/// sequence element and `OffsideBlockSep` must fire.
#[test]
fn diff_filtered_dot_new_statement_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    .Length\n)\n");
}

/// `%%` (PERCENT_OP) aligned with the SeqBlock column. FCS `isInfix` does not
/// include PERCENT_OP, so OBLOCKSEP must fire before `%%`.
#[test]
fn diff_filtered_percent_new_statement_in_parens() {
    assert_filtered_streams_match("let x = (\n    1\n    %% 2\n)\n");
}

/// `!!` (PREFIX_OP) aligned with the SeqBlock column. FCS `isInfix` does not
/// include PREFIX_OP, so OBLOCKSEP must fire before `!!`.
#[test]
fn diff_filtered_prefix_op_new_statement_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    !! b\n)\n");
}

/// `%%+` is INFIX_STAR_DIV_MOD_OP in FCS (starts with `%` but is not `%`/`%%`),
/// so OBLOCKSEP must NOT fire before it.
#[test]
fn diff_filtered_percent_infix_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    1\n    %%+ 2\n)\n");
}

/// `!=` (INFIX_COMPARE_OP) aligned with the SeqBlock column. FCS `isInfix`
/// includes INFIX_COMPARE_OP, so OBLOCKSEP must NOT fire before `!=`.
#[test]
fn diff_filtered_neq_infix_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    != b\n)\n");
}

/// `!<` (PREFIX_OP) aligned with the SeqBlock column. FCS `isInfix` does NOT
/// include PREFIX_OP — only `!=` among `!`-prefixed ops is INFIX_COMPARE_OP —
/// so OBLOCKSEP must fire before `!<`.
#[test]
fn diff_filtered_bang_less_prefix_op_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    !< b\n)\n");
}

/// `?!` has a leading FCS `ignored_op_char` (`?`) but the bucket-defining
/// character is `!`, which is PREFIX_OP. OBLOCKSEP must fire before it.
#[test]
fn diff_filtered_qmark_bang_prefix_op_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    ?! b\n)\n");
}

/// `mod` lexes as INFIX_STAR_DIV_MOD_OP in FCS (infix), so OBLOCKSEP must
/// NOT fire before a `mod` that is aligned with the SeqBlock column.
#[test]
fn diff_filtered_mod_infix_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    mod b\n)\n");
}

/// `$!` starts with `$`, which is INFIX_COMPARE_OP in the FCS lexer even
/// though `$` is also an `ignored_op_char`. OBLOCKSEP must NOT fire.
#[test]
fn diff_filtered_dollar_infix_continuation_in_parens() {
    assert_filtered_streams_match("let x = (\n    a\n    $! b\n)\n");
}

/// `in` consumed as DeclEnd must update `last_real_end` so that the next
/// OBLOCKSEP span starts at the byte after `in`, not at the prior token's end.
#[test]
fn diff_filtered_in_consumed_last_real_end() {
    assert_filtered_streams_match("let x = 1 in\nx\n");
}

// ---- leading infix operator undented below the SeqBlock head (offside grace) --
//
// FCS grants a leading infix operator a grace of `infixTokenLength + 1` columns
// in the SeqBlock offside pop (LexFilter.fs:1833-1854), so a `|> f` / `+ e`
// continuation may sit *left of* the expression head without popping the block
// (no `BlockEnd`). The grace classifies on the *raw* infix token, before the
// `ADJACENT_PREFIX_OP` rewrite — so a prefix-capable operator glued to its
// operand (`f⏎ -x`) keeps the same grace and the block stays open too. We verify
// that block-structure faithfulness only for the *spaced* forms here: a glued
// `-x` is emitted by FCS as `AdjacentPrefixOperator`, a token kind we do not yet
// model (we emit `Minus`), and that kind gap is pre-existing and pervasive — it
// diverges even for single-line `f -x`, independent of this offside grace — so a
// glued stream-diff would fail on the kind, not the block structure it is meant
// to guard.

/// `|>` undented two columns below the application head — the ubiquitous FCS
/// test-DSL shape. The block must stay open (no `BlockEnd` before `|>`).
#[test]
fn diff_filtered_leading_pipe_undented_below_head() {
    assert_filtered_streams_match("module M\nlet test () =\n        g x\n     |> f\n     |> h\n");
}

/// `+` (infixTokenLength 1, grace 2) undented exactly two columns below the head
/// — the grace boundary. Still a continuation; no `BlockEnd`.
#[test]
fn diff_filtered_leading_plus_undented_below_head() {
    assert_filtered_streams_match("module M\nlet x =\n        a + b\n      + c\n");
}
