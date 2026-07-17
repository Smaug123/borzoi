//! Struct- and parameter-marker attribute decoding against the real
//! `MiniLib.dll`, read through the public byte entry point
//! `Ecma335Assembly::parse`. Covers the BCL marker attributes a modern C#
//! compiler emits (referenced from `System.Runtime` on net10.0, the
//! cross-assembly `Reference` arm):
//!
//!   - `[ParamArrayAttribute]` on the final `params T[]` parameter of
//!     `Counter.Sum` → `Parameter::is_param_array`;
//!   - `[IsReadOnlyAttribute]` on a `readonly struct` → `Entity::is_readonly`;
//!   - `[IsByRefLikeAttribute]` on a `ref struct` → `Entity::is_byref_like`,
//!     paired with the type-level `[CompilerFeatureRequired("RefStructs")]`
//!     gate Roslyn stamps on every ref struct → `Entity::compiler_feature_required`;
//!   - `readonly ref struct` carries both struct markers at once.
//!
//! These are absolute value pins. The `assembly_diff` differential test
//! enumerates the same MiniLib fixture and proves both projectors *agree*, but
//! it would stay green even if both sides dropped a marker (every marker
//! assertion there is `false`), so this file pins the concrete Rust-side `true`
//! values (the diff pins agreement with FCS).
//!
//! These pins exercise the cross-assembly `Reference` arm of
//! `attribute_owning_type` — on net10.0 Roslyn references the marker attributes
//! from `System.Runtime`. The complementary same-assembly `Definition` arm
//! (an assembly that *defines* the marker attribute itself, e.g. mscorlib /
//! netstandard2.0 / the BCL) has no net10.0 fixture that reaches it, so it is
//! not covered here. The corpus differential test compares only
//! structural type-def shape (`NormType`), not these projected marker flags.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    CompilerFeatureRequired, Ecma335Assembly, EcmaView, Entity, Member, MethodLike, Parameter,
};

use crate::common::ensure_minilib_built;

fn load() -> Vec<Entity> {
    let dll = ensure_minilib_built();
    let bytes = std::fs::read(dll).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MiniLib");
    view.enumerate_type_defs().expect("enumerate MiniLib types")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "entity {name:?} not found among {:?}",
            entities.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

fn method<'a>(e: &'a Entity, name: &str) -> &'a MethodLike {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == name => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| panic!("method {name:?} not found on {:?}", e.name))
}

fn param<'a>(m: &'a MethodLike, name: &str) -> &'a Parameter {
    m.signature
        .parameters
        .iter()
        .find(|p| p.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("parameter {name:?} not found on {:?}", m.name))
}

#[test]
fn c_sharp_methods_are_a_single_argument_group() {
    // OV-6.1: MiniLib is a C# assembly (no host F# signature pickle), and C#/VB
    // cannot curry — every method's flattened parameter list *is* its one
    // argument group. So the projector leaves `arg_group_count: Some(1)`, letting
    // the overload engine commit multi-parameter C# calls. `Counter.Sum(params
    // int[])` and any other method alike. See `docs/completed/ov-6.1-curry-detection-plan.md`.
    let entities = load();
    let sum = method(entity(&entities, "Counter"), "Sum");
    assert_eq!(sum.arg_group_count, Some(1));
}

#[test]
fn params_array_parameter_surfaces_is_param_array() {
    // `public int Sum(params int[] values)` → `[ParamArrayAttribute]` on the
    // final parameter. The marker is the sole signal; the IL parameter type is
    // a plain `int[]`.
    let entities = load();
    let sum = method(entity(&entities, "Counter"), "Sum");
    assert!(param(sum, "values").is_param_array);
}

#[test]
fn readonly_struct_surfaces_is_readonly_only() {
    // `public readonly struct ReadOnlyPoint` → `[IsReadOnlyAttribute]`, but no
    // byref-like marker and no RefStructs gate.
    let entities = load();
    let e = entity(&entities, "ReadOnlyPoint");
    assert!(e.is_readonly);
    assert!(!e.is_byref_like);
    assert!(e.compiler_feature_required.is_empty());
}

#[test]
fn ref_struct_surfaces_is_byref_like_and_refstructs_gate() {
    // `public ref struct RefSpan` → `[IsByRefLikeAttribute]` plus the
    // type-level `[CompilerFeatureRequired("RefStructs")]` Roslyn stamps on
    // every ref struct. It is not readonly.
    let entities = load();
    let e = entity(&entities, "RefSpan");
    assert!(e.is_byref_like);
    assert!(!e.is_readonly);
    assert_eq!(
        e.compiler_feature_required,
        vec![CompilerFeatureRequired {
            feature: "RefStructs".into(),
            is_optional: false,
        }],
    );
}

#[test]
fn readonly_ref_struct_surfaces_both_markers() {
    // `public readonly ref struct ReadOnlyRefSpan` carries both struct markers
    // at once, still with the RefStructs gate.
    let entities = load();
    let e = entity(&entities, "ReadOnlyRefSpan");
    assert!(e.is_readonly);
    assert!(e.is_byref_like);
    assert_eq!(
        e.compiler_feature_required,
        vec![CompilerFeatureRequired {
            feature: "RefStructs".into(),
            is_optional: false,
        }],
    );
}

#[test]
fn non_params_parameter_does_not_set_is_param_array() {
    // Negative control: a non-`params` parameter on the same fixture stays
    // false, so the positive pins above can't be a blanket true.
    let entities = load();
    let combine = method(entity(&entities, "Counter"), "Combine");
    assert!(
        combine
            .signature
            .parameters
            .iter()
            .all(|p| !p.is_param_array)
    );
}
