//! Differential test (`parser::parse` vs FCS): a code quotation `<@ … @>` in
//! *pattern* position — FCS's `atomicPattern → quoteExpr`
//! (`pars.fsy:3776`) → `SynPat.QuoteExpr(expr, range)` (`SyntaxTree.fsi:1161`),
//! whose inner `expr` is a full `SynExpr.Quote`. The construct appears as the
//! *parameter* of a parameterised active pattern: in
//! `match e with | SpecificCall <@ f @> (args) -> …`, the `<@ f @>` is passed to
//! the active-pattern function `SpecificCall` and the following pattern matches
//! its output. This is the sole way a quotation reaches pattern position (there
//! is no quotation *binding* pattern), so every case here is an active-pattern
//! application whose head sweeps the quote as a curried atomic argument.

use crate::common::assert_asts_match;

/// A single quotation argument to a `match`-clause active-pattern head. FCS:
/// `LongIdent(["Foo"], Pats[QuoteExpr(Quote(Ident y? no — 1)); Named y])` —
/// here `Foo <@ 1 @> y`, the head sweeping the quote then the `y` binder.
#[test]
fn diff_ast_match_clause_quote_arg() {
    assert_asts_match("match x with | Foo <@ 1 @> y -> y | _ -> 0\n");
}

/// The quotation body is a non-trivial expression (`f a`), and the following
/// argument is a paren sub-pattern — the shape the corpus's `SpecificCall <@ … @>
/// (None, [ty])` uses.
#[test]
fn diff_ast_quote_arg_then_paren_pat() {
    assert_asts_match("match x with | Foo <@ f a @> (None, y) -> y | _ -> 0\n");
}

/// A *dotted* active-pattern head (`A.B`) takes the long-ident branch and sweeps
/// the quote via the curried-arg loop — FCS's `RelaxWhitespace2` shape.
#[test]
fn diff_ast_dotted_head_quote_arg() {
    assert_asts_match("match x with | A.B <@ 2 @> y -> y | _ -> 0\n");
}

/// The raw/untyped quotation form `<@@ … @@>` (`SynExpr.Quote.isRaw = true`) as
/// an active-pattern argument.
#[test]
fn diff_ast_raw_quote_arg() {
    assert_asts_match("match x with | Foo <@@ 1 @@> y -> y | _ -> 0\n");
}

/// The same quotation argument in a `let`-binding curried position (a paren
/// pattern), reached via `try_emit_head_binding_pat_element` just like the
/// clause head — proves the fix lands once for every pattern site.
#[test]
fn diff_ast_let_binding_quote_arg() {
    assert_asts_match("let g (Foo <@ 1 @> y) = y\n");
}
