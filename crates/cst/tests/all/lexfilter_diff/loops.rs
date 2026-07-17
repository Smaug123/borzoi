//! `do`/`for`/`while`/`done` loop and do-block offside handling.

use crate::common::assert_filtered_streams_match;

/// `do` followed by EOF on the next line. FCS uses `tryPushCtxtSeqBlock`
/// here (LexFilter.fs:2327): the strict undentation push of the body
/// SeqBlock at the EOF lookahead fails (EOF column is offside vs CtxtDo),
/// and because `useFallback=false`, *no* SeqBlock is pushed and *no*
/// OBLOCKBEGIN is emitted. Without the `try` variant the unconditional
/// push opens a spurious block at the EOF position, leaving extra
/// OBLOCKBEGIN/OBLOCKEND in the stream. (#10)
#[test]
fn diff_filtered_do_then_eof() {
    assert_filtered_streams_match("let f () = do\n");
}

/// `while` loop inside a `let` RHS. Exercises CtxtWhile (pushed by WHILE,
/// passes through unchanged), which pops silently when offside —
/// `endTokenForACtxt` returns None. The body is a `do`-clause whose EOF
/// cascade emits `ODECLEND`; CtxtWhile then reprocesses silently leaving
/// the outer LetDecl to emit its own `ODECLEND`. (LexFilter.fs:2521, 2043)
#[test]
fn diff_filtered_while_do() {
    assert_filtered_streams_match("let f = while true do 1\n");
}

/// `for` loop inside a `let` RHS. Exercises CtxtFor (pushed by FOR,
/// balances IN per LexFilter.fs:1274 so IN passes through without
/// force-closing the for-context), CtxtDo (DO → OffsideDo and pushes a
/// CtxtSeqBlock(AddBlockEnd) for the body, LexFilter.fs:2324), and the
/// EOF cascade that pops them in stack order — CtxtDo emits ODECLEND,
/// CtxtFor emits nothing.
#[test]
fn diff_filtered_for_in_do() {
    assert_filtered_streams_match("let f xs = for x in xs do x\n");
}

/// `for ... do ... done`. Exercises the DONE+CtxtDo balance arm
/// (LexFilter.fs:1689): DONE pops CtxtDo and is rewritten as ODECLEND at
/// DONE's own range, then the DONE token itself is swallowed. Before the
/// balance arm fires, `tokenForcesHeadContextClosure` (LexFilter.fs:1560)
/// pops the body's `CtxtSeqBlock(AddBlockEnd)` (emitting an OBLOCKEND
/// swallowed by the harness). The synthesised ODECLEND then passes
/// through unchanged — CtxtFor's offside-pop doesn't fire because `done`
/// is to the right of `for`, and CtxtFor pops silently at EOF.
#[test]
fn diff_filtered_for_in_do_done() {
    assert_filtered_streams_match("let f xs = for x in xs do x done\n");
}

/// `while ... do ... done`. Same DONE+CtxtDo machinery as the for-loop
/// variant; CtxtWhile is silent on offside-pop so the only visible
/// virtual token from the loop construct is the ODECLEND replacing DONE.
#[test]
fn diff_filtered_while_do_done() {
    assert_filtered_streams_match("let f = while true do 1 done\n");
}

/// Multi-line `for … do … done` with `done` aligned under `for`. The
/// synthesised `OffsideDeclEnd` (replacing DONE) is reprocessed against
/// `CtxtFor` and the enclosing `CtxtSeqBlock(NotFirst)`. FCS treats the
/// virtual `ODECLEND` as both an `isForLoopContinuator` and an
/// `isSeqBlockElementContinuator` (LexFilter.fs:314, 372), so the
/// for-context stays open and no `OffsideBlockSep` is emitted in front
/// of the `ODECLEND`. Without those continuator extensions, our port
/// pops `CtxtFor` early and emits a spurious `OBLOCKSEP`.
#[test]
fn diff_filtered_for_in_do_done_aligned() {
    assert_filtered_streams_match("let f xs =\n    for x in xs do\n        x\n    done\n");
}

