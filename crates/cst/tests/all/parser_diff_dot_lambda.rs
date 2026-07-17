//! Differential test (`parser::parse` vs FCS): the accessor-function shorthand
//! `_.Member` (FCS's `SynExpr.DotLambda`, `LanguageFeature.AccessorFunctionShorthand`,
//! `pars.fsy:5212` `UNDERSCORE DOT atomicExpr`). `_.Foo` is sugar for
//! `(fun x -> x.Foo)`; the parser models the body as an ordinary `atomicExpr`
//! (the lambda parameter is synthesised later, at type-check time, so it never
//! appears in the parse tree).
//!
//! The body after `_.` is exactly our `parse_atomic_expr`, so the member-chain
//! folding (`_.Foo.Bar` → one `LongIdent`), high-precedence application
//! (`_.Item(3)`), and indexer tails all fall out of the reused atom parse.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

/// The smallest form — `_.Foo`. FCS: `DotLambda(Ident "Foo", range, trivia)`.
#[test]
fn diff_ast_dot_lambda_single_member() {
    assert_asts_match("let f = _.Foo\n");
}

/// `_.Foo.Bar` — a member chain. The body `Foo.Bar` is a pure ident chain, so
/// FCS folds it to one `SynLongIdent`: `DotLambda(LongIdent ["Foo"; "Bar"])`.
/// The whole chain must bind *inside* the dot-lambda (not `DotGet(_.Foo, Bar)`).
#[test]
fn diff_ast_dot_lambda_member_chain() {
    assert_asts_match("let f = _.Foo.Bar\n");
}

/// `_.Foo.Bar.Baz` — a longer chain, same single-`LongIdent` folding.
#[test]
fn diff_ast_dot_lambda_long_member_chain() {
    assert_asts_match("let f = _.Foo.Bar.Baz\n");
}

/// `_.Item(3)` — a high-precedence (adjacent) paren application in the body:
/// `DotLambda(App(Atomic, Ident "Item", Paren(Const 3)))`.
#[test]
fn diff_ast_dot_lambda_hpa_body() {
    assert_asts_match("let f = _.Item(3)\n");
}

/// `_.Foo(x).Bar` — HPA then a member, all inside the dot-lambda body:
/// `DotLambda(DotGet(App(Atomic, Ident "Foo", Paren x), ["Bar"]))`.
#[test]
fn diff_ast_dot_lambda_hpa_then_member() {
    assert_asts_match("let f = _.Foo(x).Bar\n");
}

/// `List.map _.Length xs` — the dot-lambda as a whitespace application
/// argument: `App(App(List.map, DotLambda(Ident "Length")), xs)`. The
/// dot-lambda is an `atomicExpr`, so it stands as a bare argument.
#[test]
fn diff_ast_dot_lambda_as_app_arg() {
    assert_asts_match("let g = List.map _.Length xs\n");
}

/// `xs |> List.map _.Name` — the dot-lambda reached through an infix RHS
/// (`|>`) that contains the application. Exercises the arg-position gate from
/// inside a pratt-parsed right operand.
#[test]
fn diff_ast_dot_lambda_in_pipeline() {
    assert_asts_match("let g = xs |> List.map _.Name\n");
}

/// `(_.A, _.B)` — dot-lambdas as tuple elements, exercising the
/// expression-start gate at each element head.
#[test]
fn diff_ast_dot_lambda_tuple_elements() {
    assert_asts_match("let p = (_.A, _.B)\n");
}

/// `_ .Foo` — FCS accepts whitespace between `_` and `.` (the grammar is
/// `UNDERSCORE DOT`, token-level, not adjacency-gated). `DotLambda(Ident "Foo")`.
#[test]
fn diff_ast_dot_lambda_space_before_dot() {
    assert_asts_match("let f = _ .Foo\n");
}

/// `_. Foo` — and whitespace between `.` and the member.
#[test]
fn diff_ast_dot_lambda_space_after_dot() {
    assert_asts_match("let f = _. Foo\n");
}

