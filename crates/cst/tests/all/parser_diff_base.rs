//! Differential test (`parser::parse` vs FCS): base-class member access
//! `base.Member` — FCS's `BASE DOT atomicExprQualification` (`pars.fsy:5276`).
//!
//! FCS builds `SynExpr.Ident("base")` for the `base` keyword, then folds the
//! `.Member` qualification onto it via `mkSynDot` — exactly as for an ordinary
//! identifier head — so `base.Foo` is `SynExpr.LongIdent(["base"; "Foo"])`,
//! `base.M()` is `App(LongIdent(["base"; "M"]), ())`, etc. The `base` keyword
//! **requires** a `.` qualification: a bare `base` is an FCS parse error.

use crate::common::assert_asts_match;

/// A single base member access `base.Foo` → `LongIdent(["base"; "Foo"])`.
#[test]
fn diff_base_member() {
    assert_asts_match("let x = base.Foo\n");
}

/// A multi-segment base path `base.A.B` → `LongIdent(["base"; "A"; "B"])`.
#[test]
fn diff_base_path() {
    assert_asts_match("let x = base.A.B\n");
}

/// A base method call `base.M()` → `App(LongIdent(["base"; "M"]), unit)`.
#[test]
fn diff_base_method_call() {
    assert_asts_match("let x = base.M()\n");
}

/// A base method call with arguments `base.M(1, 2)`.
#[test]
fn diff_base_method_call_args() {
    assert_asts_match("let x = base.M(1, 2)\n");
}

/// A dotted indexer off a base member `base.Item.[0]` →
/// `DotIndexedGet(LongIdent(["base"; "Item"]), 0)`.
#[test]
fn diff_base_indexed() {
    assert_asts_match("let x = base.Item.[0]\n");
}

/// An assignment to a base property `base.P <- 1` →
/// `LongIdentSet(["base"; "P"], 1)` (the `mkSynAssign` long-ident arm).
#[test]
fn diff_base_property_set() {
    assert_asts_match("let _ = base.P <- 1\n");
}

/// A *direct* dotted indexer off `base` (no intervening member): `base.[0]` →
/// `DotIndexedGet(Ident("base"), 0)`. The `.[` qualification is a valid
/// `atomicExprQualification`, so `base` stays a bare `Ident` head and the
/// postfix tail builds the indexer.
#[test]
fn diff_base_direct_indexed() {
    assert_asts_match("let x = base.[0]\n");
}

/// An indexed-set off `base` `base.[0] <- 1` → `DotIndexedSet(Ident("base"), 0,
/// 1)`.
#[test]
fn diff_base_direct_indexed_set() {
    assert_asts_match("let _ = base.[0] <- 1\n");
}

/// `base` inside a member override — the motivating shape (`override _.M() =
/// base.M()`), exercising it in a real type body.
#[test]
fn diff_base_in_override() {
    assert_asts_match("type T() =\n  inherit B()\n  override _.M() = base.M()\n");
}
