//! Stage 1 oracle for the structured explicit-interface model
//! (`MethodLike::implements`), populated from the ECMA-335 `MethodImpl`
//! table. See `docs/xmldoc-explicit-interface-plan.md`.
//!
//! Uses the shared `DocIds` C# fixture, which declares explicit interface
//! implementations of both a single-argument generic interface
//! (`IntStore : IStore<int>`), a two-argument one with concrete arguments
//! (`IntStringLookup : ILookup<int, string>`), and a two-argument one whose
//! arguments are the implementing type's own type parameters
//! (`Wrapper<A, B> : ILookup<A, B>`). The last is the case that distinguishes a
//! structured read (type arguments are `TypeRef::Var`) from a string parse.
//!
//! Requires the .NET 10 SDK on PATH (the Nix devShell provides it).

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, ImplementedMember, InterfaceMemberImpl, Member, MethodLike,
    Primitive, TypeRef,
};

use crate::common::ensure_doc_ids_built;

fn entities() -> Vec<Entity> {
    let dll = ensure_doc_ids_built();
    let bytes = std::fs::read(dll).expect("read DocIds.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse DocIds.dll");
    view.enumerate_type_defs().expect("enumerate DocIds types")
}

/// A top-level entity by simple name (the fixture types all live in namespace
/// `DocIds`).
fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no entity `{name}` (have: {:?})", names(entities)))
}

fn names(entities: &[Entity]) -> Vec<&str> {
    entities.iter().map(|e| e.name.as_str()).collect()
}

fn methods(e: &Entity) -> impl Iterator<Item = &MethodLike> {
    e.members.iter().filter_map(|m| match m {
        Member::Method(m) => Some(m),
        _ => None,
    })
}

/// The (single) method on `e` implementing the interface *method* named
/// `member` — explicit-impl methods carry the qualified IL name, so they cannot
/// be found by bare name. This single-assembly Roslyn fixture resolves every
/// declaration through `MethodSemantics`, so the kind is always
/// [`ImplementedMember::Method`] here.
fn explicit_method<'a>(e: &'a Entity, member: &str) -> &'a MethodLike {
    methods(e)
        .find(|m| {
            m.implements
                .iter()
                .any(|ei| ei.member == ImplementedMember::Method(member.to_string()))
        })
        .unwrap_or_else(|| {
            panic!(
                "no explicit-impl method for member `{member}` on `{}`",
                e.name
            )
        })
}

/// The explicit-interface info of the (single) property on `e` implementing
/// the interface *property* named `member`.
fn explicit_property<'a>(e: &'a Entity, member: &str) -> &'a InterfaceMemberImpl {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) => p
                .implements
                .iter()
                .find(|ei| ei.member == ImplementedMember::Property(member.to_string())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no explicit-impl property `{member}` on `{}`", e.name))
}

/// The single explicit-interface entry of `m`, asserting there is exactly one
/// (every explicit impl in this Roslyn-compiled fixture satisfies exactly one
/// interface member; multi-declaration bodies are pinned by the fabricated-IL
/// tests in `methodimpl_classification.rs`).
fn single_explicit(m: &MethodLike) -> &InterfaceMemberImpl {
    assert_eq!(
        m.implements.len(),
        1,
        "expected exactly one explicit-interface entry on `{}`, got {:?}",
        m.name,
        m.implements,
    );
    &m.implements[0]
}

/// A same-assembly `Named` interface type with the given simple name and args,
/// ignoring namespace/assembly detail (the fixture is single-assembly).
fn assert_named(ty: &TypeRef, simple_name: &str, args: &[TypeRef]) {
    match ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, simple_name, "interface simple name");
            let got: Vec<&TypeRef> = type_args.iter().map(|nt| &nt.ty).collect();
            let want: Vec<&TypeRef> = args.iter().collect();
            assert_eq!(got, want, "interface `{simple_name}` type arguments");
        }
        other => panic!("expected Named interface `{simple_name}`, got {other:?}"),
    }
}

fn var(index: u16) -> TypeRef {
    TypeRef::Var {
        index,
        is_method: false,
    }
}

#[test]
fn single_arg_generic_interface_method() {
    let es = entities();
    let m = explicit_method(entity(&es, "IntStore"), "Store");
    let ei = single_explicit(m);
    assert_named(
        &ei.interface,
        "IStore",
        &[TypeRef::Primitive(Primitive::I4)],
    );
}

#[test]
fn single_arg_generic_interface_property() {
    // An explicit interface *property* surfaces as a `Property` (its accessor
    // methods are excluded from the projected method list), so the structured
    // info lands on the property, with `member` the property's own name `Count`
    // (not the accessor `get_Count`).
    let es = entities();
    let ei = explicit_property(entity(&es, "IntStore"), "Count");
    assert_named(
        &ei.interface,
        "IStore",
        &[TypeRef::Primitive(Primitive::I4)],
    );
}

