//! Differential test (`parser::parse` vs FCS): `struct`-prefixed *patterns* —
//! struct tuple patterns `struct (p1, p2, …)` → `SynPat.Tuple(isStruct = true,
//! …)`. FCS's `STRUCT LPAREN tupleParenPatternElements rparen` (`pars.fsy:3853`)
//! produces the tuple *directly*, with no `Paren` wrapper — the pattern
//! analogue of the `struct (e1, e2)` expression (phase 10.18). Because it is an
//! `atomicPattern` it appears at every pattern site: binding heads, curried
//! function args, `match`/`function` clause heads, and nested inside other
//! patterns.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

// ---- binding heads ------------------------------------------------------

/// `let struct (a, b) = …` — the smallest struct tuple pattern. FCS:
/// `headPat = Tuple(true, [Named a; Named b])`, *not* `Paren(Tuple(false, …))`.
#[test]
fn diff_ast_let_struct_tuple_pair() {
    assert_asts_match("let struct (a, b) = w\n");
}

/// Three-element struct tuple pattern — flat element list.
#[test]
fn diff_ast_let_struct_tuple_triple() {
    assert_asts_match("let struct (a, b, c) = w\n");
}

/// Wildcards and named binders mix freely as elements.
#[test]
fn diff_ast_let_struct_tuple_wildcards() {
    assert_asts_match("let struct (a, _) = w\n");
}

// ---- element shapes (everything tighter than the tuple comma) -----------

/// Constructor-pattern element: `struct (Some a, b)` — the `Some a` is an
/// applPat (`SynPat.LongIdent`), captured as one element.
#[test]
fn diff_ast_struct_tuple_ctor_element() {
    assert_asts_match("let struct (Some a, b) = w\n");
}

/// Per-element type annotation: `struct (a : int, b)` — the `:` binds to the
/// preceding element (`Typed(Named a, int)`), not the whole tuple.
#[test]
fn diff_ast_struct_tuple_typed_element() {
    assert_asts_match("let struct (a : int, b) = w\n");
}

/// Cons element: `struct (h :: t, x)` — `::` binds tighter than the comma.
#[test]
fn diff_ast_struct_tuple_cons_element() {
    assert_asts_match("let struct (h :: t, x) = w\n");
}

/// Conjunction element: `struct (a & b, c)`.
#[test]
fn diff_ast_struct_tuple_ands_element() {
    assert_asts_match("let struct (a & b, c) = w\n");
}

/// A parenthesised (regular) tuple as a struct-tuple element keeps its `Paren`
/// wrapper and `isStruct = false`, distinct from the outer struct tuple.
#[test]
fn diff_ast_struct_tuple_nested_paren_tuple() {
    assert_asts_match("let struct ((a, b), c) = w\n");
}

/// A struct tuple nested as an element of another struct tuple — both carry
/// `isStruct = true`, no `Paren` between them.
#[test]
fn diff_ast_struct_tuple_nested_struct_tuple() {
    assert_asts_match("let struct (struct (a, b), c) = w\n");
}

// ---- other pattern sites ------------------------------------------------

/// Curried function-argument position: `let f struct (a, b) = …` — `f` is a
/// function-form head whose single arg is the struct tuple pattern
/// (`SynArgPats.Pats [Tuple(true, …)]`).
#[test]
fn diff_ast_struct_tuple_as_fun_arg() {
    assert_asts_match("let f struct (a, b) = w\n");
}

/// A `match` clause head: `match v with struct (a, b) -> …`.
#[test]
fn diff_ast_struct_tuple_match_clause() {
    assert_asts_match("match v with struct (a, b) -> w\n");
}

/// A `function` clause head.
#[test]
fn diff_ast_struct_tuple_function_clause() {
    assert_asts_match("let g = function struct (a, b) -> w\n");
}

/// A `fun`-lambda argument: `fun struct (a, b) -> …`. FCS's `SimplePatsOfPat`
/// special-cases only the *non-struct* `Paren(Tuple(false, …))`, so a struct
/// tuple arg falls through to the `match`-scaffolding `SimplePatOfPat`
/// lowering — distinct from `fun (a, b) -> …`.
#[test]
fn diff_ast_struct_tuple_fun_lambda_arg() {
    assert_asts_match("let g = fun struct (a, b) -> a\n");
}

/// Parenthesised at a binding head: `let (struct (a, b)) = …` →
/// `Paren(Tuple(true, …))`.
#[test]
fn diff_ast_struct_tuple_in_paren() {
    assert_asts_match("let (struct (a, b)) = w\n");
}

/// A parenthesised *struct* tuple as an indexer-setter index is **not**
/// flattened — FCS's setter rewrite only flattens `Paren(Tuple(false, …))`, so
/// `set (struct (i, j)) v` keeps the struct tuple as one index arg:
/// `Tuple(false, [Paren(Tuple(true, [i; j])); v])`, distinct from the non-struct
/// `set (i, j) v` ⇒ `Tuple(false, [i; j; v])`.
#[test]
fn diff_ast_struct_tuple_setter_index_not_flattened() {
    assert_asts_match("type T() =\n  member _.Item with set (struct (i, j)) v = ()\n");
}

// ---- invalid forms: clean error (FCS also errors) -----------------------

/// A `struct` pattern that FCS rejects must not panic, must round-trip
/// losslessly, and must surface ≥1 parse error.
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

/// `struct (a)` — a single-element struct tuple is invalid (FCS: "Unexpected
/// symbol ')' in pattern"). Requires ≥2 elements.
#[test]
fn diff_ast_struct_tuple_pat_single_is_clean_error() {
    assert_clean_error("let struct (a) = w\n");
}

/// `struct ()` — an empty struct tuple pattern is invalid.
#[test]
fn diff_ast_struct_tuple_pat_empty_is_clean_error() {
    assert_clean_error("let struct () = w\n");
}

/// `struct` not followed by `(` is a clean error (no `{|` struct anon-record
/// *pattern* form exists in F#).
#[test]
fn diff_ast_struct_pat_no_paren_is_clean_error() {
    assert_clean_error("let struct = w\n");
}

/// `let f (struct) (x, y) = …` — a bare `struct` inside parens (FCS: parse
/// error). The struct-tuple dispatch probes the *raw* stream for the `(`, so it
/// must NOT reach past the LexFilter-swallowed `)` of `(struct)` and consume the
/// following `(x, y)` argument as a struct tuple: recovery stays lossless.
#[test]
fn diff_ast_struct_in_paren_does_not_eat_next_arg() {
    assert_clean_error("let f (struct) (x, y) = w\n");
}

/// `let struct⏎(a, b) = …` — an offside break between `struct` and `(` (FCS:
/// "Incomplete structured construct"). The struct-tuple dispatch also requires a
/// real `(` on the *filtered* stream, so it must NOT skip the intervening
/// `DeclEnd`/`BlockSep` layout virtuals and bump the next line's `(` as the
/// struct tuple's open paren across the declaration boundary.
#[test]
fn diff_ast_struct_offside_break_is_clean_error() {
    assert_clean_error("let struct\n(a, b) = w\n");
}

/// `let f struct (a,) x = …` — a trailing comma inside the struct tuple (FCS:
/// parse error) followed by a further curried arg. The element emit is
/// raw-gated, so the missing element is reported at the swallowed `)`; the
/// dispatch must NOT reach past it and consume the next argument `x` as the
/// tuple's second element.
#[test]
fn diff_ast_struct_tuple_trailing_comma_does_not_eat_next_arg() {
    assert_clean_error("let f struct (a,) x = w\n");
}
