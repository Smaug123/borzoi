//! `match`/`try` expressions and match-arm offside placement.

use crate::common::assert_filtered_streams_match;

/// Single-arm `match ... with` expression. Exercises CtxtMatch (pushed by
/// MATCH, passthrough), the WITH+CtxtMatch arm (LexFilter.fs:2347 — peeks the
/// next token, captures `leadingBar`, pushes CtxtMatchClauses, emits OWITH),
/// the RARROW gate's CtxtMatchClauses arm (LexFilter.fs:2308 — opens a
/// `SeqBlock(AddOneSidedBlockEnd)` for the arm body), and the EOF cascade:
/// SeqBlock(OneSided) → ORIGHT_BLOCK_END, CtxtMatchClauses → OEND
/// (`endTokenForACtxt`, LexFilter.fs:1525), CtxtMatch → silent reprocess.
#[test]
fn diff_filtered_match_with_single_arm() {
    assert_filtered_streams_match("let f x = match x with | _ -> 0\n");
}

/// Two-arm match. Same shape as the single-arm test but the second arm
/// exercises the CtxtMatchClauses offside-pop on BAR (LexFilter.fs:2099-2113):
/// the inner SeqBlock(OneSided) for the first arm must close before the second
/// `|` starts a new arm at the same column as the first. Without the
/// CtxtMatchClauses arm, the second BAR would either drop into the OneSided
/// body or pop the wrong context.
#[test]
fn diff_filtered_match_with_two_arms() {
    assert_filtered_streams_match("let f x =\n    match x with\n    | 1 -> 0\n    | _ -> 1\n");
}

/// `match` with a `when` guard. Exercises the WHEN+CtxtSeqBlock arm
/// (LexFilter.fs:2526 — pushes CtxtWhen, passes WHEN through). The `when`
/// arrives inside the SeqBlock(OneSided) of the previous arm-pattern? Actually
/// FCS parses the pattern before `when`, and the `when` clause sits between
/// pattern and `->`. So at WHEN, the head is the SeqBlock(OneSided) of the
/// previous arm — no wait, the very first arm's pattern is the scrutinee for
/// the WHEN. Let FCS dictate the exact stream; this test pins our port to it.
#[test]
fn diff_filtered_match_with_when_guard() {
    assert_filtered_streams_match("let f x = match x with | n when n > 0 -> n | _ -> 0\n");
}

/// `match` with a non-trivial scrutinee that leaves intermediate contexts on
/// the stack above CtxtMatch — here, an `if … then … else …` expression.
/// When `with` arrives, the head is the else-body's CtxtVanilla/SeqBlock,
/// not CtxtMatch. WITH must therefore be recognised as a balancing /
/// force-closing token (FCS `tokenBalancesHeadContext` LexFilter.fs:1266 +
/// `tokenForcesHeadContextClosure` LexFilter.fs:1552) so the scrutinee
/// subcontexts pop until CtxtMatch is at the head and the dedicated
/// `WITH + CtxtMatch` arm fires (emitting OWITH + opening CtxtMatchClauses).
#[test]
fn diff_filtered_match_scrutinee_if_then_else() {
    assert_filtered_streams_match("let f c = match if c then 1 else 2 with | _ -> 0\n");
}

/// Nested match where the inner match's scrutinee is itself an `if c then …
/// else …`. The inner `with` must force-close its own if/then/else scrutinee
/// contexts down to the inner CtxtMatch — but must *not* pop the outer
/// CtxtMatchClauses on the way. This is the case that motivated changing the
/// MatchClauses guard from "any on stack" to "head only" in
/// `token_forces_head_context_closure`. (Identified by codex review.)
#[test]
fn diff_filtered_nested_match_with_if_scrutinee() {
    assert_filtered_streams_match(
        "let f x c = match x with | _ -> match if c then 1 else 2 with | _ -> 0\n",
    );
}

/// Incomplete `match … with` at EOF (no arms typed yet). FCS's `tryPushCtxt`
/// rejects the CtxtMatchClauses push when the lookahead token is offside (here
/// the synthetic EOF), so the resulting stream is just OWITH followed by the
/// EOF cascade — no spurious OffsideEnd from a phantom MatchClauses context.
/// (LexFilter.fs:771-1020, 2347-2355) Trailing newline used so the EOF range
/// FCS reports matches our `[last_byte..last_byte)` synth — without it FCS
/// uses an `EOF.ColumnMinusOne`-based start span we don't yet replicate.
#[test]
fn diff_filtered_match_with_eof_after_with() {
    assert_filtered_streams_match("let f x = match x with\n");
}

/// Parenthesised `match` whose arms align with the opener `(`, not the
/// `match` keyword. FCS's `undentationLimit` has a special arm
/// (LexFilter.fs:790-793) — when the stack is `… :: CtxtParen(BEGIN|LPAREN)
/// :: CtxtSeqBlock :: CtxtMatch`, the limit is `min(match.col, paren.col)`,
/// not just `match.col`. Without it the strict push refuses a `|` at the
/// paren's column even though FCS accepts it. Pins the special arm.
#[test]
fn diff_filtered_paren_match_arm_at_paren_col() {
    assert_filtered_streams_match("(match x with\n| _ -> 0)\n");
}

/// Same idea as `diff_filtered_paren_match_arm_at_paren_col` but with
/// `begin`/`end`. Pins the `Begin` half of the `Opener::Paren | Opener::Begin`
/// disjunction in the `Match :: SeqBlock :: Paren(BEGIN|LPAREN)` arm so
/// either half regressing fails an independent test.
#[test]
fn diff_filtered_begin_match_arm_at_begin_col() {
    assert_filtered_streams_match("begin match 0 with\n| _ -> 0 end\n");
}

