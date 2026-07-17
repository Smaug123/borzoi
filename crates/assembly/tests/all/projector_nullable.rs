//! Value-type nullability gating: value-type positions stay `Oblivious` even
//! when an enclosing `#nullable enable` scope defaults reference positions to
//! `NotAnnotated`, asserted against the real `MemberShapes.dll` read through
//! the public byte entry point `Ecma335Assembly::parse`.
//!
//! MiniLib's `minilib_nullable_index_parameter_projects_from_getter` already
//! pins reference-type outer/inner annotation against a real DLL; this file is
//! the value-type complement it never reaches (primitives, named structs,
//! `System.Nullable<T>`, and value-typed generic arguments).
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, Field, Member, MethodLike, Nullability, NullableType,
    Primitive, TypeRef,
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

fn field<'a>(e: &'a Entity, name: &str) -> &'a Field {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Field(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("field {name:?} not found on {:?}", e.name))
}

fn host(entities: &[Entity]) -> &Entity {
    entity(entities, "ValueTypeNullability")
}

#[test]
fn reference_anchor_is_not_annotated() {
    // Proves the type-level `NullableContextAttribute(1)` is live: a plain
    // `string` field under `#nullable enable` reads NotAnnotated. Every
    // Oblivious assertion below is therefore "despite a not-null context",
    // not "in the absence of one".
    let entities = load();
    let f = field(host(&entities), "Anchor");
    assert_eq!(f.nullability, Nullability::NotAnnotated);
    assert_eq!(f.ty, TypeRef::Primitive(Primitive::String));
}

#[test]
fn value_type_parameter_stays_oblivious_under_notnull_context() {
    // A primitive `int` parameter must stay Oblivious despite the type-level
    // not-null context — value types cannot carry reference nullability.
    let entities = load();
    let p = &method(host(&entities), "TakeInt").signature.parameters[0];
    assert_eq!(p.ty, TypeRef::Primitive(Primitive::I4));
    assert_eq!(p.nullability, Nullability::Oblivious);
}

#[test]
fn named_value_type_parameter_stays_oblivious_under_notnull_context() {
    // A named value type (`DateTime`) hits the same gate: the erased
    // `TypeRef::Named` carries no value-vs-class bit, so the projector reads
    // the value-kind off the source signature blob to keep it Oblivious.
    let entities = load();
    let p = &method(host(&entities), "TakeWhen").signature.parameters[0];
    match &p.ty {
        TypeRef::Named { name, .. } => assert_eq!(name, "DateTime"),
        other => panic!("expected Named DateTime, got {other:?}"),
    }
    assert_eq!(p.nullability, Nullability::Oblivious);
}

#[test]
fn named_value_type_field_stays_oblivious_under_notnull_context() {
    // Field-position version of the named-value-type gate.
    let entities = load();
    let f = field(host(&entities), "When");
    match &f.ty {
        TypeRef::Named { name, .. } => assert_eq!(name, "DateTime"),
        other => panic!("expected Named DateTime, got {other:?}"),
    }
    assert_eq!(f.nullability, Nullability::Oblivious);
}

#[test]
fn list_of_value_type_outer_annotated_inner_oblivious() {
    // `List<int>`: the outer `List` reference reads NotAnnotated under the
    // not-null context, but the non-annotable inner `int` consumes no byte
    // and ends up Oblivious.
    let entities = load();
    let f = field(host(&entities), "Ints");
    assert_eq!(f.nullability, Nullability::NotAnnotated);
    match &f.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "List");
            assert_eq!(
                type_args.as_slice(),
                [NullableType::oblivious(TypeRef::Primitive(Primitive::I4))],
            );
        }
        other => panic!("expected Named List, got {other:?}"),
    }
}

#[test]
fn system_nullable_does_not_consume_byte() {
    // `int?` = `System.Nullable<int>`: a non-annotable value type. The outer
    // reads Oblivious (no byte consumed), and `System.Nullable` is special-
    // cased so its inner `int` is left default-Oblivious rather than being
    // walked.
    let entities = load();
    let p = &method(host(&entities), "TakeMaybeInt").signature.parameters[0];
    assert_eq!(p.nullability, Nullability::Oblivious);
    match &p.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "Nullable");
            assert_eq!(
                type_args.as_slice(),
                [NullableType::oblivious(TypeRef::Primitive(Primitive::I4))],
            );
        }
        other => panic!("expected Named Nullable, got {other:?}"),
    }
}

#[test]
fn generic_value_type_outer_byte_is_discarded() {
    // `KeyValuePair<string?, int>`: the outer generic *value* type is forced
    // to Oblivious, then the argument walk continues — inner `string?` is
    // Annotated, inner `int` is Oblivious.
    let entities = load();
    let p = &method(host(&entities), "TakeKvp").signature.parameters[0];
    assert_eq!(p.nullability, Nullability::Oblivious);
    match &p.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "KeyValuePair");
            assert_eq!(
                type_args[0],
                NullableType {
                    ty: TypeRef::Primitive(Primitive::String),
                    nullability: Nullability::Annotated,
                },
            );
            assert_eq!(
                type_args[1],
                NullableType::oblivious(TypeRef::Primitive(Primitive::I4)),
            );
        }
        other => panic!("expected Named KeyValuePair, got {other:?}"),
    }
}
