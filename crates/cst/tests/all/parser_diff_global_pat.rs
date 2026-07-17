//! Differential test (`parser::parse` vs FCS): the `global`-rooted *pattern*
//! long-identifier head — FCS's `atomicPatternLongIdent` alternative `GLOBAL DOT
//! pathOp` (`global.N.Case`) (`pars.fsy:2263`). It yields
//! `SynPat.LongIdent(SynLongIdent ids, …)` — the same shape an ordinary dotted
//! head (`A.B.Case`) produces — with the leading segment's `idText` the reused
//! keyword `"global"`.
//!
//! This mirrors the expression-side `global` head already handled in
//! `parser_diff_global.rs` (`expr_atom.rs`): `global.Foo.Bar` →
//! `SynExpr.LongIdent`. The pattern head was an explicitly-deferred slice noted
//! in `pat.rs`; the corpus file `tests/fsharp/core/namespaces/test.fs` (`|
//! global.Microsoft.FSharp.Core.None -> …`) needs it.
//!
//! The normalised-AST projector strips a single surrounding backtick pair from
//! each `SynLongIdent` segment, so both sides line up on the bare `global` text.
//!
//! Note the asymmetry with the expression side: a *bare* `global` (no `.`
//! qualification) is a **valid** expression but an **invalid** pattern (FCS
//! errors FS0010), so it is deliberately *not* tested here as an accept case —
//! see the `pat.rs` unit tests for the reject side.
//!
//! FCS's sibling `UNDERSCORE DOT pathOp` (`_.M`) is gated on the F# 4.7
//! `SingleUnderscorePattern` feature and lands with that gate in a later slice.

use crate::common::assert_asts_match;

/// A `global.`-rooted pattern in a `match` clause — the motivating corpus shape.
#[test]
fn diff_global_pat_in_match_clause() {
    assert_asts_match("match x with\n| global.A.B -> 1\n| _ -> 2\n");
}

/// The exact corpus path from `namespaces/test.fs`.
#[test]
fn diff_global_pat_full_core_path() {
    assert_asts_match("match x with\n| global.Microsoft.FSharp.Core.None -> 1\n| _ -> 2\n");
}

/// A two-segment `global.Foo` pattern head.
#[test]
fn diff_global_pat_single_qualification() {
    assert_asts_match("match x with\n| global.Foo -> 1\n| _ -> 2\n");
}

/// A `global.`-rooted union-case pattern with an argument
/// (`global.M.Case x`) — the `LongIdent` carries a nested arg pattern.
#[test]
fn diff_global_pat_with_arg() {
    assert_asts_match("match x with\n| global.M.Case y -> y\n| _ -> 0\n");
}

/// A `global.`-rooted pattern nested in a parenthesised / tuple pattern.
#[test]
fn diff_global_pat_in_tuple() {
    assert_asts_match("match x with\n| (global.A.B, y) -> y\n| _ -> 0\n");
}
