//! Differential test (`parser::parse` vs FCS): expression-level object
//! construction `new T(args)` — FCS's `SynExpr.New(isProtected = false,
//! targetType, expr, range)` (the `minusExpr` production
//! `NEW atomType opt_HIGH_PRECEDENCE_APP atomicExprAfterType`, `pars.fsy:5173`).
//!
//! The target type is FCS's `atomType` (so `Foo<int>` keeps its `<…>` inside
//! the type and the following `(` opens the constructor args), and the argument
//! is `atomicExprAfterType`: `()` → `Const Unit`, `(a, b)` → `Paren(Tuple)`,
//! `(e)` → `Paren(e)`. Distinct from the member-definition explicit constructor
//! `new(args) = body` (phase 9.10b) — that is a `SynMemberDefn.Member`, this is
//! an *expression*.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

// ---- the unit-arg form `new T()` ----------------------------------------

/// `new History()` — the motivating case: a bare type name + unit args
/// (`New(LongIdent "History", Const Unit)`).
#[test]
fn diff_ast_new_unit_arg() {
    assert_asts_match("let x = new History()\n");
}

/// Dotted type path — `New(LongIdent ["System"; "Object"], Const Unit)`.
#[test]
fn diff_ast_new_dotted_type() {
    assert_asts_match("let x = new System.Object()\n");
}

/// Deeper dotted path.
#[test]
fn diff_ast_new_deep_dotted_type() {
    assert_asts_match("let x = new System.Text.StringBuilder()\n");
}

// ---- generic target types -----------------------------------------------

/// `new List<int>()` — the `<…>` stays inside the `atomType`, the `(` opens the
/// args.
#[test]
fn diff_ast_new_generic_type() {
    assert_asts_match("let x = new List<int>()\n");
}

/// Multi-argument generic type.
#[test]
fn diff_ast_new_generic_type_two_args() {
    assert_asts_match("let x = new Dictionary<string, int>()\n");
}

/// Dotted + generic.
#[test]
fn diff_ast_new_dotted_generic_type() {
    assert_asts_match("let x = new System.Collections.Generic.List<int>()\n");
}

// ---- constructor arguments ----------------------------------------------

/// Single argument — `New(_, Paren(Const 1))`.
#[test]
fn diff_ast_new_single_arg() {
    assert_asts_match("let x = new Foo(1)\n");
}

/// Tuple arguments — `New(_, Paren(Tuple [1; 2]))`.
#[test]
fn diff_ast_new_tuple_args() {
    assert_asts_match("let x = new Foo(1, 2)\n");
}

/// Three arguments.
#[test]
fn diff_ast_new_three_args() {
    assert_asts_match("let x = new Foo(a, b, c)\n");
}

/// A string-literal argument.
#[test]
fn diff_ast_new_string_arg() {
    assert_asts_match("let x = new System.Text.StringBuilder(\"hi\")\n");
}

/// An expression argument (infix inside the parens binds tighter than the
/// arg's outer structure).
#[test]
fn diff_ast_new_expr_arg() {
    assert_asts_match("let x = new Foo(1 + 2)\n");
}

// ---- nesting / composition ----------------------------------------------

/// A `new` expression as a constructor argument to another `new`.
#[test]
fn diff_ast_new_nested_arg() {
    assert_asts_match("let x = new Foo(new Bar())\n");
}

/// `new` on both sides of an infix operator — the `minusExpr`-level `new`
/// is picked up as the operand by the outer Pratt climber.
#[test]
fn diff_ast_new_infix_operands() {
    assert_asts_match("let x = new A() = new B()\n");
}

/// A `new` expression parenthesised, then a member access — `(new T()).Member`
/// is the *valid* way to call a member off a freshly-constructed object.
#[test]
fn diff_ast_new_parenthesised_member() {
    assert_asts_match("let x = (new System.Object()).ToString()\n");
}

/// A `new` expression as a function argument must be parenthesised (it is a
/// `minusExpr`, not an `atomicExpr`).
#[test]
fn diff_ast_new_as_paren_arg() {
    assert_asts_match("let x = f (new Foo())\n");
}

/// `opt_HIGH_PRECEDENCE_APP` is *optional* — a space before the args
/// (`new Foo ()`, no adjacency marker) is the markerless form and parses the
/// same `New(Foo, Const Unit)`.
#[test]
fn diff_ast_new_spaced_args() {
    assert_asts_match("let x = new Foo ()\n");
}

/// A `new` expression as a tuple element.
#[test]
fn diff_ast_new_tuple_element() {
    assert_asts_match("let x = (new A(), new B())\n");
}

/// A `new` expression as a module-level `do` (top-level statement).
#[test]
fn diff_ast_new_top_level_do() {
    assert_asts_match("new System.Object() |> ignore\n");
}

/// A `new` inside a class's local `let` — the motivating snippet from the
/// report (a `new` RHS in object-model member-let position).
#[test]
fn diff_ast_new_in_class_local_let() {
    assert_asts_match(
        "type C() =\n    let history = new System.Object()\n    member _.X = history\n",
    );
}

// ---- error-recovery shapes ----------------------------------------------

/// `new T` with **no** constructor args — FCS's `NEW atomType
/// opt_HIGH_PRECEDENCE_APP error` recovery builds `SynExpr.New(_, type,
/// ArbitraryAfterError, _)` *with* a parse error. We don't model FCS's
/// `ArbitraryAfterError` placeholder in the diff harness, so this is a
/// parser-level assertion rather than a diff: our parser must record an error,
/// still emit a `NEW_EXPR` (carrying the type), and stay lossless — never
/// panic.
#[test]
fn new_missing_args_recovers_without_panic() {
    let src = "let x = new Foo\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for the argless `{src}`",
    );
    // Lossless: the whole input is still covered by the green tree.
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    // The type survives onto a `NEW_EXPR` node.
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == borzoi_cst::syntax::SyntaxKind::NEW_EXPR),
        "the argless recovery must still emit a NEW_EXPR carrying the type",
    );
}

// ---- the slice does not regress the explicit-constructor member ----------

/// The member-definition explicit constructor `new(a) = …` (phase 9.10b) is a
/// `SynMemberDefn.Member`, *not* a `SynExpr.New`; adding the expression form
/// must leave it parsing as before.
#[test]
fn explicit_ctor_member_still_parses() {
    let src = "type C =\n    new(a) = { x = a }\n";
    // Just assert no panic / structural acceptance via the diff harness path is
    // out of scope here (record-expr body); instead pin that our parser does not
    // newly reject the `new(` member head.
    let parse = parse(src);
    // The record-expr `{ x = a }` body is not in scope for this slice's harness,
    // but the `new(a) =` head must not regress to a `new`-*expression* error.
    assert!(
        !parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected a type after `new`")),
        "explicit ctor wrongly routed through the new-expression parser: {:?}",
        parse.errors,
    );
}
