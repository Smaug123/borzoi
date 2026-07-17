//! Differential test (`parser::parse` vs FCS): `struct`-prefixed expressions —
//! struct tuples `struct (e1, e2)` (`SynExpr.Tuple(isStruct = true)`) and struct
//! anonymous records `struct {| F = e |}` (`SynExpr.AnonRecd(isStruct = true)`).
//! Completes the `struct` form deferred by the tuple / anon-record slices.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

// ---- struct tuples ------------------------------------------------------

/// `struct (1, 2)` → `Tuple(isStruct = true, [1; 2])` — *directly*, with no
/// `Paren` wrapper (FCS's `STRUCT LPAREN tupleExpr rparen`, `pars.fsy:5314`,
/// unlike the regular `(1, 2)` = `Paren(Tuple(false, …))`).
#[test]
fn diff_ast_struct_tuple_pair() {
    assert_asts_match("let x = struct (1, 2)\n");
}

/// Three-element struct tuple.
#[test]
fn diff_ast_struct_tuple_triple() {
    assert_asts_match("let x = struct (1, 2, 3)\n");
}

/// Struct-tuple elements are full expressions (infix `+` binds tighter than the
/// tuple comma, as in a regular tuple).
#[test]
fn diff_ast_struct_tuple_expr_elements() {
    assert_asts_match("let x = struct (1 + 1, f y)\n");
}

/// `struct (…)` is atomic-precedence, so it stands in application-argument
/// position: `f struct (1, 2)` → `App(f, Tuple(true, …))` (FCS accepts this
/// without extra parens, unlike `f if …` / `f match …`).
#[test]
fn diff_ast_struct_tuple_as_app_arg() {
    assert_asts_match("let x = f struct (1, 2)\n");
}

/// Struct tuple parenthesised: `(struct (1, 2))` → `Paren(Tuple(true, …))`.
#[test]
fn diff_ast_struct_tuple_in_paren() {
    assert_asts_match("let x = (struct (1, 2))\n");
}

/// Nested struct tuple as an element.
#[test]
fn diff_ast_struct_tuple_nested() {
    assert_asts_match("let x = struct (1, struct (2, 3))\n");
}

// ---- struct anonymous records -------------------------------------------

/// `struct {| A = 1 |}` → `AnonRecd(isStruct = true, None, [(A, 1)])`.
#[test]
fn diff_ast_struct_anon_recd_single() {
    assert_asts_match("let x = struct {| A = 1 |}\n");
}

/// Multi-field struct anon-record.
#[test]
fn diff_ast_struct_anon_recd_multi() {
    assert_asts_match("let x = struct {| A = 1; B = 2 |}\n");
}

/// Struct anon-record as an application argument.
#[test]
fn diff_ast_struct_anon_recd_as_app_arg() {
    assert_asts_match("let x = f struct {| A = 1 |}\n");
}

/// A non-struct anon-record nested inside a struct one (and vice-versa) keeps
/// each `isStruct` flag distinct.
#[test]
fn diff_ast_struct_anon_recd_nested_mixed() {
    assert_asts_match("let x = struct {| A = {| B = 1 |} |}\n");
}

// ---- invalid forms: clean error (FCS also errors) -----------------------

/// A deferred / invalid `struct` form must not panic, must round-trip
/// losslessly, and (since FCS errors too) must surface ≥1 parse error.
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

/// `struct (1)` — a single-element struct tuple is invalid (FCS: "Unexpected
/// symbol ')' in expression"). Requires ≥2 elements.
#[test]
fn diff_ast_struct_tuple_single_is_clean_error() {
    assert_clean_error("let x = struct (1)\n");
}

/// `struct ()` — an empty struct tuple is invalid (FCS errors).
#[test]
fn diff_ast_struct_tuple_empty_is_clean_error() {
    assert_clean_error("let x = struct ()\n");
}

/// `struct (1,)` — a trailing comma / missing final element is invalid (FCS:
/// "Expected an expression after this point"), like the regular tuple `(1,)`.
#[test]
fn diff_ast_struct_tuple_trailing_comma_is_clean_error() {
    assert_clean_error("let x = struct (1,)\n");
}

/// An *unparenthesised* struct tuple in attribute-argument position
/// (`[<A struct (1, 2)>]`) is a clean error, matching FCS: attribute args use
/// the narrower `atomicExprAfterType`, which has the struct *anon-record*
/// (`braceBarExpr`) but not the struct *tuple* (an `atomicExpr`). Guards that
/// adding `struct` to `raw_starts_atomic_expr` did not leak the tuple form into
/// `raw_starts_attribute_arg`. (The parenthesised `[<A(struct (1, 2))>]` parses
/// — its head token is `(`.)
#[test]
fn diff_ast_struct_tuple_in_attr_arg_is_clean_error() {
    assert_clean_error("[<A struct (1, 2)>]\ntype T = class end\n");
}
