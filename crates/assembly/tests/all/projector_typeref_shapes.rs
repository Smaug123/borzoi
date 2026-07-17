//! TypeRef-shape projection: same-assembly base collapse (`assembly: None`),
//! backtick-arity stripping on both TypeDef and TypeRef names, and
//! slash-qualified names for nested types (same-assembly via the encloser walk
//! and cross-assembly via the `Nested` resolution-scope chain). Plus the
//! assembly-identity name/version pin. Asserted against the real
//! `MemberShapes.dll` read through the public byte entry point
//! `Ecma335Assembly::parse`.
//!
//! MiniLib's full-tree differential diff already compares these shapes against
//! the FCS oracle; these are the one-sided absolute pins that survive a
//! both-readers-drop-the-feature regression the symmetric diff can't catch.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use crate::common;

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Field, Member, TypeRef, Version};

use crate::common::ensure_member_shapes_built;

fn load() -> Vec<Entity> {
    let dll = ensure_member_shapes_built();
    let bytes = std::fs::read(dll).expect("read MemberShapes.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MemberShapes");
    view.enumerate_type_defs()
        .expect("enumerate MemberShapes types")
}

fn view() -> Ecma335Assembly {
    let dll = ensure_member_shapes_built();
    let bytes = std::fs::read(dll).expect("read MemberShapes.dll");
    Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MemberShapes")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "entity {name:?} not found among {:?}",
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

#[test]
fn assembly_identity_carries_name_and_version() {
    // The manifest's assembly name and version reach `identity()`. The version
    // is the non-default `1.2.3.4` pinned in `MemberShapes.csproj`, so this
    // fails if the projector ever echoes a hardcoded default instead.
    let view = view();
    assert_eq!(view.identity().name, "MemberShapes");
    assert_eq!(
        view.identity().version,
        Version {
            major: 1,
            minor: 2,
            build: 3,
            revision: 4,
        }
    );
}

#[test]
fn same_assembly_base_collapses_to_assembly_none() {
    // A base class defined in this assembly resolves through the direct-TypeDef
    // `extends` path to `assembly: None` — never a cross-asm ref to our own
    // identity.
    let entities = load();
    match &entity(&entities, "DerivedUser").base_type {
        Some(TypeRef::Named {
            assembly,
            namespace,
            name,
            ..
        }) => {
            assert!(assembly.is_none(), "expected same-assembly ref");
            assert_eq!(
                namespace,
                &vec!["MemberShapes".to_string(), "Shapes".to_string()]
            );
            assert_eq!(name, "BaseHelper");
        }
        other => panic!("expected Named BaseHelper, got {other:?}"),
    }
}

#[test]
fn current_module_typeref_base_collapses_to_assembly_none() {
    // The other encoding of a same-assembly base: a `TypeRef` whose
    // ResolutionScope is the current module (`ResolutionScope::CurrentModule`).
    // No C# compiler emits this — it uses a direct TypeDef token — so the shape
    // is fabricated by the metadata emitter. It must collapse to `assembly:
    // None`, not a cross-asm ref to our own identity.
    let bytes = common::emit_metadata_fixture("currentmodule_typeref_base");
    let view = Ecma335Assembly::parse(&bytes).expect("parse CurrentModule fixture");
    let entities = view.enumerate_type_defs().expect("enumerate types");
    match &entity(&entities, "User").base_type {
        Some(TypeRef::Named { assembly, name, .. }) => {
            assert!(assembly.is_none(), "expected same-assembly ref");
            assert_eq!(name, "Helper");
        }
        other => panic!("expected Named Helper, got {other:?}"),
    }
}

#[test]
fn backtick_arity_stripped_from_typedef_name() {
    // `GenericHolder`1` on disk projects to the bare entity name.
    let entities = load();
    assert!(
        entities.iter().any(|e| e.name == "GenericHolder"),
        "expected entity GenericHolder (arity stripped), got {:?}",
        entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

#[test]
fn backtick_arity_stripped_from_typeref_name() {
    // Deriving from the cross-asm generic `List`1` projects a base `TypeRef`
    // named `List`, with an external assembly scope.
    let entities = load();
    match &entity(&entities, "MyIntList").base_type {
        Some(TypeRef::Named { assembly, name, .. }) => {
            assert_eq!(name, "List");
            assert!(assembly.is_some(), "expected cross-asm base ref");
        }
        other => panic!("expected Named List, got {other:?}"),
    }
}

#[test]
fn nested_typedef_base_uses_slash_qualified_name() {
    // A same-assembly nested base walks the encloser chain to `Outer/Inner` and
    // carries the outer type's namespace.
    let entities = load();
    match &entity(&entities, "NestedDerived").base_type {
        Some(TypeRef::Named {
            assembly,
            namespace,
            name,
            ..
        }) => {
            assert!(assembly.is_none(), "expected same-assembly ref");
            assert_eq!(
                namespace,
                &vec!["MemberShapes".to_string(), "Shapes".to_string()]
            );
            assert_eq!(name, "Outer/Inner");
        }
        other => panic!("expected Named Outer/Inner, got {other:?}"),
    }
}

#[test]
fn nested_cross_asm_typeref_uses_slash_qualified_name() {
    // A field typed as the cross-asm nested `System.Environment+SpecialFolder`
    // walks the `Nested` resolution-scope chain to `Environment/SpecialFolder`,
    // namespace `System`, with an external assembly scope.
    let entities = load();
    match &field(entity(&entities, "CrossAsmNestedRef"), "Folder").ty {
        TypeRef::Named {
            assembly,
            namespace,
            name,
            ..
        } => {
            assert!(assembly.is_some(), "expected cross-asm ref");
            assert_eq!(namespace, &vec!["System".to_string()]);
            assert_eq!(name, "Environment/SpecialFolder");
        }
        other => panic!("expected Named Environment/SpecialFolder, got {other:?}"),
    }
}

#[test]
fn nested_generic_encloser_records_per_segment_arity_cross_asm() {
    // A field typed as the cross-asm nested `Dictionary<int,string>.Enumerator`:
    // the generic arguments belong to the enclosing `Dictionary`2`, the nested
    // `Enumerator` adds none, so the projected per-segment delta arity is
    // `[2, 0]`. This is the metadata fact the old `strip_arity` walk discarded.
    let entities = load();
    match &field(entity(&entities, "CrossAsmNestedGenericRef"), "Enum").ty {
        TypeRef::Named {
            assembly,
            namespace,
            name,
            type_args,
            segment_arities,
        } => {
            assert!(assembly.is_some(), "expected cross-asm ref");
            assert_eq!(
                namespace,
                &vec![
                    "System".to_string(),
                    "Collections".to_string(),
                    "Generic".to_string()
                ]
            );
            assert_eq!(name, "Dictionary/Enumerator");
            assert_eq!(segment_arities, &vec![2, 0]);
            assert_eq!(
                type_args.len(),
                2,
                "Dictionary<int, string> carries two args"
            );
        }
        other => panic!("expected Named Dictionary/Enumerator, got {other:?}"),
    }
}

#[test]
fn nested_generic_encloser_records_per_segment_arity_same_asm() {
    // A base typed as the same-assembly nested generic `Outer2<int>.Inner2<string>`:
    // each segment introduces one type parameter, so the per-segment delta arity
    // is `[1, 1]` (confirming the backtick number is the per-segment delta, not a
    // cumulative total — a cumulative encoding would be `[1, 2]`).
    let entities = load();
    match &entity(&entities, "SameAsmNestedGenericDerived").base_type {
        Some(TypeRef::Named {
            assembly,
            namespace,
            name,
            type_args,
            segment_arities,
        }) => {
            assert!(assembly.is_none(), "expected same-assembly ref");
            assert_eq!(
                namespace,
                &vec!["MemberShapes".to_string(), "Shapes".to_string()]
            );
            assert_eq!(name, "Outer2/Inner2");
            assert_eq!(segment_arities, &vec![1, 1]);
            assert_eq!(
                type_args.len(),
                2,
                "Outer2<int>.Inner2<string> carries two args"
            );
        }
        other => panic!("expected Named Outer2/Inner2, got {other:?}"),
    }
}
