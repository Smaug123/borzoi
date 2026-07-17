//! Direct (FCS-free) tests for first-class type-definition resolution:
//! `type` definitions are interned as [`DefKind::Type`] binders, their
//! defining occurrence resolves to itself, and a same-file type-name *use* in
//! a type-syntactic position resolves to that binder.
//!
//! These assert the resolver's own output directly (no oracle), so they run
//! fast and pin the exact ranges/kinds. The FCS differential over abbreviation
//! corpora lives in `resolve_diff.rs`.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DefKind, DeferredReason, ProjectItems, Resolution, ResolvedFile, resolve_file,
    resolve_project,
};
use rowan::TextRange;

/// Parse `src` (asserting it is in the parser subset) and resolve it as a lone
/// single file.
fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

fn impl_file(src: &str) -> ImplFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    ImplFile::cast(parsed.root).expect("impl file")
}

/// The byte range of the `n`th (0-based) occurrence of `needle` in `src`.
fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    for _ in 0..n {
        let i = src[from..].find(needle).expect("occurrence") + from;
        from = i + needle.len();
    }
    let i = src[from..].find(needle).expect("occurrence") + from;
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// Assert that the use of `needle` at occurrence index `use_idx` resolves to
/// the type definition at occurrence index `def_idx`, with [`DefKind::Type`].
fn assert_type_use(src: &str, needle: &str, use_idx: usize, def_idx: usize) {
    let rf = resolve(src);
    let use_range = nth(src, needle, use_idx);
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use ({use_idx}) in {src:?}"));
    assert!(
        matches!(res, Resolution::Local(_)),
        "expected a Local type resolution for {needle:?} in {src:?}, got {res:?}"
    );
    let def = rf
        .resolved_def(res)
        .expect("a Local resolution names an in-file def");
    assert_eq!(
        def.range,
        nth(src, needle, def_idx),
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
    assert_eq!(def.kind, DefKind::Type, "{needle:?} should be a type def");
}

fn assert_shadowable_type(rf: &ResolvedFile, src: &str, needle: &str, idx: usize) {
    let range = nth(src, needle, idx);
    assert_eq!(
        rf.resolution_at(range),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "expected ShadowableType at {needle:?} occurrence {idx} in {src:?}"
    );
}

fn assert_no_resolution(rf: &ResolvedFile, src: &str, needle: &str, idx: usize) {
    let range = nth(src, needle, idx);
    assert_eq!(
        rf.resolution_at(range),
        None,
        "expected no resolution at {needle:?} occurrence {idx} in {src:?}"
    );
}

#[test]
fn type_definition_resolves_to_itself() {
    let src = "type A = int\n";
    let rf = resolve(src);
    let def_range = nth(src, "A", 0);
    let res = rf
        .resolution_at(def_range)
        .expect("the type def occurrence resolves to itself");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, def_range);
    assert_eq!(def.kind, DefKind::Type);
}

#[test]
fn abbreviation_rhs_resolves_to_in_file_type() {
    // `type B = A` — the `A` (2nd "A") jumps to `type A` (1st "A").
    assert_type_use("type A = int\ntype B = A\n", "A", 1, 0);
}

#[test]
fn self_referential_abbreviation_rhs_is_declined() {
    // `type Ap = Ap` is FCS's `TyconCoreAbbrevThatIsReallyAUnion` (the
    // `id.idText = unionCaseName.idText` branch): a single-case *union*, not an
    // abbreviation, so its RHS is a `UnionCase`, not a type reference. We model it
    // as an abbreviation, so we must decline the RHS rather than record it against
    // the type being defined (which would classify it `Type` where FCS reports
    // `UnionCase`). The type *name* (the 1st `Ap`) still self-resolves as a type.
    let src = "type Ap = Ap\n";
    let rf = resolve(src);
    let name = rf
        .resolution_at(nth(src, "Ap", 0))
        .and_then(|r| rf.resolved_def(r))
        .expect("the type name self-resolves");
    assert_eq!(name.kind, DefKind::Type);
    assert_no_resolution(&rf, src, "Ap", 1);
    // Parens are stripped, matching FCS's `StripParenTypes`: `type Ap = (Ap)` is
    // the same single-case union, so its RHS is likewise declined.
    let paren = "type Ap = (Ap)\n";
    let prf = resolve(paren);
    assert_no_resolution(&prf, paren, "Ap", 1);
}

