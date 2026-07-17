//! Differential test (`parser::parse` vs FCS): the `expr then expr` sequential
//! separator — FCS's `declExpr OTHEN OBLOCKBEGIN typedSequentialExpr oblockend`
//! (`pars.fsy:4118`), which yields `SynExpr.Sequential(_, isTrueSeq = false, e1,
//! e2, …)`. The normaliser flattens `Sequential` and elides `isTrueSeq`/trivia,
//! so `a then b` projects like `a; b` — a flat `Sequential([a, b])`. Used in
//! secondary constructors (`new(x) = T(x, 0) then this.P <- 1`) and, in FCS's
//! grammar, any statement position.

use crate::common::assert_asts_match;

/// The `SyntaxTree/Expression/Sequential 02.fs` shape: `do a then b`.
#[test]
fn diff_ast_then_do_simple() {
    assert_asts_match("do a then b\n");
}

/// `do a then begin b end` (`Sequential 03.fs`) — a verbose-block RHS.
#[test]
fn diff_ast_then_do_begin_end() {
    assert_asts_match("do a then begin b end\n");
}

/// `then` in a `let`-body statement position.
#[test]
fn diff_ast_then_in_let_body() {
    assert_asts_match("let f () =\n  a\n  then b\n");
}

/// A multi-statement `then` block — the RHS flattens into the outer sequence.
#[test]
fn diff_ast_then_multi_statement() {
    assert_asts_match("let f () =\n  a ()\n  then\n    b ()\n    c ()\n");
}

/// The secondary-constructor `then` (the `Then01.fs` shape): a `new(x) as this`
/// ctor whose base call is followed by a `then` side-effect block.
#[test]
fn diff_ast_then_secondary_ctor() {
    assert_asts_match(
        "type T(x) =\n  member val P = 0 with get, set\n  new(x) as this =\n    T(x)\n    then\n      this.P <- 5\n",
    );
}
