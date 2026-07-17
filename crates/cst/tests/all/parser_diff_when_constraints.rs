//! Differential test (`parser::parse` vs FCS): `when`-constrained types in
//! binding return position — `let f (x: 'T) : 'T when 'T : struct = x`.
//!
//! FCS's `typeWithTypeConstraints` grammar (`pars.fsy:6023`) wraps a type
//! carrying a trailing `when` clause in
//! `SynType.WithGlobalConstraints(baseType, constraints)`. The `when` is only
//! legal *after* a return-type annotation (a bare `let f x when … = …` is an
//! FCS parse error), so these all annotate the return type. The constraint
//! payload reuses the existing typar-constraint machinery (the same
//! `WHEN typeConstraints` shape as a type-definition header).

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

/// A malformed self-constraint head must produce a recoverable parse error and
/// round-trip losslessly — never panic. FCS also rejects these, so both sides
/// reject; this only pins *our* behaviour (the differential sweep can't, since
/// it requires a clean parse on at least our side).
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

/// Single `struct` (value-type) constraint on the return type.
#[test]
fn diff_struct_constraint() {
    assert_asts_match("let f (x: 'T) : 'T when 'T : struct = x\n");
}

/// The motivating snippet's constraints: `not null` and `not struct`, joined by
/// `and` (`WhereTyparNotSupportsNull` + `WhereTyparIsReferenceType`).
#[test]
fn diff_not_null_and_not_struct() {
    assert_asts_match("let f (x: 'T) : 'T when 'T : not null and 'T : not struct = x\n");
}

/// A subtype constraint (`'T :> IBase`) on the return type.
#[test]
fn diff_subtype_constraint() {
    assert_asts_match("let f (x: 'T) : 'T when 'T :> System.IComparable = x\n");
}

/// A `comparison` member-ish constraint plus a non-trivial base return type
/// (`'T list`), exercising the wrapper around a non-atomic base.
#[test]
fn diff_constraint_over_app_return_type() {
    assert_asts_match("let f (x: 'T) : 'T list when 'T : comparison = [x]\n");
}

/// Two type parameters, one constraint each.
#[test]
fn diff_two_typars_two_constraints() {
    assert_asts_match("let f (x: 'T) (y: 'U) : 'T when 'T : struct and 'U : not struct = x\n");
}

/// A `when`-constrained *member* return type. The constraint belongs to the
/// member's return type (FCS's `SynType.WithGlobalConstraints`), **not** the
/// enclosing type definition's header — `C` itself has no `when` clause. Guards
/// against the type-defn header-constraint collector greedily absorbing the
/// member's return constraint.
#[test]
fn diff_member_return_constraint_does_not_leak_to_type_header() {
    assert_asts_match("type C() =\n    member _.M (x: 'T) : 'T when 'T : struct = x\n");
}

/// A type *definition* with a real header `when` clause and a member that also
/// carries its own `when`-constrained return type: the two must stay separate
/// (header constraint on `C`, return constraint on `M`).
#[test]
fn diff_header_and_member_constraints_stay_separate() {
    assert_asts_match(
        "type C<'a when 'a : comparison>() =\n    member _.M (x: 'T) : 'T when 'T : struct = x\n",
    );
}

// ---------------------------------------------------------------------------
// Bare self-constraints — `when IFoo<'T>` (F# 7 IWSAM shorthand, FCS's
// `SynTypeConstraint.WhereSelfConstrained(ty, range)`). No subject typar: the
// constraint head is an ordinary type, not a `'`/`^` sigil.
// ---------------------------------------------------------------------------

/// The minimal `CheckSelfConstrainedIWSAM.fs` repro — a single self-constraint
/// in a `let` head's typar decls.
#[test]
fn diff_self_constraint_on_let() {
    assert_asts_match("let f<'T when IAdditionOperator<'T>> (x: 'T) = x\n");
}

/// A self-constraint on a type-definition header.
#[test]
fn diff_self_constraint_on_type() {
    assert_asts_match("type C<'T when IFoo<'T>>() = class end\n");
}

/// Two typars, the constraint referencing both — the `CancellableTasks.fs`
/// shape (`Awaiter<'Awaiter,'T>`).
#[test]
fn diff_self_constraint_two_typars() {
    assert_asts_match("let inline f<'Awaiter, 'T when Awaiter<'Awaiter, 'T>> (x: 'Awaiter) = x\n");
}

