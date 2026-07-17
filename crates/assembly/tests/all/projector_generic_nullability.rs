//! Generic-parameter reference nullability decoding against the real
//! `MemberShapes.dll`, read through the public byte entry point
//! `Ecma335Assembly::parse`. Covers the typar-nullability shapes a C# compiler
//! emits as a direct `[NullableAttribute(byte)]` on the GenericParam row —
//! `where T : notnull` (byte 1), `where T : class?` (byte 2),
//! `where T : class` (byte 1), and the unconstrained typar that defaults to
//! byte 2 under `#nullable enable` — plus the no-attribute (oblivious) shape
//! under `#nullable disable`.
//!
//! Each fixture method makes its typar the *minority* nullable byte (five
//! opposite-polarity `string` siblings condense the method-level
//! `NullableContextAttribute` to the other value), so Roslyn stamps the typar
//! with a direct `NullableAttribute`. Each test pins that direct byte against a
//! sibling parameter reading the *opposite* byte: the typar disagreeing with
//! its siblings is only possible via the direct attribute — a context fallback
//! would make it match them. That makes these end-to-end pins exercise the
//! direct-attribute decode path, not just the context fallback.
//!
//! The mechanism-isolation cases are not covered here: a real-DLL *value* pin
//! can't distinguish a direct `NullableAttribute`
//! from an inherited `NullableContextAttribute` (both project to the same
//! `Nullability`), so isolating the fallback / override / shadow decode paths —
//! and the malformed byte[]/invalid-byte/duplicate-context forms — would need a
//! fabricated single-attribute input.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, Member, MethodLike, Nullability, NullableType, Primitive,
    TypeParameter, TypeRef,
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

/// The single `Pick<T>` method each fixture class carries.
fn pick<'a>(entities: &'a [Entity], type_name: &str) -> &'a MethodLike {
    entity(entities, type_name)
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == "Pick" => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| panic!("method `Pick` not found on {type_name:?}"))
}

/// The unique type parameter of a single-typar generic method.
fn typar(m: &MethodLike) -> &TypeParameter {
    assert_eq!(
        m.generic_parameters.len(),
        1,
        "expected exactly one typar on {:?}",
        m.name,
    );
    &m.generic_parameters[0]
}

/// The nullability the `string` siblings condensed the method context to. They
/// all share one byte, so the first non-typar parameter reports it. Asserting
/// the typar disagrees with this proves the typar's byte came from a direct
/// `NullableAttribute`, not the context.
fn sibling_context(m: &MethodLike) -> Nullability {
    m.signature.parameters[1].nullability
}

#[test]
fn notnull_constraint_typar_is_not_annotated() {
    // `where T : notnull` → direct `[Nullable(1)]` on the typar → NotAnnotated,
    // against an Annotated sibling context. `notnull` implies neither the
    // reference- nor value-type special constraint.
    let entities = load();
    let m = pick(&entities, "NotNullTypar");
    assert_eq!(sibling_context(m), Nullability::Annotated);
    let tp = typar(m);
    assert_eq!(tp.nullability, Nullability::NotAnnotated);
    assert!(!tp.reference_type_constraint);
    assert!(!tp.value_type_constraint);
}

#[test]
fn class_question_constraint_typar_is_annotated() {
    // `where T : class?` → direct `[Nullable(2)]` on the typar → Annotated,
    // against a NotAnnotated sibling context, reference-type constraint set.
    let entities = load();
    let m = pick(&entities, "ClassQuestionTypar");
    assert_eq!(sibling_context(m), Nullability::NotAnnotated);
    let tp = typar(m);
    assert_eq!(tp.nullability, Nullability::Annotated);
    assert!(tp.reference_type_constraint);
}

