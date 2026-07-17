//! Generic-parameter projection: method typars and their special/type
//! constraints, byref returns, and type-level typars with variance, asserted
//! against the real `MemberShapes.dll` read through the public byte entry point
//! `Ecma335Assembly::parse`.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, Member, MethodLike, Primitive, TypeRef, Variance,
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

#[test]
fn projects_byref_return_type() {
    let entities = load();
    let m = method(entity(&entities, "GenericMethods"), "Slot");
    assert_eq!(
        m.signature.return_type,
        TypeRef::ByRef {
            inner: Box::new(TypeRef::Primitive(Primitive::I4)),
            readonly: false
        }
    );
}

#[test]
fn projects_generic_method_with_method_typar() {
    let entities = load();
    let m = method(entity(&entities, "GenericMethods"), "Echo");

    assert_eq!(m.generic_parameters.len(), 1);
    assert_eq!(m.generic_parameters[0].name, "T");
    assert!(matches!(
        m.generic_parameters[0].variance,
        Variance::Invariant
    ));

    let typar = TypeRef::Var {
        index: 0,
        is_method: true,
    };
    assert_eq!(m.signature.return_type, typar);
    assert_eq!(m.signature.parameters[0].ty, typar);
}

#[test]
fn projects_method_special_constraints() {
    // `where T : class, new()` — both flags set, no value-type flag.
    let entities = load();
    let m = method(entity(&entities, "GenericMethods"), "MakeRef");
    let gp = &m.generic_parameters[0];
    assert!(gp.reference_type_constraint);
    assert!(!gp.value_type_constraint);
    assert!(gp.default_constructor_constraint);
}

#[test]
fn projects_method_type_constraints() {
    // `where T : IComparable` — the constraint type appears in type_constraints.
    let entities = load();
    let m = method(entity(&entities, "GenericMethods"), "PickComparable");
    let gp = &m.generic_parameters[0];
    assert_eq!(gp.type_constraints.len(), 1);
    match &gp.type_constraints[0] {
        TypeRef::Named { name, .. } => assert_eq!(name, "IComparable"),
        other => panic!("expected Named IComparable, got {other:?}"),
    }
}

#[test]
fn projects_struct_value_type_constraint() {
    // `where T : struct` — the value-type flag is set independently of the
    // explicit System.ValueType type constraint C# also emits.
    let entities = load();
    let m = method(entity(&entities, "GenericMethods"), "MakeValue");
    let gp = &m.generic_parameters[0];
    assert!(gp.value_type_constraint);
    assert!(!gp.reference_type_constraint);
}

#[test]
fn projects_type_generic_parameter() {
    // `class Box<T>` — one unconstrained typar; the backtick arity is stripped
    // from the entity name.
    let entities = load();
    let e = entity(&entities, "Box");
    assert_eq!(e.generic_parameters.len(), 1);
    assert_eq!(e.generic_parameters[0].name, "T");
    assert!(matches!(
        e.generic_parameters[0].variance,
        Variance::Invariant
    ));
    assert!(e.generic_parameters[0].type_constraints.is_empty());
}

#[test]
fn projects_type_generic_variance_and_constraint() {
    // `interface IPair<out T, in U> where T : class where U : IComparable, new()`.
    let entities = load();
    let e = entity(&entities, "IPair");
    assert_eq!(e.generic_parameters.len(), 2);

    assert_eq!(e.generic_parameters[0].name, "T");
    assert!(matches!(
        e.generic_parameters[0].variance,
        Variance::Covariant
    ));
    assert!(e.generic_parameters[0].reference_type_constraint);

    assert_eq!(e.generic_parameters[1].name, "U");
    assert!(matches!(
        e.generic_parameters[1].variance,
        Variance::Contravariant
    ));
    assert!(e.generic_parameters[1].default_constructor_constraint);
    assert_eq!(e.generic_parameters[1].type_constraints.len(), 1);
    match &e.generic_parameters[1].type_constraints[0] {
        TypeRef::Named { name, .. } => assert_eq!(name, "IComparable"),
        other => panic!("expected Named IComparable, got {other:?}"),
    }
}

// ── Constraint-row custom attributes (EX-2) ─────────────────────────────────
//
// A `GenericParamConstraint` row carries its own custom attributes, and the
// projector used to refuse *any* of them as "hand-authored metadata we cannot
// represent". That was true when written and **false on .NET 9+**: the BCL
// annotates the constraint rows of the generic-math / parsing interfaces with
// `[Nullable]` (`where TSelf : IParsable<TSelf>` — the constraint *type* carries
// a nullability annotation), so the refusal silently dropped 38 types from
// `System.Runtime` alone. They are now classified — recognised and discarded,
// since the model has no per-constraint nullability slot — and the whole .NET 10
// reference pack projects with **zero** type drops (`bcl_ref_pack_sweep`).
#[test]
fn generic_math_interfaces_with_attributed_constraints_project() {
    let dll = crate::common::sdk_ref_pack_dir().join("System.Runtime.dll");
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let (entities, skips) = view
        .enumerate_type_defs_with_skips()
        .expect("enumerate System.Runtime");
    assert!(
        skips.dropped_types.is_empty(),
        "System.Runtime must project with no dropped types, got: {:?}",
        skips.dropped_types
    );
    for name in ["IParsable", "ISpanParsable"] {
        assert!(
            entities
                .iter()
                .any(|e| e.namespace == ["System".to_string()] && e.name == name),
            "`System.{name}` must project: its `where TSelf : …` constraint row carries a \
             `[Nullable]` attribute, which the projector once refused (dropping the type)"
        );
    }
    for name in ["INumber", "IComparisonOperators"] {
        assert!(
            entities.iter().any(|e| {
                e.namespace == ["System".to_string(), "Numerics".to_string()] && e.name == name
            }),
            "`System.Numerics.{name}` must project (attributed constraint row)"
        );
    }
}
