//! Happy-path method/ctor/parameter projection, asserted against a real
//! csc-compiled DLL (`MemberShapes.dll`, built by
//! `common::ensure_member_shapes_built`) read through the public byte entry
//! point `Ecma335Assembly::parse`. Driving the projector from real PE bytes
//! validates the owned `Entity` output and keeps the reader behind
//! `Ecma335Assembly::parse` swappable.
//!
//! A real class carries an implicit/declared ctor set alongside its declared
//! members, so these tests look members up by name (or by signature shape for
//! the overloaded ctors) rather than by position.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Access, ConstantValue, Ecma335Assembly, EcmaView, Entity, Field, Member, MethodLike,
    ParamDefault, Primitive, TypeRef,
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

fn methods(e: &Entity) -> Vec<&MethodLike> {
    e.members
        .iter()
        .filter_map(|m| match m {
            Member::Method(m) => Some(m),
            _ => None,
        })
        .collect()
}

fn method<'a>(e: &'a Entity, name: &str) -> &'a MethodLike {
    methods(e)
        .into_iter()
        .find(|m| m.name == name)
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

#[test]
fn const_field_is_literal_but_static_readonly_is_not() {
    let entities = load();
    let counter = entity(&entities, "Counter");

    // A C# `const` is a compile-time constant: the CLR `Literal` flag, not
    // `initonly`.
    let limit = field(counter, "Limit");
    assert!(limit.is_literal, "a C# `const` must set is_literal");
    assert!(
        !limit.is_init_only,
        "a literal uses the `Literal` flag, not `initonly`"
    );

    // `static readonly` is `initonly`, not a literal.
    let shared = field(counter, "Shared");
    assert!(!shared.is_literal, "`static readonly` is not a literal");
    assert!(shared.is_init_only);
}

#[test]
fn projects_parameterless_default_constructor() {
    let entities = load();
    let counter = entity(&entities, "Counter");
    let ctor = methods(counter)
        .into_iter()
        .find(|m| m.is_constructor && m.signature.parameters.is_empty())
        .expect("parameterless ctor");

    assert_eq!(ctor.name, ".ctor");
    assert_eq!(ctor.access, Access::Public);
    assert!(ctor.is_constructor);
    assert!(!ctor.is_static);
    assert!(!ctor.is_virtual);
    assert!(!ctor.is_abstract);
    assert!(ctor.signature.parameters.is_empty());
    assert_eq!(
        ctor.signature.return_type,
        TypeRef::Primitive(Primitive::Void)
    );
}

#[test]
fn projects_constructor_with_parameter_name_from_metadata() {
    let entities = load();
    let counter = entity(&entities, "Counter");
    let ctor = methods(counter)
        .into_iter()
        .find(|m| m.is_constructor && m.signature.parameters.len() == 1)
        .expect("ctor(int start)");

    assert!(ctor.is_constructor);
    let p = &ctor.signature.parameters[0];
    assert_eq!(p.name.as_deref(), Some("start"));
    assert_eq!(p.ty, TypeRef::Primitive(Primitive::I4));
    assert!(!p.is_byref);
    assert!(!p.is_out);
}

#[test]
fn projects_static_method() {
    let entities = load();
    let m = method(entity(&entities, "Counter"), "Zero");
    assert!(m.is_static);
    assert!(!m.is_virtual);
    assert!(!m.is_constructor);
    assert_eq!(m.name, "Zero");
}

#[test]
fn projects_virtual_instance_method() {
    let entities = load();
    let m = method(entity(&entities, "Counter"), "Combine");
    assert!(!m.is_static);
    assert!(m.is_virtual);
    assert!(!m.is_abstract);
    assert_eq!(m.signature.return_type, TypeRef::Primitive(Primitive::I4));
    assert_eq!(m.signature.parameters.len(), 1);
}

#[test]
fn projects_abstract_method() {
    let entities = load();
    let m = method(entity(&entities, "Stepper"), "Step");
    assert!(m.is_virtual);
    assert!(m.is_abstract);
}

