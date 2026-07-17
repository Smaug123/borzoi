//! Differential test (`parser::parse` vs FCS): SRTP trait-call expressions
//! `( ^a : (static member M : ^a -> int) x )` — FCS's `SynExpr.TraitCall`.
//!
//! FCS reaches a trait call only through `parenExpr: LPAREN parenExprBody
//! rparen` (`pars.fsy:5466`), whose `parenExprBody` alt
//! `typars COLON LPAREN classMemberSpfn rparen typedSequentialExpr`
//! (`pars.fsy:5529`) builds `SynExpr.TraitCall(supportTys, traitSig, argExpr)`,
//! wrapped in the `SynExpr.Paren`. The support type (`typars`) is a bare
//! head-type typar `^a` — FCS rejects the plain `'a` form there
//! (`( 'a : (static member …) x)` is a parse error, "Unexpected keyword 'static'
//! in binding") — or the parenthesised alternatives `( ^a or … )`, whose
//! `typarAlts` is `typar (OR appTypeCanBeNullable)*`: a typar first (`'a`
//! included), then arbitrary app types. The member signature reuses the same
//! `classMemberSpfn` non-terminal as the SRTP member *constraint*
//! (`parser_diff_when_constraints.rs`), so the `MEMBER_SIG` projection is shared.

use crate::common::assert_asts_match;

/// The canonical trait call: a static member with one argument.
#[test]
fn diff_trait_call_static_member() {
    assert_asts_match("let inline g (x: ^a) = (^a : (static member M : ^a -> int) x)\n");
}

/// With a binding return-type annotation — FCS wraps the rhs in `SynExpr.Typed`
/// around the `Paren(TraitCall)`, exercising both the return-info path and the
/// trait call together.
#[test]
fn diff_trait_call_with_binding_return_type() {
    assert_asts_match("let inline g (x: ^a) : int = (^a : (static member M : ^a -> int) x)\n");
}

/// A multi-argument trait call: the member signature is curried/tupled
/// (`^a * ^a -> ^a`) and the argument is a parenthesised tuple `(x, y)`.
#[test]
fn diff_trait_call_multi_arg() {
    assert_asts_match(
        "let inline g (x: ^a) (y: ^a) = (^a : (static member Add : ^a * ^a -> ^a) (x, y))\n",
    );
}

/// An *instance*-member trait call (`member M : …`, no `static`).
#[test]
fn diff_trait_call_instance_member() {
    assert_asts_match("let inline g (x: ^a) = (^a : (member M : unit -> int) x)\n");
}

/// A `unit`-argument trait call — the argument expression is the unit literal
/// `()`, and the member signature takes `unit -> ^a`.
#[test]
fn diff_trait_call_unit_arg() {
    assert_asts_match("let inline h (x: ^a) = (^a : (static member get_Zero : unit -> ^a) ())\n");
}

/// A `new`-ctor trait call (`^a : (new : unit -> ^a) x`) — the member-sig name
/// is the `new` keyword (the ctor form `parse_member_sig` already handles), one
/// of the two `nameop` shapes the trait-call gate admits (operator names are
/// deferred).
#[test]
fn diff_trait_call_new_ctor() {
    assert_asts_match("let inline g (x: ^a) = (^a : (new : unit -> ^a) x)\n");
}

/// A `static`-only member sig (`static Zero`, no `member` keyword) — FCS accepts
/// it as a `Static`-leading-keyword member sig (the same form the SRTP member
/// *constraint* admits), and the shared `member_sig_body_is_supported` gate lets
/// the trait-call branch commit.
#[test]
fn diff_trait_call_static_only_member() {
    assert_asts_match("let inline g (x: ^a) = (^a : (static Zero : ^a) x)\n");
}

