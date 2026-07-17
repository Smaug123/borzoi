//! Differential test (`parser::parse` vs FCS): `try … with` exception
//! handlers (`SynExpr.TryWith`, phase 10.20a) and `try … finally …`
//! (`SynExpr.TryFinally`, phase 10.20b). The `with`-handler clause list is
//! FCS's `withClauses` → `patternClauses`, the same non-terminal as
//! `match … with`, so the clause surface is covered by `parser_diff_match.rs`;
//! these pin the `try` framing (body block, the `OWITH` relabel, the
//! clause-list scaffolding, the `finally` regular-block body) against the real
//! compiler.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};

/// Phase 10.20a — the single-line `try x with _ -> 0`. The body is FCS's
/// `typedSequentialExprBlockR` (a one-sided SeqBlock) and the wildcard handler
/// the simplest clause.
#[test]
fn diff_ast_try_with_single_clause() {
    assert_asts_match("try x with _ -> 0\n");
}

/// Phase 10.20a — multi-line offside `try` / `with`, the body and handler on
/// separate lines aligned under the binding. Exercises the one-sided body
/// SeqBlock closing (`OffsideRightBlockEnd`) before the offside `OWITH`.
#[test]
fn diff_ast_try_with_offside() {
    assert_asts_match("let f x =\n    try g x\n    with _ -> 0\n");
}

/// Phase 10.20a — multiple handler clauses with a leading `|`, a named ctor
/// clause (`Failure msg`), and a wildcard fallthrough. Confirms the clause
/// list parses identically to a `match`.
#[test]
fn diff_ast_try_with_multiple_clauses() {
    assert_asts_match("try f x with | Failure msg -> 0 | _ -> 1\n");
}

/// Phase 10.20a — a `when` guard on a handler clause (`patternAndGuard`),
/// shared verbatim with `match`.
#[test]
fn diff_ast_try_with_when_guard() {
    assert_asts_match("try f x with e when cond e -> 0\n");
}

/// Phase 10.20a — the dynamic type-test handler `:? T as e` (`SynPat.IsInst` +
/// `as`), the canonical exception-handler shape. Rides whatever the match
/// clause-pattern parser supports.
#[test]
fn diff_ast_try_with_isinst_clause() {
    assert_asts_match("try f x with :? System.Exception as e -> 0\n");
}

/// Phase 10.20a — a multi-statement try body (an offside SeqBlock with two
/// statements). The body wraps in `SynExpr.Sequential`, exactly as a
/// match-clause result body does.
#[test]
fn diff_ast_try_with_seq_body() {
    assert_asts_match("try\n    g x\n    h x\nwith _ -> 0\n");
}

/// A multi-statement try body whose *second* statement is a bare negative
/// literal (`-1`). The adjacent `-` is an `ADJACENT_PREFIX_OP` term-starter, so
/// the body is `Sequential([…, -1])` — not a single `App` applying the first
/// statement to `-1`. (Regression: corpus `CatchWOTypecheck01.fs` /
/// `NullAsTrueUnion01.fs`, whose `try` bodies end in `-1`.)
#[test]
fn diff_ast_try_with_seq_body_negative_literal_tail() {
    assert_asts_match("try\n    g x\n    -1\nwith _ -> 0\n");
    // The motivating shape: a piped first statement, then `-1`.
    assert_asts_match("try\n    f x |> ignore\n    -1\nwith _ -> 0\n");
}

/// Phase 10.20a — `try`/`with` nested inside a `match` arm. The inner `with`
/// must balance against the inner `try`, not pop the outer match clauses
/// (the LexFilter doom-loop case, here exercised end-to-end through the parser).
#[test]
fn diff_ast_try_with_inside_match_arm() {
    assert_asts_match("match x with | _ -> try f x with _ -> 0\n");
}

/// Phase 10.20a — `try`/`with` as a non-trailing statement (a let RHS feeding
/// a following expression), so the handler-clause close virtuals must not
/// swallow the enclosing block's continuation.
#[test]
fn diff_ast_try_with_as_let_rhs() {
    assert_asts_match("let r =\n    try f x\n    with _ -> 0\nr\n");
}

/// Phase 10.20b — the single-line `try x finally ()`. The finally body is FCS's
/// `typedSequentialExprBlock` (a regular block) — here the unit literal.
#[test]
fn diff_ast_try_finally_single_line() {
    assert_asts_match("try x finally ()\n");
}

/// Phase 10.20b — multi-line offside `try` / `finally`, body and cleanup on
/// separate lines. Exercises the one-sided try-body close (`OffsideRightBlockEnd`)
/// then the raw `finally` + the regular-block finally body.
#[test]
fn diff_ast_try_finally_offside() {
    assert_asts_match("let f x =\n    try g x\n    finally cleanup ()\n");
}

/// Phase 10.20b — a multi-statement try body (an offside SeqBlock) before the
/// `finally`. The try body wraps in `SynExpr.Sequential`, exactly as the
/// `try … with` body does.
#[test]
fn diff_ast_try_finally_seq_try_body() {
    assert_asts_match("try\n    a x\n    b x\nfinally c ()\n");
}

/// Phase 10.20b — a multi-statement *finally* body (the regular block holds two
/// statements). Confirms the finally body reuses the `do`-body SeqBlock
/// gathering (a `SynExpr.Sequential`).
#[test]
fn diff_ast_try_finally_seq_finally_body() {
    assert_asts_match("try g x\nfinally\n    a ()\n    b ()\n");
}

/// Phase 10.20b — `try`/`finally` as a non-trailing statement (a let RHS feeding
/// a following expression), so the finally-body close virtuals must not swallow
/// the enclosing block's continuation (the `consume_block_decl_end` discipline,
/// the same hazard the `while … do` tests guard).
#[test]
fn diff_ast_try_finally_as_let_rhs() {
    assert_asts_match("let r =\n    try f x\n    finally cleanup ()\nr\n");
}

/// Phase 10.20b — `try`/`finally` nested inside a `match` arm, the dual of the
/// `try`/`with`-in-match-arm case: the inner `finally` block must close cleanly
/// without disturbing the outer match clauses.
#[test]
fn diff_ast_try_finally_inside_match_arm() {
    assert_asts_match("match x with | _ -> try f x finally g ()\n");
}

/// Phase 10.20b — a `try`/`finally` whose body is itself a `try`/`with` (the
/// canonical `try (try … with …) finally …` nesting). Pins that the inner
/// handler's close virtuals and the outer finally block compose.
#[test]
fn diff_ast_try_with_inside_try_finally() {
    assert_asts_match("try\n    try f x\n    with _ -> 0\nfinally cleanup ()\n");
}

// ---- Phase 11 error recovery: incomplete `try … with` handlers -----------
//
// The `with`-handler clause list is the same `SynMatchClause list` as `match`,
// so it rides the shared recovery projection (drop the spurious empty clause;
// project a missing clause result to `NormalisedExpr::Error`). The trailing
// `let y = 2` survives as its own decl.

/// `try e with` and nothing after — FCS recovers zero handler clauses; the
/// spurious empty clause is dropped.
#[test]
fn diff_ast_try_with_recover_no_clauses() {
    assert_asts_match_allow_errors("let x = try e with\nlet y = 2\n");
}

/// A handler with a pattern but no result — `try e with A ->`. The result hole
/// projects to `Error`, matching FCS's `ArbitraryAfterError`.
#[test]
fn diff_ast_try_with_recover_clause_missing_result() {
    assert_asts_match_allow_errors("let x = try e with A ->\nlet y = 2\n");
}
