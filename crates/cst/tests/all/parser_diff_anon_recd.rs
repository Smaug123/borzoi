//! Differential test (`parser::parse` vs FCS): anonymous-record *expressions*
//! `{| F = e; … |}` (`SynExpr.AnonRecd`). The anon-record *type* `{| F: T |}`
//! already landed; this is the construction (expression) side. The
//! `struct {| … |}` form lives in `parser_diff_struct_expr` (phase 10.18).

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

/// Smallest anon-record: one field. FCS:
/// `AnonRecd(false, None, [(SynLongIdent ["A"], Some, Const 1)], …)`. Routed by
/// the new `LBraceBar` atomic-expr starter, closed by a *real* `BAR_RBRACE_TOK`
/// (unlike the record `}`, the `|}` is not swallowed by the lex-filter).
#[test]
fn diff_ast_anon_recd_single_field() {
    assert_asts_match("let x = {| A = 1 |}\n");
}

/// Two `;`-separated fields on one line. Reuses the record field-list machinery
/// (`parse_record_field`, `consume_one_seps_group`).
#[test]
fn diff_ast_anon_recd_two_fields() {
    assert_asts_match("let x = {| A = 1; B = 2 |}\n");
}

/// Offside (newline-separated) fields — the `Virtual::BlockSep` separator path
/// rather than `;`. FCS produces the same two-field `AnonRecd`.
#[test]
fn diff_ast_anon_recd_offside_fields() {
    assert_asts_match("let x =\n    {| A = 1\n       B = 2 |}\n");
}

/// Copy-and-update `{| r with A = 1 |}` → `AnonRecd(false, Some(Ident "r"), …)`.
/// Mirrors the record `{ src with … }` copy path (`with` + trailing
/// `Virtual::End`).
#[test]
fn diff_ast_anon_recd_copy_update() {
    assert_asts_match("let x = {| r with A = 1 |}\n");
}

/// A non-trivial field value exercises the value sub-expression parse (infix
/// `+` here), confirming fields hold full expressions.
#[test]
fn diff_ast_anon_recd_expr_valued_field() {
    assert_asts_match("let x = {| A = 1 + 2; B = f y |}\n");
}

/// Nested anon-record as a field value — the value parse recurses into a fresh
/// `{| … |}`.
#[test]
fn diff_ast_anon_recd_nested() {
    assert_asts_match("let x = {| A = {| B = 1 |} |}\n");
}

/// Anon-record in application-argument position: `f {| A = 1 |}` →
/// `App(Ident "f", AnonRecd …)`. Guards that `{|` is admitted as an app arg.
#[test]
fn diff_ast_anon_recd_as_app_arg() {
    assert_asts_match("let x = f {| A = 1 |}\n");
}

/// Empty anon-record `{| |}` — FCS accepts it as `AnonRecd(false, None, [])`
/// (no parse error). Our empty-field path must match (no spurious "expected a
/// field" error, empty field list).
#[test]
fn diff_ast_anon_recd_empty() {
    assert_asts_match("let x = {| |}\n");
}

// `struct {| A = 1 |}` (`isStruct = true`) is now parsed — see
// `parser_diff_struct_expr` (phase 10.18 struct expressions).

/// `{| A.B = 1 |}` — FCS rejects a *dotted* field name in (non-copy)
/// construction ("Invalid anonymous record type"; dotted names are meaningful
/// only for copy-update nesting). We report the same diagnostic rather than
/// silently accepting it. Clean error (lossless, no panic).
#[test]
fn diff_ast_anon_recd_dotted_field_is_clean_error() {
    assert_clean_error("let x = {| A.B = 1 |}\n");
}

/// Deferred — `{| f r with A = 1 |}` (a non-longident copy source). FCS accepts
/// an arbitrary `appExpr` before `with`, but our copy-update classifier only
/// recognises a longident source — the *same* pre-existing limitation as the
/// regular record `{ f r with … }`. A cross-cutting record+anon fix is a
/// separate slice; until then this is a clean error.
#[test]
fn diff_ast_anon_recd_app_copy_source_is_clean_error() {
    assert_clean_error("let x = {| f r with A = 1 |}\n");
}

/// A deferred / invalid anon-record form must not panic and must round-trip
/// losslessly, and (since FCS also errors on these) must surface ≥1 parse
/// error rather than silently accepting an AST that diverges from FCS.
fn assert_clean_error(source: &str) {
    let parsed = parse(source);
    assert_eq!(
        parsed.root.text().to_string(),
        source,
        "lossless round-trip violated for {source:?}",
    );
    assert!(
        !parsed.errors.is_empty(),
        "expected a parse error for {source:?}, got none",
    );
}