/// An *operator*-named trait call — `(^a : (static member (+) : …) (x, x))`. The
/// shared `member_sig_body_is_supported` gate now admits the operator name, so
/// the trait-call branch commits and `parse_member_sig` reads it.
#[test]
fn diff_trait_call_operator_member() {
    assert_asts_match("let inline g (x: ^a) = (^a : (static member (+) : ^a * ^a -> ^a) (x, x))\n");
}

/// An `inline` member trait call — `(^a : (static member inline Zero : ^a) ())`.
/// FCS's `classMemberSpfn` admits `opt_inline`; the shared
/// `member_sig_body_is_supported` gate now looks through it (in lockstep with
/// `parse_member_sig`), so the trait-call branch commits.
#[test]
fn diff_trait_call_inline_member() {
    assert_asts_match("let inline g (x: ^a) = (^a : (static member inline Zero : ^a) ())\n");
}

/// An `inline` method trait call — `(^a : (static member inline M : ^a -> int) x)`.
#[test]
fn diff_trait_call_inline_method() {
    assert_asts_match("let inline g (x: ^a) = (^a : (static member inline M : ^a -> int) x)\n");
}

/// A trait call whose member sig carries explicit type parameters
/// (`(^a : (static member M<'U> : ^a -> int) x)`) — FCS's
/// `opt_explicitValTyparDecls` on the member sig, consumed by the shared
/// `parse_member_sig` (typars elided).
#[test]
fn diff_trait_call_member_with_typars() {
    assert_asts_match("let inline g (x: ^a) = (^a : (static member M<'U> : ^a -> int) x)\n");
}

// ---------------------------------------------------------------------------
// Parenthesised typar-alternatives support — `((^a or ^b) : (member …) arg)`
// (FCS's `typars` alt `LPAREN typarAlts rparen`, `pars.fsy:5546`, →
// `SynExpr.TraitCall(SynType.Paren(SynType.Or(…)), …)`). The support is a
// parenthesised `or`-list of typars rather than a single `^a` — the trait-call
// counterpart of the SRTP member *constraint* alts form.
// ---------------------------------------------------------------------------

/// Two alternatives — `((^a or ^b) : (static member Zero : ^a) (x, y))`.
#[test]
fn diff_trait_call_typar_alts_two() {
    assert_asts_match(
        "let inline f (x: ^a) (y: ^b) = ((^a or ^b) : (static member Zero : ^a) (x, y))\n",
    );
}

/// The operator-binding corpus shape — `((^a or ^b) : (static member (+) : …) (a, b))`.
#[test]
fn diff_trait_call_typar_alts_operator() {
    assert_asts_match(
        "let inline (+++) (a: ^a) (b: ^b) = ((^a or ^b) : (static member (+) : ^a * ^b -> ^a) (a, b))\n",
    );
}

/// An instance-member alts trait call — `((^a or ^b) : (member M : …) (x, y))`.
#[test]
fn diff_trait_call_typar_alts_instance() {
    assert_asts_match(
        "let inline f (x: ^a) (y: ^b) = ((^a or ^b) : (member M : ^a -> int) (x, y))\n",
    );
}

// ---------------------------------------------------------------------------
// Concrete alternatives — `((^a or int) : (member …) arg)`. FCS's `typarAlts`
// (`pars.fsy:5546`) is `typar (OR appTypeCanBeNullable)*`: only the *first*
// alternative must be a typar; every later one is a full `appType`. So a
// concrete alternative is legal in the support of a trait-call *expression*,
// exactly as it is in the SRTP member *constraint* (`(Witnesses or ^T) : …`).
// ---------------------------------------------------------------------------

/// The corpus shape (`tests/service/data/SyntaxTree/SynType/`
/// `SynTypeOrWithAppTypeOnTheRightHandSide.fs`): a concrete second alternative.
#[test]
fn diff_trait_call_alts_concrete_second() {
    assert_asts_match("let inline f (x: 'T) = ((^T or int) : (static member A: int) ())\n");
}

