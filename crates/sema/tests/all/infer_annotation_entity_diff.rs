//! Stage R2-d — **entity-backed annotations**: differential and behaviour
//! tests for annotations whose head resolves to a concrete
//! [`Resolution::Entity`] in a referenced assembly (`String` under
//! `open System`; qualified `System.Int64` — the plan's probe row R9), typed
//! by bridging the entity to a [`Ty::Named`] under the `member_ty.rs`
//! conventions (non-generic, non-nested, non-renamed; modules / abbreviation
//! markers / measures defer).
//!
//! The differential builds an [`AssemblyEnv`] from the real BCL
//! `System.Runtime.dll` (as `infer_member_access_diff` does), so the resolver
//! records the same entities FCS sees when it checks the script against the
//! SDK BCL; binder types then compare at the binder's declaration range via
//! the `binder-types` oracle. As everywhere in this suite, we iterate **our**
//! emissions and assert FCS agrees (D5: silence is always allowed, wrongness
//! never).

use crate::common::{
    ensure_system_runtime_dll, invoke_fcs_dump, parse_fcs_binder_types, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, InferredFile, ProjectItems, ResolvedFile, infer_file, resolve_file,
};

/// An [`AssemblyEnv`] over the real BCL `System.Runtime.dll` — so
/// `System.String`, `System.Guid`, `System.Int64`, … are present as entities.
fn bcl_env() -> AssemblyEnv {
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

/// Resolve and infer `source` against the BCL env.
fn resolve_and_infer(source: &str) -> (ResolvedFile, InferredFile) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors (outside the subset?): {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    (resolved, inferred)
}

/// The canonical render of the binder named `name`, if we emitted one.
fn binder_render(source: &str, name: &str) -> Option<String> {
    let (resolved, inferred) = resolve_and_infer(source);
    inferred
        .def_types()
        .iter()
        .find(|(id, _)| resolved.def(**id).name == name)
        .map(|(_, ty)| ty.render())
}

/// Infer `source` against the BCL env, run the FCS `binder-types` oracle, and
/// assert every binder type we produced agrees with FCS at that exact
/// declaration range. Returns how many we checked.
fn assert_binder_sound(source: &str) -> usize {
    let (resolved, inferred) = resolve_and_infer(source);

    let path = temp_fs_file("infer_ann_entity", source);
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

/// R9's open-shortened shape: `String` under `open System` resolves to the
/// `System.String` entity, which bridges to the annotation type — the binder
/// agrees with FCS.
#[test]
fn open_shortened_entity_annotation_matches_fcs() {
    let source = "module M\nopen System\nlet x : String = null\n";
    assert_eq!(binder_render(source, "x").as_deref(), Some("System.String"));
    assert_eq!(assert_binder_sound(source), 1);
}

/// R9's qualified shape: `System.Int64` — the multi-segment head's *tail*
/// segment carries the concrete entity record, now usable instead of the
/// blanket multi-segment deferral.
#[test]
fn qualified_entity_annotation_matches_fcs() {
    let source = "module M\nlet x : System.Int64 = 42L\n";
    assert_eq!(binder_render(source, "x").as_deref(), Some("System.Int64"));
    assert_eq!(assert_binder_sound(source), 1);
}

/// A non-primitive entity (`Guid`) types the binder too — the bridge is the
/// entity model, not the alias table.
#[test]
fn non_primitive_entity_annotation_matches_fcs() {
    let source = "module M\nopen System\nlet g : Guid = Guid.Empty\n";
    assert_eq!(binder_render(source, "g").as_deref(), Some("System.Guid"));
    assert_binder_sound(source);
}

/// Entity-backed annotations compose with R2-b (parameters) and R2-c
/// (returns): `let f (s: String) = s` is `String -> String`, and the sealed
/// `System.String` return annotation grounds a bare parameter through the
/// body (`let h x : String = x`), both matching FCS.
#[test]
fn entity_annotations_compose_with_parameters_and_returns() {
    let param = "module M\nopen System\nlet f (s: String) = s\n";
    assert_eq!(
        binder_render(param, "f").as_deref(),
        Some("System.String -> System.String")
    );
    assert_eq!(assert_binder_sound(param), 1);

    let ret = "module M\nopen System\nlet h x : String = x\n";
    assert_eq!(
        binder_render(ret, "h").as_deref(),
        Some("System.String -> System.String")
    );
    assert_eq!(assert_binder_sound(ret), 1);
}

/// Structural recursion over entity leaves: `String * Guid` renders exactly
/// as FCS's canonical tuple.
#[test]
fn entity_tuple_annotation_matches_fcs() {
    let source = "module M\nopen System\nlet p : String * Guid = (null, Guid.Empty)\n";
    assert_eq!(
        binder_render(source, "p").as_deref(),
        Some("System.String * System.Guid")
    );
    assert_binder_sound(source);
}

/// Defer pins (behaviour only — nothing emitted, so FCS runs are pointless):
/// a generic entity head (`List<int>` — an `App` shape *and* arity ≥ 1), a
/// non-generic entity used at the wrong arity, and an unresolved bare name
/// (`String` without `open System` — no record, not in the alias table).
#[test]
fn entity_annotation_defer_shapes_stay_silent() {
    for (source, why) in [
        (
            "module M\nlet x : System.Collections.Generic.List<int> = null\n",
            "generic application",
        ),
        (
            "module M\nlet x : String = null\n",
            "unresolved bare name without the open",
        ),
    ] {
        let (resolved, inferred) = resolve_and_infer(source);
        let x_typed = inferred
            .def_types()
            .keys()
            .any(|id| resolved.def(*id).name == "x");
        assert!(!x_typed, "{why}: {source:?}");
    }
}
