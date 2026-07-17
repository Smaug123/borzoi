//! `MethodImpl` classification against fabricated *valid* IL that no C#/F#
//! compiler emits (`tests/fixtures/assembly/MetadataEmitter`).
//!
//! The CLR classifies a `MethodImpl` row (ECMA-335 §II.22.27) by its
//! `MethodDeclaration` target; the interface-qualified (`IFace.Member`) body
//! name C#/F# emit is a *convention*, not a rule — VB, for one, freely emits
//! plain-named bodies via `Implements`, and one body may satisfy several
//! interface members. These tests pin that the reader keys off the declaration
//! (and the row's `Class`), never the name:
//!
//! - a plain-named body implementing an in-module interface is surfaced;
//! - one body carrying two declarations surfaces both (the model is a list);
//! - an *external* interface declaration is recognised through the
//!   implementing type's `InterfaceImpl` rows;
//! - an external *class* declaration (the covariant-return-override shape) is
//!   not an explicit interface impl;
//! - a malformed row whose `Class` disagrees with the body's owner is skipped
//!   rather than attributed to a type the row does not name, while a `Class`
//!   index past the end of the `TypeDef` table is a *structural* error;
//! - the implemented member of a property/event impl comes from the
//!   declaration's `MethodSemantics`, never from its name text — and a
//!   declaration whose `MethodSemantics` is out of reach (a `MemberRef` into
//!   another assembly) stays [`ImplementedMember::Unresolved`] with its raw
//!   name, never a prefix-stripped guess.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, ImplementedMember, Member, MethodLike, TypeRef,
    UnclassifiedMethodImpl,
};

use crate::common::{emit_metadata_fixture, ensure_fsharp_core_dll};

fn entities(shape: &str) -> Vec<Entity> {
    let bytes = emit_metadata_fixture(shape);
    let view = Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {shape}: {e}"));
    let entities = view
        .enumerate_type_defs()
        .unwrap_or_else(|e| panic!("enumerate {shape}: {e}"));
    // These fixtures are all well-formed apart from the row under test; a
    // dropped member would silently weaken the assertions below.
    for e in &entities {
        assert!(
            e.skipped_members.is_empty(),
            "unexpected skipped members on `{}`: {:?}",
            e.name,
            e.skipped_members,
        );
    }
    entities
}

fn entity<'a>(es: &'a [Entity], name: &str) -> &'a Entity {
    es.iter().find(|e| e.name == name).unwrap_or_else(|| {
        let names: Vec<&str> = es.iter().map(|e| e.name.as_str()).collect();
        panic!("no entity `{name}` (have: {names:?})")
    })
}

fn method<'a>(e: &'a Entity, name: &str) -> &'a MethodLike {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == name => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no method `{name}` on `{}`", e.name))
}

/// As [`entities`], but tolerating recorded member drops: fixtures whose
/// *interface* deliberately carries a shape the projector refuses (an
/// `Other`-semantics accessor) still exercise the reader-level classification,
/// which sees the raw member model, not the projected one.
fn entities_allowing_skips(shape: &str) -> Vec<Entity> {
    let bytes = emit_metadata_fixture(shape);
    let view = Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {shape}: {e}"));
    view.enumerate_type_defs()
        .unwrap_or_else(|e| panic!("enumerate {shape}: {e}"))
}

/// Shorthand constructors for expected [`ImplementedMember`]s.
fn method_m(name: &str) -> ImplementedMember {
    ImplementedMember::Method(name.to_string())
}
fn property_m(name: &str) -> ImplementedMember {
    ImplementedMember::Property(name.to_string())
}
fn event_m(name: &str) -> ImplementedMember {
    ImplementedMember::Event(name.to_string())
}
fn unresolved_m(name: &str) -> ImplementedMember {
    ImplementedMember::Unresolved(name.to_string())
}

/// The `(interface simple name, implemented member)` pairs on the property
/// named `prop` of entity `owner`.
fn property_impls<'a>(
    es: &'a [Entity],
    owner: &str,
    prop: &str,
) -> Vec<(&'a str, ImplementedMember)> {
    let p = entity(es, owner)
        .members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == prop => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no property `{prop}` on `{owner}`"));
    p.implements
        .iter()
        .map(|ei| {
            let iface = match &ei.interface {
                TypeRef::Named { name, .. } => name.as_str(),
                other => panic!("expected a Named interface, got {other:?}"),
            };
            (iface, ei.member.clone())
        })
        .collect()
}

