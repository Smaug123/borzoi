//! C# 11 `required`-member contract decoding against the real `MiniLib.dll`,
//! read through the public byte entry point `Ecma335Assembly::parse`. MiniLib's
//! `RequiredHolder` carries the full compiler-producible matrix:
//!
//!   - `[RequiredMemberAttribute]` on a field (`Tag`) and a property (`Name`)
//!     → `is_required = true`;
//!   - a `[SetsRequiredMembersAttribute]` parameterless constructor
//!     → `sets_required_members = true`;
//!   - a non-`[SetsRequiredMembers]` `(int)` constructor, on which Roslyn
//!     emits the synthetic `[Obsolete(error)]` + `[CompilerFeatureRequired(
//!     "RequiredMembers")]` fallback pair for pre-C#-11 compilers. The
//!     projector must surface the gate but suppress the synthetic Obsolete.
//!
//! These are absolute value pins. The `assembly_diff` differential test
//! enumerates the same fixture and proves both projectors *agree*, but it
//! would stay green even if both sides dropped `is_required` or failed to
//! suppress the synthetic Obsolete — so this file pins the concrete Rust-side
//! values (the diff pins agreement with FCS).
//!
//! The shapes a net10.0 Roslyn build can't emit — `SetsRequiredMembers` under
//! the polyfill `System.Runtime.CompilerServices` namespace, and the
//! suppression narrowing guards (a non-`"RequiredMembers"` gate, a gate on a
//! non-constructor, a standalone Obsolete with no gate) — have no
//! compiler-produced fixture and so are not covered here.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Field, Member, MethodLike, Property};

use crate::common::ensure_minilib_built;

fn load() -> Vec<Entity> {
    let dll = ensure_minilib_built();
    let bytes = std::fs::read(dll).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MiniLib");
    view.enumerate_type_defs().expect("enumerate MiniLib types")
}

fn required_holder(entities: &[Entity]) -> &Entity {
    entities
        .iter()
        .find(|e| e.name == "RequiredHolder")
        .unwrap_or_else(|| {
            panic!(
                "RequiredHolder not found among {:?}",
                entities.iter().map(|e| &e.name).collect::<Vec<_>>()
            )
        })
}

fn field<'a>(e: &'a Entity, name: &str) -> &'a Field {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Field(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("field {name:?} not found on {:?}", e.name))
}

fn property<'a>(e: &'a Entity, name: &str) -> &'a Property {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == name => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("property {name:?} not found on {:?}", e.name))
}

/// The unique constructor taking `arity` parameters.
fn ctor_with_arity(e: &Entity, arity: usize) -> &MethodLike {
    let mut matches = e.members.iter().filter_map(|m| match m {
        Member::Method(m) if m.is_constructor && m.signature.parameters.len() == arity => Some(m),
        _ => None,
    });
    let found = matches
        .next()
        .unwrap_or_else(|| panic!("no {arity}-arg constructor on {:?}", e.name));
    assert!(
        matches.next().is_none(),
        "expected exactly one {arity}-arg constructor on {:?}",
        e.name,
    );
    found
}

#[test]
fn required_field_surfaces_is_required() {
    // `public required int Tag;` → `[RequiredMemberAttribute]` on the field.
    let entities = load();
    assert!(field(required_holder(&entities), "Tag").is_required);
}

#[test]
fn required_property_surfaces_is_required() {
    // `public required string Name { get; set; }` → `[RequiredMemberAttribute]`
    // on the property.
    let entities = load();
    assert!(property(required_holder(&entities), "Name").is_required);
}

#[test]
fn sets_required_members_ctor_surfaces_the_flag() {
    // The parameterless ctor carries `[SetsRequiredMembersAttribute]`, so it
    // opts out of the object-initialiser obligation. Roslyn does not pair the
    // synthetic Obsolete/feature-gate with it.
    let entities = load();
    let ctor = ctor_with_arity(required_holder(&entities), 0);
    assert!(ctor.sets_required_members);
    assert!(ctor.obsolete.is_none());
    assert!(ctor.compiler_feature_required.is_empty());
}

#[test]
fn non_sets_required_ctor_keeps_gate_but_suppresses_synthetic_obsolete() {
    // The `(int)` ctor lacks `[SetsRequiredMembers]`, so Roslyn stamps the
    // synthetic `[Obsolete(error)]` + `[CompilerFeatureRequired("RequiredMembers")]`
    // pre-C#-11 fallback pair onto it. The gate must surface (proving the pair
    // is present) while the paired synthetic Obsolete is suppressed.
    let entities = load();
    let ctor = ctor_with_arity(required_holder(&entities), 1);
    assert!(!ctor.sets_required_members);
    assert!(
        ctor.compiler_feature_required
            .iter()
            .any(|g| g.feature == "RequiredMembers"),
        "expected a CompilerFeatureRequired(\"RequiredMembers\") gate, got {:?}",
        ctor.compiler_feature_required,
    );
    assert!(
        ctor.obsolete.is_none(),
        "the synthetic Obsolete paired with the RequiredMembers gate must be \
         suppressed, got {:?}",
        ctor.obsolete,
    );
}
