//! `<-` assignment offside shapes. Exercises the LARROW arm
//! (`LexFilter.fs:2318`): the r.h.s. opens a `CtxtSeqBlock(AddBlockEnd)` —
//! emitting `OffsideBlockBegin`/`OffsideBlockEnd` around the value — exactly
//! when `isControlFlowOrNotSameLine` holds, and nothing otherwise.

use crate::common::assert_filtered_streams_match;

/// Same-line, non-control-flow RHS. `isControlFlowOrNotSameLine` is false, so
/// LARROW pushes *no* block: the stream is just `Ident / LArrow / Int32`
/// (plus the surrounding top-level scaffolding). No `OffsideBlockBegin`.
#[test]
fn diff_filtered_assign_same_line() {
    assert_filtered_streams_match("x <- 1\n");
}

/// RHS on the next, indented line. `not (isSameLine())` ⇒ LARROW pushes
/// `CtxtSeqBlock(AddBlockEnd)`, so an `OffsideBlockBegin` precedes the value
/// and an `OffsideBlockEnd` follows it.
#[test]
fn diff_filtered_assign_offside_rhs() {
    assert_filtered_streams_match("x <-\n    1\n");
}

/// Same-line but control-flow RHS (`if`). `isControlFlowOrNotSameLine` is
/// true on the keyword, so LARROW still opens the block around the `if`.
#[test]
fn diff_filtered_assign_control_flow_rhs() {
    assert_filtered_streams_match("x <- if c then 1 else 2\n");
}

/// Two top-level assignments. The first (same-line) RHS opens no block, so
/// the second statement is fenced off purely by the outer module SeqBlock's
/// `OBLOCKSEP` — the two assignments must not collapse into one block.
#[test]
fn diff_filtered_assign_two_statements() {
    assert_filtered_streams_match("x <- 1\ny <- 2\n");
}