/// The `(parent simple name, raw member name)` pairs of a member's
/// unclassified `MethodImpl` rows.
fn unclassified(rows: &[UnclassifiedMethodImpl]) -> Vec<(&str, &str)> {
    rows.iter()
        .map(|u| {
            let parent = match &u.parent {
                TypeRef::Named { name, .. } => name.as_str(),
                other => panic!("expected a Named parent, got {other:?}"),
            };
            (parent, u.member.as_str())
        })
        .collect()
}

/// Assert `ei` names the non-generic interface `iface` (by simple name) and
/// the given interface member (kind included).
fn assert_impl(m: &MethodLike, expected: &[(&str, ImplementedMember)]) {
    let got: Vec<(&str, ImplementedMember)> = m
        .implements
        .iter()
        .map(|ei| {
            let iface = match &ei.interface {
                TypeRef::Named { name, .. } => name.as_str(),
                other => panic!("expected a Named interface, got {other:?}"),
            };
            (iface, ei.member.clone())
        })
        .collect();
    assert_eq!(got, expected, "explicit-interface entries on `{}`", m.name);
}

#[test]
fn plain_named_body_is_still_an_explicit_impl() {
    // `Widget::Impl` implements `IFoo::M` via MethodImpl; the body name has no
    // dot. Classification must come from the declaration target (an in-module
    // interface TypeDef), so the impl is surfaced despite the plain name.
    let es = entities("methodimpl_unmangled_body");
    assert_impl(
        method(entity(&es, "Widget"), "Impl"),
        &[("IFoo", method_m("M"))],
    );
    // The interface's own declaration is not an implementation of anything.
    assert_impl(method(entity(&es, "IFoo"), "M"), &[]);
}

#[test]
fn one_body_satisfying_two_interface_members_carries_both() {
    // Two MethodImpl rows share `Widget::Impl` as their body (VB's
    // `Implements IFoo.M, IBar.M`). Both declarations must survive — a
    // singular slot would keep only the last row.
    let es = entities("methodimpl_multi_decl");
    assert_impl(
        method(entity(&es, "Widget"), "Impl"),
        &[("IFoo", method_m("M")), ("IBar", method_m("M"))],
    );
}

#[test]
fn external_interface_decl_classifies_via_interfaceimpl_membership() {
    // The declaration parent is `[mscorlib]System.IDisposable` — a TypeRef,
    // whose interface-ness cannot be read from flags in this module. It *is*
    // in `Widget`'s InterfaceImpl rows (only interfaces may appear there), so
    // the plain-named body `DoDispose` is an explicit impl of `Dispose`.
    let es = entities("methodimpl_external_iface_unmangled");
    let m = method(entity(&es, "Widget"), "DoDispose");
    assert_impl(m, &[("IDisposable", unresolved_m("Dispose"))]);
}

#[test]
fn duplicate_typerefs_still_classify_via_membership() {
    // The InterfaceImpl row and the MethodImpl declaration reach
    // `System.IDisposable` through two *duplicate* TypeRef rows (legal
    // metadata; IL weavers produce it). Membership must compare the referenced
    // type's identity — scope, namespace, name — not TypeRef row identity.
    let es = entities("methodimpl_dup_typeref");
    let m = method(entity(&es, "Widget"), "DoDispose");
    assert_impl(m, &[("IDisposable", unresolved_m("Dispose"))]);
}

