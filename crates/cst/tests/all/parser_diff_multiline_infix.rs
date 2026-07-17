//! Differential test (`parser::parse` vs FCS): the offside continuation rule
//! for a line that ends with an **infix operator**.
//!
//! F#'s lex-filter treats a trailing infix operator as continuing the
//! expression onto the next line: *"the r.h.s. of an infix token begins a new
//! block"* (FCS `LexFilter.fs:2330`). When an infix token (`+`, `&&`, `||`,
//! `|>`, `*`, …) is followed by its right-hand side on a *different* line, FCS
//! pushes a fresh `CtxtSeqBlock` so the continuation line does not start a new
//! statement (no `OBLOCKSEP`). FCS deliberately excludes `<`, `>`, and `=` from
//! the infix set (they collide with `f<int>` / `let f x = …`), so a line ending
//! in `=` or `>` is *not* a continuation — those stay parse errors on both
//! sides and are not exercised here.

use crate::common::assert_asts_match;

/// `a +` then the operand on the next line — the canonical arithmetic case.
#[test]
fn diff_multiline_plus() {
    assert_asts_match("let x = a +\n        b\n");
}

/// `&&` short-circuit conjunction across a line break (the motivating corpus
/// shape: multi-line boolean conditions).
#[test]
fn diff_multiline_andand() {
    assert_asts_match("let x = a &&\n        b\n");
}

/// `||` short-circuit disjunction across a line break.
#[test]
fn diff_multiline_oror() {
    assert_asts_match("let x = a ||\n        b\n");
}

/// The pipe operator `|>` at end of line — a *very* common multi-line idiom.
#[test]
fn diff_multiline_pipe() {
    assert_asts_match("let x = a\n        |> f\n        |> g\n");
}

/// A three-operand `&&`-chain spread over three lines (the `InfoReader.fs`
/// shape).
#[test]
fn diff_multiline_andand_chain() {
    assert_asts_match("let x =\n    a = b &&\n    c d e &&\n    f = g\n");
}

/// A multi-line `&&` inside a parenthesised lambda body — the exact corpus
/// construct (`(fun a b -> a && \n b)`).
#[test]
fn diff_multiline_andand_in_lambda_paren() {
    assert_asts_match("let f = (fun a b -> a &&\n                    b)\n");
}

/// A multiplication `*` continuation (an `INFIX_STAR_DIV_MOD_OP`).
#[test]
fn diff_multiline_star() {
    assert_asts_match("let x = a *\n        b\n");
}

/// An infix `&&` inside a `when` guard must NOT open a block for the arm body
/// (FCS excludes a `CtxtMatchClauses` head): `| _ when a &&⏎ b -> body` keeps
/// `body` in the clause result, not a fresh sequence block.
#[test]
fn diff_multiline_andand_in_match_guard() {
    assert_asts_match(
        "let f x =\n    match x with\n    | _ when a &&\n             b -> 1\n    | _ -> 2\n",
    );
}

/// A trailing comma continues a tuple onto the next line (`COMMA` is infix).
#[test]
fn diff_multiline_tuple_comma() {
    assert_asts_match("let x = (a,\n         b)\n");
}

/// A multi-line `&&` condition in an `if` (the other ubiquitous shape).
#[test]
fn diff_multiline_andand_in_if() {
    assert_asts_match("let f x = if a &&\n             b then 1 else 2\n");
}

// ---- operator ALONE on its own line, operand on the FOLLOWING line ----------
//
// Distinct from the cases above (where the operand follows the operator on the
// same line, e.g. `a⏎ |> f`, or the operator trails its left operand, e.g.
// `a +⏎ b`). Here the operator sits alone between operands on separate lines —
// the leading-continuator joins it to the left operand, but the right operand
// on the next line must still be carved into the continuation block.

/// `||` alone on its own line between two operands (the `ServiceParsedInputOps`
/// shape, here without the interleaved comments).
#[test]
fn diff_operator_alone_oror() {
    assert_asts_match("let flag =\n    a\n    ||\n    b\n");
}

/// `|||` alone on its own line between parenthesised operands (the `ilreflect`
/// bitwise-flags shape).
#[test]
fn diff_operator_alone_bitor_between_parens() {
    assert_asts_match("let x =\n    (a)\n    |||\n    (b)\n");
}

/// `|>` alone on its own line, operand on the next line.
#[test]
fn diff_operator_alone_pipe() {
    assert_asts_match("let x =\n    a\n    |>\n    f\n");
}

// ---- trailing infix after a CLOSER-terminated operand, operand next line -----
//
// `f a ||⏎ b` already works; a parenthesis-closed left operand (`(a) ||⏎ b`)
// inside an own-line RHS block is the gap.

/// `(a = b) ||` then the operand on the next line (the `ProvidedTypes` /
/// `MethodOverrides` shape).
#[test]
fn diff_trailing_oror_after_paren() {
    assert_asts_match("let flag =\n    (a = b) ||\n    c\n");
}

/// `(...) |||` after a parenthesised operand, operand on the next line
/// (the `E_Regression02` / `magic` bitwise shape).
#[test]
fn diff_trailing_bitor_after_paren() {
    assert_asts_match("let x =\n    (a &&& m) |||\n    (b)\n");
}

/// Negative guard: an infix in a `when` guard, operator alone on its line, must
/// still NOT open a block for the arm body (mirrors the existing trailing-op
/// guard). The `2` stays the arm result, not a fresh statement.
#[test]
fn diff_operator_alone_in_match_guard_stays_in_clause() {
    assert_asts_match(
        "let f x =\n    match x with\n    | _ when a\n             ||\n             b -> 1\n    | _ -> 2\n",
    );
}

// ---- leading infix operator undented below the SeqBlock head ---------------
//
// FCS grants a leading infix operator a grace of `infixTokenLength + 1` columns
// in the SeqBlock offside pop (`LexFilter.fs:1833-1854`), so a `|> f` / `+ e`
// continuation may sit *left of* the expression head without closing the block:
//     let x =
//           expr
//        |> f expr      <-- `|>` at a column < `expr`, still a continuation
// This is the ubiquitous FCS test-DSL shape (`Fsx """…""" |> compile |> …`).

/// A `|>` chain undented below the application head (the `CastingTests.fs` shape).
#[test]
fn diff_ast_leading_pipe_undented_below_head() {
    assert_asts_match("module M\nlet test () =\n        g x\n     |> f\n     |> h\n");
}

/// The FCS test-DSL shape verbatim: a triple-quoted string arg, then an undented
/// `|>` pipe chain.
#[test]
fn diff_ast_triple_string_then_undented_pipes() {
    assert_asts_match(
        "module M\nlet test () =\n        Fsx \"\"\"\nlet y = 1\n    \"\"\"\n     |> ignoreWarnings\n     |> compile\n",
    );
}

/// A `+` chain undented below the head (the other FCS grace example). `+` has
/// `infixTokenLength` 1, so its grace is 2: two columns left of the head is still
/// a continuation (a third column left closes, matching FCS).
#[test]
fn diff_ast_leading_plus_undented_below_head() {
    assert_asts_match("module M\nlet x =\n        a + b\n      + c\n");
}
