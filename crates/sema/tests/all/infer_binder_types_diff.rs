//! Differential test for [`borzoi_sema::infer_file`]'s **binder** types
//! against FCS's typed tree (the `fcs-dump binder-types` oracle): Stage-3.2c-2b
//! monomorphic function-type emission and condition typing.
//!
//! The expression-node oracle (`infer_literals_diff.rs`) cannot reach a function
//! *value* — its type lives on the binder, not on any expression node — so this
//! diffs the binder side directly. The headline property is the same
//! **soundness** (D5): *for every binder type we infer, FCS agrees at that exact
//! declaration range.* We iterate **our** `def_type` map — not FCS's binders —
//! because the danger is saying something *wrong*; staying silent (Deferred, e.g.
//! a polymorphic function) is always allowed.
//!
//! Each binder is keyed by its declaration range: our `Def::range` (the defining
//! identifier token) and FCS's `DeclarationLocation` are the same span, so the
//! two maps line up by byte offset.

use crate::common::{invoke_fcs_dump, parse_fcs_binder_types, temp_fs_file};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, InferredFile, ProjectItems, ResolvedFile, Ty, infer_file, resolve_file,
};

/// Resolve and infer `source` (single-file: empty project + no referenced
/// assemblies), returning both so a binder's `DefId` can be mapped to its
/// declaration range. Asserts the snippet is in our parseable subset.
fn resolve_and_infer(source: &str) -> (ResolvedFile, InferredFile) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors (outside the subset?): {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = AssemblyEnv::default();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    (resolved, inferred)
}