#[test]
fn property_impls_split_across_accessors_are_unioned() {
    // `C::P`'s getter satisfies get-only `IRead.P` while its setter satisfies
    // set-only `IWrite.P` (VB's `Property P … Implements IRead.P, IWrite.P`).
    // The projection must union the accessors' MethodImpls — a getter-else-
    // setter fallback drops IWrite. `C::B` implements one get+set interface
    // property through both accessors; its two rows must dedup to one entry.
    let es = entities("methodimpl_split_property");
    let property = |name: &str| {
        entity(&es, "C")
            .members
            .iter()
            .find_map(|m| match m {
                Member::Property(p) if p.name == name => Some(p),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no property `{name}` on C"))
    };
    let got = |name: &str| {
        property(name)
            .implements
            .iter()
            .map(|ei| {
                let iface = match &ei.interface {
                    TypeRef::Named { name, .. } => name.as_str(),
                    other => panic!("expected a Named interface, got {other:?}"),
                };
                (iface, ei.member.clone())
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        got("P"),
        [("IRead", property_m("P")), ("IWrite", property_m("P"))]
    );
    assert_eq!(got("B"), [("IBoth", property_m("B"))]);
}

#[test]
fn interface_reachable_through_an_in_module_interface_classifies() {
    // `Widget`'s direct InterfaceImpl rows list only the in-module `IMid`,
    // whose own rows list the external `IDisposable`; the declaration targets
    // IDisposable. The CLR places declarations against the full interface map
    // (transitive closure), so the row is loadable IL — membership must expand
    // through in-module interface edges rather than require Roslyn's flattened
    // direct listing.
    let es = entities("methodimpl_iface_via_interface");
    let m = method(entity(&es, "Widget"), "DoDispose");
    assert_impl(m, &[("IDisposable", unresolved_m("Dispose"))]);
}

#[test]
fn interface_reachable_through_an_in_module_base_class_classifies() {
    // `Widget` has no InterfaceImpl rows of its own; the external
    // `IDisposable` comes from its in-module base class `BaseW`. The CLR's
    // interface map includes base-class interfaces, so membership must expand
    // through the in-module `Extends` chain.
    let es = entities("methodimpl_iface_via_base");
    let m = method(entity(&es, "Widget"), "DoDispose");
    assert_impl(m, &[("IDisposable", unresolved_m("Dispose"))]);
}

#[test]
fn interface_member_name_resolves_through_method_semantics() {
    // Accessor naming is a CLS convention, not a CLR rule, and it misleads in
    // every direction; only MethodSemantics is authoritative.
    //  * `IProp::P` has a getter named `Read`: the recovered member name must
    //    be the owning property's name `P` — resolved through the interface's
    //    MethodSemantics — not the accessor name `Read` (nor a prefix-stripped
    //    mangling of it).
    //  * `IProp` has a property literally named `get_Value`: the
    //    MethodSemantics-resolved name must be kept verbatim, not
    //    conventionally stripped down to `Value`.
    //  * `IProp::get_Q` is an ordinary method that no MethodSemantics row
    //    claims, implemented by C's property getter `Fetch`. It is surfaced on
    //    the implementing property (that is the only member the accessor's
    //    `MethodImpl` can hang from) under its own name — stripping it to `Q`
    //    would report an interface property that does not exist.
    let es = entities("methodimpl_unconventional_accessor");
    let property = |name: &str| {
        entity(&es, "C")
            .members
            .iter()
            .find_map(|m| match m {
                Member::Property(p) if p.name == name => Some(p),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no property `{name}` on C"))
    };
    let got = |name: &str| {
        property(name)
            .implements
            .iter()
            .map(|ei| {
                let iface = match &ei.interface {
                    TypeRef::Named { name, .. } => name.as_str(),
                    other => panic!("expected a Named interface, got {other:?}"),
                };
                (iface, ei.member.clone())
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(got("P"), [("IProp", property_m("P"))]);
    assert_eq!(got("get_Value"), [("IProp", property_m("get_Value"))]);
    assert_eq!(got("Q"), [("IProp", method_m("get_Q"))]);
}

#[test]
fn method_semantics_is_read_through_a_local_generic_interfaces_memberref() {
    // An explicit impl of a *generic* interface defined in this same module can
    // only be spelled as a `MemberRef` over a `TypeSpec` — a `MethodDef` token
    // cannot name an instantiation. That `MemberRef` is not "external": the
    // declaration's `MethodSemantics` is right here, so the implemented member
    // must resolve to the owning property `P` and not fall back to stripping a
    // conventional prefix off the getter's name `Read` (which, having no such
    // prefix, would surface as `Read`).
    let es = entities("methodimpl_local_generic_iface_memberref");
    let p = entity(&es, "C")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == "P" => Some(p),
            _ => None,
        })
        .expect("property P on C");
    let got: Vec<(&str, ImplementedMember)> = p
        .implements
        .iter()
        .map(|ei| {
            let iface = match &ei.interface {
                TypeRef::Named {
                    name, type_args, ..
                } => {
                    assert_eq!(type_args.len(), 1, "IGen<int32> is a closed instantiation");
                    name.as_str()
                }
                other => panic!("expected a Named interface, got {other:?}"),
            };
            (iface, ei.member.clone())
        })
        .collect();
    assert_eq!(got, [("IGen", property_m("P"))]);
}

#[test]
fn external_class_decl_is_not_an_explicit_impl() {
    // The declaration parent is `[mscorlib]System.Object` — an external
    // *class* (the covariant-return-override shape, externally based). It is
    // not in `Widget`'s InterfaceImpl rows, and it *is* `Widget`'s direct
    // `Extends` target — a provable ancestor — so the row is a decided
    // override redirection: not an explicit interface implementation, and
    // not an unclassified row either.
    let es = entities("methodimpl_external_class_decl");
    let m = method(entity(&es, "Widget"), "ToString");
    assert_impl(m, &[]);
    assert!(
        m.unclassified_impls.is_empty(),
        "a decl on a provable ancestor is decided, not unclassified: {:?}",
        m.unclassified_impls,
    );
}

#[test]
fn fsharp_core_impls_agree_with_the_fsharp_compilers_mangling() {
    // The F# compiler is a second, independent emitter (the BCL ref-pack sweep
    // covers Roslyn). On *nominal* types it interface-qualifies the IL name of
    // every explicit impl body it emits, so a plain-named flagged method on
    // one would be a base-class override misclassified as an interface impl.
    // Its *compiler-generated* closure and state-machine types (`@` in the
    // type name — `TaskBuilder-MergeSources@384`) are the exception that
    // proves the classification point: they implement `IAsyncStateMachine` /
    // `IResumableStateMachine` through `MethodImpl` rows whose bodies keep
    // plain names (`MoveNext`, `get_Data`), the exact valid-IL shape a
    // name-keyed reader silently missed — FSharp.Core carries ~140 of them.
    //
    // The reverse direction (dotted ⇒ flagged) deliberately isn't asserted: F#
    // also mangles dots into names that are *not* interface impls (F#-native
    // extension members such as `Counter.Tripled` live as static methods on a
    // module class). Anti-vacuity floors guard against the classifier
    // silently dropping either population (F# resolves same-module interfaces
    // through module-scoped TypeRef aliases, a shape Roslyn never emits).
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate FSharp.Core types");

    let mut flagged_total = 0usize;
    let mut plain_state_machine_impls = 0usize;
    let mut violations: Vec<String> = Vec::new();
    fn walk(
        e: &Entity,
        flagged_total: &mut usize,
        plain_state_machine_impls: &mut usize,
        violations: &mut Vec<String>,
    ) {
        let compiler_generated = e.name.contains('@');
        for m in &e.members {
            let Member::Method(m) = m else { continue };
            if m.implements.is_empty() {
                continue;
            }
            *flagged_total += 1;
            let is_ctor = m.name == ".ctor" || m.name == ".cctor";
            if !m.name.contains('.') || is_ctor {
                if compiler_generated {
                    *plain_state_machine_impls += 1;
                } else {
                    violations.push(format!(
                        "method `{}::{}` flagged as interface impl despite plain name: {:?}",
                        e.name, m.name, m.implements,
                    ));
                }
            }
        }
        for n in &e.nested_types {
            walk(n, flagged_total, plain_state_machine_impls, violations);
        }
    }
    for e in &entities {
        walk(
            e,
            &mut flagged_total,
            &mut plain_state_machine_impls,
            &mut violations,
        );
    }

    assert!(
        violations.is_empty(),
        "{} over-classifications in FSharp.Core:\n{}",
        violations.len(),
        violations.join("\n"),
    );
    assert!(
        flagged_total >= 100,
        "only {flagged_total} interface impls recognised in FSharp.Core — the \
         classifier is dropping the F# compiler's shapes",
    );
    assert!(
        plain_state_machine_impls >= 50,
        "only {plain_state_machine_impls} plain-named state-machine impls \
         recognised in FSharp.Core — the name-independent classification regressed",
    );
}

#[test]
fn external_accessor_decl_stays_verbatim_not_prefix_stripped() {
    // The declaration is a MemberRef into another assembly named `get_Q`.
    // Whether that is a property accessor or an ordinary method that merely
    // looks like one is unknowable without the referenced assembly's
    // MethodSemantics, so the model must carry the raw declaration verbatim —
    // stripping the CLS prefix would fabricate an interface property `Q` that
    // may not exist.
    let es = entities("methodimpl_external_accessor_decl");
    let p = entity(&es, "C")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == "P" => Some(p),
            _ => None,
        })
        .expect("property P on C");
    let got: Vec<(&str, ImplementedMember)> = p
        .implements
        .iter()
        .map(|ei| {
            let iface = match &ei.interface {
                TypeRef::Named { name, .. } => name.as_str(),
                other => panic!("expected a Named interface, got {other:?}"),
            };
            (iface, ei.member.clone())
        })
        .collect();
    assert_eq!(got, [("IExt", unresolved_m("get_Q"))]);
}

#[test]
fn row_whose_class_is_out_of_range_is_a_structural_error() {
    // `Class` is TypeDef RID 100 in a 3-row TypeDef table. The reader's
    // structural contract (`apply_method_impls`'s doc) draws the line between
    // *semantically* odd-but-well-formed rows, which are skipped, and indices
    // past the end of a table, which are an `Err` — this row is the latter and
    // must not vanish as a silent skip.
    let bytes = emit_metadata_fixture("methodimpl_class_out_of_range");
    // The refusal surfaces wherever member building runs (parse eagerly builds
    // the member runs today); accept it from either stage, but require it.
    let err = match Ecma335Assembly::parse(&bytes) {
        Err(e) => e,
        Ok(view) => view
            .enumerate_type_defs()
            .expect_err("an out-of-range MethodImpl.Class must fail loud"),
    };
    assert!(
        err.to_string().contains("table index out of range"),
        "expected a table-index error, got: {err}"
    );
}

#[test]
fn inherited_external_interface_impl_surfaces_as_unclassified() {
    // The shape F# (`interface IDerived with member _.M()`) and VB
    // (`Implements IDerived` + `Sub Body() Implements IBase.M`) emit when a
    // member of an *inherited external* interface is implemented through the
    // derived interface's clause: InterfaceImpl lists only `IDerived`; the
    // MethodImpl declaration targets `IBase::M`. From this image alone,
    // `IBase` can be neither proven an implemented interface (its inheritance
    // link lives in the external assembly) nor proven an ancestor class (the
    // in-image-identical shape a multi-hop C# covariant-return override
    // produces) — so the row must surface on the *unclassified* channel for a
    // multi-assembly consumer to finish, never silently dropped and never
    // published as a proven `implements`.
    let es = entities("methodimpl_external_inherited_iface");
    let body = method(entity(&es, "C"), "Body");
    assert_impl(body, &[]);
    assert_eq!(unclassified(&body.unclassified_impls), [("IBase", "M")]);
    // The accessor path: `C.P`'s getter implements `IBase::get_Q`; the
    // unclassified row is unioned onto the property, with the declaration's
    // raw name (whether `get_Q` is an accessor is equally unknowable here).
    let p = entity(&es, "C")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == "P" => Some(p),
            _ => None,
        })
        .expect("property P on C");
    assert!(p.implements.is_empty(), "no proven impls on P");
    assert_eq!(unclassified(&p.unclassified_impls), [("IBase", "get_Q")]);
}

#[test]
fn generic_inherited_local_interface_substitutes_through_the_closure() {
    // Real F# output (`interface IDerived<int> with member _.M()` where
    // same-module `IDerived<'T> :> IBase<'T>`): C's InterfaceImpl lists only
    // `IDerived<int32>`, IDerived`1's own row lists `IBase<!0>` (the
    // definition's type parameter), and the MethodImpl declaration targets
    // the *constructed* `IBase<int32>`. The closure walk must substitute
    // IDerived's instantiation through its interface rows — expanding the
    // bare definition would contribute `IBase<!0>`, mismatch the constructed
    // declaration, and silently drop a real F# implementation.
    let es = entities("methodimpl_generic_inherited_local_iface");
    let m = method(entity(&es, "C"), "Impl");
    assert_impl(m, &[("IBase", method_m("M"))]);
    // The surfaced interface is the substituted instantiation, not the
    // definition's own parameter.
    match &m.implements[0].interface {
        TypeRef::Named { type_args, .. } => {
            assert_eq!(type_args.len(), 1);
            assert!(
                matches!(type_args[0].ty, TypeRef::Primitive(_)),
                "expected the substituted int32 argument, got {:?}",
                type_args[0].ty,
            );
        }
        other => panic!("expected a Named interface, got {other:?}"),
    }
}

#[test]
fn fbounded_self_growing_closure_is_budget_bounded() {
    // Hostile metadata: `I<T> : I<Pair<T,T>>` doubles the instantiated tree
    // on every closure frame, so an unbudgeted substitution walk allocates
    // exponentially long before any frame-count cap trips. The walk must
    // complete promptly with a partial closure — and the direct `I<int32>`
    // row still classifies the impl.
    let es = entities("methodimpl_fbounded_growth");
    assert_impl(method(entity(&es, "C"), "Impl"), &[("I", method_m("M"))]);
}

#[test]
fn overloaded_same_named_external_decls_are_not_collapsed() {
    // `C.P`'s getter implements `IExt::X(): int32` and its setter
    // `IExt::X(int32): void` — two distinct overloads of a same-named
    // external member (interfaces may overload), one MethodImpl row each.
    // Their projections are identical (`Unresolved("X")` on `IExt`; no
    // signature is carried), but name equality cannot prove two external
    // declarations are one member, so the accessor union must keep both.
    let es = entities("methodimpl_overloaded_external_accessor_decls");
    assert_eq!(
        property_impls(&es, "C", "P"),
        [("IExt", unresolved_m("X")), ("IExt", unresolved_m("X")),],
    );
}

#[test]
fn accessor_shared_across_event_roles_contributes_once() {
    // `C.E`'s Adder and Remover MethodSemantics rows both name the *same*
    // MethodDef (crafted IL), which carries exactly one MethodImpl row to
    // external `IExt::add_X`. The union must contribute the shared accessor
    // once — projecting it per role would make one metadata row look like
    // two implementations, since unresolved entries are (deliberately)
    // never deduplicated by value.
    let es = entities("methodimpl_shared_event_accessor");
    let ev = entity(&es, "C")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Event(ev) if ev.name == "E" => Some(ev),
            _ => None,
        })
        .expect("event E on C");
    let got: Vec<(&str, ImplementedMember)> = ev
        .implements
        .iter()
        .map(|ei| {
            let iface = match &ei.interface {
                TypeRef::Named { name, .. } => name.as_str(),
                other => panic!("expected a Named interface, got {other:?}"),
            };
            (iface, ei.member.clone())
        })
        .collect();
    assert_eq!(got, [("IExt", unresolved_m("add_X"))]);
}

#[test]
fn decl_on_an_unrelated_local_interface_is_skipped() {
    // The declaration parent `IBar` is a local interface by its flag, but
    // `Widget` does not implement it (its InterfaceImpl row lists only
    // `IFoo`). §II.22.27 requires the declaration to be on `Class`'s
    // ancestor chain or interface tree — the CLR resolves declarations
    // against the computed interface map — so the row cannot load and must
    // not be published as an implementation relationship the type does not
    // have. Interface-ness alone is not enough for local parents; membership
    // is required just as for external ones.
    let es = entities("methodimpl_unrelated_local_iface");
    assert_impl(method(entity(&es, "Widget"), "Impl"), &[]);
}

#[test]
fn dim_reabstraction_with_an_abstract_body_is_surfaced() {
    // C# 8 default-interface-method *reabstraction* — mirrored from what
    // Roslyn emits for `interface I2 : I1 { abstract void I1.M(); }`: a
    // MethodImpl on the interface `I2` whose body is I2's own *abstract*
    // (RVA-0) method. VB's `MustOverride Sub M() Implements IFoo.M` is the
    // class-side analogue and the runtime loads both, so a gate requiring an
    // executable body would drop genuine compiler output. Pins that the
    // abstract body is accepted and classified.
    let es = entities("methodimpl_reabstraction");
    assert_impl(method(entity(&es, "I2"), "I1.M"), &[("I1", method_m("M"))]);
}

#[test]
fn memberref_sig_through_a_duplicate_typeref_still_resolves_semantics() {
    // As `method_semantics_is_read_through_a_local_generic_interfaces_memberref`,
    // but the MemberRef's signature blob spells `System.Object` through a
    // *duplicate* TypeRef row, so it is not byte-identical to the MethodDef's
    // signature. Signature identity is semantic, not byte-wise (the CLR
    // compares MemberRef signatures by resolving their tokens): the
    // declaration must still resolve to the owning property `P`, not degrade
    // to `Unresolved("Read")`.
    let es = entities("methodimpl_dup_typeref_sig");
    assert_eq!(property_impls(&es, "C", "P"), [("IGen", property_m("P"))]);
}

#[test]
fn event_impl_carried_only_by_the_fire_accessor_surfaces() {
    // `C.E`'s add/remove carry no MethodImpl; only the *fire* accessor maps to
    // `IEvt`'s fire accessor. Fire is a first-class event semantic
    // (§II.22.28), so the implementation must appear on the event — a union
    // over add/remove alone loses it.
    let es = entities("methodimpl_event_fire_impl");
    let ev = entity(&es, "C")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Event(ev) if ev.name == "E" => Some(ev),
            _ => None,
        })
        .expect("event E on C");
    let got: Vec<(&str, ImplementedMember)> = ev
        .implements
        .iter()
        .map(|ei| {
            let iface = match &ei.interface {
                TypeRef::Named { name, .. } => name.as_str(),
                other => panic!("expected a Named interface, got {other:?}"),
            };
            (iface, ei.member.clone())
        })
        .collect();
    assert_eq!(got, [("IEvt", event_m("E"))]);
}

