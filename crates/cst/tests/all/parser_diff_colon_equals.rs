//! Differential test (`parser::parse` vs FCS): the `:=` (`COLON_EQUALS`)
//! ref-cell assignment operator (`pars.fsy:4658 declExpr COLON_EQUALS
//! declExpr`, lowered by `mkSynInfix`). Unlike `<-` (`mkSynAssign`, which has
//! the dedicated `LongIdentSet` / `Set` nodes), `:=` is an *ordinary infix
//! operator* — the same two-tier `App(App(op_ColonEquals, lhs), rhs)` shape as
//! `+` / `*`, with the source text carried in `IdentTrivia.OriginalNotation`.
//!
//! Its precedence is the only reason it is parsed *above* the tuple loop
//! rather than in the Pratt classifier: `%right COLON_EQUALS` (pars.fsy:344)
//! sits between `%right LARROW` (`<-`, line 343) and `%left COMMA` (the tuple,
//! line 346). So both operands are tuple-inclusive and `:=` is
//! right-associative, while `<-` (whose LHS is a bare `minusExpr`) stays one
//! frame down and the two compose. Every shape below was ground-truthed with
//! `dotnet tools/fcs-dump ast` before the parser change.

use crate::common::assert_asts_match;

// ---- the basic operator -------------------------------------------------

/// `r := a` — the motivating shape: `App(App(op_ColonEquals, r), a)`, the
/// identical `mkSynInfix` two-tier App an ordinary infix op produces.
#[test]
fn diff_ast_colon_equals_basic() {
    assert_asts_match("r := a\n");
}

/// `r := 1` — a literal RHS, the canonical `ref`-cell write.
#[test]
fn diff_ast_colon_equals_literal_rhs() {
    assert_asts_match("r := 1\n");
}

// ---- tuple-inclusive on both sides --------------------------------------

/// `r := a, b` — `:=` binds looser than the comma, so the RHS is the whole
/// tuple: `App(App(:=, r), Tuple(a, b))`.
#[test]
fn diff_ast_colon_equals_rhs_tuple() {
    assert_asts_match("r := a, b\n");
}

/// `a, b := c` — and the *LHS* is tuple-inclusive too: `App(App(:=,
/// Tuple(a, b)), c)`. This is the exact case the old Pratt-level exclusion
/// existed to avoid mis-nesting (`(a := b), c`).
#[test]
fn diff_ast_colon_equals_lhs_tuple() {
    assert_asts_match("a, b := c\n");
}

/// `a, b := c, d` — both operands are tuples.
#[test]
fn diff_ast_colon_equals_both_tuples() {
    assert_asts_match("a, b := c, d\n");
}

// ---- associativity ------------------------------------------------------

/// `a := b := c` — `%right COLON_EQUALS`, so this is `App(App(:=,a),
/// App(App(:=,b),c))`. Right-associativity falls out of parsing the RHS back
/// at the `:=` level.
#[test]
fn diff_ast_colon_equals_right_assoc() {
    assert_asts_match("a := b := c\n");
}

// ---- interaction with infix operators -----------------------------------

/// `a + b := c` — `+` binds tighter, so the `:=` LHS is the whole `a + b`
/// infix subtree: `App(App(:=, App(App(+,a),b)), c)`.
#[test]
fn diff_ast_colon_equals_infix_lhs() {
    assert_asts_match("a + b := c\n");
}

/// `r := a + b` — symmetric: the RHS carries the infix subtree.
#[test]
fn diff_ast_colon_equals_infix_rhs() {
    assert_asts_match("r := a + b\n");
}

// ---- interaction with `<-` ----------------------------------------------

/// `x <- y := z` — `<-` is looser, so it is outermost and its RHS is the whole
/// `:=`: `LongIdentSet(x, App(App(:=,y),z))`. The `<-` RHS
/// (`parse_assign_rhs`) re-enters the `:=` level.
#[test]
fn diff_ast_arrow_then_colon_equals() {
    assert_asts_match("x <- y := z\n");
}

/// `a := b <- c` — `:=` is outermost here, because `<-`'s LHS must be a
/// `minusExpr` (so `(a := b) <- c` is ungrammatical): `App(App(:=,a),
/// Set(b,c))`.
#[test]
fn diff_ast_colon_equals_then_arrow() {
    assert_asts_match("a := b <- c\n");
}

// ---- parens / swallowed-closer ------------------------------------------

/// `(a) := 1` — `:=` binds the *whole* `Paren(a)` from outside:
/// `App(App(:=, Paren(a)), 1)`. The swallowed-closer guard keeps the paren
/// body's own `parse_expr` from folding the outer `:=` into `Paren(Set …)`.
#[test]
fn diff_ast_colon_equals_paren_lhs() {
    assert_asts_match("(a) := 1\n");
}

/// `(r := 1)` — `:=` parses *inside* the parens: `Paren(App(App(:=,r),1))`.
#[test]
fn diff_ast_colon_equals_inside_paren() {
    assert_asts_match("(r := 1)\n");
}

/// `(a := 1) + b` — a parenthesised `:=` as an infix operand:
/// `App(App(+, Paren(App(App(:=,a),1))), b)`.
#[test]
fn diff_ast_colon_equals_paren_then_infix() {
    assert_asts_match("(a := 1) + b\n");
}

// ---- in expression positions --------------------------------------------

/// `:=` as a `let`-binding RHS — `let x = r := 1` is a full `declExpr` RHS, so
/// the `:=` parses there.
#[test]
fn diff_ast_colon_equals_let_rhs() {
    assert_asts_match("let x = r := 1\n");
}

/// `:=` as an `if`-branch body.
#[test]
fn diff_ast_colon_equals_if_branch() {
    assert_asts_match("if c then r := 1 else r := 2\n");
}

// ---- interaction with the `..` range operator ---------------------------

/// `r := 1..3` — the `..` range binds tighter than the comma, which binds
/// tighter than `:=`, so the RHS is the whole range: `App(App(:=, r),
/// IndexRange(1, 3))`. Pins that `:=` nests *above* the `parse_range_expr`
/// level (`:=` 344 > comma 346 > `..` 348 in `pars.fsy`).
#[test]
fn diff_ast_colon_equals_range_rhs() {
    assert_asts_match("r := 1..3\n");
}

/// `cell := ..3` — an *open-lower* range RHS (`IndexRange(None, Some 3)`).
/// `..` is a `declExpr` starter that the broad `peek_is_expr_start` admits but
/// the narrower infix-level `is_expr_start_at` does not — so this pins that
/// `:=`'s RHS uses the gate matching its `parse_expr` production.
#[test]
fn diff_ast_colon_equals_open_range_rhs() {
    assert_asts_match("cell := ..3\n");
}

// ---- interaction with CE binders ----------------------------------------

/// `r := do! m` inside a computation expression — `do!` (`SynExpr.DoBang`) is a
/// virtual `declExpr` starter `parse_expr` handles, so the `:=` RHS is the
/// whole `do! m`: `App(App(:=, r), DoBang m)`. Same broad-gate requirement as
/// the open range: confirms `:=` admits every starter `parse_expr` accepts,
/// matching FCS (which parses `r + do! m` the same way).
#[test]
fn diff_ast_colon_equals_do_bang_rhs() {
    assert_asts_match("async {\n    r := do! m\n}\n");
}