#[test]
fn projects_byref_out_parameter() {
    let entities = load();
    let m = method(entity(&entities, "Counter"), "TryGet");
    assert_eq!(m.signature.parameters.len(), 1);
    let p = &m.signature.parameters[0];
    assert_eq!(p.name.as_deref(), Some("value"));
    assert!(p.is_byref);
    assert!(p.is_out);
    assert_eq!(p.ty, TypeRef::Primitive(Primitive::I4));
}

#[test]
fn in_out_parameter_renders_as_byref_not_out() {
    let entities = load();
    let m = method(entity(&entities, "Interop"), "Exchange");
    let p = &m.signature.parameters[0];
    assert!(p.is_byref);
    assert!(!p.is_out, "[In, Out] must not project as out");
}

#[test]
fn optional_parameter_without_constant_is_optional() {
    let entities = load();
    let m = method(entity(&entities, "Counter"), "Take");
    let p = &m.signature.parameters[0];
    // `[Optional]` alone (no `Constant`, no `[<OptionalArgument>]`) is a .NET
    // optional with no value.
    assert_eq!(
        p.default,
        ParamDefault::Optional(None),
        "Optional flag without Constant must project as a value-less .NET optional"
    );
}

#[test]
fn dotnet_default_value_parameter_carries_value() {
    // A C# default value (`int n = 5`) sets `Optional` + a `Constant` row; the
    // decoded value is carried.
    let entities = load();
    let m = method(entity(&entities, "Counter"), "TakeDefault");
    let p = &m.signature.parameters[0];
    assert_eq!(
        p.default,
        ParamDefault::Optional(Some(ConstantValue::Int(5)))
    );
}

#[test]
fn decimal_default_decodes_from_decimal_constant_attribute() {
    // `decimal d = 1.5m` has no `Constant` row; the value rides on
    // `[DecimalConstantAttribute(1, 0, 0, 0, 15)]`. 1.5 = mantissa 15 at scale 1,
    // positive. This pin also confirms Roslyn emits the `uint`-word ctor (the
    // reader needs `IntegralParam::UInt32` to decode it).
    let entities = load();
    let m = method(entity(&entities, "Counter"), "TakeDecimalDefault");
    let p = &m.signature.parameters[0];
    assert_eq!(
        p.default,
        ParamDefault::Optional(Some(ConstantValue::Decimal {
            negative: false,
            scale: 1,
            mantissa: 15,
        }))
    );
}

#[test]
fn negative_decimal_default_decodes_with_sign() {
    // `decimal d = -2.5m`: mantissa 25 at scale 1, negative (sign byte non-zero).
    let entities = load();
    let m = method(entity(&entities, "Counter"), "TakeNegativeDecimalDefault");
    let p = &m.signature.parameters[0];
    assert_eq!(
        p.default,
        ParamDefault::Optional(Some(ConstantValue::Decimal {
            negative: true,
            scale: 1,
            mantissa: 25,
        }))
    );
}

#[test]
fn decimal_constant_without_optional_stays_required() {
    // `[DecimalConstant]` without the `Optional` flag is *not* a default — the
    // value-carrying attribute alone must not make a required parameter
    // omittable (only Roslyn's real `= <value>` defaults set `Optional`).
    let entities = load();
    let m = method(entity(&entities, "Counter"), "TakeDecimalAttrNotOptional");
    let p = &m.signature.parameters[0];
    assert_eq!(p.default, ParamDefault::None);
}

#[test]
fn datetime_default_decodes_from_datetime_constant_attribute() {
    // `[Optional, DateTimeConstant(630822816000000000L)] DateTime t` carries its
    // value as a tick count (the reader needs `IntegralParam::Int64`).
    let entities = load();
    let m = method(entity(&entities, "Counter"), "TakeDateTimeDefault");
    let p = &m.signature.parameters[0];
    assert_eq!(
        p.default,
        ParamDefault::Optional(Some(ConstantValue::DateTime(630822816000000000)))
    );
}
