//! Type-level projection (kind discrimination, namespace splitting, nesting,
//! implemented interfaces, base types), asserted against the real
//! `MemberShapes.dll` read through the public byte entry point
//! `Ecma335Assembly::parse`. Driving the projector from real PE bytes validates
//! the owned `Entity` output and keeps the reader behind `Ecma335Assembly::parse`
//! swappable.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{Access, Ecma335Assembly, EcmaView, Entity, EntityKind, TypeRef};

use crate::common::ensure_member_shapes_built;

fn load() -> Vec<Entity> {
    let dll = ensure_member_shapes_built();
    let bytes = std::fs::read(dll).expect("read MemberShapes.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MemberShapes");
    view.enumerate_type_defs()
        .expect("enumerate MemberShapes types")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "entity {name:?} not found among {:?}",
            entities.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

#[test]
fn skips_synthetic_module_type() {
    let entities = load();
    assert!(
        !entities.iter().any(|e| e.name == "<Module>"),
        "the synthetic <Module> type must not be projected"
    );
}

#[test]
fn projects_a_public_class_in_a_namespace() {
    let entities = load();
    let gadget = entity(&entities, "Gadget");
    assert_eq!(
        gadget.namespace,
        vec!["MemberShapes".to_string(), "Widgets".to_string()]
    );
    assert_eq!(gadget.name, "Gadget");
    assert_eq!(gadget.kind, EntityKind::Class);
    assert_eq!(gadget.access, Access::Public);
    assert!(
        matches!(&gadget.base_type, Some(TypeRef::Named { namespace, name, .. })
            if namespace == &["System"] && name == "Object"),
        "unexpected base_type: {:?}",
        gadget.base_type,
    );
    assert!(gadget.nested_types.is_empty());
}

#[test]
fn discriminates_struct_from_class() {
    let entities = load();
    assert_eq!(entity(&entities, "PointStruct").kind, EntityKind::Struct);
}

#[test]
fn discriminates_interface_from_class() {
    let entities = load();
    let iface = entity(&entities, "IThing");
    assert_eq!(iface.kind, EntityKind::Interface);
    assert!(iface.base_type.is_none());
}

#[test]
fn nests_children_under_parents_and_omits_them_from_top_level() {
    let entities = load();
    assert!(
        !entities.iter().any(|e| e.name == "Inner"),
        "Inner should not appear at top level"
    );
    let outer = entity(&entities, "Outer");
    assert_eq!(outer.nested_types.len(), 1);
    let inner = &outer.nested_types[0];
    assert_eq!(inner.name, "Inner");
    assert_eq!(inner.access, Access::Public);
    // Per the model contract, a nested type's own namespace is empty (the
    // path lives on the outer type).
    assert!(inner.namespace.is_empty());
}

#[test]
fn projects_interfaces_implemented_by_type() {
    let entities = load();
    let thing = entity(&entities, "Thing");
    assert_eq!(thing.interfaces.len(), 1);
    assert!(
        matches!(&thing.interfaces[0], TypeRef::Named { name, assembly: None, .. } if name == "IThing"),
        "unexpected interface ref: {:?}",
        thing.interfaces[0],
    );
}
