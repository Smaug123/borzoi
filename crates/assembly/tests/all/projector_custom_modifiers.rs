//! Value pins for the two custom modifiers a real compiler emits, over the
//! MiniLib fixture (`ModifierHost`, `IModifierSink`/`ModifierSink`,
//! `ReadonlyRefFieldHost`, `ReadonlyRefAccessorHost`, `VolatileHost`).
//!
//! The differential diff (`assembly_diff::diff_assembly_minilib_one_class`)
//! already pins that fcs-dump and the projector agree on these members; what it
//! compares is the *normalised string*. These tests pin the **model** — that the
//! bits land on `TypeRef::ByRef { readonly }`, `Parameter::is_readonly_ref` and
//! `Field::is_volatile` rather than being flattened away — and, crucially, that
//! read-only-ness reads the same whichever of its two metadata encodings Roslyn
//! chose (a `modreq(InAttribute)` in the signature, or an `[IsReadOnly]` /
//! `[RequiresLocation]` attribute on the position; see
//! `has_readonly_ref_attribute`). A `readonly` that were true only for virtual
//! members would be worse than none.

use crate::common;

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member, Primitive, TypeRef};

fn minilib() -> Vec<Entity> {
    let bytes = std::fs::read(common::ensure_minilib_built()).expect("MiniLib.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse MiniLib");
    view.enumerate_type_defs().expect("enumerate MiniLib")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("MiniLib must expose the `{name}` fixture"))
}

fn method<'a>(e: &'a Entity, name: &str) -> &'a borzoi_assembly::MethodLike {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm) if mm.name == name => Some(mm),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{}` must expose method `{name}`", e.name))
}

fn field<'a>(e: &'a Entity, name: &str) -> &'a borzoi_assembly::Field {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Field(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{}` must expose field `{name}`", e.name))
}

fn property<'a>(e: &'a Entity, name: &str) -> &'a borzoi_assembly::Property {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == name => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{}` must expose property `{name}`", e.name))
}

/// A read-only byref over `T`.
fn in_ref(inner: TypeRef) -> TypeRef {
    TypeRef::ByRef {
        inner: Box::new(inner),
        readonly: true,
    }
}

/// The nub: an `in` parameter reads as read-only whether Roslyn put the fact in
/// the *signature* (`modreq(InAttribute)` — emitted only where an override must
/// match, i.e. the interface/virtual `Accept` below) or in an *attribute*
/// (`[IsReadOnly]` / `[RequiresLocation]` — emitted for an ordinary method's
/// `in` / `ref readonly`, as on `ModifierHost`). Both are `in int` in source;
/// both must be `in int` in the model.
#[test]
fn in_parameters_are_readonly_under_either_encoding() {
    let entities = minilib();

    // Attribute-encoded (`ModifierHost` is a plain static class): the signature
    // carries a *bare* byref, and `[IsReadOnly]` / `[RequiresLocation]` sit on
    // the parameter row.
    let host = entity(&entities, "ModifierHost");
    for (name, arity) in [("Sum", 2usize), ("Peek", 1)] {
        let m = method(host, name);
        assert_eq!(m.signature.parameters.len(), arity);
        for p in &m.signature.parameters {
            assert!(p.is_byref, "`{name}`: an `in` parameter is a byref");
            assert!(
                p.is_readonly_ref,
                "`{name}`: an `in` parameter is read-only even though its signature \
                 carries no `modreq` (the fact rides `[IsReadOnly]`/`[RequiresLocation]`)"
            );
            assert!(!p.is_out, "`{name}`: `in` is not `out`");
        }
    }

    // Modifier-encoded (an interface member and the virtual method implementing
    // it): here Roslyn *does* put `modreq(InAttribute)` in the signature, because
    // the implementation has to match the declaration.
    for owner in ["IModifierSink", "ModifierSink"] {
        let m = method(entity(&entities, owner), "Accept");
        let p = m.signature.parameters.first().expect("one parameter");
        assert!(
            p.is_byref && p.is_readonly_ref,
            "`{owner}.Accept`: `in int`"
        );
        assert_eq!(p.ty, TypeRef::Primitive(Primitive::I4));
    }
}