/// A `^`-sigil (head-type) typar carrying a self-constraint.
#[test]
fn diff_self_constraint_head_type_typar() {
    assert_asts_match("let inline f< ^T when IAdditionOperator< ^T>> (x: ^T) = x\n");
}

/// A self-constraint joined by `and` to an ordinary subject-typar constraint —
/// the two constraint shapes must coexist in one `when` clause.
#[test]
fn diff_self_constraint_and_ordinary_constraint() {
    assert_asts_match("let f<'T when IFoo<'T> and 'T : struct> (x: 'T) = x\n");
}

/// An ordinary constraint joined by `and` to a *following* self-constraint —
/// the reverse order, so the `and`-list handler reaches the self-constraint
/// branch after a subject-typar one.
#[test]
fn diff_ordinary_constraint_and_self_constraint() {
    assert_asts_match("let f<'T when 'T : struct and IFoo<'T>> (x: 'T) = x\n");
}

/// A **bare typar** self-constraint `when 'T` — FCS reduces a typar that is
/// *not* followed by `:` / `:>` via `appTypeWithoutNull` to
/// `WhereSelfConstrained(Var 'T)`, not a subject-typar constraint. Guards the
/// disambiguation: a typar head is a subject only before a `:` / `:>`.
#[test]
fn diff_self_constraint_bare_typar() {
    assert_asts_match("let f<'T when 'T> (x: 'T) = x\n");
}

/// A bare `^`-sigil typar self-constraint `when ^T`.
#[test]
fn diff_self_constraint_bare_head_type_typar() {
    assert_asts_match("let inline f< ^T when ^T> (x: ^T) = x\n");
}

/// A **postfix-app** self-constraint whose head is a bare typar — `when 'T list`
/// (`appTypeWithoutNull appTypeConPower` → `App(list, ['T])`). Confirms the
/// self-constraint arm parses the whole postfix run, not just the head atom.
#[test]
fn diff_self_constraint_postfix_app_typar_head() {
    assert_asts_match("let f<'T when 'T list> (x: 'T) = x\n");
}

/// A postfix-app self-constraint whose head is an interface app — `when
/// IFoo<'T> list` (`App(list, [IFoo<'T>])`).
#[test]
fn diff_self_constraint_postfix_app_interface_head() {
    assert_asts_match("let f<'T when IFoo<'T> list> (x: 'T) = x\n");
}

/// An **array-suffix** self-constraint — `when IFoo<'T>[]`
/// (`appTypeWithoutNull arrayTypeSuffix` → `Array(rank=1, IFoo<'T>)`), the
/// other `appTypeWithoutNull` continuation.
#[test]
fn diff_self_constraint_array_suffix() {
    assert_asts_match("let f<'T when IFoo<'T>[]> (x: 'T) = x\n");
}

/// Panic guard: a **leading-slash measure** head (`when /s`) is a full-`typ`-only
/// starter that the self-constraint's `appTypeWithoutNull` layer cannot consume.
/// The self-constraint branch gates on `peek_starts_type_or_anon_recd` (not the
/// slash-admitting `peek_starts_type`), so this falls through to a clean error
/// instead of hitting `parse_atomic_type`'s `unreachable!` arm. FCS rejects it too.
#[test]
fn self_constraint_leading_slash_measure_is_clean_error() {
    assert_clean_error("let f<'T when /s> (x: 'T) = x\n");
}

/// The same panic guard on the trailing-`when` (binding-return) constraint path,
/// which shares `parse_typar_constraint`.
#[test]
fn trailing_when_leading_slash_measure_is_clean_error() {
    assert_clean_error("let g (x: int) : int when /s = x\n");
}

/// A `(`-led constraint head that is a valid `typeAlts` (`(IFoo)`, `(int)`,
/// `(IFoo<int>)`) is **not** a self-constraint: FCS commits `(` to the
/// `(typeAlts) : (member …)` production and reports FS0010 when no `: (member)`
/// follows. We reject it too (both-reject), rather than accepting it as a bare
/// self-constraint the way an unparenthesised `IFoo` head is.
#[test]
fn paren_typealts_head_without_member_is_clean_error() {
    assert_clean_error("type C<'T when (IFoo)> = class end\n");
    assert_clean_error("type C<'T when (int)> = class end\n");
    assert_clean_error("let g (x: int) : int when (IFoo<int>) = x\n");
}

// ---------------------------------------------------------------------------
// SRTP member constraints — `^T : (static member M : sig)` /
// `^T : (member M : sig)` (FCS's `SynTypeConstraint.WhereTyparSupportsMember`).
// ---------------------------------------------------------------------------

