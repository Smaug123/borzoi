//! Reference-type nullability decoding: a tighter per-position
//! `NullableAttribute` beats the enclosing `#nullable enable` context default,
//! and the pre-order DFS byte-walk assigns `Nullability` left-to-right over
//! composite types (`List<string?>`, `Dictionary<string, string?>`, nested
//! generics, and arrays in both `string?[]` and `string[]?` forms). Asserted
//! against the real `MemberShapes.dll` read through the public byte entry point
//! `Ecma335Assembly::parse`.
//!
//! MiniLib's `minilib_nullable_index_parameter_projects_from_getter` pins a
//! single reference position; this file is the byte-walk complement it never
//! reaches. These are observable-outcome pins — whether a not-null context
//! lands on the method or the type is Roslyn's encoding choice, so the
//! `*_inherits_*` cases assert the resulting `Nullability`, not the rung it
//! resolved through.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, Field, Member, MethodLike, Nullability, NullableType,
    Primitive, Property, TypeRef,
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

fn property<'a>(e: &'a Entity, name: &str) -> &'a Property {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == name => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("property {name:?} not found on {:?}", e.name))
}

fn host(entities: &[Entity]) -> &Entity {
    entity(entities, "ReferenceNullability")
}

fn param0<'a>(e: &'a Entity, method_name: &str) -> &'a borzoi_assembly::Parameter {
    &method(e, method_name).signature.parameters[0]
}

#[test]
fn field_inherits_type_nullable_context() {
    // A not-null `string` field with no direct `NullableAttribute` reads
    // NotAnnotated via the not-null context. This also proves the context is
    // live, so every other assertion here is "despite a not-null default".
    let entities = load();
    let f = field(host(&entities), "Anchor");
    assert_eq!(f.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(f.nullability, Nullability::NotAnnotated);
}

#[test]
fn property_inherits_type_nullable_context() {
    // Property-position equivalent: a not-null `string` property with no direct
    // `NullableAttribute` inherits the not-null context → NotAnnotated.
    let entities = load();
    let p = property(host(&entities), "Name");
    assert_eq!(p.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(p.nullability, Nullability::NotAnnotated);
}

#[test]
fn parameter_direct_nullable_attribute_wins() {
    // `TakeNullableString(string a, string b, string? s)`: the not-null siblings
    // make NotAnnotated the method's majority, so `s`'s Annotated cannot come
    // from the enclosing context (which the NotAnnotated `a` proves resolves to
    // NotAnnotated here) — it must come from a direct `NullableAttribute(2)` on
    // `s` that wins over that context. Asserting both in the same method pins
    // the precedence without depending on whether Roslyn put the context on the
    // method or the type.
    let entities = load();
    let m = method(host(&entities), "TakeNullableString");
    let a = &m.signature.parameters[0];
    assert_eq!(a.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(
        a.nullability,
        Nullability::NotAnnotated,
        "sibling proves the enclosing context resolves to NotAnnotated"
    );
    let s = &m.signature.parameters[2];
    assert_eq!(s.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(
        s.nullability,
        Nullability::Annotated,
        "direct NullableAttribute(2) on s beats the NotAnnotated context"
    );
}

#[test]
fn parameter_inherits_nullable_context() {
    // A not-null `string` parameter with no direct attribute inherits the
    // not-null context → NotAnnotated.
    let entities = load();
    let p = param0(host(&entities), "TakeString");
    assert_eq!(p.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(p.nullability, Nullability::NotAnnotated);
}

#[test]
fn list_of_annotated_string_parameter_decodes_inner_annotated() {
    // `List<string?>`: pre-order DFS consumes byte 1 for the outer `List<...>`
    // (NotAnnotated) and byte 2 for the inner `string` (Annotated).
    let entities = load();
    let p = param0(host(&entities), "TakeListOfNullable");
    assert_eq!(
        p.nullability,
        Nullability::NotAnnotated,
        "outer List<...> reads byte 1 (NotAnnotated)"
    );
    match &p.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "List");
            assert_eq!(
                type_args.as_slice(),
                [NullableType {
                    ty: TypeRef::Primitive(Primitive::String),
                    nullability: Nullability::Annotated,
                }],
                "inner string? reads byte 2 (Annotated)"
            );
        }
        other => panic!("expected Named List, got {other:?}"),
    }
}

#[test]
fn dictionary_mixed_inner_nullability_left_to_right_walk_order() {
    // `Dictionary<string, string?>`: bytes consumed in declaration order —
    // outer Dictionary (NotAnnotated), then K (string, NotAnnotated), then V
    // (string?, Annotated).
    let entities = load();
    let f = field(host(&entities), "Map");
    assert_eq!(f.nullability, Nullability::NotAnnotated);
    match &f.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "Dictionary");
            assert_eq!(
                type_args.as_slice(),
                [
                    NullableType {
                        ty: TypeRef::Primitive(Primitive::String),
                        nullability: Nullability::NotAnnotated,
                    },
                    NullableType {
                        ty: TypeRef::Primitive(Primitive::String),
                        nullability: Nullability::Annotated,
                    },
                ],
            );
        }
        other => panic!("expected Named Dictionary, got {other:?}"),
    }
}

#[test]
fn nested_generic_list_of_list_of_annotated_string() {
    // `List<List<string?>>`: outer List (1), inner List (1), innermost string?
    // (2) — three annotable visits in pre-order DFS.
    let entities = load();
    let p = param0(host(&entities), "TakeNestedList");
    assert_eq!(p.nullability, Nullability::NotAnnotated);
    let outer_args = match &p.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "List");
            type_args
        }
        other => panic!("expected outer Named List, got {other:?}"),
    };
    assert_eq!(outer_args[0].nullability, Nullability::NotAnnotated);
    match &outer_args[0].ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "List");
            assert_eq!(
                type_args.as_slice(),
                [NullableType {
                    ty: TypeRef::Primitive(Primitive::String),
                    nullability: Nullability::Annotated,
                }],
            );
        }
        other => panic!("expected inner Named List, got {other:?}"),
    }
}

#[test]
fn annotated_array_element_decodes() {
    // `string?[]`: outer array reads byte 1 (NotAnnotated) wrapping the inner
    // string at byte 2 (Annotated).
    let entities = load();
    let p = param0(host(&entities), "TakeNullableElemArray");
    assert_eq!(p.nullability, Nullability::NotAnnotated);
    match &p.ty {
        TypeRef::Array { element, rank, .. } => {
            assert_eq!(*rank, 1);
            assert_eq!(
                **element,
                NullableType {
                    ty: TypeRef::Primitive(Primitive::String),
                    nullability: Nullability::Annotated,
                }
            );
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn array_outer_annotated_decodes() {
    // `string[]?`: outer array Annotated (byte 2), inner string NotAnnotated
    // (byte 1) — mirror of `annotated_array_element_decodes` with the
    // outer/inner annotations swapped.
    let entities = load();
    let p = param0(host(&entities), "TakeNullableArray");
    assert_eq!(p.nullability, Nullability::Annotated);
    match &p.ty {
        TypeRef::Array { element, rank, .. } => {
            assert_eq!(*rank, 1);
            assert_eq!(element.nullability, Nullability::NotAnnotated);
            assert_eq!(element.ty, TypeRef::Primitive(Primitive::String));
        }
        other => panic!("expected Array, got {other:?}"),
    }
}