#[test]
fn other_semantics_decl_is_an_accessor_of_its_owning_property() {
    // `IProp::Aux` is an `Other`-semantics accessor of property `P`
    // (§II.22.28 — an authoritative association, not an ordinary method).
    // `C.Q`'s getter implements it; the implemented member is property `P`.
    // The interface's own `P` carries a shape the projector refuses (the
    // `Other` accessor), so this loader tolerates that recorded drop — the
    // classification reads the raw member model.
    let es = entities_allowing_skips("methodimpl_other_accessor_decl");
    assert_eq!(property_impls(&es, "C", "Q"), [("IProp", property_m("P"))],);
}

#[test]
fn decl_claimed_by_two_properties_surfaces_both_owners() {
    // `MethodSemantics` does not make `Method` unique: `IProp::G` is the
    // getter of both `P1` and `P2`. Both associations are authoritative, so
    // `C.R` (whose getter implements `G`) must carry one entry per owner —
    // keeping only the first silently loses the second.
    let es = entities("methodimpl_multi_owner_accessor");
    assert_eq!(
        property_impls(&es, "C", "R"),
        [("IProp", property_m("P1")), ("IProp", property_m("P2"))],
    );
}

#[test]
fn module_scoped_typeref_decl_parent_fails_soft_to_unclassified() {
    // The declaration parent is a *module-scoped* TypeRef aliasing the
    // in-module `IFoo` (legal per ECMA-335, which nonetheless recommends the
    // TypeDef token; probed compilers — FSharp.Core, FSharp.Compiler.Service,
    // MiniLibFs, the whole net10.0 ref pack — emit zero of these in
    // MethodImpl/InterfaceImpl). The reader deliberately does not
    // name-resolve such aliases back to TypeDefs; this pins the documented
    // fail-soft residual: the alias never compares equal to the TypeDef token
    // the InterfaceImpl row holds, so nothing is *proven* — the row surfaces
    // on the unclassified channel (never as `implements`).
    let es = entities("methodimpl_module_typeref_decl");
    let m = method(entity(&es, "Widget"), "Impl");
    assert_impl(m, &[]);
    assert_eq!(unclassified(&m.unclassified_impls), [("IFoo", "M")]);
}

#[test]
fn row_whose_class_disagrees_with_the_body_owner_is_skipped() {
    // A malformed row: `Class` = `Other` but the body method lives on
    // `Widget` (§II.22.27 requires the body to be a method of `Class`). The
    // body even carries a Roslyn-style mangled name; ignoring `Class` and
    // trusting the name would attribute the impl to `Widget`, a type the row
    // does not name. The row must be skipped.
    let es = entities("methodimpl_class_mismatch");
    assert_impl(method(entity(&es, "Widget"), "IFoo.M"), &[]);
}