/// A long-ident alternative — `System.Int32` (a `SynType.LongIdent`).
#[test]
fn diff_trait_call_alts_long_ident() {
    assert_asts_match(
        "let inline f (x: 'T) = ((^T or System.Int32) : (static member A: int) ())\n",
    );
}

/// An *applied* alternative — `int list` (a `SynType.App`), the postfix form the
/// `appType` operand admits but a bare typar list would not.
#[test]
fn diff_trait_call_alts_app_type() {
    assert_asts_match("let inline f (x: 'T) = ((^T or int list) : (static member A: int) ())\n");
}

/// A generic-application alternative — `Set<int>` (the prefix `appType` form).
#[test]
fn diff_trait_call_alts_generic_app() {
    assert_asts_match("let inline f (x: 'T) = ((^T or Set<int>) : (static member A: int) ())\n");
}

/// An *array* alternative — `int[]`. The `[`/`]` are the other bracket pair the
/// commit scan depth-counts (an unbalanced `]` is not this list's shape).
#[test]
fn diff_trait_call_alts_array_type() {
    assert_asts_match("let inline f (x: 'T) = ((^T or int[]) : (static member A: int) ())\n");
}

/// A generic alternative carrying a comma and a nested postfix app —
/// `Map<int, string list>`. The comma is inside the alternative, not a list
/// separator, and only `or` / `)` at depth zero end an alternative.
#[test]
fn diff_trait_call_alts_generic_multi_arg() {
    assert_asts_match(
        "let inline f (x: 'T) = ((^T or Map<int, string list>) : (static member A: int) ())\n",
    );
}

/// A *parenthesised* alternative — `(int * string)`. The inner parens nest, so
/// the alternatives' own closing `)` is the one at depth zero.
#[test]
fn diff_trait_call_alts_paren_type() {
    assert_asts_match(
        "let inline f (x: 'T) = ((^T or (int * string)) : (static member A: int) ())\n",
    );
}

/// A *nullable* alternative — `string | null`. The trait call's `typarAlts`
/// operand is `appTypeCanBeNullable`, so the `| null` suffix belongs to the
/// alternative — unlike the SRTP member *constraint*, whose `typeAlts` operand is
/// `appTypeWithoutNull` and where FCS rejects the same suffix (see
/// `srtp_constraint_support_alternative_rejects_nullable` in
/// `src/parser/tests/`).
#[test]
fn diff_trait_call_alts_nullable() {
    assert_asts_match(
        "let inline f (x: 'T) = ((^T or string | null) : (static member A: int) ())\n",
    );
}

/// A *subtype-constrained* alternative — `'U :> obj`. FCS's `appTypeWithoutNull`
/// admits `typar COLON_GREATER typ` (a `SynType.WithGlobalConstraints`), so it is
/// a legal alternative under the `appType` operand.
#[test]
fn diff_trait_call_alts_subtype_constrained() {
    assert_asts_match("let inline f (x: 'T) = ((^T or 'U :> obj) : (static member A: int) ())\n");
}

/// Three alternatives, mixing typar and concrete — `(^a or int or string)`.
#[test]
fn diff_trait_call_alts_three_mixed() {
    assert_asts_match(
        "let inline f (x: ^a) = ((^a or int or string) : (static member A: int) ())\n",
    );
}

/// A plain `'T` *first* alternative. FCS's `typarAlts` base case is `typar`,
/// which admits the `'a` form — so unlike the bare single support (`('a : (…) x)`,
/// a parse error), `(('T or int) : (…) x)` is accepted.
#[test]
fn diff_trait_call_alts_quote_typar_first() {
    assert_asts_match("let inline f (x: 'T) = (('T or int) : (static member A: int) ())\n");
}

// The error/recovery cases for unsupported member-sig forms (no introducer,
// `inline`) and the missing-argument guard live as parser-only unit tests in
// `src/parser/tests/expressions.rs`: FCS recovers the bare form as
// `SynExpr.FromParseError`, which the diff oracle cannot model, so a diff
// assertion is not possible there.