/// A static-member SRTP constraint on a type definition header.
#[test]
fn diff_srtp_static_member_constraint_on_type() {
    assert_asts_match("type C< ^T when ^T : (static member Zero : ^T) > = class end\n");
}

/// A static-member SRTP constraint in a `let inline` head's typar decls.
#[test]
fn diff_srtp_static_member_constraint_on_let() {
    assert_asts_match("let inline f< ^T when ^T : (static member Zero : ^T) > (x: ^T) = x\n");
}

/// An *instance*-member SRTP constraint (`member M : …`).
#[test]
fn diff_srtp_instance_member_constraint() {
    assert_asts_match("type C< ^T when ^T : (member M : int -> int) > = class end\n");
}

/// A member SRTP constraint with a multi-argument signature.
#[test]
fn diff_srtp_member_constraint_multi_arg() {
    assert_asts_match("type C< ^T when ^T : (static member Add : ^T * ^T -> ^T) > = class end\n");
}

/// A `new` constructor SRTP constraint (`^T : (new : unit -> ^T)`) — FCS's
/// `WhereTyparSupportsMember` with the ctor member sig (the same `new` form
/// `parse_member_sig` handles in a type body).
#[test]
fn diff_srtp_new_ctor_constraint() {
    assert_asts_match("type C< ^T when ^T : (new : unit -> ^T) > = class end\n");
}

/// An `abstract`-member SRTP constraint (`^T : (abstract M : int)`) — FCS
/// accepts it; the `abstract` introducer routes through `parse_member_sig`.
#[test]
fn diff_srtp_abstract_member_constraint() {
    assert_asts_match("type C< ^T when ^T : (abstract M : int) > = class end\n");
}

/// A `static`-only SRTP constraint (`^T : (static Zero : ^T)`, no `member`) —
/// FCS accepts it with a distinct `Static` leading keyword.
#[test]
fn diff_srtp_static_only_member_constraint() {
    assert_asts_match("type C< ^T when ^T : (static Zero : ^T) > = class end\n");
}

/// An `inline`-member SRTP constraint (`^T : (static member inline Zero : ^T)`) —
/// FCS's `classMemberSpfn` `opt_inline`. The shared `member_sig_body_is_supported`
/// gate now looks through `inline`, keeping the constraint gate and
/// `parse_member_sig` in lockstep.
#[test]
fn diff_srtp_inline_member_constraint() {
    assert_asts_match("type C< ^T when ^T : (static member inline Zero : ^T) > = class end\n");
}

/// A member SRTP constraint whose member sig carries *its own* explicit type
/// parameters (`^T : (static member M<'U> : ^T -> int)`) — FCS's
/// `opt_explicitValTyparDecls` on the constrained member, parsed by the shared
/// `parse_member_sig` (the typars are elided, like every other member-sig typar).
#[test]
fn diff_srtp_member_constraint_with_typars() {
    assert_asts_match("type C< ^T when ^T : (static member M<'U> : ^T -> int) > = class end\n");
}

// ---------------------------------------------------------------------------
// Parenthesised typar-alternatives SRTP member constraints —
// `(^a or ^b) : (static member M : sig)` (FCS's
// `LPAREN typeAlts rparen COLON LPAREN classMemberSpfn rparen`, `pars.fsy:2679`,
// → `WhereTyparSupportsMember(SynType.Paren(SynType.Or(…)), memberSig)`). The
// support is a parenthesised `or`-list of typars rather than a single `^T`.
// ---------------------------------------------------------------------------

/// Two alternatives on a type-definition header — `(^a or ^b) : (static member …)`.
#[test]
fn diff_srtp_typar_alts_two() {
    assert_asts_match("type C< ^a, ^b when (^a or ^b) : (static member Zero : ^a) > = class end\n");
}

/// Three alternatives in a `let inline` head's typar decls (the corpus shape).
#[test]
fn diff_srtp_typar_alts_three_on_let() {
    assert_asts_match(
        "let inline f< ^t, ^u, ^v when (^t or ^u or ^v) : (static member Zero : ^t) > (x: ^t) = x\n",
    );
}

/// An *instance*-member alts constraint — `(^a or ^b) : (member M : …)`.
#[test]
fn diff_srtp_typar_alts_instance_member() {
    assert_asts_match("type C< ^a, ^b when (^a or ^b) : (member M : int -> int) > = class end\n");
}