/// Aligned `done` followed by another statement aligned with the enclosing
/// SeqBlock. After DONE→ODECLEND, the next real token (`1`) starts a new
/// SeqBlock element: `OffsideBlockSep` must fire between them. Stresses
/// that the virtual-continuator extension to `is_seq_block_element_continuator`
/// only suppresses OBLOCKSEP for the reprocessed virtual itself, not for
/// the following real token.
#[test]
fn diff_filtered_for_in_do_done_then_stmt() {
    assert_filtered_streams_match("let f xs =\n    for x in xs do\n        x\n    done\n    1\n");
}

/// Misindented `done` aligned with the enclosing `let`. After DONE→ODECLEND
/// at column 0, FCS treats the reprocessed `ODECLEND` as an
/// `isLetContinuator` (LexFilter.fs:332-340), so `CtxtLetDecl` stays open
/// and emits its own `ODECLEND` only at EOF — the two `ODECLEND`s land at
/// different ranges. Without that extension, our port pops `CtxtLetDecl`
/// on the reprocessed virtual and emits both `ODECLEND`s at `done`'s range.
#[test]
fn diff_filtered_done_misaligned_with_let() {
    assert_filtered_streams_match("let _ =\n    for x in [1] do\n        x\ndone\n");
}

/// Nested `do … done` where the inner `done` is offside relative to the
/// outer body's SeqBlock (it sits at the outer `do`'s column). The
/// reprocessed `Virtual::DeclEnd` first pops the outer body SeqBlock
/// (strict-`<` offside), then meets the outer `CtxtDo` at exactly its own
/// column. FCS treats the reprocessed virtual as an `isDoContinuator`
/// (LexFilter.fs:254-264), bumping the offside-pop guard to `+1 <=` so the
/// outer `CtxtDo` stays open; without that extension our port pops it and
/// inserts a spurious `OffsideDeclEnd` at the inner `done`'s range.
#[test]
fn diff_filtered_inner_done_aligned_with_outer_do() {
    assert_filtered_streams_match("let _ =\n    do\n        do\n            ()\n    done\n");
}

/// `done` closing a nested `do` inside an `if … then` branch, aligned with
/// the `if`/`then` column, followed by an `else`. The reprocessed
/// `Virtual::DeclEnd` cascades through the then-body SeqBlock, then through
/// `CtxtThen` and `CtxtIf`. FCS treats virtual endings as both
/// `isThenBlockContinuator` (LexFilter.fs:247-252) and
/// `isIfBlockContinuator` (LexFilter.fs:202-218), so both conditional
/// contexts stay open across the virtual; the subsequent `else` then
/// force-closes `CtxtThen` against the still-live `CtxtIf` and pushes
/// `CtxtElse` as usual. Without those continuator extensions, our port pops
/// `CtxtIf` on the virtual, the trailing `else` finds no balancing
/// conditional, and the stream diverges from FCS.
#[test]
fn diff_filtered_done_aligned_with_then_else_follows() {
    assert_filtered_streams_match(
        "let f c =\n    if c\n    then\n        do ()\n    done\n    else ()\n",
    );
}

/// Aligned `done` followed by a real `in` keyword. The reprocessed
/// `OffsideDeclEnd` lands at the same column as `for`, and FCS treats it as
/// an `isForLoopContinuator` (LexFilter.fs:314-323), so `CtxtFor` stays open
/// across the virtual. The next real token (`in`) then balances against
/// `CtxtFor` (LexFilter.fs:1075) and is emitted as a plain `In`. Without
/// extending `is_for_loop_continuator` with virtual reprocessed endings,
/// our port pops `CtxtFor` on the reprocessed virtual; the trailing `in`
/// then reaches `CtxtLetDecl` and is rewritten to a spurious `OffsideDeclEnd`.
#[test]
fn diff_filtered_done_aligned_then_in() {
    assert_filtered_streams_match("let y =\n    for x in [1] do\n        ()\n    done in y\n");
}

/// `do!` inside a computation-expression-style brace block. Same CtxtDo
/// machinery as `do`, but FCS's `(DO | DO_BANG)` arm at LexFilter.fs:2324
/// emits `ODO_BANG` instead of `ODO`. The trailing `}` force-closes the
/// CtxtDo's SeqBlock and emits ODECLEND for CtxtDo before balancing the
/// outer CtxtParen.
#[test]
fn diff_filtered_do_bang() {
    assert_filtered_streams_match("let f = async { do! x }\n");
}
