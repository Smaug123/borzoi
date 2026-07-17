//! Raw MethodDef/TypeDef flag-word projection (OV-2 of
//! `docs/overload-resolution-plan.md`): `Entity::is_sealed` and
//! `MethodLike::{is_final, is_newslot, is_hide_by_sig}`, asserted against the
//! real `MemberShapes.dll` (source `OverloadFlags.cs`) read through the public
//! byte entry point `Ecma335Assembly::parse`.
//!
//! These are *raw IL bits* — their ground truth is the emitted IL, which the C#
//! modifiers in the fixture fix exactly, so the projection is pinned directly
//! (a stronger oracle than the FCS `entities` differential, which has no IL
//! MethodDef for F#-authored members and so cannot faithfully diff these bits
//! for the F# fixtures — see the OV-2 note in the plan).
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member, MethodLike};

use crate::common::ensure_member_shapes_built;

fn load() -> Vec<Entity> {
    let dll = ensure_member_shapes_built();
    let bytes = std::fs::read(dll).expect("read MemberShapes.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MemberShapes");
    view.enumerate_type_defs()
        .expect("enumerate MemberShapes types")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("entity {name:?} not found"))
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

// ── Entity::is_sealed ────────────────────────────────────────────────────────

#[test]
fn sealed_class_is_sealed() {
    let es = load();
    assert!(entity(&es, "SealedType").is_sealed);
}

#[test]
fn open_class_is_not_sealed() {
    let es = load();
    assert!(!entity(&es, "OpenBase").is_sealed);
    assert!(!entity(&es, "Derived").is_sealed);
}

#[test]
fn abstract_class_is_not_sealed() {
    let es = load();
    assert!(!entity(&es, "AbstractHost").is_sealed);
}

#[test]
fn value_type_is_always_sealed() {
    let es = load();
    assert!(
        entity(&es, "SealedStruct").is_sealed,
        "the CLR marks every value type sealed"
    );
}

// ── MethodLike::{is_final, is_newslot, is_hide_by_sig} ───────────────────────

#[test]
fn new_virtual_is_newslot_not_final() {
    let es = load();
    let v = method(entity(&es, "OpenBase"), "V");
    assert!(v.is_virtual);
    assert!(v.is_newslot, "a new virtual claims a fresh vtable slot");
    assert!(!v.is_final);
    assert!(v.is_hide_by_sig);
}

#[test]
fn sealed_override_is_final_not_newslot() {
    let es = load();
    let v = method(entity(&es, "Derived"), "V");
    assert!(v.is_virtual);
    assert!(v.is_final, "a sealed override is final");
    assert!(
        !v.is_newslot,
        "an override reuses the base vtable slot, so is not newslot"
    );
    assert!(v.is_hide_by_sig);
}

#[test]
fn abstract_member_is_newslot_and_hidebysig() {
    let es = load();
    let a = method(entity(&es, "AbstractHost"), "A");
    assert!(a.is_abstract && a.is_virtual);
    assert!(a.is_newslot);
    assert!(!a.is_final);
    assert!(a.is_hide_by_sig);
}

#[test]
fn non_virtual_method_has_no_vtable_flags() {
    let es = load();
    let p = method(entity(&es, "OpenBase"), "P");
    assert!(!p.is_virtual && !p.is_abstract && !p.is_newslot && !p.is_final);
    assert!(p.is_hide_by_sig, "C# emits hidebysig on every method");
    let plain = method(entity(&es, "SealedType"), "Plain");
    assert!(!plain.is_virtual && !plain.is_newslot && !plain.is_final);
    assert!(plain.is_hide_by_sig);
}