// General-type (not typar-only) alternatives — `(Witnesses or ^T) : (member …)`
// (FCS's `typeAlts` operands are `appTypeWithoutNull`, so a *concrete* type may
// stand alongside a typar; `WhereTyparSupportsMember(Paren(Or(LongIdent, Var)),
// …)`). The `pos36-srtp-lib.fs` corpus form.

/// A concrete type as the first alternative, a typar as the second —
/// `(Witnesses or ^T) : (static member …)`.
#[test]
fn diff_srtp_general_type_alt_concrete_then_typar() {
    assert_asts_match(
        "let inline f< ^T when (Witnesses or ^T) : (static member M : ^T -> string)> (x: ^T) = x\n",
    );
}

/// The same on a type-definition header, with a dotted concrete type
/// (`(System.Object or ^T)`).
#[test]
fn diff_srtp_general_type_alt_on_type_header() {
    assert_asts_match(
        "type C< ^T when (System.Object or ^T) : (static member M : ^T -> string) > = class end\n",
    );
}

/// Both alternatives concrete — `(Foo or Bar) : (member …)` (still an
/// `appTypeWithoutNull or appTypeWithoutNull` `typeAlts`).
#[test]
fn diff_srtp_general_type_alt_both_concrete() {
    assert_asts_match(
        "type C< ^T when (Foo or Bar) : (static member M : ^T -> int) > = class end\n",
    );
}

/// A generic concrete type as an alternative — `(#seq<int> or ^T)`? No: an
/// application `IParsable<'T>` (`pos36`-adjacent shape) as the concrete
/// alternative, exercising a multi-token `appTypeWithoutNull` operand.
#[test]
fn diff_srtp_general_type_alt_generic_concrete() {
    assert_asts_match(
        "let inline f< ^T when (IParsable<int> or ^T) : (static member M : ^T -> int)> (x: ^T) = x\n",
    );
}

/// A typar first, a concrete type second — `(^T or int)` — the reverse operand
/// order of `concrete_then_typar`, so the leading typar doesn't send the whole
/// `typeAlts` down a typar-only path.
#[test]
fn diff_srtp_general_type_alt_typar_then_concrete() {
    assert_asts_match("type C< ^T when (^T or int) : (static member Zero : ^T) > = class end\n");
}

/// A **two-token** `appTypeWithoutNull` first operand — a `struct (int * int)`
/// struct-tuple — which the single-token gate can't spot but `parse_app_type`
/// consumes. Confirms `at_paren_type_alts` routes every `(` to the branch (not
/// just a sigil / ident head).
#[test]
fn diff_srtp_general_type_alt_struct_tuple_operand() {
    assert_asts_match(
        "type C< ^T when (struct (int * int) or ^T) : (static member M : ^T -> int) > = class end\n",
    );
}

/// An **anon-record** first operand — `({| A: int |} or ^T)` — the other
/// two-token `appTypeWithoutNull` head.
#[test]
fn diff_srtp_general_type_alt_anon_record_operand() {
    assert_asts_match(
        "type C< ^T when ({| A: int |} or ^T) : (static member M : ^T -> int) > = class end\n",
    );
}

/// A **parenthesised** operand — `((IFoo) or ^T)` — whose *inner* `Paren` must
/// be preserved on both sides (FCS keeps `Paren(LongIdent)`, the CST a
/// `PAREN_TYPE`); only the outer alternatives-list paren is structural.
#[test]
fn diff_srtp_general_type_alt_parenthesised_operand() {
    assert_asts_match(
        "type C< ^T when ((IFoo) or ^T) : (static member M : ^T -> int) > = class end\n",
    );
}

/// An **incomplete** `(^T or )` support (a missing alternative after `or`) is an
/// FCS parse error; the operand panic-guard makes it a clean recoverable error
/// rather than hitting `parse_atomic_type`'s `unreachable!` arm.
#[test]
fn incomplete_srtp_alt_operand_is_clean_error() {
    assert_clean_error("type C< ^T when (^T or ) : (static member M : ^T -> int) > = class end\n");
}

/// An empty `()` support subject — no alternative at all — is likewise a clean
/// error, not a panic.
#[test]
fn empty_paren_srtp_support_is_clean_error() {
    assert_clean_error("type C< ^T when () : (static member M : ^T -> int) > = class end\n");
}

