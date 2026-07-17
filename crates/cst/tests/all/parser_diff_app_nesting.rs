//! Differential test (`parser::parse` vs FCS): the precedence of
//! high-precedence (adjacent) application against whitespace application.
//! FCS's `atomicExpr` is left-recursive and *tighter* than `appExpr argExpr`,
//! so an adjacent `g(x)` binds before a surrounding whitespace application:
//! `f g(x)` is `f (g(x))`, not `(f g) (x)`. These pin that nesting, now that
//! the atomic flag round-trips (so the inner atomic app diffs against FCS).
//!
//! Some no-space argument forms below still make FCS set `ParseHadErrors` while
//! emitting the recovery AST shown in the comments. Those use the explicit
//! `fcs_rejects_ours_accepts` helper so the parser acceptance gap is visible.

use crate::common::{assert_asts_match, assert_asts_match_fcs_rejects_ours_accepts};

/// `f g(x)` — whitespace application of `f` to the *adjacent* application
/// `g(x)`. FCS: `App(NonAtomic, f, App(Atomic, g, (x)))` = `f (g(x))`. The
/// adjacent `g(x)` binds tighter than the surrounding `f _`.
#[test]
fn diff_ast_ws_then_adjacent() {
    assert_asts_match_fcs_rejects_ours_accepts("f g(x)\n");
}

/// `f g (x)` — all whitespace-separated, so left-associative:
/// `App(App(f, g), (x))` = `(f g) (x)`. Contrast with `f g(x)`: the only
/// difference is the space before `(`, which flips the nesting.
#[test]
fn diff_ast_all_whitespace() {
    assert_asts_match("f g (x)\n");
}

/// `f(x) g` — adjacent application `f(x)` then a whitespace argument `g`:
/// `App(NonAtomic, App(Atomic, f, (x)), g)` = `(f(x)) g`.
#[test]
fn diff_ast_adjacent_then_ws() {
    assert_asts_match("f(x) g\n");
}

/// `f(x)(y)` — a chain of adjacent applications stays left-associative within
/// the atomic level: `App(Atomic, App(Atomic, f, (x)), (y))`.
#[test]
fn diff_ast_adjacent_chain() {
    assert_asts_match("f(x)(y)\n");
}

/// `f g h(x)` — only the last argument is adjacent: `h(x)` binds tight, the
/// rest is left-associative whitespace application:
/// `App(App(f, g), App(h, (x)))` = `(f g) (h(x))`.
#[test]
fn diff_ast_ws_chain_trailing_adjacent() {
    assert_asts_match_fcs_rejects_ours_accepts("f g h(x)\n");
}

/// `f a(x) b` — an adjacent application in the *middle* of a whitespace
/// chain: `((f (a(x))) b)`. The adjacent `a(x)` is one argument atom; the
/// outer chain stays left-associative around it.
#[test]
fn diff_ast_adjacent_in_middle() {
    assert_asts_match_fcs_rejects_ours_accepts("f a(x) b\n");
}

/// `f -g(x)` — an *adjacent-prefix* argument (`-g`) whose operand itself has
/// an adjacent call. FCS's `ADJACENT_PREFIX_OP atomicExpr` binds the `(x)`
/// inside the prefix operand: `App(f, App(~-, App(g, (x))))` = `f (-(g(x)))`.
/// Regression guard — the prefix operand must be a full `atomicExpr`
/// (`parse_postfix_expr`), not just an atom, or the `(x)` is stranded.
#[test]
fn diff_ast_adjacent_prefix_arg_with_call() {
    assert_asts_match_fcs_rejects_ours_accepts("f -g(x)\n");
}

/// `f &g(x)` — the address-of counterpart: `App(f, AddressOf(App(g, (x))))`.
/// The `&` operand is `atomicExpr` too, so `(x)` binds inside it.
#[test]
fn diff_ast_adjacent_addressof_arg_with_call() {
    assert_asts_match_fcs_rejects_ours_accepts("f &g(x)\n");
}
