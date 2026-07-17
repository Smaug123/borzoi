//! `lazy`/`assert` offside-operand relabel (`OLAZY`/`OASSERT`).
//!
//! When a `lazy`/`assert` operand is on a later line (or is a control-flow
//! keyword), FCS relabels the keyword to `OffsideLazy`/`OffsideAssert` and
//! pushes a `CtxtSeqBlock` (LexFilter.fs:2232, `isControlFlowOrNotSameLine`);
//! the same-line, non-control-flow form keeps the plain `Lazy`/`Assert`. These
//! assert our filtered stream matches FCS's token *kinds* for both.

use crate::common::assert_filtered_streams_match;

/// Same-line operand: no relabel, no operand block — plain `Lazy`.
#[test]
fn diff_filtered_lazy_same_line() {
    assert_filtered_streams_match("let x = lazy a\n");
}

/// Offside operand (next line): `Lazy` → `OffsideLazy` plus an inner
/// `OffsideBlockBegin` for the operand block.
#[test]
fn diff_filtered_lazy_offside_operand() {
    assert_filtered_streams_match("let x =\n    lazy\n        a\n        |> b\n");
}

/// `assert` takes the identical arm → `OffsideAssert`.
#[test]
fn diff_filtered_assert_offside_operand() {
    assert_filtered_streams_match("let x =\n    assert\n        a\n        |> b\n");
}

/// Same-line *control-flow* operand also relabels (`isControlFlowOrNotSameLine`
/// is true for a peeked `if`), even though the keyword and `if` share a line.
#[test]
fn diff_filtered_lazy_control_flow_same_line() {
    assert_filtered_streams_match("let x = lazy if a then b else c\n");
}
