//! Differential test (`parser::parse` vs FCS): the `<-` mutation operator
//! (`pars.fsy:4661 minusExpr LARROW declExprBlock`, dispatched by
//! `mkSynAssign`, `SyntaxTreeOps.fs:518`). Stage A covers the LHS shapes our
//! parser can already build: `Ident`/`LongIdent` → `SynExpr.LongIdentSet`,
//! and the `mkSynAssign` fallback → `SynExpr.Set`. Stage B adds
//! `SynExpr.NamedIndexedPropertySet` — an application whose function is a
//! `LongIdent` (`Type.Items(i) <- e`). The `DotGet`-/`DotIndexedGet`-keyed
//! variants (`DotSet`, `DotIndexedSet`, `DotNamedIndexedPropertySet`) are
//! tracked separately — they wait on postfix `.field` / `arr.[i]` parsing.

use crate::common::assert_asts_match;

/// `x <- 1` — assignment to a single identifier. FCS's `mkSynAssign` matches
/// the `LongOrSingleIdent` arm and produces
/// `SynExpr.LongIdentSet(SynLongIdent ["x"], Const(Int32 1), _)`.
#[test]
fn diff_ast_assign_ident() {
    assert_asts_match("x <- 1\n");
}

/// `obj.Field <- 2` — a dotted path lexes as one `SynExpr.LongIdent`, so the
/// `LongOrSingleIdent` arm still fires: `LongIdentSet(["obj"; "Field"], …)`.
/// This (not `DotSet`) is the common `receiver.field <- v` mutation shape.
#[test]
fn diff_ast_assign_dotted_path() {
    assert_asts_match("obj.Field <- 2\n");
}

/// `M.N.x <- y` — a longer dotted path; still one `LongIdent` → `LongIdentSet`.
#[test]
fn diff_ast_assign_long_path() {
    assert_asts_match("M.N.x <- y\n");
}

/// `(x) <- 1` — a parenthesised LHS. `mkSynAssign` deliberately does *not*
/// unwrap `Paren` (the unwrap arm is commented out, `SyntaxTreeOps.fs:522`),
/// so this falls through to the `Set(Paren(Ident "x"), Const 1, _)` default.
#[test]
fn diff_ast_assign_paren_lhs_is_set() {
    assert_asts_match("(x) <- 1\n");
}

/// `f x <- 2` — an application LHS. Not an `Ident`/`LongIdent` and (since
/// `f`'s func is an `Ident`, not the `LongIdent`/`DotGet` the indexed-property
/// arms require) not a property-set either, so `mkSynAssign` falls through to
/// `Set(App(f, x), Const 2, _)`.
#[test]
fn diff_ast_assign_app_lhs_is_set() {
    assert_asts_match("f x <- 2\n");
}

/// `x <- a, b` — the RHS is a full `declExprBlock`, so it swallows the tuple:
/// `LongIdentSet(["x"], Tuple [a; b], _)`. Confirms `<-` binds looser than
/// the comma on its right (the tuple nests *inside* the assignment).
#[test]
fn diff_ast_assign_tuple_rhs() {
    assert_asts_match("x <- a, b\n");
}

/// `a, x <- b` — the mirror of `x <- a, b`: here the `<-` LHS is the
/// `minusExpr` `x` only, so the comma binds the whole thing into a tuple:
/// `Tuple [a; LongIdentSet(["x"], b)]`. Confirms `<-`'s LHS is tight while
/// its RHS is loose.
#[test]
fn diff_ast_assign_as_tuple_element() {
    assert_asts_match("a, x <- b\n");
}

/// `x <- y <- z` — `%right LARROW` (`pars.fsy:343`) makes assignment
/// right-associative: `LongIdentSet(["x"], LongIdentSet(["y"], z), _)`.
#[test]
fn diff_ast_assign_right_associative() {
    assert_asts_match("x <- y <- z\n");
}

/// `a + b <- c` — the LARROW rule's LHS is `minusExpr`, *tighter* than the
/// infix `+`. So the assignment binds only `b`, and the whole parse is
/// `App(+, a, LongIdentSet(["b"], c))` ≡ `a + (b <- c)`, **not**
/// `(a + b) <- c`. This pins the operator at the `minusExpr` level inside the
/// Pratt climber.
#[test]
fn diff_ast_assign_below_infix_on_left() {
    assert_asts_match("a + b <- c\n");
}

