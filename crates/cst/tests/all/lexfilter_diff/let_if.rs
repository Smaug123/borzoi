//! `let` bindings and `if`/`then`/`else` offside shapes.

use crate::common::assert_filtered_streams_match;

/// The simplest possible binding. FCS produces
/// `OffsideLet / Identifier / Equals / OffsideBlockBegin / Int32 / OffsideDeclEnd`.
#[test]
fn diff_filtered_trivial_let_binding() {
    assert_filtered_streams_match("let x = 1\n");
}

/// Adds an `in` to the trivial binding. Exercises the `IN+CtxtLetDecl`
/// balancing rule (LexFilter.fs:1679): `in` swallows the `Let`-decl context
/// and is replaced by `ODECLEND` at the `in` token's range. Also exercises
/// `tokenForcesHeadContextClosure` for `IN` (LexFilter.fs:1561) — it must
/// first close the RHS `CtxtSeqBlock(AddBlockEnd)` (emitting `OBLOCKEND`
/// internally, swallowed by the harness) before the IN/LetDecl pop fires.
#[test]
fn diff_filtered_let_in_binding() {
    assert_filtered_streams_match("let x = 1 in x\n");
}

/// Top-level binding followed by a bare expression on a fresh line at the
/// same column. Forces three new offside rules to fire in order:
/// 1. SeqBlock(AddBlockEnd) pop (LexFilter.fs:1803) — the bare `1` is offside
///    relative to the RHS block anchored at column 8, emitting `OBLOCKEND`
///    (swallowed by the harness).
/// 2. CtxtLetDecl pop (LexFilter.fs:1939) — `1` at column 0 ≤ `let`'s column 0,
///    emitting `ODECLEND` at the `1`'s range.
/// 3. SeqBlock(NotFirstInSeqBlock) `OBLOCKSEP` (LexFilter.fs:1912) — same column
///    as the outer block, different line, zero-width at the `1`'s start.
#[test]
fn diff_filtered_let_then_bare_expr() {
    assert_filtered_streams_match("let x = 1\n1\n");
}

/// `in` on a new line aligned with `let`. FCS still swallows the `in` and
/// emits `ODECLEND` at its range (the `IN+CtxtLetDecl` arm at LexFilter.fs:1679
/// runs before the LetDecl offside-pop at 1939), so the rule order in our
/// dispatch must match — IN-balance first, indentation pop second.
#[test]
fn diff_filtered_let_then_aligned_in() {
    assert_filtered_streams_match("let x = 1\nin x\n");
}

/// Mutually-recursive bindings. `and` aligned with `let` must NOT close the
/// `CtxtLetDecl` — FCS's `isLetContinuator` (LexFilter.fs:336) bumps the pop
/// guard from `tokenStartCol <= offsidePos.Column` to `+1 <= …`, keeping the
/// declaration scope open so the next `=` can start a fresh RHS block.
#[test]
fn diff_filtered_let_rec_and() {
    assert_filtered_streams_match("let rec f = 1\nand g = 2\n");
}

/// Two top-level bindings, same column. Exercises the full pop cascade
/// at the start of the second `let`: SeqBlock (RHS of first let) pops →
/// LetDecl pops → SeqBlock(NotFirst) emits OBLOCKSEP for the outer
/// top-level block → second `let` pushes a fresh LetDecl. Pins the
/// byte-level zero-width spans for ODECLEND/OBLOCKSEP across a line break
/// so a future change to `insert_token_from_prev_to_current` is caught.
#[test]
fn diff_filtered_two_top_level_lets() {
    assert_filtered_streams_match("let f x = 1\nlet g y = 2\n");
}

/// `use` binding inside a `let` RHS. FCS maps both `let` and `use` to the
/// same `FSharpTokenKind.OffsideLet` (ServiceLexing.fs:1418), so the LET
/// rule matches `Token::Use` as well as `Token::Let` —
/// mirroring FCS's `LET _` arm whose internal token carries an `isUse` bool.
#[test]
fn diff_filtered_use_binding() {
    assert_filtered_streams_match("let f x = use y = x in y\n");
}

/// `if c then 1 else 2` inside a `let` RHS. Exercises CtxtIf (pushed by
/// IF/ELIF, passthrough), CtxtThen (pushed by THEN, also pushes
/// SeqBlock(AddBlockEnd) for the body, emits OTHEN), and CtxtElse (same
/// shape, emits OELSE). The critical mechanism is FCS's
/// `tokenForcesHeadContextClosure` on ELSE (LexFilter.fs:1558): when ELSE
/// arrives, the then-body SeqBlock + CtxtThen on top of the stack don't
/// balance ELSE, but a deeper CtxtIf does — so ELSE force-closes them
/// (emitting an OBLOCKEND that the harness drops) before the ELSE push
/// rule fires. CtxtIf/CtxtThen/CtxtElse all pop silently at EOF; only the
/// outer LetDecl emits ODECLEND.
#[test]
fn diff_filtered_if_then_else() {
    assert_filtered_streams_match("let f c = if c then 1 else 2\n");
}

/// `if`/`then`/`else` aligned at the same column as `if`. Forces FCS's
/// `isIfBlockContinuator` (LexFilter.fs:202): without it, the CtxtIf
/// offside-pop guard `tokenStartCol <= offsidePos.Column` fires the moment
/// `then` aligns with `if`, popping CtxtIf before the THEN push rule runs
/// — and an aligned `else` then sees no surrounding CtxtIf, so its
/// force-closure can't find a balance and the THEN-body SeqBlock isn't
/// closed. The continuator bumps the guard to `+1 <=` so aligned
/// THEN/ELSE/ELIF keep CtxtIf open.
#[test]
fn diff_filtered_if_then_else_aligned() {
    assert_filtered_streams_match("let f c =\n    if c\n    then 1\n    else 2\n");
}

/// Same-line `else if` chain. FCS's ELSE arm peeks the next token; if it's
/// IF on the same line, the IF is consumed, CtxtIf is pushed at the `else`'s
/// position, and a single `ELIF` token spans `else if` (LexFilter.fs:2486).
/// Without the rewrite, OELSE + OBLOCKBEGIN + IF are emitted instead and
/// the stream gains two spurious tokens — and worse, the inner if pushes
/// a second CtxtIf so a final aligned `else` can attach to the wrong
/// conditional.
#[test]
fn diff_filtered_else_if_chain() {
    assert_filtered_streams_match("let g a b c = if a then 1 else if b then 2 else 3\n");
}

/// `if c then x else\nlet y = 1` — the else-body deindents to column 0,
/// aligned with `if`. FCS's `relaxWhitespace2` arm at LexFilter.fs:866
/// (MAJOR PERMITTED UNDENTATION for CtxtElse :: CtxtIf) treats CtxtElse
/// as transparent so the body's offside is gated by whatever sits below
/// the `if`, not by the `else`'s own column. With strict pushes everywhere,
/// the SeqBlock anchored at `let` (col 0) must be accepted even though
/// `else` sits at col 7. Pins the L866 arm.
#[test]
fn diff_filtered_if_then_else_body_deindent_to_if() {
    assert_filtered_streams_match("if true then 1 else\nlet y = 1\ny\n");
}