#[test]
fn class_constraint_typar_is_not_annotated() {
    // `where T : class` → direct `[Nullable(1)]` on the typar → NotAnnotated
    // (the non-`?` form is the not-null reference constraint), against an
    // Annotated sibling context, reference-type constraint set.
    let entities = load();
    let m = pick(&entities, "ClassTypar");
    assert_eq!(sibling_context(m), Nullability::Annotated);
    let tp = typar(m);
    assert_eq!(tp.nullability, Nullability::NotAnnotated);
    assert!(tp.reference_type_constraint);
}

#[test]
fn unconstrained_typar_under_nullable_enable_is_annotated() {
    // An unconstrained, reference-capable typar in a `#nullable enable` scope
    // gets a direct `[Nullable(2)]` → Annotated (it may be substituted with a
    // nullable reference type), against a NotAnnotated sibling context.
    let entities = load();
    let m = pick(&entities, "UnconstrainedTypar");
    assert_eq!(sibling_context(m), Nullability::NotAnnotated);
    let tp = typar(m);
    assert_eq!(tp.nullability, Nullability::Annotated);
    assert!(!tp.reference_type_constraint);
    assert!(!tp.value_type_constraint);
}

#[test]
fn unconstrained_typar_under_nullable_disable_is_oblivious() {
    // Outside any nullable scope the typar carries no `NullableAttribute` and
    // there is no context to inherit, so it reads Oblivious — the BCL / pre-C#8
    // shape that surfaces no nullable token downstream.
    let entities = load();
    let tp = typar(pick(&entities, "ObliviousTypar"));
    assert_eq!(tp.nullability, Nullability::Oblivious);
}

// ── Nullability *inside* a type constraint ──────────────────────────────────
//
// The `[Nullable]` Roslyn hangs off the GenericParamConstraint row. The
// constraint's own outer nullability has no slot in `TypeParameter` (a
// constraint is not a value position), but the annotations *inside* it are what
// `TypeRef`'s `NullableType` args model, and they must survive: without them
// `where T : IEquatable<string?>` and `where T : IEquatable<string>` project
// identically.
//
// These two pin both rungs of the ladder against real Roslyn output — the
// direct attribute on the constraint row, and the enclosing `[NullableContext]`
// the row is *omitted* in favour of. FCS cannot see either (its
// `ILGenericParameterDef.Constraints` is bare `ILTypes`, no custom attributes),
// so this is the only place the decode is checked.

/// The single `NullableType` argument of a single-arg generic constraint on
/// `ConstraintNullability::<method>`.
fn sole_constraint_arg(entities: &[Entity], method: &str) -> NullableType {
    let m = entity(entities, "ConstraintNullability")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == method => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| panic!("method {method:?} not found on ConstraintNullability"));
    match typar(m).type_constraints.as_slice() {
        [
            TypeRef::Named {
                name, type_args, ..
            },
        ] if name == "IEquatable" && type_args.len() == 1 => type_args[0].clone(),
        other => panic!("expected a single `IEquatable<_>` constraint, got {other:?}"),
    }
}

#[test]
fn constraint_generic_argument_nullability_survives() {
    // `where T : IEquatable<string?>`. The constraint row carries a direct
    // `[Nullable]` whose payload covers both nodes (the interface and its
    // argument); the argument's `Annotated` is representable and kept.
    let entities = load();
    let arg = sole_constraint_arg(&entities, "PickAnnotated");
    assert_eq!(arg.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(arg.nullability, Nullability::Annotated);
}

#[test]
fn constraint_generic_argument_inherits_the_scope_context() {
    // `where T : IEquatable<string>` in the same `#nullable enable` scope. Roslyn
    // emits *no* `[Nullable]` on this constraint row — the annotation equals the
    // enclosing `[NullableContext]`, which is exactly the compression every other
    // position is subject to. So the constraint must resolve through the context
    // rung: reading it bare would claim `Oblivious` (no annotation at all) for a
    // constraint the source annotated.
    let entities = load();
    let arg = sole_constraint_arg(&entities, "PickNotNull");
    assert_eq!(arg.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(arg.nullability, Nullability::NotAnnotated);
}
