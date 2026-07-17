//! Differential test (`parser::parse` vs FCS): the `global` keyword as the root
//! of a qualified (long) identifier, in expression position — FCS's
//! `GLOBAL DOT …` path head (`pars.fsy`).
//!
//! FCS spells the `global` marker as an *identifier* whose `idText` is the
//! single-backtick-quoted string `` `global` `` (the keyword reused as an
//! identifier), then folds any `.Member` qualification onto it via `mkSynDot`.
//! So `global.Foo.Bar` is `SynExpr.LongIdent(["global"; "Foo"; "Bar"])` — the
//! same shape an ordinary identifier head produces — and a *bare* `global` is a
//! *single-segment* `SynExpr.LongIdent(["global"])` (NOT `SynExpr.Ident`).
//!
//! The crucial difference from `base` (see `parser_diff_base.rs`): a bare
//! `global` with no `.` qualification is **valid** (FCS accepts it), whereas a
//! bare `base` is a parse error.
//!
//! The normalised-AST projector strips a single surrounding backtick pair from
//! each `SynLongIdent` segment, so both sides line up on the bare `global` text.

use crate::common::assert_asts_match;

/// A bare `global` — VALID (unlike `base`). FCS emits a single-segment
/// `SynExpr.LongIdent(["global"])`, so our side must be a one-segment
/// `LONG_IDENT_EXPR`, not an `IDENT_EXPR`.
#[test]
fn diff_global_bare() {
    assert_asts_match("let v = global\n");
}

/// A qualified `global.Foo.Bar` → `LongIdent(["global"; "Foo"; "Bar"])`.
#[test]
fn diff_global_qualified_path() {
    assert_asts_match("let v = global.Foo.Bar\n");
}

/// A single qualification `global.Foo` → `LongIdent(["global"; "Foo"])`.
#[test]
fn diff_global_single_qualification() {
    assert_asts_match("let v = global.Foo\n");
}

/// A method call off a `global` path `global.Foo()` →
/// `App(LongIdent(["global"; "Foo"]), unit)`.
#[test]
fn diff_global_method_call() {
    assert_asts_match("let v = global.Foo()\n");
}

/// A deeper method call `global.C.M()` → `App(LongIdent(["global"; "C"; "M"]),
/// unit)`.
#[test]
fn diff_global_deep_method_call() {
    assert_asts_match("let v = global.C.M()\n");
}

/// `global` heading a real namespace path `global.System.Console` — the
/// motivating shape (disambiguating a shadowed top-level module).
#[test]
fn diff_global_system_path() {
    assert_asts_match("let v = global.System.Console\n");
}

/// `global.C.M()` inside an `if` condition — the corpus divergence that failed
/// with "expected expression after `if`". Exercises the atom head reached
/// through the condition's expression parser.
#[test]
fn diff_global_in_if_condition() {
    assert_asts_match("let f () = (if global.C.M() = 1 then 0 else 1)\n");
}

/// A copy-update record whose source is a `global`-rooted application
/// (`{ global.R () with X = 1 }`). Now that `global` is a parseable atomic
/// expression, the brace disambiguator must route it to the appExpr-source path
/// (`SynExpr.Record(Some copyInfo, …)`) rather than the computation-expression
/// path — FCS parses it as a copy-update record.
#[test]
fn diff_global_copy_update_record() {
    assert_asts_match("let r = { global.R () with X = 1 }\n");
}
