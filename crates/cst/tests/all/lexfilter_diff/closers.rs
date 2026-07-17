//! Closers dedented left of the enclosing `let` (relaxWhitespace2 ODUMMY).

use crate::common::assert_filtered_streams_match;

/// Paren closer dedented past the enclosing `let`'s column, with a
/// continuation token to the right of `let` on the next line. Exercises
/// FCS's `relaxWhitespace2OffsideRule` at LexFilter.fs:1473-1500: after the
/// paren is popped on `)`, FCS queues an `ODUMMY RPAREN` at the `)`'s
/// column (LexFilter.fs:1712) and that Dummy runs through the CtxtLetDecl
/// offside-pop arm (LexFilter.fs:1939) with the `+1` bump enabled —
/// `closer.col + 1 <= LetDecl.col` triggers the pop, emitting `ODECLEND`
/// at the `)`'s range. The next real token (`hello`) then arrives indented
/// to the right of the LetDecl anchor, so without the Dummy nothing would
/// have popped the LetDecl until EOF; the public stream would show
/// `ODECLEND` at the wrong byte range.
///
/// Source layout (columns matter):
/// ```text
/// 0123456789...
///        let a = (
///              1
///    )
///        hello
/// ```
/// `)` at col 3, `hello` at col 7, `let` at col 7. FCS emits
/// `OffsideDeclEnd @ L3:3-L3:4` (the closer's range), then `Identifier @
/// L4:7-L4:12` for `hello`. The Rust port without ODUMMY synthesis
/// instead pops LetDecl when `hello` arrives at col 7 and emits
/// `OffsideDeclEnd @ L4:7-L4:10` — same kind, wrong range.
#[test]
fn diff_filtered_let_paren_closer_left_of_let_then_aligned_ident() {
    assert_filtered_streams_match("       let a = (\n             1\n   )\n       hello\n");
}

/// `]` closer dedented past the enclosing `let`'s column. Same shape as
/// the bug-first test, just with `[ ]` brackets. Pins `RBrack` in the
/// extended `TokenRExprParen` Dummy gate at mod.rs:4686-4701.
#[test]
fn diff_filtered_let_brack_closer_left_of_let_then_aligned_ident() {
    assert_filtered_streams_match("       let a = [\n             1\n   ]\n       hello\n");
}

/// `}` closer dedented past the enclosing `let`'s column. `RBrace` is
/// FCS-swallowed (LexFilter.fs:2834), so the closer doesn't appear in
/// the FCS stream, but its Dummy still runs through the offside arms.
#[test]
fn diff_filtered_let_record_brace_closer_left_of_let_then_aligned_ident() {
    assert_filtered_streams_match(
        "       let a = { x = 1\n                y = 2\n   }\n       hello\n",
    );
}

/// `|]` (array closer) dedented past the enclosing `let`'s column. Pins
/// `BarRBrack` in the extended TokenRExprParen Dummy gate.
#[test]
fn diff_filtered_let_arr_closer_left_of_let_then_aligned_ident() {
    assert_filtered_streams_match("       let a = [|\n             1\n   |]\n       hello\n");
}

/// `|}` (anonymous-record closer) dedented past the enclosing `let`'s
/// column. Pins `BarRBrace` in the extended TokenRExprParen Dummy gate.
#[test]
fn diff_filtered_let_anon_record_closer_left_of_let_then_aligned_ident() {
    assert_filtered_streams_match(
        "       let a = {| x = 1\n                 y = 2\n   |}\n       hello\n",
    );
}

/// `type T = struct\n    val x : int\n  end\n` with `end` at col 2,
/// `type` at col 0, follow-on `let y = 2` at col 0. Without the relax
/// bump on the TypeDefns arm, the `end` would not pop TypeDefns
/// (col 2 \> 0), so the issue is whether the inner WithAsAugment-style
/// scope is handled correctly. This pins the `End` Dummy queue + the
/// TypeDefns arm interaction.
#[test]
fn diff_filtered_type_struct_end_dedented() {
    assert_filtered_streams_match("type T = struct\n    val x : int\n  end\nlet y = 2\n");
}