/// An *operator*-named SRTP member constraint — `^T : (static member (+) : …)`
/// (the `zero_constraint.fs` form). The constraint gate
/// (`member_sig_body_is_supported`) admits the operator name and
/// `parse_member_sig` reads it.
#[test]
fn diff_srtp_operator_member_constraint() {
    assert_asts_match("type C< ^T when ^T : (static member (+) : ^T * ^T -> ^T) > = class end\n");
}

/// The same operator constraint reached through a parenthesised parameter
/// pattern — the `let inline average (array: 'T[] when ^T : (static member (+) :
/// …))` shape from the corpus.
#[test]
fn diff_srtp_operator_member_constraint_in_paren_pattern() {
    assert_asts_match("let inline f (x: ^T when ^T : (static member (+) : ^T * ^T -> ^T)) = x\n");
}

// ---------------------------------------------------------------------------
// `when`-constrained types *inside a parenthesised pattern annotation* —
// `let f (x: 'T when 'T : not null) = …`. FCS reaches the same
// `typeWithTypeConstraints` production from a `parenPattern`'s `: type`
// annotation (`pars.fsy:3929`), so the type wraps in
// `SynType.WithGlobalConstraints` exactly as a binding-return annotation does.
// The enclosing pattern's `)` is LexFilter-swallowed, so the trailing-`when`
// attachment must be raw-stream-gated to avoid stealing a following `match`
// guard (see `diff_paren_pat_match_guard_not_absorbed`).
// ---------------------------------------------------------------------------

/// A single parenthesised parameter whose type annotation carries a `when`
/// clause (`'T : not null`).
#[test]
fn diff_paren_pat_single_constraint() {
    assert_asts_match("let f (x: 'T when 'T : not null) = x\n");
}

/// The constraint attaches to the *element* it follows inside a tuple pattern,
/// not the surrounding tuple: `(x: 'T when 'T : comparison, y: int)`.
#[test]
fn diff_paren_pat_tuple_element_constraint() {
    assert_asts_match("let f (x: 'T when 'T : comparison, y: int) = y\n");
}

/// The same paren-pattern path drives a lambda parameter — `fun (x: 'T when …)`.
#[test]
fn diff_paren_pat_lambda_param_constraint() {
    assert_asts_match("let f = fun (x: 'T when 'T : equality) -> x\n");
}

/// An SRTP member constraint on a parenthesised parameter's type
/// (`^T : (static member Zero : ^T)`) — the member-sig machinery reached from
/// pattern position.
#[test]
fn diff_paren_pat_srtp_member_constraint() {
    assert_asts_match("let inline f (x: ^T when ^T : (static member Zero : ^T)) = x\n");
}

/// Regression guard: a `match`-clause guard `when` after a parenthesised typed
/// pattern must stay a guard, **not** get absorbed into the pattern's type as a
/// global constraint. The pattern's `)` is LexFilter-swallowed, so in the
/// filtered stream `(y: int)` is immediately followed by the guard `when`; the
/// raw-stream gate is what keeps them apart. FCS models the `when` as
/// `SynMatchClause.whenExpr`, so absorbing it would diverge here.
#[test]
fn diff_paren_pat_match_guard_not_absorbed() {
    assert_asts_match(
        "let f x =\n    match x with\n    | (y: int) when y > 0 -> y\n    | _ -> 0\n",
    );
}

/// Converse guard for the raw-stream gate: a *genuine* binding-return constraint
/// whose base type is itself parenthesised (`: ('T) when 'T : struct`). The
/// type parser consumes the type's own `)`, so the gate must still see the
/// trailing `when` and attach it — proving the raw-stream check does not
/// over-reject (a false negative) when a `)` legitimately precedes the `when`.
#[test]
fn diff_paren_return_type_constraint_still_attaches() {
    assert_asts_match("let f (x: 'T) : ('T) when 'T : struct = x\n");
}

// ---------------------------------------------------------------------------
// `enum` / `delegate` typar constraints — `'a : enum<'b>` /
// `'a : delegate<args, ret>` (FCS's `WhereTyparIsEnum` / `WhereTyparIsDelegate`,
// `pars.fsy:2684`–`2693`). Both are `typar COLON <name> typeArgsNoHpaDeprecated`:
// `delegate` is the `DELEGATE` keyword, `enum` a bare `IDENT`, each followed by
// a `< … >` type-argument list.
// ---------------------------------------------------------------------------

/// An `enum<'b>` constraint in a binding-return annotation.
#[test]
fn diff_enum_constraint_binding_return() {
    assert_asts_match("let f (x: 'a) : 'a when 'a : enum<int> = x\n");
}

