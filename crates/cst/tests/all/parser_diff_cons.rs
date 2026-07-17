//! Differential test (`parser::parse` vs FCS): the expression-level cons
//! operator `::` (`SynExpr` `declExpr COLON_COLON declExpr`, `pars.fsy:4765`).
//!
//! Unlike the `mkSynInfix` operators (`+`, `*`, …), FCS lowers `a :: b` to a
//! *single* `App(NonAtomic, isInfix=true, op_ColonColon, Tuple(false, [a; b]))`
//! — the operator applied to a synthesised pair — rather than the two-tier
//! `App(App(op, lhs), rhs)` shape. Our parser builds a dedicated `CONS_EXPR`
//! green node and the normaliser projects it to that App-of-Tuple shape, so the
//! diff against FCS lines up. `::` is right-associative (`%right COLON_COLON`,
//! `pars.fsy:361`) and sits between `@`/`^` (looser) and `:?` / `+`/`-`
//! (tighter).

use crate::common::assert_asts_match;

/// `a :: b` — the minimal cons. FCS: `App(NonAtomic, isInfix=true,
/// LongIdent ["::"], Tuple(false, [Ident a; Ident b]))`. Pins the core
/// `CONS_EXPR` → App-of-Tuple projection (and the `op_ColonColon`
/// `OriginalNotation "::"` unwrap on the FCS side).
#[test]
fn diff_ast_cons_two_idents() {
    assert_asts_match("let xs = a :: b\n");
}

/// `a :: b :: c` — right-associative per `%right COLON_COLON`. FCS nests the
/// tail: `App(::, Tuple[a, App(::, Tuple[b, c])])`. Pins that the cons branch
/// recurses at `rbp == lbp` (right-leaning chain).
#[test]
fn diff_ast_cons_right_associative() {
    assert_asts_match("let xs = a :: b :: c\n");
}

/// `a :: b + c` — `+` (PLUS_MINUS, `pars.fsy:364`) binds tighter than `::`
/// (`:361`), so FCS groups `a :: (b + c)`. The cons RHS parsed at the cons
/// rbp keeps consuming the tighter `+`.
#[test]
fn diff_ast_cons_looser_than_plus() {
    assert_asts_match("let xs = a :: b + c\n");
}

/// `a + b :: c` — the mirror: `+` tighter, so `a + b` groups first and `::`
/// wraps the whole left operand → `(a + b) :: c`.
#[test]
fn diff_ast_plus_tighter_than_cons_lhs() {
    assert_asts_match("let xs = a + b :: c\n");
}

/// `a @ b :: c` — `@` (INFIX_AT_HAT_OP, `pars.fsy:360`) is *looser* than `::`
/// (`:361`), so the cons binds first → `a @ (b :: c)`. Pins the precedence
/// ordering between the at/hat band and cons.
#[test]
fn diff_ast_cons_tighter_than_at() {
    assert_asts_match("let xs = a @ b :: c\n");
}

/// `a :: b, c` — the tuple comma (`pars.fsy:346`) sits below `::`, so FCS
/// produces `Tuple[a :: b, c]` (the cons binds inside the first element).
#[test]
fn diff_ast_cons_inside_tuple_first_element() {
    assert_asts_match("let xs = a :: b, c\n");
}

/// `a, b :: c` — symmetric: the cons binds inside the *second* tuple element
/// → `Tuple[a, b :: c]`.
#[test]
fn diff_ast_cons_inside_tuple_second_element() {
    assert_asts_match("let xs = a, b :: c\n");
}

/// `f x :: g y` — application binds tighter than every infix band, so each
/// operand is a full application: `App(::, Tuple[App(f, x), App(g, y)])`.
#[test]
fn diff_ast_cons_of_applications() {
    assert_asts_match("let xs = f x :: g y\n");
}

/// `1 :: []` — the canonical list-build form: a literal head consed onto an
/// empty list. FCS: `App(::, Tuple[Const 1, ArrayOrList []])`.
#[test]
fn diff_ast_cons_onto_empty_list() {
    assert_asts_match("let xs = 1 :: []\n");
}

/// `(a) :: b` — a parenthesised LHS. Inside the paren the body stops at the
/// LexFilter-swallowed `)`, and the enclosing frame takes the `::` — pins the
/// swallowed-closer gate in `peek_cons_continuation` (without it the cons
/// would wrongly build inside the paren).
#[test]
fn diff_ast_cons_paren_lhs() {
    assert_asts_match("let xs = (a) :: b\n");
}

/// `(a :: b)` — a parenthesised cons. Confirms the cons builds *inside* the
/// paren when both operands precede the `)`.
#[test]
fn diff_ast_cons_inside_paren() {
    assert_asts_match("let xs = (a :: b)\n");
}

/// The reported motivating case: `hits <- attr :: hits`. The `<-` assignment
/// (`pars.fsy:343 %right LARROW`) is the loosest operator, so its RHS is the
/// full `declExpr` `attr :: hits`. Pins that the cons parses on the right of
/// an assignment (it previously drained `:: hits` as error recovery).
#[test]
fn diff_ast_cons_on_assignment_rhs() {
    assert_asts_match("let f () =\n    let mutable hits = []\n    hits <- attr :: hits\n");
}

/// `a :: b <- c` — `<-` (`%right LARROW`, the loosest) binds a `minusExpr` LHS,
/// and `a :: b` is a `declExpr`, so the assignment cannot take the cons as its
/// target. FCS instead parses the cons RHS as a `declExpr` containing the
/// assignment: `a :: (b <- c)`. Pins that the cons branch's `built_continuation`
/// flag leaves the `<-` for the recursive RHS frame (where `b` is a bare
/// `minusExpr` target).
#[test]
fn diff_ast_cons_rhs_absorbs_assignment() {
    assert_asts_match("let g () = a :: b <- c\n");
}

/// `a :: b :> T` — `:>` (upcast, `pars.fsy:358`) is *looser* than `::`, so the
/// cons binds first and the cast wraps the whole left operand:
/// `(a :: b) :> T`. Pins the cons/type-relation interleave in the unified Pratt
/// loop.
#[test]
fn diff_ast_cons_tighter_than_upcast() {
    assert_asts_match("let h = a :: b :> T\n");
}

/// `a :: b :? T` — `:?` (type-test, `pars.fsy:363`) is *tighter* than `::`, so
/// the test binds inside the cons tail: `a :: (b :? T)`. The mirror of
/// [`diff_ast_cons_tighter_than_upcast`].
#[test]
fn diff_ast_typetest_tighter_than_cons() {
    assert_asts_match("let i = a :: b :? T\n");
}

/// `- a :: b` — the prefix `minusExpr` `- a` is the cons LHS (application /
/// prefix bands all bind tighter than `::`): `(- a) :: b`. Pins that a
/// `parse_minus_expr`-built prefix flows into the cons branch as the head.
#[test]
fn diff_ast_cons_lhs_prefix_minus() {
    assert_asts_match("let j = - a :: b\n");
}

/// `a :: b :: c :: d` — a longer right-associative chain, nesting three deep:
/// `a :: (b :: (c :: d))`. Stress-pins the `rbp == lbp` recursion.
#[test]
fn diff_ast_cons_long_right_chain() {
    assert_asts_match("let k = a :: b :: c :: d\n");
}
