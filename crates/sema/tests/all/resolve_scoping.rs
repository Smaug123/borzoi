//! FCS-free tests for the intra-file resolver.
//!
//! Two layers:
//! * Targeted unit tests pin the load-bearing scoping rules (let-vs-rec
//!   right-hand-side visibility, position-ordered shadowing, parameter and
//!   `match`/`fun` local scoping) on hand-written snippets.
//! * A generator-as-oracle property generates random *well-scoped* programs
//!   within the parser subset. The generator records, by construction, the
//!   exact binder each reference must resolve to (the latest binder of that
//!   name visible at the reference). The resolver must reproduce those
//!   resolutions — and every binder must resolve to itself.
//!
//! These do not use FCS: the generator is its own oracle for the scoping
//! *model*. `resolve_diff.rs` separately confirms that model agrees with FCS.

use crate::common::generator::generate;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, DefKind, ProjectItems, Resolution, ResolvedFile, resolve_file};
use proptest::prelude::*;
use rowan::TextRange;

// ============================================================================
// Helpers
// ============================================================================

fn resolve(source: &str) -> ResolvedFile {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected parse errors for {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("root is an impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

/// Byte range of the `n`-th (0-based) *whole-word* occurrence of `needle` in
/// `source` — i.e. not flanked by identifier characters, so a single-letter
/// needle does not match inside a keyword like `and` / `with`.
fn nth(source: &str, needle: &str, n: usize) -> TextRange {
    fn is_ident(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_' || b == b'\''
    }
    let bytes = source.as_bytes();
    let mut found = 0usize;
    let mut from = 0usize;
    while let Some(rel) = source[from..].find(needle) {
        let at = from + rel;
        let end = at + needle.len();
        let before_ok = at == 0 || !is_ident(bytes[at - 1]);
        let after_ok = end == bytes.len() || !is_ident(bytes[end]);
        if before_ok && after_ok {
            if found == n {
                return TextRange::new(
                    u32::try_from(at).unwrap().into(),
                    u32::try_from(end).unwrap().into(),
                );
            }
            found += 1;
        }
        from = at + needle.len();
    }
    panic!(
        "fewer than {} whole-word occurrences of {needle:?} in {source:?}",
        n + 1
    );
}

/// Assert the use at `use_range` resolves to an in-file binder whose defining
/// range is `def_range`.
fn assert_resolves_to(rf: &ResolvedFile, use_range: TextRange, def_range: TextRange) {
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {use_range:?}"));
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{res:?} at {use_range:?} points at no in-file def"));
    assert_eq!(
        def.range, def_range,
        "resolved to the wrong binder ({res:?})"
    );
}

// ============================================================================
// Targeted scoping tests
// ============================================================================

#[test]
fn rec_function_resolves_its_own_reference() {
    let src = "let rec fac n = fac n\n";
    let rf = resolve(src);
    // The recursive `fac` (2nd occurrence) resolves to the binder (1st).
    assert_resolves_to(&rf, nth(src, "fac", 1), nth(src, "fac", 0));
    // The argument `n` (2nd occurrence) resolves to the parameter (1st).
    assert_resolves_to(&rf, nth(src, "n", 1), nth(src, "n", 0));
}

#[test]
fn non_rec_self_reference_does_not_bind_to_itself() {
    // `let g = g`: the RHS `g` is *not* the binding being defined (non-rec),
    // and no outer `g` exists — so it is Deferred, never resolved to the binder.
    let src = "let g = g\n";
    let rf = resolve(src);
    let rhs = nth(src, "g", 1);
    let res = rf.resolution_at(rhs).expect("rhs use is recorded");
    assert!(
        matches!(res, Resolution::Deferred(_)),
        "non-rec self-reference should be Deferred, got {res:?}"
    );
    assert!(rf.resolved_def(res).is_none());
}

#[test]
fn non_rec_self_reference_resolves_to_outer_binding() {
    // `let g = 1` then `let g = g`: the second binding's RHS `g` sees the
    // *first* `g` (its own binder is not yet in scope), not itself.
    let src = "let g = 1\nlet g = g\n";
    let rf = resolve(src);
    // Occurrences of "g": 0 = first binder, 1 = second binder, 2 = RHS use.
    assert_resolves_to(&rf, nth(src, "g", 2), nth(src, "g", 0));
}

#[test]
fn shadowing_resolves_to_the_latest_prior_binder() {
    let src = "let x = 1\nlet x = 2\nlet y = x\n";
    let rf = resolve(src);
    // Occurrences of "x": 0 = first binder, 1 = second binder, 2 = use.
    assert_resolves_to(&rf, nth(src, "x", 2), nth(src, "x", 1));
}

#[test]
fn function_parameter_is_visible_in_the_body() {
    let src = "let f a b = a\n";
    let rf = resolve(src);
    // "a": 0 = parameter binder, 1 = body use.
    assert_resolves_to(&rf, nth(src, "a", 1), nth(src, "a", 0));
    // The body use is a Local (parameter), not an Item.
    let res = rf.resolution_at(nth(src, "a", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn optional_value_parameter_is_visible_in_the_body() {
    // `?x` introduces a single name binding (the optional-argument pattern);
    // the body use resolves to it exactly as a plain parameter would. The
    // construct is parse-valid — FCS rejects optional args outside type members
    // only at type-check, which the name resolver doesn't model — so this
    // exercises the `Pat::OptionalVal` binder path directly.
    let src = "let f (?x) = x\n";
    let rf = resolve(src);
    // "x": 0 = the `?x` binder, 1 = the body use.
    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
    let res = rf.resolution_at(nth(src, "x", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn equality_operands_in_an_ordinary_application_both_resolve() {
    let src = "let x = 1\nlet y = 2\nlet result = not (x = y)\n";
    let rf = resolve(src);

    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
    assert_resolves_to(&rf, nth(src, "y", 1), nth(src, "y", 0));
}

#[test]
fn inline_il_argument_resolves_to_binder() {
    // The value argument inside an inline-IL expression `(# "neg" x : int #)`
    // resolves to the enclosing parameter — the resolver descends into
    // `SynExpr.LibraryOnlyILAssembly`'s arguments. The instruction string and
    // return type carry no resolvable name.
    let src = "let inline neg (x: int) = (# \"neg\" x : int #)\n";
    let rf = resolve(src);
    // "x": 0 = parameter binder, 1 = the use inside the inline IL.
    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
    let res = rf.resolution_at(nth(src, "x", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn trait_call_argument_resolves_to_binder() {
    // The argument expression of an SRTP trait call
    // `(^a : (static member M : ^a -> int) x)` resolves to the enclosing
    // parameter — the resolver descends into `SynExpr.TraitCall`'s `argExpr`.
    // The support type (`^a`) is a head typar and the member signature's own
    // types carry no in-file value reference.
    let src = "let inline g (x: ^a) = (^a : (static member M : ^a -> int) x)\n";
    let rf = resolve(src);
    // "x": 0 = parameter binder, 1 = the use inside the trait-call argument.
    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
    let res = rf.resolution_at(nth(src, "x", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn trait_call_concrete_support_alternative_resolves_to_type() {
    // A trait call's support alternatives are `typar (or appType)*`, so a *later*
    // alternative can be a real type reference — `((^a or Witness) : …)` — not just
    // a head typar. The resolver must therefore visit *every* support type, not
    // only the first: resolving `support_type()` alone was a sound no-op while the
    // parser admitted typars exclusively (a `Type::Var` resolves to nothing), and
    // became a silent go-to-definition gap the moment concrete alternatives parsed.
    let src = "type Witness = class end\n\
               let inline g (x: ^a) = ((^a or Witness) : (static member M : ^a -> int) x)\n";
    let rf = resolve(src);
    // "Witness": 0 = the type definition, 1 = the use in the support alternative.
    assert_resolves_to(&rf, nth(src, "Witness", 1), nth(src, "Witness", 0));
}

#[test]
fn dot_lambda_argument_resolves_to_binder() {
    // The value argument of an accessor-function shorthand `_.M(arg)`
    // (= `fun x -> x.M(arg)`) resolves to the enclosing parameter — the
    // resolver descends into `SynExpr.DotLambda`'s body and resolves member
    // *arguments* in the enclosing scope, while the member spine (`M`) is
    // resolved against the synthesised parameter's type (which sema does not
    // model) and so carries no in-file value reference.
    let src = "let f arg = _.M(arg)\n";
    let rf = resolve(src);
    // "arg": 0 = parameter binder, 1 = the use inside the dot-lambda argument.
    assert_resolves_to(&rf, nth(src, "arg", 1), nth(src, "arg", 0));
    let res = rf.resolution_at(nth(src, "arg", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn dot_lambda_member_does_not_capture_a_same_named_local() {
    // The member name in `_.M(arg)` is accessed off the *synthesised* parameter,
    // not a value binder — so a same-named local `M` in scope must NOT capture
    // it (the bug a naive "resolve the whole body" would introduce). The member
    // use records no resolution at all; only the argument `arg` resolves.
    let src = "let M = 99\nlet f arg = _.M(arg)\n";
    let rf = resolve(src);
    // "M": 0 = the `let M` binder, 1 = the member access in `_.M`.
    assert!(
        rf.resolution_at(nth(src, "M", 1)).is_none(),
        "the `_.M` member must not resolve to the same-named local `let M`",
    );
    // The argument still resolves to the parameter.
    assert_resolves_to(&rf, nth(src, "arg", 1), nth(src, "arg", 0));
}

#[test]
fn dot_lambda_method_chain_arguments_resolve() {
    // A chained shorthand `_.M(x).N(y)` (= `fun a -> a.M(x).N(y)`) resolves both
    // call arguments to their enclosing parameters; the members `M` / `N` stay
    // on the spine and resolve to nothing in-file.
    let src = "let f x y = _.M(x).N(y)\n";
    let rf = resolve(src);
    // "x": 0 = parameter, 1 = the use in the first call argument.
    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
    // "y": 0 = parameter, 1 = the use in the second call argument.
    assert_resolves_to(&rf, nth(src, "y", 1), nth(src, "y", 0));
}

#[test]
fn dot_lambda_index_argument_resolves() {
    // The index expression of `_.Items.[i]` (= `fun a -> a.Items.[i]`) resolves
    // to the enclosing parameter; the indexed member `Items` stays on the spine.
    let src = "let f i = _.Items.[i]\n";
    let rf = resolve(src);
    // "i": 0 = parameter, 1 = the index use.
    assert_resolves_to(&rf, nth(src, "i", 1), nth(src, "i", 0));
}

#[test]
fn dynamic_lhs_and_paren_arg_resolve_to_binders() {
    // `a?(k)` — the LHS `a` and the parenthesised dynamic argument `k` are both
    // ordinary value expressions, resolved in the enclosing scope. The dynamic
    // *member name* in the bare-ident form (`a?b`) is not a value reference, so
    // it is exercised separately below.
    let src = "let f a k = a?(k)\n";
    let rf = resolve(src);
    // "a": 0 = parameter binder, 1 = the LHS use.
    assert_resolves_to(&rf, nth(src, "a", 1), nth(src, "a", 0));
    // "k": 0 = parameter binder, 1 = the use inside the paren argument.
    assert_resolves_to(&rf, nth(src, "k", 1), nth(src, "k", 0));
}

#[test]
fn dynamic_member_name_is_not_resolved_to_a_local() {
    // `a?b` where a local `b` is in scope: the dynamic member name `b` is
    // resolved against `a`'s type at runtime, *not* the value binder, so it must
    // not bind to the local `b` (that would be a wrong resolution).
    let src = "let f a b = a?b\n";
    let rf = resolve(src);
    // The LHS `a` resolves to the parameter.
    assert_resolves_to(&rf, nth(src, "a", 1), nth(src, "a", 0));
    // The member name `b` (2nd occurrence) must NOT be recorded as a use of the
    // parameter `b` — it has no value resolution.
    let member_b = nth(src, "b", 1);
    assert!(
        rf.resolution_at(member_b).is_none(),
        "the dynamic member name `b` should not resolve to the local `b`"
    );
}

#[test]
fn base_member_call_resolves_its_argument_but_not_base() {
    // `base.M(arg)` — `base` heads a long-ident path (FCS's `Ident("base")`); it
    // is a reserved keyword referring to the base object, never an in-file value
    // binder, so it does not resolve to a local. The call *argument* `arg` is an
    // ordinary value expression that resolves in the enclosing scope.
    let src = "let f arg = base.M(arg)\n";
    let rf = resolve(src);
    // "arg": 0 = parameter binder, 1 = the use inside the call argument.
    assert_resolves_to(&rf, nth(src, "arg", 1), nth(src, "arg", 0));
    // `base` must not resolve to any in-file binder (it has none).
    let base_use = nth(src, "base", 0);
    assert!(
        rf.resolved_def(rf.resolution_at(base_use).unwrap_or(Resolution::Unresolved))
            .is_none(),
        "`base` must not resolve to an in-file binder"
    );
}

#[test]
fn base_keyword_does_not_resolve_to_a_quoted_base_binder() {
    // A back-ticked `` ``base`` `` is a genuine value binder (distinct from the
    // `base` *keyword*, whose token text is the *unquoted* `base`). The `base`
    // keyword in `base.M()` must NOT bind to it — the resolver's `base`-keyword
    // special-case keys on the raw token text (`base`, no backticks), not the
    // backtick-stripped name (which would collide).
    let src = "let ``base`` = 1\nlet f () = base.M()\n";
    let rf = resolve(src);
    // "base": 0 = the `` ``base`` `` binder, 1 = the keyword use in `base.M()`.
    let kw_base = nth(src, "base", 1);
    assert!(
        rf.resolved_def(rf.resolution_at(kw_base).unwrap_or(Resolution::Unresolved))
            .is_none(),
        "the `base` keyword must not resolve to the `` ``base`` `` binder",
    );
}

#[test]
fn base_direct_indexer_receiver_does_not_resolve_to_a_local() {
    // The direct-indexer form `base.[i]` leaves `base` a bare `Ident` receiver
    // (resolved via the single-name path, not the long-ident path), so it needs
    // the same `base`-keyword suppression: with a `` ``base`` `` binder in scope,
    // `base.[i]`'s receiver must not bind to it. The index `i` still resolves.
    let src = "let ``base`` = [|1|]\nlet f i = base.[i]\n";
    let rf = resolve(src);
    // "base": 0 = the `` ``base`` `` binder, 1 = the keyword receiver in `base.[i]`.
    let kw_base = nth(src, "base", 1);
    assert!(
        rf.resolved_def(rf.resolution_at(kw_base).unwrap_or(Resolution::Unresolved))
            .is_none(),
        "the `base` indexer receiver must not resolve to the `` ``base`` `` binder",
    );
    // The index `i` resolves to the parameter.
    assert_resolves_to(&rf, nth(src, "i", 1), nth(src, "i", 0));
}

#[test]
fn lambda_parameter_shadows_the_enclosing_parameter() {
    let src = "let q a = fun a -> a\n";
    let rf = resolve(src);
    // "a": 0 = q's param, 1 = lambda's param, 2 = lambda body use.
    // The body resolves to the *inner* lambda parameter.
    assert_resolves_to(&rf, nth(src, "a", 2), nth(src, "a", 1));
}

#[test]
fn match_clause_binder_is_visible_in_the_result() {
    let src = "let m q = match q with w -> w\n";
    let rf = resolve(src);
    // The scrutinee `q` resolves to the parameter.
    assert_resolves_to(&rf, nth(src, "q", 1), nth(src, "q", 0));
    // The result `w` resolves to the clause binder.
    assert_resolves_to(&rf, nth(src, "w", 1), nth(src, "w", 0));
    let res = rf.resolution_at(nth(src, "w", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn named_field_pattern_binder_is_visible_in_the_result() {
    // A named-field union-case clause pattern `Case (field = x)` binds the
    // field *value* `x` (not the field name `field`), visible in the result.
    let src = "let m q = match q with Case (field = x) -> x\n";
    let rf = resolve(src);
    // The result `x` resolves to the clause binder (the field value).
    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
    let res = rf.resolution_at(nth(src, "x", 1)).unwrap();
    assert!(matches!(res, Resolution::Local(_)));
}

#[test]
fn let_rec_and_group_sees_every_binding() {
    // Mutual recursion: each RHS sees the other binding of the group.
    let src = "let rec a = b\nand b = a\n";
    let rf = resolve(src);
    // "a": 0 = binder, 1 = use in `and b = a`. "b": 0 = use in `a = b`, 1 = binder.
    assert_resolves_to(&rf, nth(src, "b", 0), nth(src, "b", 1));
    assert_resolves_to(&rf, nth(src, "a", 1), nth(src, "a", 0));
}

#[test]
fn local_let_value_is_visible_in_the_body() {
    // An expression-level (block) `let` binds a value visible in its body, as a
    // Local (not an exported item — it is interior to `outer`).
    let src = "let outer () =\n    let v = 1\n    v\n";
    let rf = resolve(src);
    // "v": 0 = local binder, 1 = body use.
    assert_resolves_to(&rf, nth(src, "v", 1), nth(src, "v", 0));
    let res = rf.resolution_at(nth(src, "v", 1)).unwrap();
    assert!(
        matches!(res, Resolution::Local(_)),
        "local let value should resolve to a Local, got {res:?}"
    );
    // It is interior, so not exported.
    let names: Vec<&str> = rf.exports().iter().map(|i| i.name()).collect();
    assert_eq!(names, ["outer"]);
}

#[test]
fn local_let_function_binds_head_and_params() {
    // The function-binding form `let f a = a` in expression position: `f` is a
    // bound function value (visible in the body), `a` a parameter visible only
    // in `f`'s RHS. `BinderRole::Pattern` (the CE-binder reading) would mis-read
    // `f` as a constructor reference and never bind it.
    let src = "let outer () =\n    let f a = a\n    f 1\n";
    let rf = resolve(src);
    // "f": 0 = binder, 1 = body use.
    assert_resolves_to(&rf, nth(src, "f", 1), nth(src, "f", 0));
    // "a": 0 = parameter, 1 = RHS use — the param scopes the RHS, not the body.
    assert_resolves_to(&rf, nth(src, "a", 1), nth(src, "a", 0));
}

#[test]
fn local_let_rec_sees_itself_in_its_rhs() {
    // A local `let rec` must put its binder in scope for its own RHS; a plain
    // (non-rec) local let must not.
    let src = "let outer () =\n    let rec g x = g x\n    g 1\n";
    let rf = resolve(src);
    // "g": 0 = binder, 1 = recursive RHS use, 2 = body use.
    assert_resolves_to(&rf, nth(src, "g", 1), nth(src, "g", 0));
    assert_resolves_to(&rf, nth(src, "g", 2), nth(src, "g", 0));
    // "x": 0 = param, 1 = RHS use.
    assert_resolves_to(&rf, nth(src, "x", 1), nth(src, "x", 0));
}

#[test]
fn local_let_non_rec_rhs_sees_outer_then_shadows() {
    // Nested local lets: the inner `let w = w`'s RHS sees the *outer* `w`
    // (its own binder is not yet in scope), and the body sees the inner `w`.
    let src = "let outer () =\n    let w = 1\n    let w = w\n    w\n";
    let rf = resolve(src);
    // "w": 0 = first binder, 1 = second binder, 2 = second RHS use, 3 = body use.
    assert_resolves_to(&rf, nth(src, "w", 2), nth(src, "w", 0));
    assert_resolves_to(&rf, nth(src, "w", 3), nth(src, "w", 1));
}

#[test]
fn type_application_head_resolves_to_its_binder() {
    // `f<int>` is `SynExpr.TypeApp(f, [int])`; the type-applied head is a value
    // use that must resolve to its binder. The `int` type argument names a type,
    // not a value, so it is not resolved (no in-file type defns here).
    let src = "let myFunc x = x\nlet g = myFunc<int> 0\n";
    let rf = resolve(src);
    // "myFunc": 0 = binder, 1 = TypeApp-head use.
    assert_resolves_to(&rf, nth(src, "myFunc", 1), nth(src, "myFunc", 0));
}

#[test]
fn top_level_bindings_are_exported_in_order() {
    let src = "let x = 1\nlet f a = a\n";
    let rf = resolve(src);
    let names: Vec<&str> = rf.exports().iter().map(|i| i.name()).collect();
    assert_eq!(names, ["x", "f"]);
    // The function is flagged as such; the value is not; the parameter is not
    // exported.
    let kinds: Vec<DefKind> = rf
        .exports()
        .iter()
        .map(|i| {
            rf.def(i.def().expect("own-arena def in a single-file resolve"))
                .kind
        })
        .collect();
    assert_eq!(
        kinds,
        [
            DefKind::Value { is_function: false },
            DefKind::Value { is_function: true },
        ]
    );
}

#[test]
fn nullary_constructor_pattern_head_does_not_bind() {
    // `None` in a clause pattern is a (nullary) constructor reference, not a
    // variable binder — FCS resolves it into FSharp.Core. We cannot resolve
    // constructors yet, so neither the pattern `None` nor the result `None`
    // may become an in-file Local; both must be Deferred (never a fake binder).
    let src = "let m opt = match opt with None -> None\n";
    let rf = resolve(src);

    let result_none = nth(src, "None", 1);
    let res = rf.resolution_at(result_none).expect("recorded");
    assert!(
        matches!(res, Resolution::Deferred(_)),
        "result `None` should be Deferred, got {res:?}"
    );
    assert!(rf.resolved_def(res).is_none());

    // The pattern occurrence must not introduce a binder anything resolves to.
    let pat_none = nth(src, "None", 0);
    if let Some(p) = rf.resolution_at(pat_none) {
        assert!(
            matches!(p, Resolution::Deferred(_)),
            "pattern `None` must not bind, got {p:?}"
        );
    }

    // The genuine binder in the same file still resolves: scrutinee → param.
    assert_resolves_to(&rf, nth(src, "opt", 1), nth(src, "opt", 0));
}

#[test]
fn unbound_name_is_deferred_not_unresolved() {
    // A name we cannot find locally is Deferred (it may be an import / assembly
    // symbol), never Unresolved — the load-bearing soundness rule (D5).
    let src = "let x = printfn\n";
    let rf = resolve(src);
    let res = rf.resolution_at(nth(src, "printfn", 0)).unwrap();
    assert!(matches!(res, Resolution::Deferred(_)), "got {res:?}");
    assert!(!matches!(res, Resolution::Unresolved));
}

// ============================================================================
// Generator-as-oracle property
// ============================================================================
//
// `common::generator::generate` interprets a tape of random numbers into a
// scope-correct program and reports, by construction, the binder each reference
// must resolve to. Shrinking the tape shrinks the program; the interpreter
// always yields a valid program, so shrinking never produces garbage. This is
// the resolver's own-model oracle; `resolve_diff.rs` confirms the model against
// FCS, including on a sample of these generated programs.

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn generated_programs_resolve_every_reference_to_its_binder(
        nums in prop::collection::vec(any::<u32>(), 30..400)
    ) {
        let g = generate(nums);

        let parsed = parse(&g.src);
        prop_assert!(
            parsed.errors.is_empty(),
            "generated program failed to parse: {:?}\nerrors: {:?}",
            g.src, parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());

        // Every binder resolves to itself.
        for (uid, range) in &g.binder_ranges {
            let res = rf.resolution_at(*range).ok_or_else(|| {
                TestCaseError::fail(format!("binder {uid} at {range:?} has no self-resolution in {:?}", g.src))
            })?;
            let def = rf.resolved_def(res).ok_or_else(|| {
                TestCaseError::fail(format!("binder {uid} self-resolves to {res:?} (no def) in {:?}", g.src))
            })?;
            prop_assert_eq!(def.range, *range, "binder {} self-range mismatch in {:?}", uid, g.src);
        }

        // Every reference resolves to the latest in-scope binder of its name.
        for (use_range, target) in &g.refs {
            let res = rf.resolution_at(*use_range).ok_or_else(|| {
                TestCaseError::fail(format!("reference at {use_range:?} unrecorded in {:?}", g.src))
            })?;
            prop_assert!(
                !matches!(res, Resolution::Unresolved),
                "reference at {:?} is Unresolved in {:?}",
                use_range, g.src
            );
            let def = rf.resolved_def(res).ok_or_else(|| {
                TestCaseError::fail(format!("reference at {use_range:?} resolved to {res:?} (no in-file def) in {:?}", g.src))
            })?;
            let expected = g.binder_ranges[target];
            prop_assert_eq!(
                def.range, expected,
                "reference at {:?} resolved to {:?}, expected binder {:?} in {:?}",
                use_range, def.range, expected, g.src
            );
        }
    }
}