/// `x <- if c then 1 else 2` — a control-flow RHS. FCS's `isControlFlowOrNotSameLine`
/// pushes a seq block after LARROW even on one line, so the RHS is the full
/// `if`-expression: `LongIdentSet(["x"], IfThenElse(…), _)`.
#[test]
fn diff_ast_assign_control_flow_rhs() {
    assert_asts_match("x <- if c then 1 else 2\n");
}

/// Multi-line RHS — LARROW opens an offside `CtxtSeqBlock`
/// (`LexFilter.fs:2318`), so the indented next line is the RHS:
/// `LongIdentSet(["x"], Const(Int32 1), _)`.
#[test]
fn diff_ast_assign_offside_rhs() {
    assert_asts_match("x <-\n    1\n");
}

/// Offside RHS nested inside a function body, followed by another body
/// statement. The assignment must **consume** its own `OBLOCKEND` so `y`
/// stays in `f`'s body: FCS produces `f`'s body as
/// `Sequential [LongIdentSet(["x"], 1); Ident "y"]`. Regression guard —
/// leaving the RHS block-end unconsumed (decl-style) ended the binding early
/// and dropped `y` to module level.
#[test]
fn diff_ast_assign_offside_rhs_in_body_keeps_following_stmt() {
    assert_asts_match("let f () =\n    x <-\n        1\n    y\n");
}

/// Offside RHS inside parens — `(x <-⏎    1)`. The assignment consumes its
/// RHS block-end, then the paren closes cleanly: `Paren(LongIdentSet(["x"],
/// 1))`. A leftover block-end here would surface as a spurious "expected `)`".
#[test]
fn diff_ast_assign_offside_rhs_in_parens() {
    assert_asts_match("(x <-\n    1)\n");
}

/// Two top-level assignments. The first RHS must stop at end-of-statement and
/// **not** absorb the second: two independent `LongIdentSet`s, mirroring
/// `let`-binding RHS termination.
#[test]
fn diff_ast_assign_two_statements() {
    assert_asts_match("x <- 1\ny <- 2\n");
}

// --- Stage B: NamedIndexedPropertySet -------------------------------------

/// `Foo.Bar(3) <- 4` — an application whose function is a `LongIdent`. The
/// `mkSynAssign` arm `App(_, _, LongIdent v, x, _) -> NamedIndexedPropertySet`
/// fires: `NamedIndexedPropertySet(["Foo"; "Bar"], Paren(Const 3), Const 4)`.
/// The index arg is the application argument; here the atomic `(3)` is a
/// `Paren`.
#[test]
fn diff_ast_assign_named_indexed_property_set() {
    assert_asts_match("Foo.Bar(3) <- 4\n");
}

/// `Foo.Bar 3 <- 4` — the non-atomic (whitespace) application form. The
/// atomic flag is irrelevant to `mkSynAssign` (it ignores it), so this is
/// still `NamedIndexedPropertySet`, with the bare `Const 3` as the index arg.
#[test]
fn diff_ast_assign_named_indexed_property_set_spaced() {
    assert_asts_match("Foo.Bar 3 <- 4\n");
}

/// `Foo.Bar(1, 2) <- 4` — a multi-arg index. The argument is a single
/// `Paren(Tuple [1; 2])`, so `expr1` is that paren-tuple:
/// `NamedIndexedPropertySet(["Foo"; "Bar"], Paren(Tuple [1; 2]), Const 4)`.
#[test]
fn diff_ast_assign_named_indexed_property_set_tuple_index() {
    assert_asts_match("Foo.Bar(1, 2) <- 4\n");
}

/// `f 3 <- 4` — a *single-identifier* function is `SynExpr.Ident`, **not**
/// `SynExpr.LongIdent`, so the `NamedIndexedPropertySet` arm does not match and
/// `mkSynAssign` falls through to `Set(App(Ident "f", Const 3), Const 4)`.
/// Boundary guard against over-eager property-set classification. (Spaced,
/// non-atomic application — the atomic `f(3)` form additionally exercises the
/// still-unmodelled `ExprAtomicFlag`, an orthogonal gap.)
#[test]
fn diff_ast_assign_single_ident_app_is_set() {
    assert_asts_match("f 3 <- 4\n");
}

/// `Foo.Bar 3 4 <- 5` — a *curried* application: the outer `App`'s function
/// is itself an `App`, not a `LongIdent`, so this is the `Set` fallback
/// (`Set(App(App(LongIdent, 3), 4), 5)`), not a property set. Boundary guard.
#[test]
fn diff_ast_assign_curried_app_is_set() {
    assert_asts_match("Foo.Bar 3 4 <- 5\n");
}
