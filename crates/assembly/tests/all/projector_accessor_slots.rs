//! `Property::accessor_slots`, `Event::accessor_slots` and
//! `MethodLike::has_other_method_impl`, asserted against the real
//! `MemberShapes.dll` (source `AccessorSlots.cs`).
//!
//! These are what a consumer needs to decide whether a property, event or
//! method *overrides* a base member the way C# reads it from metadata: a
//! property is an override when any accessor reuses a base slot, and a
//! covariant-return override reuses none — it is `newslot` plus a `MethodImpl`
//! row to the base class method, which the model would otherwise drop.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    AccessorSlot, Ecma335Assembly, EcmaView, Entity, Event, Member, MethodLike, Property,
};

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
        .find(|e| e.name == name && e.namespace == ["MemberShapes", "AccessorSlots"])
        .unwrap_or_else(|| panic!("entity {name:?} not found"))
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

fn event<'a>(e: &'a Entity, name: &str) -> &'a Event {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Event(ev) if ev.name == name => Some(ev),
            _ => None,
        })
        .unwrap_or_else(|| panic!("event {name:?} not found on {:?}", e.name))
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

const NEW_VIRTUAL: AccessorSlot = AccessorSlot {
    is_virtual: true,
    is_newslot: true,
    is_abstract: false,
    is_final: false,
    has_other_method_impl: false,
};

const OVERRIDE: AccessorSlot = AccessorSlot {
    is_virtual: true,
    is_newslot: false,
    is_abstract: false,
    is_final: false,
    has_other_method_impl: false,
};

const PLAIN: AccessorSlot = AccessorSlot {
    is_virtual: false,
    is_newslot: false,
    is_abstract: false,
    is_final: false,
    has_other_method_impl: false,
};

#[test]
fn a_virtual_property_has_a_new_virtual_slot_per_accessor() {
    let es = load();
    assert_eq!(
        property(entity(&es, "SlotBase"), "Both").accessor_slots,
        [NEW_VIRTUAL, NEW_VIRTUAL]
    );
}

#[test]
fn a_getter_only_override_has_one_reused_slot() {
    let es = load();
    assert_eq!(
        property(entity(&es, "SlotDerived"), "Both").accessor_slots,
        [OVERRIDE]
    );
}

#[test]
fn a_non_virtual_property_has_plain_slots() {
    let es = load();
    for ty in ["SlotBase", "SlotDerived"] {
        assert_eq!(
            property(entity(&es, ty), "Plain").accessor_slots,
            [PLAIN, PLAIN],
            "{ty}.Plain"
        );
    }
}

#[test]
fn event_slots_are_add_then_remove() {
    let es = load();
    assert_eq!(
        event(entity(&es, "SlotBase"), "Changed").accessor_slots,
        [NEW_VIRTUAL, NEW_VIRTUAL]
    );
    assert_eq!(
        event(entity(&es, "SlotDerived"), "Changed").accessor_slots,
        [OVERRIDE, OVERRIDE]
    );
}

/// The covariant-return override is `newslot` — by its flags, a fresh
/// virtual — and only the `MethodImpl` row the model records here says it
/// overrides `SlotBase.Clone`.
#[test]
fn a_covariant_return_override_carries_its_method_impl() {
    let es = load();
    let derived = method(entity(&es, "SlotDerived"), "Clone");
    assert!(derived.is_virtual && derived.is_newslot);
    assert!(derived.implements.is_empty() && derived.unclassified_impls.is_empty());
    assert!(derived.has_other_method_impl);
    assert!(!method(entity(&es, "SlotBase"), "Clone").has_other_method_impl);
}