/// `(_.Foo)` — a parenthesised dot-lambda. The `(`-after expr-start gate
/// (`raw_after_lparen_starts_expr`) must admit the leading `_`, routing the
/// body to the paren parser: `Paren(DotLambda(Ident "Foo"))`.
#[test]
fn diff_ast_dot_lambda_parenthesised() {
    assert_asts_match("let f = (_.Foo)\n");
}

/// `f (_.Foo)` — a parenthesised dot-lambda in application-argument position:
/// `App(Ident "f", Paren(DotLambda(Ident "Foo")))`.
#[test]
fn diff_ast_dot_lambda_paren_app_arg() {
    assert_asts_match("let q = f (_.Foo)\n");
}

/// A form that must (a) not panic, (b) round-trip losslessly, and (c) surface
/// at least one parse error. Used for the recovery cases below, where both our
/// parser and FCS report errors (so `assert_asts_match` does not apply).
fn assert_clean_error(source: &str) {
    let parsed = parse(source);
    assert_eq!(
        parsed.root.text().to_string(),
        source,
        "lossless round-trip violated for {source:?}",
    );
    assert!(
        !parsed.errors.is_empty(),
        "expected a parse error for {source:?}, got none",
    );
}

/// Bare `_` in expression position is *not* a dot-lambda — FCS recovers it as
/// `FromParseError(Ident "_")` (a parse error), which we don't model. We
/// require the `_.` shape, so `(_)` stays a clean (non-panicking, lossless)
/// error on our side too, matching FCS's "Expected '.'" rejection. Pins that
/// admitting `_` after `(` did not start accepting the bare form.
#[test]
fn bare_underscore_in_parens_is_clean_error() {
    assert_clean_error("let r = (_)\n");
}

/// `_.` with no body expression (at a delimiter / EOF) — FCS's `UNDERSCORE DOT
/// recover` arm. `at_dot_lambda` admits the `_.` head, but the body position is
/// empty, so the parser must recover (a missing-operand error + placeholder)
/// rather than drive `parse_atomic_expr` into `parse_const_payload`'s
/// `unreachable!`. FCS reports "Incomplete structured construct" here; we report
/// "expected expression after `_.`". Both error, so this pins the clean-error
/// guarantee. Regression guard against a parser/LSP panic on a half-typed
/// shorthand.
#[test]
fn incomplete_dot_lambda_at_block_end_is_clean_error() {
    assert_clean_error("let f = _.\n");
}

/// `_.` with no body in application-argument position (`List.map _.`) — same
/// `UNDERSCORE DOT recover` path reached through the arg gate.
#[test]
fn incomplete_dot_lambda_as_app_arg_is_clean_error() {
    assert_clean_error("let g = List.map _.\n");
}

/// `_.` with no body inside parens (`(_.)`) — the recovery fires with the `)`
/// (swallowed by the lex-filter) as the delimiter, so the body position is
/// empty. Exercises the recovery from the parenthesised dispatch path.
#[test]
fn incomplete_dot_lambda_in_parens_is_clean_error() {
    assert_clean_error("let h = (_.)\n");
}

/// `(_).Foo` — the `_` is inside parens and the `.` belongs to the *outer*
/// member access. The lex-filter swallows `)`, so a filtered-stream `_.`
/// lookahead would wrongly see the outer `.` and pull `.Foo` into a dot-lambda
/// spanning the `)`. `at_dot_lambda` probes the *raw* stream, where the next
/// token after `_` is the `)`, so `_` stays a bare (error) atom and the `.Foo`
/// is left to the enclosing construct. Both sides error (FCS rejects bare `_`),
/// so this pins the clean-error + no-mis-nesting guarantee.
#[test]
fn underscore_then_swallowed_closer_then_dot_is_clean_error() {
    assert_clean_error("let v = (_).Foo\n");
}

/// `(_.) x` — an incomplete shorthand before a swallowed `)`, followed by an
/// outside atom. A filtered-stream body probe would see `x` (the `)` is gone)
/// and drag it in as the dot-lambda body. The raw-adjacency guard recovers at
/// `_.` instead, leaving `)` to close the paren and `x` as an application
/// argument outside it. Regression guard against crossing a swallowed closer.
#[test]
fn incomplete_dot_lambda_does_not_swallow_outside_atom() {
    assert_clean_error("let f = (_.) x\n");
}
