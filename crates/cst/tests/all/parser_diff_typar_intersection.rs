//! Differential test (`parser::parse` vs FCS): typar-declaration **intersection
//! constraints** `'t & #seq<int>` — FCS's `SynTyparDecl(attrs, typar,
//! intersectionConstraints, trivia)` (`pars.fsy:2570`, `typarDecl: opt_attributes
//! typar AMP intersectionConstraints`, the `ConstraintIntersectionOnFlexibleTypes`
//! feature). After a typar in a `<…>` declaration list, an `&`-separated run of
//! flexible-type (`#T`) constraints attaches to that typar's declaration — a
//! *different* construct from `SynType.Intersection` (`'T & 'U` in type
//! position). Each constraint is a `hashConstraint` (`#atomType`); a bare
//! `atomType` operand is an FCS error (still parsed) covered by the
//! shared-with-`SynType.Intersection` operand check.

use crate::common::assert_asts_match;

/// A single flexible-type constraint on a type-definition header typar.
#[test]
fn diff_single_intersection_constraint_on_type() {
    assert_asts_match("type C<'t & #seq<int>> = class end\n");
}

/// Several `&`-chained constraints on one typar — the `Constraint intersection
/// 01.fs` first-typar shape (`#seq<int> & #IDisposable & #I`).
#[test]
fn diff_multiple_intersection_constraints() {
    assert_asts_match("type C<'t & #seq<int> & #System.IDisposable & #I> = class end\n");
}

/// Two typars, each carrying its own intersection constraints — the whole
/// `Constraint intersection 01.fs` header (`'t & #seq<int>, 'y & #seq<'t>`).
#[test]
fn diff_two_typars_each_with_intersection() {
    assert_asts_match("type C<'t & #seq<int>, 'y & #seq<'t>> = class end\n");
}

/// A `let inline` head with an intersection constraint whose flexible type is a
/// multi-argument application — the `ConstrainedAndInterfaceCalls.fs` shape
/// `<'T & #IAdditionOperators<'T, 'T, 'T>>`.
#[test]
fn diff_intersection_constraint_on_let() {
    assert_asts_match("let f<'T & #IAdditionOperators<'T, 'T, 'T>> (x: 'T) = x\n");
}

/// An intersection constraint alongside an ordinary comma-separated plain typar
/// — `<'a, 'b & #IDisposable>` — so the plain and intersection-bearing decls
/// share one list.
#[test]
fn diff_intersection_mixed_with_plain_typar() {
    assert_asts_match("type C<'a, 'b & #System.IDisposable> = class end\n");
}

/// The intersection-constrained typar heads a real member body — the whole
/// `Constraint intersection 01.fs` shape including the member.
#[test]
fn diff_intersection_with_member_body() {
    assert_asts_match("type C<'t & #seq<int>> =\n    member _.G (x: 't) = ()\n");
}
