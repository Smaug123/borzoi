//! Differential test (`parser::parse` vs FCS): the `ExprAtomicFlag` of
//! function application — FCS's `SynExpr.App(flag, isInfix, f, x, _)`. The
//! adjacent form `f(x)` is `Atomic` (`flag = 0`); the whitespace-separated
//! `f (x)` is `NonAtomic` (`flag = 1`). The parser records the atomic case
//! with a [`HIGH_PRECEDENCE_PAREN_APP_TOK`] marker; both normaliser sides
//! project the flag so these diff against the oracle.

use crate::common::assert_asts_match;

/// `f(x)` — adjacent application, `App(Atomic, false, f, (x))`. The marker
/// makes our projection report `is_atomic: true`, matching FCS.
#[test]
fn diff_ast_atomic_app() {
    assert_asts_match("f(x)\n");
}

/// `f (x)` — whitespace application, `App(NonAtomic, false, f, (x))`. No
/// marker, so `is_atomic: false`. Confirms the flag distinguishes the two
/// forms (otherwise `f(x)` and `f (x)` would normalise equal).
#[test]
fn diff_ast_nonatomic_app() {
    assert_asts_match("f (x)\n");
}

/// `f()` — adjacent application to unit. Atomic.
#[test]
fn diff_ast_atomic_app_unit() {
    assert_asts_match("f()\n");
}

/// `f(x)(y)` — a chain of adjacent applications. Only the *ident*-adjacent
/// `(x)` is high-precedence (`Atomic`); the `(y)` after `)` is markerless, so
/// FCS makes the outer application `NonAtomic`:
/// `App(NonAtomic, App(Atomic, f, (x)), (y))`. (Our atomic postfix tail
/// consumes the marked `(x)`; the markerless `(y)` is picked up by the
/// whitespace-application loop.)
#[test]
fn diff_ast_atomic_app_chain() {
    assert_asts_match("f(x)(y)\n");
}

/// `f(x) y` — atomic application then a whitespace argument:
/// `App(NonAtomic, App(Atomic, f, (x)), y)`. The inner layer is atomic, the
/// outer is not — exercising both flags in one tree.
#[test]
fn diff_ast_atomic_then_whitespace() {
    assert_asts_match("f(x) y\n");
}