#[test]
fn two_arg_concrete_generic_interface() {
    let es = entities();
    let m = explicit_method(entity(&es, "IntStringLookup"), "Get");
    let ei = single_explicit(m);
    assert_named(
        &ei.interface,
        "ILookup",
        &[
            TypeRef::Primitive(Primitive::I4),
            TypeRef::Primitive(Primitive::String),
        ],
    );
}

#[test]
fn two_arg_type_parameter_generic_interface_uses_vars() {
    // The discriminating case: a structured read renders the interface's
    // arguments as the implementing type's type parameters (`TypeRef::Var`),
    // which a back-parse of the IL name (`…ILookup<A,B>.Get`) could not recover
    // as indices.
    let es = entities();
    let m = explicit_method(entity(&es, "Wrapper"), "Get");
    let ei = single_explicit(m);
    assert_named(&ei.interface, "ILookup", &[var(0), var(1)]);
}

#[test]
fn covariant_return_override_is_not_an_explicit_interface_impl() {
    // A C# covariant-return override (`CloneDerived.Clone` returning the derived
    // type, overriding `CloneBase.Clone`) emits a `MethodImpl` whose declaration
    // parent is the base *class*, not an interface. The CLR does not name-mangle
    // it, so it must not be read as an explicit interface implementation —
    // otherwise the field would falsely report `Some(CloneBase, "Clone")`.
    let es = entities();
    let clone = methods(entity(&es, "CloneDerived"))
        .find(|m| m.name == "Clone")
        .expect("Clone override");
    assert!(
        clone.implements.is_empty(),
        "covariant-return override wrongly flagged as explicit impl: {:?}",
        clone.implements,
    );
}

#[test]
fn classification_agrees_with_roslyn_name_mangling_convention() {
    // Differential property: the reader classifies a `MethodImpl` row by its
    // *declaration* target (an interface vs a base class), never by the body's
    // name — but Roslyn name-mangles exactly the explicit interface
    // implementations (`IFace<…>.Member`, hence a `.`) and nothing else. So on
    // this Roslyn-compiled fixture the two notions must coincide: a member
    // carries `implements` exactly when its IL name is
    // interface-qualified (contains a `.` and is not a constructor), and each
    // recovered interface-side `member` equals the final dotted segment of the
    // IL name (the interface arguments may themselves contain `.`, but they
    // sit inside `<...>`, before the final `.member`). A disagreement in
    // either direction is a classification bug (the fabricated-IL shapes where
    // the notions *deliberately* diverge live in
    // `methodimpl_classification.rs`).
    // (kind, IL name, implements) for every member that can carry one.
    fn members(e: &Entity) -> Vec<(&str, &str, &[InterfaceMemberImpl])> {
        e.members
            .iter()
            .filter_map(|m| match m {
                Member::Method(m) => Some(("method", m.name.as_str(), m.implements.as_slice())),
                Member::Property(p) => Some(("property", p.name.as_str(), p.implements.as_slice())),
                Member::Event(ev) => Some(("event", ev.name.as_str(), ev.implements.as_slice())),
                Member::Field(_) => None,
            })
            .collect()
    }

    fn check(e: &Entity) {
        for (kind, name, eis) in members(e) {
            let is_ctor = name == ".ctor" || name == ".cctor";
            let dotted = name.contains('.') && !is_ctor;
            assert_eq!(
                !eis.is_empty(),
                dotted,
                "{kind} `{name}` on `{}`: implements presence should match \
                 dotted-name-ness",
                e.name,
            );
            for ei in eis {
                let last = name.rsplit('.').next().unwrap();
                // Roslyn emits no cross-kind explicit impls, and every
                // implemented interface in this single-assembly fixture is
                // in-module, so each declaration resolves through
                // MethodSemantics to the implementing member's own kind — an
                // `Unresolved` here would mean the resolution regressed.
                let expected = match kind {
                    "method" => ImplementedMember::Method(last.to_string()),
                    "property" => ImplementedMember::Property(last.to_string()),
                    "event" => ImplementedMember::Event(last.to_string()),
                    other => panic!("unexpected member kind `{other}`"),
                };
                assert_eq!(
                    ei.member, expected,
                    "implemented member should be the final dotted segment of \
                     `{name}` under the implementing member's own kind",
                );
            }
        }
        for n in &e.nested_types {
            check(n);
        }
    }

    let es = entities();
    for e in &es {
        check(e);
    }
}