/// Single-line `try x with _ -> 0`. Pins the full CtxtTry push/dispatch
/// cycle: TRY pushes CtxtTry + SeqBlock(OneSided) for the body; WITH
/// force-closes the SeqBlock(OneSided) (emitting ORightBlockEnd) and
/// dispatches against CtxtTry (emit OWITH, push CtxtMatchClauses anchored
/// at the lookahead). The arm body opens a fresh SeqBlock(OneSided) on
/// RARROW; EOF cascade closes both and emits OEND for CtxtMatchClauses.
/// (LexFilter.fs:2589-2598, 2347-2355.)
#[test]
fn diff_filtered_try_with_single_line() {
    assert_filtered_streams_match("let f x = try x with _ -> 0\n");
}

/// Multi-line `try` / `with` aligned at the same column. Exercises the
/// CtxtTry offside-pop's `isTryBlockContinuator` (LexFilter.fs:236-245):
/// `with` aligned with `try` must NOT pop CtxtTry — the +1 grace under
/// the continuator predicate keeps it open until WITH's balance arm
/// swallows it. Without the predicate the WITH would pop CtxtTry first
/// and the dispatch would miss.
#[test]
fn diff_filtered_try_with_aligned() {
    assert_filtered_streams_match("let f x =\n    try\n        x\n    with _ -> 0\n");
}

/// `try x finally ()`. Pins the FINALLY+CtxtTry dispatch arm
/// (LexFilter.fs:2357-2360): FINALLY balances CtxtTry, force-closes the
/// inner SeqBlock(OneSided), and pushes a fresh SeqBlock(AddBlockEnd) for
/// the finally body. Finally is NOT rewritten — it passes through as a
/// real token (unlike WITH→OWITH).
#[test]
fn diff_filtered_try_finally() {
    assert_filtered_streams_match("let f x = try x finally ()\n");
}

/// `try … with` with multiple arms, including a `:?` type test. Pins that
/// CtxtMatchClauses pushed via WITH+CtxtTry behaves identically to one
/// pushed via WITH+CtxtMatch — the dispatch arm fuses both contexts.
/// (LexFilter.fs:2347 matches `(CtxtTry _ | CtxtMatch _) :: _`.)
#[test]
fn diff_filtered_try_with_multiple_arms() {
    assert_filtered_streams_match("let f x = try x with | :? System.Exception -> 0 | _ -> 1\n");
}

/// `try … with` nested inside a match arm — the doom-loop case from
/// #17 (LEXFILTER.md). The inner `with` MUST balance against CtxtTry,
/// NOT cut through the outer CtxtMatchClauses to the outer CtxtMatch.
/// Without CtxtTry the WITH MatchClauses-on-stack shim suppressed the
/// cut-through but couldn't emit the inner OWITH; with CtxtTry in place
/// the balance arm stops force-closure at CtxtTry and the dispatch fires.
/// Reintroduces `diff_filtered_try_with_inside_match_arm` deleted in
/// commit 3e833cc.
#[test]
fn diff_filtered_try_with_inside_match_arm() {
    assert_filtered_streams_match("let f x = match x with | _ -> try f x with _ -> 0\n");
}

/// Literal nested-match-as-scrutinee: `match match x with | _ -> 1 with
/// | _ -> 0`. The previous head-only WITH+MatchClauses shim refused to
/// pop the inner CtxtMatchClauses, leaving the outer WITH stranded. With
/// the shim removed (CtxtTry makes it unnecessary), force-closure for
/// the outer WITH pops the inner CtxtMatchClauses (emitting OEND) and
/// dispatches against the inner CtxtMatch — matching FCS. Pins the case
/// that #17's LEXFILTER.md notes called out as the residual.
#[test]
fn diff_filtered_nested_match_as_scrutinee() {
    assert_filtered_streams_match("let f x = match match x with | _ -> 1 with | _ -> 0\n");
}

/// `match x with` followed by a fresh top-level `let g` at column 0. The
/// lookahead after `with` (the `let` on the next line) is offside relative to
/// every context FCS's `undentationLimit` allows for a new CtxtMatchClauses —
/// so `tryPushCtxt` declines, no CtxtMatchClauses is pushed, and the second
/// `let` is reprocessed at the outer SeqBlock. Pre-port (#10) our port pushed
/// CtxtMatchClauses unconditionally and emitted a spurious OEND on the `let g`
/// pop cascade. (LexFilter.fs:771-1020, 2347-2355; ISSUES.txt symptom.)
#[test]
fn diff_filtered_match_with_then_offside_let() {
    assert_filtered_streams_match("let f x = match x with\nlet g y = ()\n");
}

/// `match x with\n    | _ ->\n0` — the match-arm body deindents past
/// the `|` column. The arrow body's SeqBlock(OneSided) push must succeed
/// at the body column, not fall back to the arrow position. FCS treats
/// CtxtMatchClauses as transparent for arrow-body pushes when the body's
/// column is between `match`/`try`.col and `|`.col (FCS L823-840 family),
/// using the enclosing construct's column as the floor.
#[test]
fn diff_filtered_match_arm_body_deindent_past_bar() {
    assert_filtered_streams_match("match x with\n    | _ ->\n0\n");
}

/// `(match x with\n| _ ->\n0)` — paren-wrapped match where the arm body
/// is aligned with the opening `(` (col 0). FCS L804-807 (more specific
/// than L827 `MatchClauses :: CtxtMatch`) returns `min(MatchClauses.col,
/// Paren.col)` so the body's strict push at col 0 succeeds even though
/// `match` sits at col 1.
#[test]
fn diff_filtered_paren_match_arm_body_at_paren_col() {
    assert_filtered_streams_match("(match x with\n| _ ->\n0)\n");
}
