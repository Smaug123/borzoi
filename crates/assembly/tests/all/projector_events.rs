//! Event projection against the real `MiniLib.dll`, read through the public
//! byte entry point `Ecma335Assembly::parse`. MiniLib's `Counter` carries the
//! full Roslyn-producible event matrix — field-like, custom-accessor with a
//! closed generic delegate, static, and the protected / protected-internal /
//! private accessibility rungs — so these are absolute pins on the same
//! shapes the `assembly_diff` differential test cross-checks (this file pins
//! the values; the diff pins agreement with FCS).
//!
//! The non-producible event shapes (open-ended `OtherMethods`, accessors
//! disagreeing on static-ness, a modreq or generic accessor, a `raise_`
//! fire accessor) have no C#/F# fixture and so are not covered here.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Access, Ecma335Assembly, EcmaView, Entity, Event, Member, Nullability, NullableType, Primitive,
    TypeRef,
};

use crate::common::ensure_minilib_built;

fn load() -> Vec<Entity> {
    let dll = ensure_minilib_built();
    let bytes = std::fs::read(dll).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MiniLib");
    view.enumerate_type_defs().expect("enumerate MiniLib types")
}

fn counter(entities: &[Entity]) -> &Entity {
    entities
        .iter()
        .find(|e| e.name == "Counter")
        .unwrap_or_else(|| {
            panic!(
                "Counter not found among {:?}",
                entities.iter().map(|e| &e.name).collect::<Vec<_>>()
            )
        })
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

/// Assert `delegate_type` is the non-generic `System.EventHandler`.
fn assert_event_handler(ty: &TypeRef) {
    match ty {
        TypeRef::Named {
            namespace,
            name,
            type_args,
            ..
        } => {
            assert_eq!(namespace.as_slice(), ["System"]);
            assert_eq!(name, "EventHandler");
            assert!(
                type_args.is_empty(),
                "non-generic EventHandler carries no type args, got {type_args:?}"
            );
        }
        other => panic!("expected Named System.EventHandler, got {other:?}"),
    }
}

#[test]
fn field_like_public_instance_event_decodes() {
    // `public event System.EventHandler Tick;` — the field-like form. The
    // synthesised private backing field is filtered; only the Event surfaces.
    let entities = load();
    let ev = event(counter(&entities), "Tick");
    assert_eq!(ev.access, Access::Public);
    assert!(!ev.is_static);
    assert!(!ev.has_fire, "C# never emits a raise_ accessor");
    assert_eq!(ev.nullability, Nullability::Oblivious);
    assert_event_handler(&ev.delegate_type);
}

#[test]
fn static_event_derives_static_from_accessor() {
    // `public static event System.EventHandler Reset;` — there is no
    // top-level event static flag; static-ness is read off the add accessor.
    let entities = load();
    let ev = event(counter(&entities), "Reset");
    assert_eq!(ev.access, Access::Public);
    assert!(ev.is_static);
    assert_event_handler(&ev.delegate_type);
}

#[test]
fn protected_event_surfaces_protected_access() {
    // `protected event System.EventHandler ProtectedTick;` — ECMA-335 Family
    // on both accessors → Protected at the event level.
    let entities = load();
    let ev = event(counter(&entities), "ProtectedTick");
    assert_eq!(ev.access, Access::Protected);
    assert!(!ev.is_static);
}

#[test]
fn protected_internal_event_joins_to_protected_or_internal() {
    // `protected internal event System.EventHandler InternalTick;` —
    // FamORAssem on the accessors → ProtectedOrInternal.
    let entities = load();
    let ev = event(counter(&entities), "InternalTick");
    assert_eq!(ev.access, Access::ProtectedOrInternal);
}

#[test]
fn custom_accessor_event_decodes_closed_generic_delegate() {
    // `public event System.EventHandler<int> CustomTick { add; remove; }` —
    // a custom-accessor event whose delegate is a closed generic
    // instantiation, exercising the TypeRef generic-args path through the
    // event surface.
    let entities = load();
    let ev = event(counter(&entities), "CustomTick");
    assert_eq!(ev.access, Access::Public);
    assert!(!ev.is_static);
    match &ev.delegate_type {
        TypeRef::Named {
            namespace,
            name,
            type_args,
            ..
        } => {
            assert_eq!(namespace.as_slice(), ["System"]);
            assert_eq!(name, "EventHandler");
            assert_eq!(
                type_args.as_slice(),
                [NullableType {
                    ty: TypeRef::Primitive(Primitive::I4),
                    nullability: Nullability::Oblivious,
                }],
            );
        }
        other => panic!("expected Named System.EventHandler<int>, got {other:?}"),
    }
}

#[test]
fn private_event_surfaces_with_private_access() {
    // `private event System.EventHandler HiddenTick;` — the projector
    // surfaces every accessibility rung faithfully; dropping inaccessible
    // members is a downstream concern (the `AccessibleFromSomeFSharpCode`
    // normaliser), not the projector's. This pins the Private rung.
    let entities = load();
    let ev = event(counter(&entities), "HiddenTick");
    assert_eq!(ev.access, Access::Private);
    assert!(!ev.is_static);
}
