//! Differential test (`parser::parse` vs FCS): FSharp.Core's static-optimization
//! `when` clauses on a binding RHS — `let inline f x = mainExpr when 'T : int =
//! e1 when 'T : float = e2`.
//!
//! FCS's `typedExprWithStaticOptimizations` grammar (`pars.fsy:3391`) attaches a
//! list of `(constraints, branchExpr)` static optimizations after the binding's
//! main RHS expression; `mkSynBindingRhs` (`SyntaxTreeOps.fs:744`) folds them
//! right into nested `SynExpr.LibraryOnlyStaticOptimization(constraints, branch,
//! fallthrough, _)`. Each constraint is a `SynStaticOptimizationConstraint`:
//! `WhenTyparTyconEqualsTycon('T, ty)` (`'T : ty`) or `WhenTyparIsStruct('T)`
//! (the bare `'T struct`). Unlike inline IL, every field is serialisable, so the
//! construct is fully modelled in the diff oracle.

use crate::common::assert_asts_match;

/// A single `'T : ty` static optimization over a fallthrough main expression.
#[test]
fn diff_single_when() {
    assert_asts_match("let inline f x =\n    g x\n    when 'T : int = h x\n");
}

/// Two clauses fold into nested `LibraryOnlyStaticOptimization`.
#[test]
fn diff_two_whens() {
    assert_asts_match(
        "let inline f x =\n    g x\n    when 'T : int = a x\n    when 'T : float = b x\n",
    );
}

/// `and`-chained conditions in one clause —
/// `WhenTyparTyconEqualsTycon` list of length 2.
#[test]
fn diff_and_chained_conditions() {
    assert_asts_match("let inline f x =\n    g x\n    when 'T : int and 'U : float = h x\n");
}

/// The bare `'T struct` form (`WhenTyparIsStruct`), no colon.
#[test]
fn diff_struct_condition() {
    assert_asts_match("let inline f x =\n    g x\n    when 'T struct = h x\n");
}

/// A generic-type condition RHS (`'T : C<'a>`) — the `typ` is a real applied type.
#[test]
fn diff_generic_type_condition() {
    assert_asts_match("let inline f x =\n    g x\n    when 'T : list<int> = h x\n");
}

/// A multi-line branch body (an `if`/`else` spanning lines) is a single branch
/// expression, not a new statement.
#[test]
fn diff_multiline_branch() {
    assert_asts_match(
        "let inline f x =\n    g x\n    when 'T : int = if a then b\n                    else c\n",
    );
}