/// Infer `source`, run the FCS `binder-types` oracle over it, and assert the D5
/// soundness property for every binder type we produced. Returns how many we
/// checked.
fn assert_binder_sound(source: &str) -> usize {
    let (resolved, inferred) = resolve_and_infer(source);

    let path = temp_fs_file("infer_binder", source);
    let json = invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let fcs = parse_fcs_binder_types(&json, source);

    let mut checked = 0usize;
    for (def_id, ty) in inferred.def_types() {
        let def = resolved.def(*def_id);
        let key = (
            u32::from(def.range.start()) as usize,
            u32::from(def.range.end()) as usize,
        );
        let fcs_ty = fcs.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{}` for binder `{}` at {key:?} but FCS reports no binder there \
                 in {source:?}",
                ty.render(),
                def.name
            )
        });
        assert_eq!(
            &ty.render(),
            fcs_ty,
            "binder-type mismatch for `{}` at {key:?} in {source:?}: ours=`{}`, FCS=`{fcs_ty}`",
            def.name,
            ty.render()
        );
        checked += 1;
    }
    checked
}

/// A monomorphic function typed via condition grounding: `let f c = if c then 1
/// else 2` ⇒ `f : bool -> int`, agreeing with FCS. The parameter `c` is grounded
/// to bool to build the type but is not published as a standalone binder, so only
/// `f` is checked.
#[test]
fn monomorphic_function_matches_fcs() {
    let source = "module M\nlet f c = if c then 1 else 2\n";
    // Only `f` (bool -> int); the parameter `c` is not published standalone.
    assert_eq!(assert_binder_sound(source), 1);
}

/// A curried function right-associates and agrees with FCS: `let f a b = …` ⇒
/// `f : bool -> bool -> int`. The parameters `a`, `b` are grounded internally but
/// not published standalone.
#[test]
fn curried_function_matches_fcs() {
    let source = "module M\nlet f a b = if a then (if b then 1 else 2) else 3\n";
    // Only `f` (bool -> bool -> int).
    assert_eq!(assert_binder_sound(source), 1);
}

/// Value binders (and value-chain propagation) agree with FCS on the binder axis:
/// `x : int` and `y : int` (from `let y = x`).
#[test]
fn value_binders_match_fcs() {
    let source = "module M\nlet x = 42\nlet y = x\n";
    assert_eq!(assert_binder_sound(source), 2);
}

/// A monomorphic and a polymorphic function side by side: the monomorphic one
/// (`f : bool -> int`) *and* the now-generalised `id` (`'a -> 'a`, Stage 3.2c-2c)
/// are both emitted and agree with FCS. (Before generalisation `id` deferred; the
/// old `polymorphic_function_defers` binder-diff test is subsumed by
/// `identity_generalises_to_a_to_a`.)
#[test]
fn mixed_mono_and_generic_functions() {
    let source = "module M\nlet f c = if c then 1 else 2\nlet id x = x\n";
    // `f` (bool -> int) and `id` (`'a -> 'a`); parameters not published standalone.
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(binder_render(source, "id").as_deref(), Some("'a -> 'a"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// An annotated parameter grounds through its annotation (Stage R2-b): the
/// parameter annotation is exact in F# (subsumption applies at call sites, not
/// the binder), so `let f (c: bool) = if c then 1 else 2` is
/// `f : bool -> int`, agreeing with FCS. The parameter is still not published
/// standalone. (Flipped from the pre-R2-b defer pin.)
#[test]
fn annotated_parameter_types_the_function() {
    let source = "module M\nlet f (c: bool) = if c then 1 else 2\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(binder_render(source, "c"), None);
    assert_eq!(assert_binder_sound(source), 1);
}

/// R6's parameter shapes: `let f (x: int) = x` ⇒ `int -> int` — the annotated
/// parameter curries into the function type exactly like a condition-grounded
/// one, and its bare-use body grounds the return.
#[test]
fn annotated_parameter_grounds_the_return_matching_fcs() {
    let source = "module M\nlet f (x: int) = x\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Int32 -> System.Int32")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// Annotation + generalisation composing (the R2-b oracle's mixed shape):
/// `let f (b: bool) x = ((if b then 1 else 2), x)` ⇒
/// `bool -> 'a -> int * 'a` — the annotated parameter grounds, the bare one
/// quantifies, matching FCS.
#[test]
fn annotated_and_bare_parameters_compose() {
    let source = "module M\nlet f (b: bool) x = ((if b then 1 else 2), x)\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> 'a -> System.Int32 * 'a")
    );
    assert_eq!(assert_binder_sound(source), 1);

    // The branch-tuple variant `if b then (1, x) else (2, x)` keeps `x` in a
    // check-mode else branch, whose dropped relation poisons it — so the
    // function defers (silence, where FCS generalises the same scheme). A
    // poison-rule refinement could lift this; the pin is that nothing *wrong*
    // is emitted.
    let deferred = "module M\nlet f (b: bool) x = if b then (1, x) else (2, x)\n";
    assert_eq!(binder_render(deferred, "f"), None);
    assert_binder_sound(deferred);
}

/// The ill-typed-condition counterpart of the R2-a ill-typed-RHS differential:
/// `let f (c: int) = if c then 1 else 2` errors at the condition, but FCS
/// keeps the annotation's `c : int` and `f : int -> int` — a positive
/// differential on erroring code.
#[test]
fn ill_typed_condition_annotation_matches_fcs() {
    let source = "module M\nlet f (c: int) = if c then 1 else 2\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Int32 -> System.Int32")
    );
    assert_binder_sound(source);
}

/// The 3.3c hazard shape with the annotation modelled (R2-b): `let h (y:
/// string) = (y, fi y)` against an in-file `fi : int -> int64`, an erroring
/// file. FCS keeps `y : string` (the annotation is exact on the binder; the
/// application is the error site) and `h : string -> string * int64` (the
/// result fixed by `fi`'s own shape). We agree on both counts: the annotation
/// grounds `y` before the wake, the wake's `Eq(string, int)` fails and rolls
/// back, and the already-ground result is unaffected by the failed check's
/// poison — a positive differential on erroring code.
#[test]
fn conflicting_wake_keeps_annotation_and_result() {
    let source = "module M\nlet fi (n: int) = 7L\nlet h (y: string) = (y, fi y)\n";
    assert_eq!(
        binder_render(source, "fi").as_deref(),
        Some("System.Int32 -> System.Int64")
    );
    assert_eq!(
        binder_render(source, "h").as_deref(),
        Some("System.String -> System.String * System.Int64")
    );
    assert_eq!(assert_binder_sound(source), 2);
}

// ============================================================================
// Stage R2-c — function return-type annotations
// ============================================================================

/// R6's grounding-through-the-return shape: `let h x : int = x` ⇒
/// `int -> int` — the annotation is a no-subsumption type (sealed `int`), so
/// the body↔annotation relation discharges as a genuine equality, grounding
/// the bare parameter through its body use. Matches FCS.
#[test]
fn return_annotation_grounds_the_parameter() {
    let source = "module M\nlet h x : int = x\n";
    assert_eq!(
        binder_render(source, "h").as_deref(),
        Some("System.Int32 -> System.Int32")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// R6's annotated-both-ends shape: `let g (x: int) : string = "s"` ⇒
/// `int -> string`, both annotations firing (R2-b on the parameter, R2-c on
/// the return), matching FCS.
#[test]
fn parameter_and_return_annotations_compose() {
    let source = "module M\nlet g (x: int) : string = \"s\"\n";
    assert_eq!(
        binder_render(source, "g").as_deref(),
        Some("System.Int32 -> System.String")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// The `obj` defer pin: `let f x : obj = x` is legal via subsumption
/// (FCS: `obj -> obj`), but the dropped body↔annotation relation must not
/// ground `x` — the wake's no-subsumption gate excludes `obj`, the
/// undischarged check poisons `x`, and `f` defers. Nothing wrong is emitted.
#[test]
fn subsumption_return_annotation_defers() {
    let source = "module M\nlet f x : obj = x\n";
    assert_eq!(binder_render(source, "f"), None);
    assert_eq!(assert_binder_sound(source), 0);
}

/// A subsumption-possible return annotation still emits when the parameter
/// side is ground: `let f (b: bool) : obj = b` ⇒ `bool -> obj` — the
/// annotated return rides into the function type; the dropped body relation
/// poisons only ground/irrelevant variables. Matches FCS.
#[test]
fn subsumption_return_with_ground_params_emits() {
    let source = "module M\nlet f (b: bool) : obj = b\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> System.Object")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// Ill-typed bodies keep the annotation's truth (the R2-c counterpart of the
/// R2-a ill-typed-RHS differential): `let f x : int = "s"` errors at the
/// body, but FCS still says `f : 'a -> int` — the failed body discharge rolls
/// back and poisons only its own ground endpoints, so the unused parameter
/// still generalises. Same for the retargeted-literal shape
/// `let f x : int64 = 42` (`'a -> int64`; the literal's default `int` loses
/// to nothing — its node stays silent, the binder truth stands).
#[test]
fn ill_typed_body_keeps_return_annotation() {
    let source = "module M\nlet f x : int = \"s\"\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("'a -> System.Int32")
    );
    assert_binder_sound(source);

    let retarget = "module M\nlet f x : int64 = 42\n";
    assert_eq!(
        binder_render(retarget, "f").as_deref(),
        Some("'a -> System.Int64")
    );
    assert_binder_sound(retarget);
}

/// A tuple of sealed primitives is a no-subsumption return too — the same
/// domain judgment as the ArgCheck wake, not a second sealedness rule:
/// `let p x : int * bool = (1, true)` ⇒ `'a -> int * bool`, matching FCS.
#[test]
fn sealed_tuple_return_annotation_matches_fcs() {
    let source = "module M\nlet p x : int * bool = (1, true)\n";
    assert_eq!(
        binder_render(source, "p").as_deref(),
        Some("'a -> System.Int32 * System.Boolean")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// A ground `Ty::Fun` flowing into a tuple renders unambiguously: `let t = (f, 0)`
/// where `f : bool -> int` types `t` as `(bool -> int) * int` — the function
/// element parenthesised — and agrees with FCS byte-for-byte (the oracle's
/// `renderTypeCanonical` parenthesises function tuple elements the same way).
#[test]
fn tuple_containing_a_function_is_parenthesised() {
    let source = "module M\nlet f c = if c then 1 else 2\nlet t = (f, 0)\n";
    let (resolved, inferred) = resolve_and_infer(source);
    let t_ty = inferred
        .def_types()
        .iter()
        .find(|(id, _)| resolved.def(**id).name == "t")
        .map(|(_, ty)| ty.render());
    assert_eq!(
        t_ty.as_deref(),
        Some("(System.Boolean -> System.Int32) * System.Int32"),
        "the function element must be parenthesised in the tuple"
    );
    assert_binder_sound(source);
}

// ============================================================================
// Stage 3.2c-2c — generalisation differential
// ============================================================================

/// Look up the canonical render of the binder named `name` in `inferred`.
fn binder_render(source: &str, name: &str) -> Option<String> {
    let (resolved, inferred) = resolve_and_infer(source);
    inferred
        .def_types()
        .iter()
        .find(|(id, _)| resolved.def(**id).name == name)
        .map(|(_, ty)| ty.render())
}

/// The identity function generalises to `'a -> 'a`, agreeing with FCS's
/// canonicalised typar rendering. This is the headline 3.2c-2c payoff.
#[test]
fn identity_generalises_to_a_to_a() {
    let source = "module M\nlet f x = x\n";
    assert_eq!(binder_render(source, "f").as_deref(), Some("'a -> 'a"));
    // `f` is checked against FCS (`'a -> 'a` on both sides); the parameter `x` is
    // not published standalone.
    assert_eq!(assert_binder_sound(source), 1);
}

/// A constant function `let k a b = a` generalises to `'a -> 'b -> 'a` — two
/// distinct parameters, the return reusing the first — matching FCS.
#[test]
fn const_function_generalises() {
    let source = "module M\nlet k a b = a\n";
    assert_eq!(
        binder_render(source, "k").as_deref(),
        Some("'a -> 'b -> 'a")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// A swap-tuple `let f a b = (b, a)` generalises to `'a -> 'b -> 'b * 'a`: the
/// parameters number by head order, the return tuple by first appearance.
#[test]
fn swap_tuple_generalises() {
    let source = "module M\nlet f a b = (b, a)\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("'a -> 'b -> 'b * 'a")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// A mixed ground+param function `let f c x = ((if c then 1 else 2), x)` ⇒
/// `bool -> 'a -> int * 'a`: `c` grounds to bool via the condition, `x` is
/// quantified, the tuple's first element is the ground `int` if-result.
#[test]
fn mixed_ground_and_param_generalises() {
    let source = "module M\nlet f c x = ((if c then 1 else 2), x)\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> 'a -> System.Int32 * 'a")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// The slot=binder payoff `let f x = if x then (1, x) else (2, x)` ⇒
/// `bool -> int * bool`: the condition grounds `x`'s slot to bool, the slot=binder
/// reunification (a *complete* binding) flows that to the tuple's `x` use, so the
/// whole function is ground. 2b had to defer this.
#[test]
fn condition_grounded_param_use_is_bool() {
    let source = "module M\nlet f x = if x then (1, x) else (2, x)\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> System.Int32 * System.Boolean")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// A generalised function and its use in a later binding: `let id x = x` ⇒
/// `'a -> 'a`, and `let h x = (id, x)` instantiates `id` afresh, generalising to
/// `'a -> ('b -> 'b) * 'a` — the nested scheme's fresh `'b` distinct from `h`'s
/// own `'a`, matching FCS.
#[test]
fn nested_instantiation_generalises() {
    let source = "module M\nlet id x = x\nlet h x = (id, x)\n";
    assert_eq!(binder_render(source, "id").as_deref(), Some("'a -> 'a"));
    assert_eq!(
        binder_render(source, "h").as_deref(),
        Some("'a -> ('b -> 'b) * 'a")
    );
    // Both `id` and `h` are checked against FCS.
    assert_eq!(assert_binder_sound(source), 2);
}

/// An unused parameter still generalises: `let f x = 42` ⇒ `'a -> int` — the
/// parameter quantified, the return the literal's ground `int` — matching FCS.
#[test]
fn unused_parameter_generalises_over_ground_return() {
    let source = "module M\nlet f x = 42\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("'a -> System.Int32")
    );
    assert_eq!(assert_binder_sound(source), 1);
}

/// Defer cases stay silent (nothing wrong emitted): an unmodelled body
/// (`let f x = x + 1`), a poisoned parameter (`let f c x = if c then x else x`),
/// and a compound condition (`let f x y = if x && y then 1 else 2`). Each is a
/// case where FCS grounds or relates a variable through a constraint we drop, so
/// we must not generalise — `assert_binder_sound` confirms we say nothing.
#[test]
fn generalisation_defer_cases_stay_silent() {
    for source in [
        "module M\nlet f x = x + 1\n",
        "module M\nlet f c x = if c then x else x\n",
        "module M\nlet f x y = if x && y then 1 else 2\n",
    ] {
        let (_, inferred) = resolve_and_infer(source);
        assert!(
            inferred.def_types().is_empty(),
            "a defer case must emit no binder type; got {:?} for {source:?}",
            inferred
                .def_types()
                .values()
                .map(Ty::render)
                .collect::<Vec<_>>()
        );
        assert_eq!(assert_binder_sound(source), 0);
    }
}

/// A **ground** environment reference must NOT be over-deferred by the mark check:
/// `let a = 42` then `let g x = (x, a)` ⇒ `g : 'a -> 'a * int`. The mark rule only
/// blocks an *inherited-open* variable; a ground earlier binder (`a : int`) flows
/// into the tuple and `x` still generalises — matching FCS. (The open-env case
/// `let a = <unmodelled> … let g x = (x, a)` defers, covered by a behaviour test.)
#[test]
fn ground_environment_reference_still_generalises() {
    let source = "module M\nlet a = 42\nlet g x = (x, a)\n";
    assert_eq!(binder_render(source, "a").as_deref(), Some("System.Int32"));
    assert_eq!(
        binder_render(source, "g").as_deref(),
        Some("'a -> 'a * System.Int32")
    );
    // `a` and `g` are both checked against FCS.
    assert_eq!(assert_binder_sound(source), 2);
}

// ============================================================================
// Stage 3.2c-3 — function application (v1: no worklist)
// ============================================================================

/// A ground function applied to an argument grounds the result binder: `let f c =
/// if c then 1 else 2` (`f : bool -> int`) then `let n = f true` ⇒ `n : int`. The
/// return type is right regardless of how the argument coerces (the argument
/// relation is dropped/poisoned, but the *result* `r` is fixed by
/// `Eq(f, Fun(d, r))` against `f`'s ground `bool -> int`). Both `f` and `n` agree
/// with FCS.
#[test]
fn ground_application_result_matches_fcs() {
    let source = "module M\nlet f c = if c then 1 else 2\nlet n = f true\n";
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(binder_render(source, "n").as_deref(), Some("System.Int32"));
    // `f` (bool -> int) and `n` (int); the parameter `c` is not published.
    assert_eq!(assert_binder_sound(source), 2);
}

/// Curried **full** application grounds the result: `let add a b = …`
/// (`add : bool -> bool -> int`) then `let n = add true false` ⇒ `n : int`. The
/// nested `App(App(add, true), false)` curries — the outer result is `int`.
#[test]
fn curried_full_application_grounds_result() {
    let source =
        "module M\nlet add a b = if a then (if b then 1 else 2) else 3\nlet n = add true false\n";
    assert_eq!(
        binder_render(source, "add").as_deref(),
        Some("System.Boolean -> System.Boolean -> System.Int32")
    );
    assert_eq!(binder_render(source, "n").as_deref(), Some("System.Int32"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// **Partial** application grounds a function-typed result binder: `let add a b =
/// …` then `let g = add true` ⇒ `g : bool -> int`. Applying the 2-ary ground
/// `add` to one argument leaves `r = bool -> int` (the residual function), which
/// is ground and flows to `g`. FCS agrees.
#[test]
fn partial_application_grounds_function_result() {
    let source =
        "module M\nlet add a b = if a then (if b then 1 else 2) else 3\nlet g = add true\n";
    assert_eq!(
        binder_render(source, "g").as_deref(),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(assert_binder_sound(source), 2);
}

/// An application result flowing into a tuple: `let f c = …` (`f : bool -> int`)
/// then `let t = (f true, 0)` ⇒ `t : int * int`. The ground application `f true`
/// (`int`) is the first tuple element, the literal `0` the second; both ground, so
/// the tuple is ground and agrees with FCS.
#[test]
fn application_result_in_tuple_matches_fcs() {
    let source = "module M\nlet f c = if c then 1 else 2\nlet t = (f true, 0)\n";
    assert_eq!(
        binder_render(source, "t").as_deref(),
        Some("System.Int32 * System.Int32")
    );
    assert_eq!(assert_binder_sound(source), 2);
}

/// A function whose body **dead-ends an application into ground types** can still
/// generalise an unrelated parameter: `let f c = …` (ground `bool -> int`) then
/// `let h x = (f true, x)` ⇒ `'a -> int * 'a`. The application `f true` is ground
/// `int` (its poison bites only the dropped argument relation, and a *ground* var
/// is unaffected by poison), the tuple's first element; `x` is quantified. FCS
/// agrees.
#[test]
fn ground_application_dead_end_lets_unrelated_param_generalise() {
    let source = "module M\nlet f c = if c then 1 else 2\nlet h x = (f true, x)\n";
    assert_eq!(
        binder_render(source, "h").as_deref(),
        Some("'a -> System.Int32 * 'a")
    );
    assert_eq!(assert_binder_sound(source), 2);
}

// ============================================================================
// Stage 3.3c — the application wake rule (suspended arg↔param, coercion-free)
// ============================================================================

/// The 3.3c headline: `let id x = x` (`'a -> 'a`) then `let n = id 42` ⇒
/// `n : int`, agreeing with FCS. The literal argument is suspended as an
/// `ArgCheck` against `id`'s domain `d` (a scheme instantiation variable of ours,
/// a no-subsumption domain); the complete value binding's wake discharges
/// `Eq(int, d)`, grounding `d = r = int`. (Flipped from the 3.2c-3
/// `polymorphic_application_defers_result` deferral.)
#[test]
fn polymorphic_application_wakes_and_grounds_result() {
    let source = "module M\nlet id x = x\nlet n = id 42\n";
    assert_eq!(binder_render(source, "id").as_deref(), Some("'a -> 'a"));
    assert_eq!(binder_render(source, "n").as_deref(), Some("System.Int32"));
    // Both `id` (`'a -> 'a`) and `n` (int) are checked against FCS.
    assert_eq!(assert_binder_sound(source), 2);
}

/// The `let g y = id y` payoff: the argument `y` is suspended against `id`'s
/// domain; on this walk-complete function binding the wake discharges `Eq(y, d)`,
/// so `y = d = r` and `g` generalises to `'a -> 'a`, matching FCS. (Flipped from
/// the 3.2c-3 defer pin.)
#[test]
fn applied_polymorphic_argument_generalises() {
    let source = "module M\nlet id x = x\nlet g y = id y\n";
    assert_eq!(binder_render(source, "id").as_deref(), Some("'a -> 'a"));
    assert_eq!(binder_render(source, "g").as_deref(), Some("'a -> 'a"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// A chained application wakes through the chain: `let c x = id (id x)` ⇒
/// `'a -> 'a`. The inner `id x` wakes, grounding its result into `x`'s class; the
/// outer `id (…)` wakes likewise, so the whole chain is one quantifiable class.
#[test]
fn chained_application_generalises() {
    let source = "module M\nlet id x = x\nlet c x = id (id x)\n";
    assert_eq!(binder_render(source, "c").as_deref(), Some("'a -> 'a"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// An in-file monomorphic function's application wakes on a sealed-primitive
/// domain: `let fb b = if b then 1 else 2` (`bool -> int`) then `let h y = fb y`
/// ⇒ `h : bool -> int`. The domain grounds to the sealed `bool` (no-subsumption),
/// so the complete binding's wake discharges `Eq(y, bool)`. FCS agrees.
#[test]
fn in_file_monomorphic_application_wakes() {
    let source = "module M\nlet fb b = if b then 1 else 2\nlet h y = fb y\n";
    assert_eq!(
        binder_render(source, "fb").as_deref(),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(
        binder_render(source, "h").as_deref(),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(assert_binder_sound(source), 2);
}

/// A ground value argument wakes: `let b = true`, `let fb b2 = if b2 …`
/// (`bool -> int`), `let n = fb b` ⇒ `n : int`. The domain grounds to `bool`, the
/// argument is the ground `b : bool`, and the wake's `Eq(bool, bool)` is a no-op;
/// the result `n : int`. FCS agrees on every binder.
#[test]
fn ground_value_argument_application_matches_fcs() {
    let source = "module M\nlet b = true\nlet fb b2 = if b2 then 1 else 2\nlet n = fb b\n";
    assert_eq!(
        binder_render(source, "b").as_deref(),
        Some("System.Boolean")
    );
    assert_eq!(binder_render(source, "n").as_deref(), Some("System.Int32"));
    // `b`, `fb`, and `n` are all checked against FCS.
    assert_eq!(assert_binder_sound(source), 3);
}

/// An instantiated-scheme argument flowing into a tuple: `let k y = (id y, y)` ⇒
/// `'a -> 'a * 'a`. The `id y` wake collapses `y`, the inner result, and the first
/// tuple element into one quantifiable class; the second element shares `y`. FCS
/// agrees.
#[test]
fn instantiated_scheme_argument_in_tuple_generalises() {
    let source = "module M\nlet id x = x\nlet k y = (id y, y)\n";
    assert_eq!(binder_render(source, "k").as_deref(), Some("'a -> 'a * 'a"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// A woken argument grounds a **synth-position** use in the same body, and that
/// emission must match FCS: `let fb b = …` (`bool -> int`), `let h y = (fb y, y)`
/// ⇒ `h : bool -> int * bool`. The wake grounds `y := bool` (the sealed domain),
/// so the tuple's second `y` element — a synth-mode use — emits `bool`, agreeing
/// with FCS's `y : bool`. This is the sound-emission complement of the annotated
/// hazard (there the gate *blocks* the wake to protect a synth use FCS keeps
/// generic; here the wake *fires* and FCS agrees).
#[test]
fn woken_argument_grounds_a_synth_use_matching_fcs() {
    let source = "module M\nlet fb b = if b then 1 else 2\nlet h y = (fb y, y)\n";
    assert_eq!(
        binder_render(source, "h").as_deref(),
        Some("System.Boolean -> System.Int32 * System.Boolean")
    );
    assert_eq!(assert_binder_sound(source), 2);
}

/// A ground-value argument against a sealed domain, differential end-to-end:
/// `let fb b = …`, `let m = fb 1` where the ill-typed `1 : int` vs the sealed
/// `bool` fails the wake's `Eq` and rolls back — yet the result `m : int` is still
/// grounded by the function shape, agreeing with FCS. The failed arg discharge
/// leaves no wrong binder type anywhere.
#[test]
fn ill_typed_literal_argument_keeps_only_the_result() {
    let source = "module M\nlet fb b = if b then 1 else 2\nlet m = fb 1\n";
    assert_eq!(binder_render(source, "m").as_deref(), Some("System.Int32"));
    // `fb` (bool -> int) and `m` (int) are checked; nothing wrong for the argument.
    assert_binder_sound(source);
}

// ============================================================================
// Stage R2-a — annotated value binders (docs/completed/r2-annotation-typing-plan.md)
// ============================================================================

/// The R1 probe shapes: a table-alias return annotation types the *binder*
/// (`let a : int64 = 42L` ⇒ `a : System.Int64`), agreeing with FCS's canonical
/// FQN. Includes `obj` (the annotation is an exact equality on the binder
/// regardless of `obj`'s subsumption-target role — `let c : obj = "hi"` has
/// `c : System.Object`) and `string`-with-`null`.
#[test]
fn annotated_value_binders_match_fcs() {
    let source = "module M\nlet a : int64 = 42L\nlet b : int = 1\nlet c : obj = \"hi\"\nlet d : string = null\nlet e : float = 1.0\n";
    assert_eq!(binder_render(source, "a").as_deref(), Some("System.Int64"));
    assert_eq!(binder_render(source, "b").as_deref(), Some("System.Int32"));
    assert_eq!(binder_render(source, "c").as_deref(), Some("System.Object"));
    assert_eq!(binder_render(source, "d").as_deref(), Some("System.String"));
    assert_eq!(binder_render(source, "e").as_deref(), Some("System.Double"));
    assert_eq!(assert_binder_sound(source), 5);
}

/// The R2 probe — the fact that makes R2-a sound at all: the annotation types
/// the binder **even on ill-typed code**. `let x : int64 = "s"` is a type
/// error, yet FCS keeps `x : System.Int64`, and the use-chain `let y = x` is
/// `Int64` too. A *positive* differential on an erroring file: we emit, FCS
/// agrees. (The RHS↔annotation relation is the error site; the binder is not.)
#[test]
fn ill_typed_rhs_annotation_still_types_binder() {
    let source = "module M\nlet x : int64 = \"s\"\nlet y = x\n";
    assert_eq!(binder_render(source, "x").as_deref(), Some("System.Int64"));
    assert_eq!(binder_render(source, "y").as_deref(), Some("System.Int64"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// R10: a *value* named `int64` does not shadow the type position, so the
/// annotation still fires — `x : System.Int64` — while the value binder itself
/// types from its literal RHS.
#[test]
fn value_shadow_does_not_defer_annotation() {
    let source = "module M\nlet int64 = 5\nlet x : int64 = 42L\n";
    assert_eq!(
        binder_render(source, "int64").as_deref(),
        Some("System.Int32")
    );
    assert_eq!(binder_render(source, "x").as_deref(), Some("System.Int64"));
    assert_eq!(assert_binder_sound(source), 2);
}

/// R8: in a non-rec module a *later* `type int64` does not shadow an earlier
/// annotation (position-ordering is real — probe-confirmed), so the alias
/// fires and agrees with FCS.
#[test]
fn later_type_does_not_defer_annotation() {
    let source = "module M\nlet x : int64 = 42L\ntype int64 = A\n";
    assert_eq!(binder_render(source, "x").as_deref(), Some("System.Int64"));
    assert_binder_sound(source);
}

/// R11: structural annotation shapes — tuple, function, array — recurse through
/// the table leaves and render byte-for-byte as FCS's canonical forms.
#[test]
fn structural_annotations_match_fcs() {
    let source = "module M\nlet a : int * string = (1, \"s\")\nlet f : int -> int = fun x -> x\nlet arr : int[] = [| 1; 2 |]\n";
    assert_eq!(
        binder_render(source, "a").as_deref(),
        Some("System.Int32 * System.String")
    );
    assert_eq!(
        binder_render(source, "f").as_deref(),
        Some("System.Int32 -> System.Int32")
    );
    assert_eq!(
        binder_render(source, "arr").as_deref(),
        Some("System.Int32[]")
    );
    assert_eq!(assert_binder_sound(source), 3);
}

/// Every table entry and source synonym, one binding each, all agreeing with
/// FCS — the 17-arm `fsharp_primitive_alias` table (plus synonyms) is exactly
/// FCS's abbreviation semantics for these names.
#[test]
fn alias_table_sweep_matches_fcs() {
    let source = "\
module M
let i8 : int8 = 1y
let u8 : uint8 = 1uy
let i16v : int16 = 1s
let u16v : uint16 = 1us
let i32v : int32 = 1
let u32v : uint32 = 1u
let uv : uint = 2u
let i64v : int64 = 1L
let u64v : uint64 = 1UL
let sb : sbyte = 1y
let by : byte = 1uy
let ni : nativeint = 1n
let un : unativeint = 1un
let fl : float = 1.0
let db : double = 1.0
let f32v : float32 = 1.0f
let sg : single = 1.0f
let dc : decimal = 1.0m
let bo : bool = true
let ch : char = 'c'
let st : string = \"s\"
let ob : obj = \"hi\"
";
    // 22 annotated bindings, each emitted and agreeing with FCS.
    assert_eq!(assert_binder_sound(source), 22);
}

/// A trivial typed-pattern head `let (x : int64) = 42` rides along: the same
/// annotation gate, same binder emission.
#[test]
fn typed_pattern_head_matches_fcs() {
    let source = "module M\nlet (x : int64) = 42\n";
    assert_eq!(binder_render(source, "x").as_deref(), Some("System.Int64"));
    assert_eq!(assert_binder_sound(source), 1);
}

/// Shadow shapes must defer — the gate sees the resolver's record (`Local` for
/// an in-file `type int64` [R3], the open-module resolution [R4], and the R2-0
/// `Deferred(ShadowableType)` markers for a same-file `[<AutoOpen>]` type [V1]
/// and a `module rec` forward type [V2]) and stays silent; whatever else we
/// emit agrees with FCS.
#[test]
fn shadowed_annotations_defer() {
    for source in [
        // R3: an in-file `type int64` shadows the primitive.
        "module M\ntype int64 = A of int\nlet x : int64 = A 1\n",
        // R4: an opened module's `type int64` shadows it.
        "module M\nmodule Inner =\n    type int64 = A\nopen Inner\nlet x : int64 = A\n",
        // V1: a same-file `[<AutoOpen>]` module's nested type shadows it.
        "module M\n[<AutoOpen>]\nmodule Auto =\n    type int64 = A\nlet x : int64 = A\n",
        // V2: inside `module rec`, the *later* type shadows the earlier annotation.
        "module rec M\nlet x : int64 = A\ntype int64 = A\n",
    ] {
        let (resolved, inferred) = resolve_and_infer(source);
        let x_typed = inferred
            .def_types()
            .keys()
            .any(|id| resolved.def(*id).name == "x");
        assert!(
            !x_typed,
            "the shadowed annotation must defer `x` in {source:?}"
        );
        assert_binder_sound(source);
    }
}

/// R5/R7: a measure application (`float<m>`) and a generic application
/// (`int64 option`) are non-bare heads, deferred *by shape* — while a bare
/// `float` beside the measure still types (`System.Double`, not the measured
/// type), agreeing with FCS.
#[test]
fn measure_and_generic_annotations_defer() {
    let source = "module M\n[<Measure>] type m\nlet x : float<m> = 3.0<m>\nlet y : float = 1.0\n";
    assert_eq!(binder_render(source, "x"), None);
    assert_eq!(binder_render(source, "y").as_deref(), Some("System.Double"));
    assert_eq!(assert_binder_sound(source), 1);

    let generic = "module M\nlet x : int64 option = None\n";
    assert_eq!(binder_render(generic, "x"), None);
    assert_eq!(assert_binder_sound(generic), 0);
}

// ============================================================================
// The ArgCheck wake's environment guard
// ============================================================================

/// A later application must not retro-ground an earlier binding's still-open
/// binder: `let g = id` is generalised by FCS (`g : 'a -> 'a` — probed
/// 2026-07-08), so when `let n = g 1` fires the application machinery, the
/// wake's environment guard blocks the discharge against `g`'s inherited
/// instantiation variables. We stay silent on `g` and `n` (FCS knows both;
/// silence is allowed, `g : int -> int` was not). The annotated variant still
/// types `n` from its annotation.
#[test]
fn later_application_does_not_monomorphise_an_earlier_binder() {
    let source = "module M\nlet id x = x\nlet g = id\nlet n = g 1\n";
    assert_eq!(binder_render(source, "id").as_deref(), Some("'a -> 'a"));
    assert_eq!(binder_render(source, "g"), None);
    assert_binder_sound(source);

    let annotated = "module M\nlet id x = x\nlet g = id\nlet n : int = g 1\n";
    assert_eq!(binder_render(annotated, "g"), None);
    assert_eq!(
        binder_render(annotated, "n").as_deref(),
        Some("System.Int32")
    );
    assert_binder_sound(annotated);
}