#[test]
fn measure_self_referential_abbreviation_rhs_is_declined() {
    // FCS's `TyconCoreAbbrevThatIsReallyAUnion` is gated on `not hasMeasureAttr`,
    // so a *genuine* `[<Measure>] type X = X` stays a measure abbreviation whose
    // RHS resolves as the type. But whether `[<Measure>]` is the real FSharp.Core
    // attribute or a same-named user type shadowing it — in which case FCS leaves
    // the RHS a `UnionCase` (verified) — is an attribute-identity question this
    // resolution-only pass cannot settle. So we decline the RHS unconditionally:
    // sound either way (never a wrong `Type` vs `UnionCase` commitment), at the
    // cost of a rare missed resolution on a genuine measure self-abbreviation.
    let src = "[<Measure>] type X = X\n";
    let rf = resolve(src);
    // The type name still self-resolves as a type.
    let name = rf
        .resolution_at(nth(src, "X", 0))
        .and_then(|r| rf.resolved_def(r))
        .expect("the measure type name self-resolves");
    assert_eq!(name.kind, DefKind::Type);
    assert_no_resolution(&rf, src, "X", 1);
    // Parenthesized form likewise declines.
    let paren = "[<Measure>] type X = (X)\n";
    let prf = resolve(paren);
    assert_no_resolution(&prf, paren, "X", 1);
}

#[test]
fn abbreviation_rhs_inside_compound_type_resolves() {
    // Postfix application `A list`: the head `list` is out-of-file, the arg `A`
    // resolves through the `App` recursion.
    assert_type_use("type A = int\ntype B = A list\n", "A", 1, 0);
    // Function type `A -> A`: both occurrences resolve.
    assert_type_use("type A = int\ntype B = A -> A\n", "A", 1, 0);
    assert_type_use("type A = int\ntype B = A -> A\n", "A", 2, 0);
    // Tuple type `A * A`.
    assert_type_use("type A = int\ntype B = A * A\n", "A", 2, 0);
}

#[test]
fn record_field_type_resolves_to_in_file_type() {
    // `type R = { Field : A }` — the field's type `A` resolves to `type A`.
    assert_type_use("type A = int\ntype R = { Field : A }\n", "A", 1, 0);
}

#[test]
fn union_case_field_type_resolves_to_in_file_type() {
    // `type U = Case of A` — the case payload `A` resolves to `type A`.
    assert_type_use("type A = int\ntype U = Case of A\n", "A", 1, 0);
}

#[test]
fn mutually_recursive_abbrevs_resolve_both_ways() {
    // An `and`-group is recursive: each name is in scope for the other's RHS.
    // `type R1 = { Next : R2 }\nand R2 = { Prev : R1 }` — R2's use resolves to
    // R2's def, R1's use to R1's def.
    let src = "type R1 = { Next : R2 }\nand R2 = { Prev : R1 }\n";
    // "R2" occurrences: def-in-`and` is... actually R2 first appears as a *use*
    // in R1's field, then as its definition. So use is idx 0, def is idx 1.
    assert_type_use(src, "R2", 0, 1);
    // "R1": definition (idx 0), then used in R2's field (idx 1).
    assert_type_use(src, "R1", 1, 0);
}

#[test]
fn value_return_type_annotation_resolves() {
    // `let x : A = 0` — the return-type annotation `A` resolves to `type A`.
    assert_type_use("type A = int\nlet x : A = 0\n", "A", 1, 0);
}

