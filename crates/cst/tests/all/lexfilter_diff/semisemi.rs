//! `;;` (`SemicolonSemicolon`) short-circuit across every context.

use crate::common::assert_filtered_streams_match;

/// `;;` between two top-level `let` bindings in a `module A = …` (non-whole-file)
/// header. FCS L1939 short-circuits the `CtxtLetDecl` offside-pop on `isSemiSemi`
/// regardless of indentation; the LetDecl pops at `;;` and emits `ODECLEND` at
/// the `;;` boundary. Without the `isSemiSemi` guard the Rust port keeps the
/// LetDecl open until `let y` arrives — wrong byte range for the ODECLEND.
#[test]
fn diff_filtered_semisemi_between_top_level_lets() {
    assert_filtered_streams_match("module A =\n    let x = 1\n    ;;\n    let y = 2\n");
}

/// `;;` between match clauses. FCS L2031 (CtxtMatch) and L2099 (CtxtMatchClauses)
/// both short-circuit on `isSemiSemi`. Without the guard, the Rust port leaves
/// the MatchClauses context open and treats the next `|` as a continuation arm.
#[test]
fn diff_filtered_semisemi_between_match_clauses() {
    assert_filtered_streams_match("let z x =\n    match x with\n    | 1 -> 1\n    ;;\n");
}

/// `;;` inside a try/with body. Exercises the `CtxtTry` arm (FCS L2074):
/// `isSemiSemi` must close the try-block scope.
#[test]
fn diff_filtered_semisemi_inside_try_with() {
    assert_filtered_streams_match(
        "let z =\n    try\n        1\n    with _ -> 0\n    ;;\nlet w = 2\n",
    );
}

/// `;;` inside a for-loop body. Exercises the `CtxtFor` arm (FCS L2038):
/// `isSemiSemi` short-circuits the for-loop scope's offside pop.
#[test]
fn diff_filtered_semisemi_inside_for_loop() {
    assert_filtered_streams_match("let z =\n    for i = 1 to 3 do\n        ignore i\n    ;;\n");
}

/// `;;` inside a while-do body. Exercises the `CtxtWhile` arm (FCS L2044):
/// `isSemiSemi` short-circuits the while-loop scope's offside pop.
#[test]
fn diff_filtered_semisemi_inside_while_do() {
    assert_filtered_streams_match("let z =\n    while true do\n        ()\n    ;;\n");
}

/// `;;` after `if … then …` branches. Exercises the `CtxtIf` / `CtxtThen` /
/// `CtxtElse` arms (FCS L2014, L2085, L2093): `isSemiSemi` short-circuits all
/// three indentation guards.
#[test]
fn diff_filtered_semisemi_after_if_then_else() {
    assert_filtered_streams_match("let z x =\n    if x then 1\n    else 2\n    ;;\nlet w = 3\n");
}

/// `;;` inside a `type T = …` body. Exercises the `CtxtTypeDefns` arm
/// (FCS L1967): `isSemiSemi` short-circuits the type-defn indentation guard.
#[test]
fn diff_filtered_semisemi_inside_type_defn() {
    assert_filtered_streams_match("type T = { x : int }\n;;\nlet w = 1\n");
}

/// `;;` at the top of a whole-file module (no `=`/`:`). FCS L1979's
/// `isSemiSemi && not wholeFile` guard must NOT close the outer module body —
/// otherwise the rest of the file becomes unanchored.
#[test]
fn diff_filtered_semisemi_in_whole_file_module() {
    assert_filtered_streams_match("module A\n\nlet x = 1\n;;\nlet y = 2\n");
}

/// FSI-style member ending with `;;` immediately after the RHS. FCS's
/// force-closure (L1556) pops `CtxtMemberBody` silently via
/// `endTokenForACtxt = None` — no `ODECLEND` is emitted. The Rust port
/// mirrors that here by popping via `delay + continue` rather than
/// `Virtual::DeclEnd`. Without the silent-pop branch the stream gains a
/// stray `OffsideDeclEnd` before `SemicolonSemicolon`. (codex review
/// catch.)
#[test]
fn diff_filtered_semisemi_after_member_inline() {
    assert_filtered_streams_match("type T() =\n    member x.Data = 1;;\nlet y = 2\n");
}

/// `;;` arriving while a `CtxtParen` is on the stack. The previous per-arm
/// `isSemiSemi` cascade had no arm for `CtxtParen`, so the paren context
/// stayed on the stack and the following `let y` lost its `OffsideLet`
/// re-seeding. FCS handles this via `tokenForcesHeadContextClosure`
/// (L1556) — `;;` force-closes any non-balanced head, including `CtxtParen`
/// whose `endTokenForACtxt` is `None`. The Rust port now mirrors that by
/// routing `;;` through `token_forces_head_context_closure`. (codex review
/// catch.)
#[test]
fn diff_filtered_semisemi_inside_paren_expr() {
    assert_filtered_streams_match("let x = (1;;\nlet y = 2\n");
}
