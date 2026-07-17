//! Differential test (`parser::parse` vs FCS): the F# 7 typar-expression
//! `'T.Member` — a type parameter used as an *expression*, the head of a
//! statically-resolved (SRTP) member call. FCS's grammar `QUOTE ident`
//! (`pars.fsy:5263`) builds `SynExpr.Typar(SynTypar(id, None, false), range)`
//! at the `atomicExpr` level; the trailing `.Member` and `(args)` then chain
//! through the ordinary `DotGet` / high-precedence-application postfix tail, so
//! `'T.op_Addition(x, y)` is `App(DotGet(Typar 'T, [op_Addition]), Paren(Tuple
//! [x; y]))`.
//!
//! Only the **quote** sigil reaches `SynExpr.Typar`: a `^`-sigil `^T.M(x)` is
//! parsed by FCS as `IndexFromEnd(App(LongIdent ["T"; "M"], …))` — the `^`
//! from-end index prefix — which our parser already matches, so it is not
//! retested here.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

/// A malformed / FCS-rejected typar-expr position must produce a recoverable
/// parse error and round-trip losslessly — never panic. Used for the
/// after-type positions FCS rejects an *unparenthesised* typar-expr in.
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

/// A static-property read `'T.StaticProperty` — the bare `DotGet(Typar, …)`
/// with no trailing application.
#[test]
fn diff_typar_static_property() {
    assert_asts_match("let f (x: 'T) = 'T.StaticProperty\n");
}

/// A static-method call with one argument — `'T.StaticMethod(x)`
/// (`App(DotGet(Typar, [StaticMethod]), Paren(x))`).
#[test]
fn diff_typar_static_method_one_arg() {
    assert_asts_match("let f (x: 'T) = 'T.StaticMethod(x)\n");
}

/// A named-member SRTP call with a tupled argument — `'T.op_Addition(x, y)`
/// (`App(DotGet(Typar, [op_Addition]), Paren(Tuple [x; y]))`), the
/// `CheckSelfConstrainedIWSAM.fs` `op_Addition` body.
#[test]
fn diff_typar_op_addition_tupled() {
    assert_asts_match("let f (x: 'T) (y: 'T) = 'T.op_Addition(x, y)\n");
}

/// The *pretty* operator form `'T.(+)(x, y)` — the member name is the
/// parenthesised operator, which FCS desugars to `op_Addition` (with
/// `OriginalNotationWithParen` trivia the normaliser elides), so it must match
/// the `op_Addition` form above.
#[test]
fn diff_typar_pretty_operator() {
    assert_asts_match("let f (x: 'T) (y: 'T) = 'T.(+)(x, y)\n");
}

/// A unit-argument call — `'T.UnitMethod()`
/// (`App(DotGet(Typar, [UnitMethod]), Const Unit)`).
#[test]
fn diff_typar_unit_method() {
    assert_asts_match("let f () = 'T.UnitMethod()\n");
}

/// The typar-expression combined with its F# 7 self-constraint header (the
/// whole `CheckSelfConstrainedIWSAM.fs` shape): `when IStaticProperty<'T>`
/// header (parsed since the WhereSelfConstrained work) plus a `'T.` body.
#[test]
fn diff_typar_expr_with_self_constraint_header() {
    assert_asts_match("let f<'T when IStaticProperty<'T>>() = 'T.StaticProperty\n");
}

/// A `let`-bound intermediate off a typar call — `let v = 'T.UnitMethod() in
/// [v]` (the `f_IWSAM_declared_UnitMethod_list` corpus shape), exercising the
/// typar-expr as the RHS of a nested binding.
#[test]
fn diff_typar_expr_as_nested_binding_rhs() {
    assert_asts_match("let f () =\n    let v = 'T.UnitMethod()\n    [ v ]\n");
}

/// The typar expression as a whitespace-application argument — `g 'T.P` is
/// `App(g, DotGet(Typar 'T, [P]))`, confirming `'T` is admitted as an atom in
/// argument position, not only at expression start.
#[test]
fn diff_typar_expr_in_arg_position() {
    assert_asts_match("let f (g: 'T -> int) = g 'T.StaticProperty\n");
}

// ---------------------------------------------------------------------------
// After-type positions (`atomicExprAfterType`) — FCS reaches a typar-expr only
// through the wider `atomicExpr`, so it admits a *parenthesised* typar-expr
// argument to an attribute / `new` / `inherit` but rejects the bare form. Our
// atom-start gate mirrors that split by blacklisting a bare `'` in
// `raw_starts_attribute_arg` (the `(`-headed paren form is unaffected).
// ---------------------------------------------------------------------------

/// A **parenthesised** typar-expr as an attribute argument — `[<A('T)>]`. FCS
/// accepts it (the `(` heads a `parenExpr` `atomicExprAfterType`); both sides
/// must agree on the `App(A, Paren(Typar 'T))` attribute shape.
#[test]
fn diff_typar_expr_paren_attribute_arg() {
    assert_asts_match("[<A('T)>]\ntype C = class end\n");
}

/// A **parenthesised** typar-expr as an `inherit` argument — `inherit B('T)`.
#[test]
fn diff_typar_expr_paren_inherit_arg() {
    assert_asts_match("type C() =\n    inherit B('T)\n");
}

/// A **bare** typar-expr as an attribute argument (`[<A 'T>]`) is an FCS parse
/// error (`atomicExprAfterType` omits the `QUOTE ident` production); we reject
/// it too rather than accepting it as an application argument.
#[test]
fn bare_typar_expr_attribute_arg_is_clean_error() {
    assert_clean_error("[<A 'T>]\ntype C = class end\n");
}

/// A bare typar-expr as a `new` argument (`new B 'T`) — FCS-rejected, clean
/// error on our side.
#[test]
fn bare_typar_expr_new_arg_is_clean_error() {
    assert_clean_error("let x = new B 'T\n");
}

/// A bare typar-expr as an `inherit` argument (`inherit B 'T`) — FCS-rejected,
/// clean error on our side.
#[test]
fn bare_typar_expr_inherit_arg_is_clean_error() {
    assert_clean_error("type C() =\n    inherit B 'T\n");
}

/// A `'` split from its name by an **offside layout break** — `let f =⏎    '⏎
/// T` — is an FCS parse error (`QUOTE ident` needs the two adjacent; the break
/// inserts a `Virtual::BlockSep` between them). The typar-name gate keys off the
/// *filtered* cursor, so the virtual is not consumed as a zero-width identifier
/// and the split records a clean error rather than silently accepting.
#[test]
fn typar_expr_split_by_offside_break_is_clean_error() {
    assert_clean_error("let f =\n    '\n    T\n");
}