/// `ref`, `in` and `out` are three distinct parameter shapes and must stay
/// distinguishable: `Mixed(in int, ref int, out int)`.
#[test]
fn ref_in_and_out_parameters_stay_distinct() {
    let entities = minilib();
    let m = method(entity(&entities, "ModifierHost"), "Mixed");
    let flags: Vec<(bool, bool, bool)> = m
        .signature
        .parameters
        .iter()
        .map(|p| (p.is_byref, p.is_readonly_ref, p.is_out))
        .collect();
    assert_eq!(
        flags,
        vec![
            (true, true, false),  // in int
            (true, false, false), // ref int
            (true, false, true),  // out int
        ]
    );
}

/// A `ref readonly` *return* — the position where the `modreq(InAttribute)` is
/// mandatory — keeps the byref in the type, marked read-only.
#[test]
fn readonly_ref_returns_and_properties_carry_the_bit() {
    let entities = minilib();

    let pick = method(entity(&entities, "ModifierHost"), "Pick");
    assert_eq!(
        pick.signature.return_type,
        in_ref(TypeRef::Primitive(Primitive::I4)),
        "`ref readonly int Pick(int)` returns a read-only byref"
    );

    let accessors = entity(&entities, "ReadonlyRefAccessorHost");
    // A `ref readonly` property, and a `ref readonly` indexer (whose own index
    // dimension is an ordinary by-value `int`).
    assert_eq!(
        property(accessors, "First").ty,
        in_ref(TypeRef::Primitive(Primitive::I4))
    );
    let indexer = property(accessors, "Item");
    assert_eq!(indexer.ty, in_ref(TypeRef::Primitive(Primitive::I4)));
    assert_eq!(indexer.parameters.len(), 1);
    assert_eq!(
        indexer.parameters[0].ty.ty,
        TypeRef::Primitive(Primitive::I4)
    );

    // A byref *interface* property, mirrored by its implementation.
    for owner in ["IModifierSink", "ModifierSink"] {
        assert_eq!(
            property(entity(&entities, owner), "Latest").ty,
            in_ref(TypeRef::Primitive(Primitive::I4))
        );
    }
}

/// A `ref readonly` **field** carries no modifier at all — its signature is a
/// plain byref and the read-only-ness is the `[IsReadOnly]` attribute. It must
/// still project as a read-only byref, and a plain `ref` field must not.
#[test]
fn readonly_ref_fields_carry_the_bit_and_plain_ref_fields_do_not() {
    let entities = minilib();

    let ro = entity(&entities, "ReadonlyRefFieldHost");
    assert_eq!(
        field(ro, "Slot").ty,
        in_ref(TypeRef::Primitive(Primitive::I4))
    );
    assert_eq!(
        field(ro, "Name").ty,
        in_ref(TypeRef::Primitive(Primitive::String))
    );

    // The pre-existing writable-`ref`-field fixture stays writable — the pin that
    // the new bit is read *from metadata* and not defaulted on.
    let rw = entity(&entities, "RefFieldHost");
    assert_eq!(
        field(rw, "Slot").ty,
        TypeRef::ByRef {
            inner: Box::new(TypeRef::Primitive(Primitive::I4)),
            readonly: false,
        }
    );
}

/// `volatile` is *only* a `modreq(IsVolatile)` on the field type — no flag bit
/// exists — so dropping the modifier would silently mismodel the field's memory
/// semantics. It projects to `Field::is_volatile`, and the field's own type is
/// the modifier's operand, unchanged.
#[test]
fn volatile_fields_project_the_marker_as_a_flag() {
    let entities = minilib();
    let host = entity(&entities, "VolatileHost");

    let counter = field(host, "Counter");
    assert!(counter.is_volatile);
    assert_eq!(counter.ty, TypeRef::Primitive(Primitive::I4));
    assert!(!counter.is_static);

    // A reference-typed volatile field: the modifier consumes no `[Nullable]`
    // byte, so the annotation still lands on the referent (`string?`).
    let label = field(host, "Label");
    assert!(label.is_volatile);
    assert_eq!(label.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(label.nullability, borzoi_assembly::Nullability::Annotated);

    let ready = field(host, "Ready");
    assert!(ready.is_volatile && ready.is_static);
    assert_eq!(ready.ty, TypeRef::Primitive(Primitive::Bool));

    // An ordinary field is not volatile — again, the bit is read, not defaulted.
    assert!(!field(entity(&entities, "Counter"), "ProtectedField").is_volatile);
}