#[test]
fn non_rec_later_type_does_not_shadow_annotation() {
    let src = "module M\nlet x : int64 = 0L\ntype int64 = Shadow\n";
    let rf = resolve(src);
    assert_no_resolution(&rf, src, "int64", 0);
}

#[test]
fn module_rec_later_type_marks_annotation_shadowable() {
    let src = "module rec M\nlet x : int64 = 0L\ntype int64 = Shadow\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 0);
}

#[test]
fn namespace_rec_later_type_marks_annotation_shadowable() {
    let src = "namespace rec N\nmodule M =\n    let x : int64 = 0L\n\ntype int64 = Shadow\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 0);
}

#[test]
fn project_type_shadow_through_open_marks_annotation_shadowable() {
    let files = [
        "namespace Ns\ntype int64 = Shadow\n",
        "module M\nopen Ns\nlet x : int64 = 0L\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|src| impl_file(src)).collect();
    let project = resolve_project(&asts, &AssemblyEnv::default());
    assert_shadowable_type(project.file(1), files[1], "int64", 0);
}

#[test]
fn project_type_shadow_through_enclosing_namespace_marks_annotation_shadowable() {
    let files = [
        "namespace Ns\ntype int64 = Shadow\n",
        "namespace Ns\nmodule M =\n    let x : int64 = 0L\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|src| impl_file(src)).collect();
    let project = resolve_project(&asts, &AssemblyEnv::default());
    assert_shadowable_type(project.file(1), files[1], "int64", 0);
}

#[test]
fn project_auto_open_module_through_open_marks_annotation_shadowable() {
    let files = [
        "namespace Ns\n[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\n",
        "module M\nopen Ns\nlet x : int64 = 0L\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|src| impl_file(src)).collect();
    let project = resolve_project(&asts, &AssemblyEnv::default());
    assert_shadowable_type(project.file(1), files[1], "int64", 0);
}

#[test]
fn project_auto_open_module_through_enclosing_namespace_marks_annotation_shadowable() {
    let files = [
        "namespace Ns\n[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\n",
        "namespace Ns\nmodule M =\n    let x : int64 = 0L\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|src| impl_file(src)).collect();
    let project = resolve_project(&asts, &AssemblyEnv::default());
    assert_shadowable_type(project.file(1), files[1], "int64", 0);
}

#[test]
fn private_project_auto_open_module_does_not_shadow_across_files() {
    // Regression pin (codex review P2, round 5, on
    // `docs/completed/r2-annotation-typing-plan.md`): F# does not bring a `private`
    // module into scope for another file's `open` of its namespace, so file 1
    // must not see file 0's `[<AutoOpen>] module private Auto` as a shadow
    // source — unlike the *public* sibling test just above, which the same
    // `open Ns` correctly does defer under.
    let files = [
        "namespace Ns\n[<AutoOpen>]\nmodule private Auto =\n    type int64 = Shadow\n",
        "module M\nopen Ns\nlet x : int64 = 0L\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|src| impl_file(src)).collect();
    let project = resolve_project(&asts, &AssemblyEnv::default());
    assert_no_resolution(project.file(1), files[1], "int64", 0);
}

#[test]
fn private_project_auto_open_module_still_shadows_within_its_own_file() {
    // The same-file counterpart: within its *own* file, a `private` auto-open
    // module's `open` is fully valid F#, so the shadow check must still apply
    // there — only the cross-file export is filtered.
    let src = "namespace Ns\n[<AutoOpen>]\nmodule private Auto =\n    type int64 = Shadow\nmodule M =\n    open Ns\n    let x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 1);
}

#[test]
fn opaque_project_module_open_marks_annotation_shadowable() {
    let src = "module Opened =\n    let value = 1\nopen Opened\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 0);
}

#[test]
fn parameter_type_annotation_resolves() {
    // `let f (x : A) = x` — the parameter annotation `A` resolves to `type A`.
    assert_type_use("type A = int\nlet f (x : A) = x\n", "A", 1, 0);
}

#[test]
fn typed_bang_binder_return_type_annotation_resolves() {
    // `let! x : A = m` — a typed computation-expression binder's return-type
    // annotation `A` resolves to `type A`, like a plain `let`'s.
    let src = "type A = int\nlet f = async {\n    let! x : A = m\n    return x\n}\n";
    assert_type_use(src, "A", 1, 0);
}

#[test]
fn lambda_parameter_annotation_resolves() {
    // `fun (x : A) -> x` — the lambda parameter annotation resolves.
    assert_type_use("type A = int\nlet g = fun (x : A) -> x\n", "A", 1, 0);
}

#[test]
fn expression_type_annotation_resolves() {
    // `(0 : A)` — an `Expr::Typed` annotation resolves to `type A`.
    assert_type_use("type A = int\nlet y = (0 : A)\n", "A", 1, 0);
}

#[test]
fn match_clause_pattern_annotation_resolves() {
    // `match o with (x : A) -> x` — the clause pattern's annotation resolves;
    // both the parameter annotation (idx 1) and the clause annotation (idx 2).
    let src = "type A = int\nlet f (o : A) = match o with (x : A) -> x\n";
    assert_type_use(src, "A", 1, 0);
    assert_type_use(src, "A", 2, 0);
}

#[test]
fn anon_record_field_type_resolves_to_in_file_type() {
    // `type B = {| F : A |}` — the anonymous-record field's type `A` resolves
    // to `type A`, exactly like a named record's field type.
    assert_type_use("type A = int\ntype B = {| F : A |}\n", "A", 1, 0);
}

#[test]
fn split_namespace_continuation_resolves_across_blocks() {
    // F# treats two `namespace N` blocks as one namespace: a type defined in the
    // first block is visible to a use in the second. `B`'s `A` (3rd "A", after
    // the keyword-free `namespace`/`type` text) resolves to the first block's
    // `type A`.
    let src = "namespace N\ntype A = int\nnamespace N\ntype B = A\n";
    assert_type_use(src, "A", 1, 0);
}

#[test]
fn distinct_namespaces_keep_separate_type_tables() {
    // Two *different* namespaces each defining `T`: a use in the second resolves
    // to the second's `T`, never leaking to the first's same-named type.
    let src = "namespace A\ntype T = int\nnamespace B\ntype T = string\ntype U = T\n";
    // "T" occurrences: A's def (0), B's def (1), the use in `type U = T` (2).
    // The use must point at B's def (occurrence 1), not A's (occurrence 0).
    assert_type_use(src, "T", 2, 1);
}

#[test]
fn augmentation_head_resolves_to_the_type_definition() {
    // `type A with member …` augments an existing type: its head `A` (2nd "A")
    // is a *use* that jumps to the original `type A` definition (1st "A"), and
    // is not re-interned as a new binder.
    let src = "type A = { X : int }\ntype A with member this.M = 1\n";
    assert_type_use(src, "A", 1, 0);
}

#[test]
fn unrelated_value_use_is_unaffected_by_type_namespace() {
    // A type `T` and a value `f` coexist; a value-position use of `f` resolves
    // to the value, and is never confused with a type of any name.
    let src = "type T = int\nlet f x = x\nlet g = f\n";
    let rf = resolve(src);
    // The `f` use in `let g = f` resolves to the function `f`, a Value.
    let use_range = nth(src, "f", 1);
    let res = rf.resolution_at(use_range).expect("f resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "f", 0));
    assert!(matches!(def.kind, DefKind::Value { .. }));
}

#[test]
fn subtype_constraint_in_return_when_clause_resolves() {
    // `A` is defined in-file and used as the subtype-constraint target of a
    // `when`-constrained return type (`SynType.WithGlobalConstraints`). The
    // constraint's `:> A` names a real type, so go-to-def on that `A` (2nd "A")
    // must reach the `type A` definition (1st "A") — resolving the base type
    // alone is not enough.
    let src = "type A = int\nlet f (x: 'T) : 'T when 'T :> A = x\n";
    assert_type_use(src, "A", 1, 0);
}

#[test]
fn subtype_shorthand_target_resolves() {
    // The `'T :> A` shorthand (`SynType.WithGlobalConstraints` with no `when`
    // group — the target is the node's second `Type` child) names a real type, so
    // go-to-def on that `A` (2nd "A") must reach `type A` (1st "A"). Guards that
    // the resolver visits the shorthand's subtype target, not just `base`.
    let src = "type A = int\nlet f (x: 'T :> A) = x\n";
    assert_type_use(src, "A", 1, 0);
}

#[test]
fn nested_module_sees_enclosing_type() {
    // A nested module sees a type declared in the enclosing module/namespace: the
    // annotation `(x : T)` in `Inner` resolves to `Outer`'s `type T`, walking the
    // container path outward (FCS-verified). "T": type def (0), annotation use in
    // Inner (1).
    let src = "module Outer\ntype T = int\nmodule Inner =\n    let f (x : T) = x\n";
    assert_type_use(src, "T", 1, 0);
}

// ----------------------------------------------------------------------------
// R1: type-position names the resolver defers *because a shadow is possible*
// record `Deferred(ShadowableType)`, so a consumer (inference, R2) can tell
// them apart from a name that genuinely resolves to nothing (left unrecorded).
// ----------------------------------------------------------------------------

/// `open type Foo` (no such type in this single-file env) is opaque: it could
/// supply a type shadowing `int64`, so the type-position `int64` after it is
/// recorded `Deferred(ShadowableType)` rather than left unrecorded.
#[test]
fn type_name_under_opaque_open_defers_shadowable() {
    let src = "module M\nopen type Foo\nlet x : int64 = 1\n";
    let rf = resolve(src);
    assert_eq!(
        rf.resolution_at(nth(src, "int64", 0)),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "a type name under an opaque open must defer as shadowable, not stay unrecorded"
    );
}

/// With no opens, `int64` resolves to nothing in our model (it *is* the
/// primitive) and is left **unrecorded** — the signal a consumer reads as "no
/// shadow possible". This is the safe case R2's alias typing keys on.
#[test]
fn bare_primitive_type_name_is_unrecorded() {
    let src = "module M\nlet x : int64 = 1\n";
    let rf = resolve(src);
    assert_eq!(
        rf.resolution_at(nth(src, "int64", 0)),
        None,
        "an unshadowed primitive must stay unrecorded (no possible shadow)"
    );
}

#[test]
fn auto_open_module_in_a_module_container_marks_annotation_shadowable() {
    // Review finding #4 (probe-confirmed twice): the namespace-keyed shadow
    // signal can never match an auto-open module nested in a MODULE container
    // (`[M, Auto]` is not a walked prefix), so the annotation read as
    // unshadowed — while FCS auto-opens `Auto` into the remainder of `M` and
    // binds `M.Auto.int64`. The same-file signal is name-keyed: sema models
    // the module's own types exactly.
    let src =
        "module M\n[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 1);
}

#[test]
fn auto_open_module_in_an_anonymous_root_marks_annotation_shadowable() {
    // The anonymous-root variant: the path-keyed bookkeeping skips anonymous
    // roots entirely, but the name-keyed same-file signal must still fire.
    let src = "[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 1);
}

#[test]
fn transitive_auto_open_modules_in_a_module_container_shadow_too() {
    // FCS auto-opens recursively; an auto-open module's exit propagates its
    // shadow names (own + accumulated) to the enclosing scope, so the chain
    // falls out of the scope discipline.
    let src = "module M\n[<AutoOpen>]\nmodule Outer =\n    [<AutoOpen>]\n    module Inner =\n        type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 1);
}

#[test]
fn auto_open_shadow_names_are_name_keyed() {
    // Only the names the auto-open module actually declares defer — `uint64`
    // is not one of them.
    let src =
        "module M\n[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\nlet y : uint64 = 1UL\n";
    let rf = resolve(src);
    assert_no_resolution(&rf, src, "uint64", 0);
}

#[test]
fn non_auto_open_nested_module_types_do_not_shadow() {
    // Negative control: without [<AutoOpen>] the nested module's types stay
    // qualified-only.
    let src = "module M\nmodule Closed =\n    type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_no_resolution(&rf, src, "int64", 1);
}

#[test]
fn auto_open_shadow_names_do_not_leak_past_their_container() {
    // `Auto` opens into `Sib`'s remainder only; an annotation at `M` level is
    // outside that scope and must keep its no-shadow reading.
    let src = "module M\nmodule Sib =\n    [<AutoOpen>]\n    module Auto =\n        type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_no_resolution(&rf, src, "int64", 1);
}

#[test]
fn later_direct_type_outranks_the_auto_open_import() {
    // F#'s positional latest-wins: a same-container `type int64` declared
    // AFTER the auto-open module outranks the import, so the annotation must
    // resolve to it exactly (codex on this change: the shadow must not
    // preempt later higher-priority bindings). Occurrences of `int64`: the
    // auto-open module's (0), the direct declaration (1), the annotation (2).
    let src = "module M\n[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\ntype int64 = Direct\nlet x : int64 = 0L\n";
    assert_type_use(src, "int64", 2, 1);
}

#[test]
fn earlier_direct_type_defers_on_the_auto_open_import() {
    // The flip side: the direct `type int64` comes FIRST, so the auto-open
    // import is the later introduction and FCS binds Auto.int64 — the exact
    // positional contest the in-file lookup alone would get backwards (it is
    // name-keyed last-wins, not position-keyed), so this must defer.
    let src = "module M\ntype int64 = Direct\n[<AutoOpen>]\nmodule Auto =\n    type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 2);
}

#[test]
fn private_type_in_an_auto_open_module_does_not_shadow() {
    // codex round 2 on this change: `type private int64` inside the auto-open
    // module is visible within `Auto` only — FCS binds the bare annotation to
    // the primitive, so it must keep its no-shadow reading.
    let src = "module M\n[<AutoOpen>]\nmodule Auto =\n    type private int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_no_resolution(&rf, src, "int64", 1);
}

#[test]
fn private_auto_open_descendant_does_not_leak_past_its_container() {
    // A public type in an auto-open module that is itself `module private` to
    // `Outer`: visible (and shadowing) within Outer, invisible from M.
    let src = "module M\n[<AutoOpen>]\nmodule Outer =\n    [<AutoOpen>]\n    module private Inner =\n        type int64 = Shadow\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_no_resolution(&rf, src, "int64", 1);
}

#[test]
fn private_auto_open_descendant_still_shadows_within_its_container() {
    // The same shape, but the annotation sits INSIDE Outer, where the private
    // Inner is visible and auto-opened — the shadow must fire there.
    let src = "module M\n[<AutoOpen>]\nmodule Outer =\n    [<AutoOpen>]\n    module private Inner =\n        type int64 = Shadow\n    let y : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 1);
}

#[test]
fn private_collision_does_not_evict_a_public_auto_open_shadow() {
    // codex round 3 on this change: sibling auto-open descendants declare the
    // same type name; the LATER one is `module private`. Its depth-pinned
    // entry must not evict the earlier public sibling's — A's `int64` is
    // still visible (and shadowing) from M even though B's is not.
    let src = "module M\n[<AutoOpen>]\nmodule Outer =\n    [<AutoOpen>]\n    module A =\n        type int64 = Shadow\n    [<AutoOpen>]\n    module private B =\n        type int64 = Shadow2\nlet x : int64 = 0L\n";
    let rf = resolve(src);
    assert_shadowable_type(&rf, src, "int64", 2);
}
