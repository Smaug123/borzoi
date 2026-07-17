//! Differential test (`parser::parse` vs FCS): the inferred (typeless) coercion
//! prefixes `upcast e` / `downcast e` — FCS's `SynExpr.InferredUpcast(expr,
//! range)` / `InferredDowncast(expr, range)` (the `minusExpr` productions
//! `UPCAST minusExpr` / `DOWNCAST minusExpr`, `pars.fsy:5182`/`:5185`).
//!
//! These are the *inferred* coercions — they carry no target type (it is
//! supplied by inference), unlike the `:>` / `:?>` infix forms (`SynExpr.Upcast`
//! / `Downcast`, each with an explicit `SynType`), which are a separate slice.
//! The operand is a `minusExpr`, the same precedence layer as the address-of /
//! `new` prefixes, so the parser dispatches `upcast`/`downcast` beside those in
//! `parse_minus_expr` and parses the operand recursively at that level.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, Expr, SyntaxKind};

// ---- the basic prefix forms ---------------------------------------------

/// `upcast y` — `InferredUpcast(Ident "y")`, the motivating shape.
#[test]
fn diff_ast_upcast_ident() {
    assert_asts_match("let x = upcast y\n");
}

/// `downcast y` — `InferredDowncast(Ident "y")`.
#[test]
fn diff_ast_downcast_ident() {
    assert_asts_match("let x = downcast y\n");
}

// ---- application / paren operands ---------------------------------------

/// The operand is an `appExpr` wrapped in parens — `InferredUpcast(Paren(App f
/// x))`. (A bare `upcast f x` is `App(InferredUpcast f, x)` per FCS precedence;
/// the parens pin the whole application as the coerced expression.)
#[test]
fn diff_ast_upcast_paren_app() {
    assert_asts_match("let x = upcast (f x)\n");
}

/// `downcast` over a paren'd application.
#[test]
fn diff_ast_downcast_paren_app() {
    assert_asts_match("let x = downcast (f x)\n");
}

/// A bare application operand: `upcast f` is `InferredUpcast(Ident "f")` and the
/// trailing `x` applies *outside* — `App(InferredUpcast f, x)` — because the
/// coercion is a `minusExpr` and application binds tighter only as `appExpr`
/// under it. Pin FCS's nesting.
#[test]
fn diff_ast_upcast_then_app() {
    assert_asts_match("let x = upcast f x\n");
}

// ---- nesting ------------------------------------------------------------

/// `upcast (downcast x)` — coercions nest through a paren.
#[test]
fn diff_ast_upcast_of_downcast() {
    assert_asts_match("let x = upcast (downcast x)\n");
}

/// The reverse nesting.
#[test]
fn diff_ast_downcast_of_upcast() {
    assert_asts_match("let x = downcast (upcast x)\n");
}

/// Directly chained (no parens): `UPCAST minusExpr` where the operand is itself
/// `DOWNCAST minusExpr` — `InferredUpcast(InferredDowncast(Ident "x"))`.
#[test]
fn diff_ast_upcast_downcast_chained() {
    assert_asts_match("let x = upcast downcast x\n");
}

// ---- positions ----------------------------------------------------------

/// In a tuple element (each element is a fresh expression-start position).
#[test]
fn diff_ast_upcast_in_tuple() {
    assert_asts_match("let x = (upcast a, downcast b)\n");
}

/// As a function argument (must be parenthesised — a `minusExpr` is not an
/// `atomicExpr`, so `f upcast x` would not nest the coercion under the arg).
#[test]
fn diff_ast_upcast_as_arg() {
    assert_asts_match("let x = f (upcast y)\n");
}

/// Piped into a function — `upcast y` is the `minusExpr`, the `|>` infix sits
/// above it: `App(|>, InferredUpcast y, ignore)`.
#[test]
fn diff_ast_upcast_piped() {
    assert_asts_match("upcast y |> ignore\n");
}

/// A typed binding whose RHS is `upcast (…)` — the shape that motivated the
/// slice (`let empty : IList<_> = upcast (…)`), with a parenthesised
/// `ResizeArray()` operand to stay clear of the unrelated expression-position
/// `SynExpr.TypeApp` (`ResizeArray<_>()`) gap.
#[test]
fn diff_ast_typed_binding_upcast() {
    assert_asts_match("let xs : System.Collections.IList = upcast (ResizeArray())\n");
}

// ---- green-tree shape (no FCS) ------------------------------------------

/// The node wraps the keyword token then the operand expr:
/// `INFERRED_UPCAST_EXPR > [UPCAST_TOK, <inner-expr>]`.
#[test]
fn green_tree_upcast_shape() {
    let parse = parse("let x = upcast y\n");
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::INFERRED_UPCAST_EXPR)
        .expect("expected an INFERRED_UPCAST_EXPR node");
    // First child token is the `upcast` keyword.
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::UPCAST_TOK),
        "INFERRED_UPCAST_EXPR must carry an UPCAST_TOK",
    );
    // The operand is a structured `Expr` child, not swallowed trivia.
    assert!(
        node.children().any(|c| Expr::can_cast(c.kind())),
        "INFERRED_UPCAST_EXPR must contain a structured operand expr",
    );
}

#[test]
fn green_tree_downcast_shape() {
    let parse = parse("let x = downcast y\n");
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::INFERRED_DOWNCAST_EXPR),
        "expected an INFERRED_DOWNCAST_EXPR node",
    );
}

// ---- error recovery -----------------------------------------------------

/// A bare `upcast` with no operand — like the address-of / `new` recovery
/// paths, the parser records the missing-operand error, still emits the
/// `INFERRED_UPCAST_EXPR` (carrying just the keyword), and stays lossless
/// (never panics).
#[test]
fn upcast_missing_operand_recovers_without_panic() {
    let src = "let x = upcast\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for the operandless `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::INFERRED_UPCAST_EXPR),
        "the operandless recovery must still emit an INFERRED_UPCAST_EXPR",
    );
}