/// The motivating corpus form — an `enum<'b>` constraint inside a parenthesised
/// parameter pattern (`(x : 'a when 'a : enum<'b>)`).
#[test]
fn diff_enum_constraint_paren_pattern() {
    assert_asts_match("let isEnum (x: 'a when 'a : enum<'b>) = ()\n");
}

/// An `enum<int>` constraint on a type-definition header.
#[test]
fn diff_enum_constraint_type_header() {
    assert_asts_match("type C<'a when 'a : enum<int>>() = class end\n");
}

/// A two-argument `delegate<tupledArgs, ret>` constraint.
#[test]
fn diff_delegate_constraint_two_args() {
    assert_asts_match("let f (x: 'a when 'a : delegate<System.EventArgs, unit>) = ()\n");
}

/// A `delegate<…>` whose first argument is itself a tuple type
/// (`System.EventArgs * float`) — exercises the type-arg parser over a
/// non-atomic arg.
#[test]
fn diff_delegate_constraint_tuple_first_arg() {
    assert_asts_match("let f (x: 'a when 'a : delegate<System.EventArgs * float, unit>) = ()\n");
}

// ---- The `'a :> T` subtype-constraint shorthand in a type annotation -------
// FCS parses `'a :> T` (a type) as `SynType.WithGlobalConstraints(Var 'a,
// [WhereTyparSubtypeOfType('a, T)])` — the same node the explicit `'a when 'a :>
// T` form produces, with the subject typar shared with the base. Our parser now
// recognises the `:>` after a base type at the constraint sites
// (`parse_type_with_constraints`).

/// The motivating case — a parameter typed `'a :> System.IComparable`
/// (`productioncoverage01`/`LessRestrictive03` shape).
#[test]
fn diff_subtype_shorthand_param() {
    assert_asts_match("let constraintTest1 (x : 'a :> System.IComparable) = x\n");
}

/// The sibling app-type production uses `_ :> T` rather than a named typar.
#[test]
fn diff_subtype_shorthand_anon_param() {
    assert_asts_match("let constraintTest2 (x : _ :> System.IComparable) = x\n");
}

/// `:>` shorthand with no space before the typar tail (`'a:> T`-style spacing).
#[test]
fn diff_subtype_shorthand_param_tight() {
    assert_asts_match("let f (x:'a :> System.ICloneable) = 3\n");
}

/// A single-segment constraint type (`'T :> CodeRefactoringProvider`).
#[test]
fn diff_subtype_shorthand_single_segment() {
    assert_asts_match("let tryRefactor (p: 'T :> System.IDisposable) = p\n");
}

/// The constraint type carries a `| null` nullness suffix
/// (`'T :> IDisposable | null`) — the post-`:>` type is a full `parse_type`, so
/// the nullable layer composes: `WhereTyparSubtypeOfType('T, WithNull(IDisposable))`.
#[test]
fn diff_subtype_shorthand_nullable_constraint() {
    assert_asts_match("let f (x: 'T :> System.IDisposable | null) = x\n");
}

/// `:>` binds tighter than tuple `*` (FCS's `appTypeWithoutNull` level): `'a *
/// 'b :> T` is `'a * ('b :> T)`, so the constraint sits on the second tuple
/// element only.
#[test]
fn subtype_shorthand_inside_tuple() {
    assert_asts_match("let f (x: 'a * 'b :> System.IDisposable) = x\n");
}

/// `:>` binds tighter than the function arrow: `'a -> 'b :> T` is
/// `'a -> ('b :> T)`.
#[test]
fn subtype_shorthand_inside_arrow() {
    assert_asts_match("let f (x: 'a -> 'b :> System.IDisposable) = x\n");
}

/// A parenthesised subtype shorthand that is then the domain of an arrow —
/// `('a :> T) -> unit`. The `:>` is parsed inside the parens, so it works in a
/// nested type position, not only at the annotation top.
#[test]
fn subtype_shorthand_nested_paren() {
    assert_asts_match("let f (x: ('a :> System.IDisposable) -> unit) = x\n");
}

/// A trailing `when` clause after the `:>` shorthand — `'a :> T when 'a : null`.
/// The `:>` is the base type (handled a layer down), then the top-level
/// constraint wrap adds the `when` group around it.
#[test]
fn subtype_shorthand_then_when() {
    assert_asts_match("let f (x: 'a :> System.IDisposable when 'a : null) = x\n");
}
