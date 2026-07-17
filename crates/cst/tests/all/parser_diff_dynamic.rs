//! Differential test (`parser::parse` vs FCS): the dynamic-lookup operator
//! `a?b` — FCS's `SynExpr.Dynamic(funcExpr, qmarkRange, argExpr, range)`.
//!
//! FCS's grammar `atomicExpr QMARK dynamicArg` (`pars.fsy:5284`) parses `?` as a
//! *postfix* operator at the precedence of `.` (`%left DOT QMARK`,
//! `pars.fsy:377`), left-associative and with no adjacency requirement (`a?b`
//! and `a ? b` both parse). The `dynamicArg` is either a single `IDENT`
//! (`SynExpr.Ident`, the dynamic member name) or a parenthesised
//! `( typedSequentialExpr )` (`SynExpr.Paren`). Because it sits in the postfix
//! tail, `.member` / adjacent application chain onto the *whole* dynamic
//! expression: `a?b.c` is `DotGet(Dynamic(a, b), [c])`, `a?b(c)` is
//! `App(Dynamic(a, b), (c))`, and `a?b?c` is `Dynamic(Dynamic(a, b), c)`.

use crate::common::assert_asts_match;

/// The canonical dynamic lookup `a?b` → `Dynamic(Ident a, Ident b)`.
#[test]
fn diff_dynamic_ident_arg() {
    assert_asts_match("let x = a?b\n");
}

/// Spaces around `?` make no difference (`a ? b` is still `Dynamic`).
#[test]
fn diff_dynamic_spaced() {
    assert_asts_match("let x = a ? b\n");
}

/// A parenthesised dynamic argument `a?(b + c)` → `Dynamic(a, Paren(b + c))`.
#[test]
fn diff_dynamic_paren_arg() {
    assert_asts_match("let x = a?(b + c)\n");
}

/// Left-associative chaining `a?b?c` → `Dynamic(Dynamic(a, b), c)`.
#[test]
fn diff_dynamic_chain() {
    assert_asts_match("let x = a?b?c\n");
}

/// A dotted member access chains onto the whole dynamic expression:
/// `a?b.c` → `DotGet(Dynamic(a, b), [c])`.
#[test]
fn diff_dynamic_then_dot() {
    assert_asts_match("let x = a?b.c\n");
}

/// A dotted-path LHS: `a.b?c` → `Dynamic(LongIdent [a; b], c)`.
#[test]
fn diff_dynamic_dotted_lhs() {
    assert_asts_match("let x = a.b?c\n");
}

/// Adjacent application chains onto the dynamic: `a?b(c)` →
/// `App(Dynamic(a, b), Paren c)`.
#[test]
fn diff_dynamic_then_app() {
    assert_asts_match("let x = a?b(c)\n");
}

/// A parenthesised LHS: `(f x)?y` → `Dynamic(Paren(App(f, x)), y)`.
#[test]
fn diff_dynamic_paren_lhs() {
    assert_asts_match("let x = (f x)?y\n");
}

/// A *typed* parenthesised argument `a?(b : int)` — FCS's `dynamicArg` `(` body
/// is a `typedSequentialExpr`, so a trailing `: T` annotation is part of the
/// argument (`Dynamic(a, Paren(Typed(b, int)))`).
#[test]
fn diff_dynamic_typed_paren_arg() {
    assert_asts_match("let f a = a?(b : int)\n");
}

/// A *nested* trait call as the argument `a?((^T : (static member M : …) x))` —
/// the dynamic-argument parens are a bare `typedSequentialExpr`, so the trait
/// call must sit in its *own* inner parens (the bare `a?(^T : …)` is an FCS
/// error, covered by a parser-only recovery test). This confirms the trait-call
/// form is still reachable when properly nested.
#[test]
fn diff_dynamic_nested_trait_call_arg() {
    assert_asts_match("let f a x = a?((^T : (static member M : ^T -> int) x))\n");
}

/// A mixed chain stressing the postfix-tail interaction of `?`, `.`, and the
/// parenthesised argument: `obj?(name)?other` →
/// `Dynamic(Dynamic(obj, Paren name), other)`.
#[test]
fn diff_dynamic_paren_then_ident_chain() {
    assert_asts_match("let x = obj?(name)?other\n");
}

/// `?` and `.` interleaved: `a?b?c.d?e` →
/// `Dynamic(DotGet(Dynamic(Dynamic(a, b), c), [d]), e)`.
#[test]
fn diff_dynamic_mixed_dot_chain() {
    assert_asts_match("let g = a?b?c.d?e\n");
}

/// The assignment form `a?b <- 1` composes for free: FCS lowers it to
/// `Set(Dynamic(a, b), 1)` via the generic `<-` fallback (no special
/// `DynamicSet` node), so once `Dynamic` parses the existing assignment handling
/// produces the right shape.
#[test]
fn diff_dynamic_set() {
    assert_asts_match("let _ = a?b <- 1\n");
}

// ---------------------------------------------------------------------------
// `?ident` optional named arguments (FCS's `QMARK nameop` →
// `SynExpr.LongIdent(isOptional = true, [ident], …)`, `pars.fsy:5280`). A
// *prefix* `?` at an expression head (no preceding atom) is the caller-side
// optional-argument form `M(?opt = value)` — distinct from the *postfix*
// dynamic `a?b` above. FCS elides `isOptional` in the AST projection, so both
// sides reduce to a one-segment `LongIdent`.
// ---------------------------------------------------------------------------

/// The canonical optional named argument — `M(?opt = value)` →
/// `App(M, Paren(App(op_Equality, LongIdent [opt], Ident value)))`.
#[test]
fn diff_optional_named_arg() {
    assert_asts_match("let r = M(?opt = value)\n");
}

/// An optional arg following a positional one — `M(x, ?opt = None)`. The
/// `?opt = None` is the second tuple element.
#[test]
fn diff_optional_named_arg_after_positional() {
    assert_asts_match("let r = M(x, ?opt = None)\n");
}

/// A bare `?opt` argument (passing an `option` directly to an optional
/// parameter) — `M(?opt)` → `App(M, Paren(LongIdent [opt]))`.
#[test]
fn diff_optional_named_arg_bare() {
    assert_asts_match("let r = M(?opt)\n");
}

/// Two optional named arguments — `M(?a = x, ?b = y)`.
#[test]
fn diff_optional_named_arg_multiple() {
    assert_asts_match("let r = M(?a = x, ?b = y)\n");
}

/// Regression guard for the head-vs-postfix boundary: a `?` *after* a
/// parenthesised atom is still the dynamic operator, `(a)?b` →
/// `Dynamic(Paren a, b)`, not an optional-arg head.
#[test]
fn diff_paren_then_dynamic_not_optional() {
    assert_asts_match("let x = (a)?b\n");
}

/// `?opt` as a bare application argument — `f ?opt` → `App(f, LongIdent [opt])`.
/// FCS's `QMARK nameop` is an `atomicExpr`, so it is a valid app argument.
#[test]
fn diff_optional_named_arg_as_app_arg() {
    assert_asts_match("let r = f ?opt\n");
}

/// `?opt` as an infix RHS — `x + ?opt` → `App(+, x, LongIdent [opt])`. Confirms
/// the atomic-expr-start gates admit `?ident` in operand position too.
#[test]
fn diff_optional_named_arg_infix_rhs() {
    assert_asts_match("let r = x + ?opt\n");
}
