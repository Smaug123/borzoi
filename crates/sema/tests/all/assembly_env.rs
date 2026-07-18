//! Self-contained tests for [`AssemblyEnv`] — the name index over referenced
//! assemblies. No FCS: the assembly *model* is already differentially tested
//! against Roslyn in `crates/assembly`, so here we only check that the index
//! reaches and round-trips what `EcmaView` enumerates.
//!
//! The fixture is a tiny sema-owned C# assembly (`tests/fixtures/assembly_env`),
//! read through `Ecma335Assembly`. It is owned by this crate (not shared with the
//! assembly crate's fixtures) so the test stays decoupled from that crate's
//! evolving test data, and — since it only *reads* `Entity`/`Member` fields —
//! robust to the assembly crate adding model fields.

use std::path::{Path, PathBuf};

use borzoi_assembly::{
    Access, Augmentation, Ecma335Assembly, EcmaView, Entity, EntityKind, Member, Nullability,
    NullableType, ParamDefault, Parameter, Primitive, TypeRef,
};
use borzoi_sema::{
    AbbreviationVisibility, AssemblyEnv, EntityHandle, ExtensionMembers, StaticLookup,
};

/// Build the fixture assembly once per test binary and return the `.dll` path.
///
/// Delegates to [`crate::common::ensure_assembly_fixture_built`] so the build
/// takes the binary-wide `BUILD_LOCK` — a per-module builder would race the same
/// project's `obj/`/`bin/` against the other groups now they share one binary.
fn ensure_fixture_built() -> &'static Path {
    crate::common::ensure_assembly_fixture_built()
}

fn fixture_entities() -> Vec<Entity> {
    let bytes = std::fs::read(ensure_fixture_built()).expect("read fixture dll");
    Ecma335Assembly::parse(&bytes)
        .expect("parse fixture dll")
        .enumerate_type_defs()
        .expect("enumerate fixture types")
}

fn fixture_env() -> AssemblyEnv {
    AssemblyEnv::from_entities(fixture_entities())
}

/// A fixture env where `Demo.<name>` is **replaced** by a copy tagged as if from a
/// different assembly (its `AssemblyIdentity.name` changed). It is the *only*
/// definition of that type — so its key is not even ambiguous — modelling "the
/// base's own assembly is absent while a same-named type from another assembly is
/// present". A derived type's `base_type` names its own (fixture) assembly, so
/// resolving that base against this ghost is an assembly-name mismatch and the walk
/// must defer rather than walk the wrong-assembly base.
fn env_with_ghost_type(name: &str) -> AssemblyEnv {
    let mut ghost = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == name)
        .unwrap_or_else(|| panic!("Demo.{name} in fixture"));
    ghost.assembly.name = "GhostAssembly".to_string();
    let mut entities: Vec<Entity> = fixture_entities()
        .into_iter()
        .filter(|e| !(e.namespace == ns(&["Demo"]) && e.name == name))
        .collect();
    entities.push(ghost);
    AssemblyEnv::from_entities(entities)
}

fn ns(segments: &[&str]) -> Vec<String> {
    segments.iter().map(|s| s.to_string()).collect()
}

#[test]
fn has_namespace_recognises_declared_and_parent_namespaces() {
    let env = fixture_env();
    // A namespace that directly holds types.
    assert!(env.has_namespace(&ns(&["Demo"])));
    assert!(env.has_namespace(&ns(&["Demo", "Sub"])));
    // A *root* namespace (`Sub`) distinct from the nested `Demo.Sub`.
    assert!(env.has_namespace(&ns(&["Sub"])));
    // Not a namespace: a type name used as a namespace, or an absent one.
    assert!(!env.has_namespace(&ns(&["Demo", "Thing"]))); // `Thing` is a type
    assert!(!env.has_namespace(&ns(&["Nope"])));
    assert!(!env.has_namespace(&ns(&["Demo", "Nope"])));
    // A namespace whose only type is *internal* is not cross-assembly-visible, so
    // it does not count — but the *public* root `Hush` does.
    assert!(!env.has_namespace(&ns(&["Demo", "Hush"])));
    assert!(env.has_namespace(&ns(&["Hush"])));
}

#[test]
fn has_namespace_sees_a_public_type_that_lost_the_first_wins_slot() {
    // Review round 15, P2: `has_namespace` scanned the **first-wins** `by_type` index and
    // asked `is_public` of whichever handle won the `(namespace, name, arity)` slot. An
    // *inaccessible* type enumerated first therefore hid a **public** same-keyed type
    // from another assembly, and the namespace vanished from view.
    //
    // That is not a cosmetic miss: the module-open cut reads this to decide whether a
    // path is a module/namespace cross-kind merge, and a false `false` means it commits a
    // **definite target** for a name the namespace half may well contest — the wrong
    // target the cut exists to prevent. Scan the full top-level set instead.
    let template = fixture_entities()
        .into_iter()
        .find(|e| {
            e.namespace == ns(&["Demo"])
                && e.kind == EntityKind::Class
                && e.generic_parameters.is_empty()
        })
        .expect("an arity-0 Demo class to clone");

    // Enumerated FIRST, so it takes the `by_type` slot — and it is not accessible.
    let mut hidden = template.clone();
    hidden.namespace = ns(&["Merged", "Half"]);
    hidden.name = "T".to_string();
    hidden.access = Access::Internal;
    hidden.members = vec![];
    hidden.nested_types = vec![];

    // Same `(namespace, name, arity)`, from another assembly — and PUBLIC. F# can open
    // `Merged.Half` and see this, so the namespace is real.
    let mut visible = hidden.clone();
    visible.access = Access::Public;
    visible.assembly.name = "OtherAssembly".to_string();

    let env = AssemblyEnv::from_entities(vec![hidden, visible]);
    assert!(
        env.has_namespace(&ns(&["Merged", "Half"])),
        "a public type sharing its key with an earlier inaccessible one still makes \
         `Merged.Half` a namespace; asking the first-wins index answers `false` and lets \
         a cross-kind module open commit a definite target"
    );
}

#[test]
fn known_types_resolve_by_namespace_name_and_arity() {
    let env = fixture_env();

    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing resolves");
    assert_eq!(env.entity(thing).name, "Thing");
    assert_eq!(env.entity(thing).namespace, ns(&["Demo"]));

    // A type in a deeper namespace.
    let deep = env
        .lookup_type(&ns(&["Demo", "Sub"]), "Deep", 0)
        .expect("Demo.Sub.Deep resolves");
    assert_eq!(env.entity(deep).namespace, ns(&["Demo", "Sub"]));
}

#[test]
fn same_name_different_arity_types_are_distinct() {
    // `Pair`, `Pair<T>`, `Pair<T,U>` share namespace + simple name and differ
    // only in generic arity. Each must resolve to a *distinct* handle whose
    // entity declares the matching number of type parameters — the index must
    // not collapse them.
    let env = fixture_env();

    let p0 = env.lookup_type(&ns(&["Demo"]), "Pair", 0).expect("Pair");
    let p1 = env.lookup_type(&ns(&["Demo"]), "Pair", 1).expect("Pair<T>");
    let p2 = env
        .lookup_type(&ns(&["Demo"]), "Pair", 2)
        .expect("Pair<T,U>");

    assert!(
        p0 != p1 && p1 != p2 && p0 != p2,
        "the three Pair arities must be distinct handles"
    );
    assert_eq!(env.entity(p0).generic_parameters.len(), 0);
    assert_eq!(env.entity(p1).generic_parameters.len(), 1);
    assert_eq!(env.entity(p2).generic_parameters.len(), 2);

    // An arity nothing declares misses.
    assert!(env.lookup_type(&ns(&["Demo"]), "Pair", 3).is_none());
}

#[test]
fn nested_type_resolves_by_descent_not_by_namespace() {
    let env = fixture_env();
    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing");

    // `Inner` is reached by descending from `Thing`, not via the top-level index.
    let inner = env
        .nested(thing, "Inner", 0)
        .expect("Thing.Inner via descent");
    assert_eq!(env.entity(inner).name, "Inner");
    // It is *not* a top-level type (nested types carry an empty namespace).
    assert!(
        env.lookup_type(&ns(&["Demo"]), "Inner", 0).is_none(),
        "nested type must not be in the top-level index"
    );
    assert!(env.lookup_type(&[], "Inner", 0).is_none());
}

#[test]
fn members_resolve_by_name() {
    let env = fixture_env();
    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing");

    let go = env.member(thing, "Go").expect("Thing.Go");
    // The handle + index round-trips to a member named `Go`.
    assert!(matches!(
        env.member_at(thing, go),
        borzoi_assembly::Member::Method(m) if m.name == "Go"
    ));
    // The display name is the F# source name; a C# fixture has no
    // `[<CompiledName>]` rewrite, so it equals the IL name.
    assert_eq!(env.member_display_name(thing, go), "Go");

    // A member of the nested type.
    let inner = env.nested(thing, "Inner", 0).expect("Thing.Inner");
    assert!(env.member(inner, "Tick").is_some(), "Inner.Tick resolves");
}

#[test]
fn instance_data_member_ty_resolves_only_readable_data_members() {
    // The Stage-3.3a member-access filter: a *single unambiguous public instance
    // readable data member* (field / public-getter non-indexer property) resolves;
    // a write-only property, a *private-getter* property, an indexer, a static, a
    // non-public, and a method all defer. Exercised over the fixture's
    // `Demo.Widget`.
    let env = fixture_env();
    let widget = env
        .lookup_type(&ns(&["Demo"]), "Widget", 0)
        .expect("Demo.Widget");

    // Readable data members resolve, to their declared types.
    for (name, ty) in [
        ("Count", "System.Int32"),
        ("Name", "System.String"),
        ("ReadOnly", "System.Int32"),
    ] {
        let member_ty = env
            .instance_data_member_ty(widget, name)
            .unwrap_or_else(|| panic!("{name} should resolve"));
        assert_eq!(render_type_ref(member_ty), ty, "{name}");
    }

    // Everything else defers (D5 silence): a set-only property is unreadable, an
    // indexer carries an index parameter, statics need no receiver, non-public
    // members are inaccessible cross-assembly, and a method is not a data member.
    for name in [
        "WriteOnly",
        "PrivGet",
        "Item",
        "StaticCount",
        "StaticProp",
        "Secret",
        "Hidden",
        "Go",
        "Absent",
    ] {
        assert!(
            env.instance_data_member_ty(widget, name).is_none(),
            "{name} must defer"
        );
    }
}

#[test]
fn instance_data_member_returns_index_and_type() {
    // Stage 3.3b: `instance_data_member` selects exactly the same member as
    // `instance_data_member_ty` (a single unambiguous public instance readable
    // data member) and additionally returns its `MemberIndex`, round-tripping to
    // the named member so a consumer can build a `Resolution::Member`.
    let env = fixture_env();
    let widget = env
        .lookup_type(&ns(&["Demo"]), "Widget", 0)
        .expect("Demo.Widget");
    let (decl, idx, ty) = env
        .instance_data_member(widget, "Count")
        .expect("Count resolves");
    assert_eq!(render_type_ref(ty), "System.Int32");
    // `Count` is declared on `Widget` itself (not inherited).
    assert_eq!(decl, widget);
    // The index names the `Count` member.
    assert_eq!(env.member_display_name(widget, idx), "Count");
    // A member that `instance_data_member_ty` declines (a method) declines here too.
    assert!(env.instance_data_member(widget, "Go").is_none());
}

#[test]
fn public_instance_member_names_lists_completable_members() {
    // Stage 3.3b dot-completion candidate set: the public instance readable
    // *fields, non-indexer properties, and methods* of `Demo.Widget`. Excluded:
    // the write-only `WriteOnly`, the private-getter `PrivGet`, the indexer
    // `Item`, the statics `StaticCount`/`StaticProp`, and the non-public
    // `Secret`/`Hidden`. Methods (`Go`) *are* included — a completion list is a
    // set of callable candidates.
    let env = fixture_env();
    let widget = env
        .lookup_type(&ns(&["Demo"]), "Widget", 0)
        .expect("Demo.Widget");
    let mut names = env.public_instance_member_names(widget);
    names.sort_unstable();
    assert_eq!(names, vec!["Count", "Go", "Name", "ReadOnly"]);
}

#[test]
fn instance_method_resolves_single_candidate() {
    // Stage 3.3d: a single non-overloaded, non-generic public instance method
    // resolves to its `(MemberIndex, return type)`. Over the fixture's `Demo.Gizmo`.
    let env = fixture_env();
    let gizmo = env
        .lookup_type(&ns(&["Demo"]), "Gizmo", 0)
        .expect("Demo.Gizmo");

    let (decl, idx, ty, params) = env.instance_method(gizmo, "Ping").expect("Ping resolves");
    assert_eq!(render_type_ref(ty), "System.Int32");
    assert_eq!(params, 0, "Ping is parameterless");
    assert_eq!(decl, gizmo, "Ping is declared on Gizmo itself");
    assert_eq!(env.member_display_name(gizmo, idx), "Ping");

    let (_, _, ty, params) = env.instance_method(gizmo, "Label").expect("Label resolves");
    assert_eq!(render_type_ref(ty), "System.String");
    assert_eq!(params, 0, "Label is parameterless");
}

#[test]
fn instance_method_defers_overloaded_generic_static_and_absent() {
    // An overloaded method (`Over`), a generic method (`Echo<T>`), a static method
    // (`Stat`), and an absent name all defer (D5) — none is a single-candidate,
    // non-generic public *instance* method.
    let env = fixture_env();
    let gizmo = env
        .lookup_type(&ns(&["Demo"]), "Gizmo", 0)
        .expect("Demo.Gizmo");
    for name in ["Over", "Echo", "Stat", "Absent"] {
        assert!(
            env.instance_method(gizmo, name).is_none(),
            "{name} must defer"
        );
    }
}

#[test]
fn instance_method_finds_void_method() {
    // A `void` instance method IS selected (returning `System.Void`): the *wake*
    // records the member's identity but defers the `unit` type it cannot model, so
    // the selection must still surface the method (hover / go-to-def on its name).
    let env = fixture_env();
    let gizmo = env
        .lookup_type(&ns(&["Demo"]), "Gizmo", 0)
        .expect("Demo.Gizmo");
    let (_, idx, ty, params) = env.instance_method(gizmo, "Act").expect("Act is found");
    assert_eq!(render_type_ref(ty), "System.Void");
    assert_eq!(params, 0, "Act is parameterless");
    assert_eq!(env.member_display_name(gizmo, idx), "Act");
}

#[test]
fn instance_method_excludes_data_members_and_constructors() {
    // On `Widget`: a field (`Count`) and a property (`Name`) are not methods, and a
    // constructor (`.ctor`) is excluded; the void instance method `Go` IS selected.
    let env = fixture_env();
    let widget = env
        .lookup_type(&ns(&["Demo"]), "Widget", 0)
        .expect("Demo.Widget");
    assert!(
        env.instance_method(widget, "Count").is_none(),
        "a field is not a method"
    );
    assert!(
        env.instance_method(widget, "Name").is_none(),
        "a property is not a method"
    );
    assert!(
        env.instance_method(widget, "Go").is_some(),
        "a single void instance method is selected"
    );
    assert!(
        env.instance_method(widget, ".ctor").is_none(),
        "a constructor is excluded"
    );
}

// ===== Stage 3.x-inh: base-class walk =====

#[test]
fn instance_method_resolves_inherited_single_candidate() {
    // Stage 3.x-inh: a method declared on a *base* (`Demo.Base.Inherited`) resolves
    // through the derived receiver — the group is collected across `Derived → Base`
    // and has a single candidate — returned under its **declaring** base's handle.
    let env = fixture_env();
    let base = env.lookup_type(&ns(&["Demo"]), "Base", 0).expect("Base");
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");

    let (decl, idx, ty, params) = env
        .instance_method(derived, "Inherited")
        .expect("inherited method resolves");
    assert_eq!(render_type_ref(ty), "System.Int32");
    assert_eq!(params, 0);
    assert_eq!(decl, base, "the method is declared on the base");
    assert_eq!(env.member_display_name(base, idx), "Inherited");

    // A method declared on the derived type itself still resolves (declared there).
    let (decl_own, _, _, _) = env.instance_method(derived, "Own").expect("Own resolves");
    assert_eq!(decl_own, derived);
}

#[test]
fn instance_data_member_resolves_inherited() {
    // An *inherited* data member (`Demo.Base.BaseField`) resolves through the derived
    // receiver, returned under the base's handle.
    let env = fixture_env();
    let base = env.lookup_type(&ns(&["Demo"]), "Base", 0).expect("Base");
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");

    let (decl, idx, ty) = env
        .instance_data_member(derived, "BaseField")
        .expect("inherited field resolves");
    assert_eq!(render_type_ref(ty), "System.Int32");
    assert_eq!(decl, base, "the field is declared on the base");
    assert_eq!(env.member_display_name(base, idx), "BaseField");
}

#[test]
fn instance_method_resolves_overridden_method() {
    // OV-3: `Named` is `virtual string` on Base and `override string` on Derived —
    // it appears at *two* levels with the same partial signature (no params). The
    // partial-signature dedup collapses it to the nearest level (Derived), so the
    // overridden single method now **resolves** to its return type — relaxing the
    // 3.x-inh "no cross-level dedup" deferral. (Flipped from the old
    // `instance_method_defers_overridden_method` behaviour.)
    let env = fixture_env();
    let base = env.lookup_type(&ns(&["Demo"]), "Base", 0).expect("Base");
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    let (decl, idx, ty, params) = env
        .instance_method(derived, "Named")
        .expect("an overridden single method now resolves");
    assert_eq!(render_type_ref(ty), "System.String");
    assert_eq!(params, 0);
    assert_eq!(
        decl, derived,
        "the nearest (overriding) declaration wins the dedup"
    );
    assert_ne!(decl, base);
    assert_eq!(env.member_display_name(derived, idx), "Named");
}

#[test]
fn instance_method_defers_bounded_vs_plain_array_overload() {
    // OV-3 array-shape key: a bounded ECMA array (`int[10..]`, with `sizes`) is a
    // distinct signature from a plain vector (`int[]`); the key must not drop the
    // array bounds and collapse them. Synthesised across Base/Derived: Base declares
    // `Arr(int[])`, Derived `Arr(int[10..])` — distinct ⇒ overload ⇒ defers.
    fn arr_param(sizes: Vec<u32>) -> Parameter {
        Parameter {
            name: Some("a".to_string()),
            ty: TypeRef::Array {
                element: Box::new(NullableType::oblivious(TypeRef::Primitive(Primitive::I4))),
                rank: 1,
                sizes,
                lower_bounds: vec![],
            },
            is_byref: false,
            is_out: false,
            is_readonly_ref: false,
            default: ParamDefault::None,
            is_param_array: false,
            nullability: Nullability::Oblivious,
        }
    }
    let mut entities = fixture_entities();
    // Clone `Demo.Derived.Own` as a well-formed method template.
    let template = entities
        .iter()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Derived")
        .and_then(|d| {
            d.members.iter().find_map(|m| match m {
                Member::Method(mm) if mm.name == "Own" => Some(mm.clone()),
                _ => None,
            })
        })
        .expect("Own template");
    let make = |sizes: Vec<u32>| {
        let mut m = template.clone();
        m.name = "Arr".to_string();
        m.source_name = None;
        m.signature.parameters = vec![arr_param(sizes)];
        Member::Method(m)
    };
    for e in &mut entities {
        if e.namespace == ns(&["Demo"]) && e.name == "Base" {
            e.members.push(make(vec![])); // Arr(int[])
        }
        if e.namespace == ns(&["Demo"]) && e.name == "Derived" {
            e.members.push(make(vec![10])); // Arr(int[10..])
        }
    }
    let env = AssemblyEnv::from_entities(entities);
    let d = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    assert!(
        env.instance_method(d, "Arr").is_none(),
        "a bounded array and a plain vector are distinct signatures ⇒ must not collapse"
    );
}

#[test]
fn instance_method_defers_same_level_duplicate_signature() {
    // OV-3: the cross-level hiding rule must NOT collapse two public instance methods
    // that share a partial signature on the *same* declaring level (raw IL can carry
    // MethodDefs differing only by return type). They are an ambiguous group ⇒ defer.
    // Synthesised by cloning `Demo.Derived.Own` back onto its own entity, so the level
    // holds two members with an identical partial key.
    let mut entities = fixture_entities();
    let derived = entities
        .iter_mut()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Derived")
        .expect("Demo.Derived");
    let own = derived
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm) if mm.name == "Own" => Some(mm.clone()),
            _ => None,
        })
        .expect("Own method");
    derived.members.push(Member::Method(own));
    let env = AssemblyEnv::from_entities(entities);
    let d = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    assert!(
        env.instance_method(d, "Own").is_none(),
        "two same-level methods sharing a partial signature are ambiguous ⇒ defer (no collapse)"
    );
}

#[test]
fn instance_method_resolves_renamed_method_by_source_name() {
    // OV-3 regression (review): a `[<CompiledName>]` / `CompilationSourceName`
    // instance method is referenced by its *source* name — callers pass the F#
    // identifier and the lookup matches via `member_name`. The group collection
    // must use the same source-name comparison, not the raw IL name, or a renamed
    // method resolves nowhere. Synthesised by renaming a fixture method's IL name
    // while giving it a distinct source name.
    let mut entities = fixture_entities();
    let base = entities
        .iter_mut()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Base")
        .expect("Demo.Base");
    for m in &mut base.members {
        if let Member::Method(mm) = m
            && mm.name == "Inherited"
        {
            mm.name = "Inherited_il".to_string();
            mm.source_name = Some("Renamed".to_string());
        }
    }
    let env = AssemblyEnv::from_entities(entities);
    let b = env.lookup_type(&ns(&["Demo"]), "Base", 0).expect("Base");
    assert!(
        env.instance_method(b, "Renamed").is_some(),
        "a renamed instance method resolves by its F# source name"
    );
    assert!(
        env.instance_method(b, "Inherited_il").is_none(),
        "the raw IL name is not the caller-facing source name"
    );
}

#[test]
fn instance_method_defers_byref_vs_byvalue_overload() {
    // OV-3 byref key: `RefClash` is `RefClash(int)` on Base and `RefClash(ref int)`
    // on Derived. The projector stores byref-ness on the parameter flag (leaving the
    // referent `int` on `p.ty`), so a naive key would collapse `int&` into `int` and
    // wrongly treat these as an override — publishing Derived's `string` return for a
    // by-value `d.RefClash(1)`. The partial key marks the byref referent distinctly,
    // so the two stay distinct signatures ⇒ overload ⇒ defers.
    let env = fixture_env();
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    assert!(
        env.instance_method(derived, "RefClash").is_none(),
        "a byref-vs-by-value overload split across the chain must not collapse"
    );
}

#[test]
fn instance_method_defers_overload_across_chain() {
    // `Clash` is `Clash(string)` on Base and `Clash(int)` on Derived — two *distinct*
    // signatures across the chain, so the complete group is an overload (2 members):
    // it defers (the B3 overload-resolution hard pile), which the 3.3d exact-entity
    // scan (seeing only Derived's `Clash(int)`) would have wrongly resolved.
    let env = fixture_env();
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    assert!(
        env.instance_method(derived, "Clash").is_none(),
        "an overload split across the base chain defers"
    );
}

#[test]
fn instance_method_defers_object_named_overload_when_object_absent() {
    // `Demo.Clashy` declares `Equals(int)`, an overload of `System.Object.Equals`.
    // Object is absent from this single-assembly fixture, so the `Equals` group is
    // *incomplete* (the inherited `object.Equals(object)` is invisible): the call
    // must DEFER rather than resolve the visible `Equals(int)` — else it would
    // over-type where FCS binds the inherited overload. A non-Object name (`Ping`) on
    // the same Object-capped type still resolves.
    let env = fixture_env();
    let clashy = env
        .lookup_type(&ns(&["Demo"]), "Clashy", 0)
        .expect("Clashy");
    assert!(
        env.instance_method(clashy, "Equals").is_none(),
        "an Object-method-named overload defers when Object is absent"
    );
    let (decl, _, ty, _) = env
        .instance_method(clashy, "Ping")
        .expect("a non-Object name resolves on an Object-capped type");
    assert_eq!(render_type_ref(ty), "System.Int32");
    assert_eq!(decl, clashy);
}

#[test]
fn instance_method_defers_when_base_assembly_name_mismatches() {
    // Base resolution honours assembly *name*: when the `by_type` slot for the base's
    // full name is a same-named type from the *wrong* assembly (here a ghost `Base`
    // tagged `GhostAssembly`, enumerated first), resolving `Derived`'s base — which
    // names its own (fixture) assembly — is a name mismatch, so the chain can't
    // complete and an inherited lookup defers rather than walk the wrong base. A type
    // whose base resolves cleanly (Gizmo → the absent Object) is unaffected.
    let env = env_with_ghost_type("Base");
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    assert!(
        env.instance_method(derived, "Inherited").is_none(),
        "an inherited lookup defers when the base's assembly name doesn't match the slot"
    );
    let gizmo = env.lookup_type(&ns(&["Demo"]), "Gizmo", 0).expect("Gizmo");
    assert!(
        env.instance_method(gizmo, "Ping").is_some(),
        "a type whose base resolves cleanly is unaffected"
    );
}

#[test]
fn instance_method_defers_method_field_name_clash() {
    // A public instance method and a public instance field sharing a name on ONE type
    // (illegal in C#, representable in metadata): F# resolves the member-kind clash by
    // precedence we don't model, so it defers. Injected by cloning `Widget`'s `Count`
    // field renamed to `Go` — `Widget` already declares a `Go()` method.
    use borzoi_assembly::Member;
    let mut entities = fixture_entities();
    let widget = entities
        .iter_mut()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Widget")
        .expect("Widget");
    let mut go_field = widget
        .members
        .iter()
        .find_map(|m| match m {
            Member::Field(f) if f.name == "Count" => Some(f.clone()),
            _ => None,
        })
        .expect("Count field");
    go_field.name = "Go".to_string();
    widget.members.push(Member::Field(go_field));
    let env = AssemblyEnv::from_entities(entities);
    let widget = env
        .lookup_type(&ns(&["Demo"]), "Widget", 0)
        .expect("Widget");
    assert!(
        env.instance_method(widget, "Go").is_none(),
        "a method/field name clash on one type defers"
    );
    // The data-member path likewise defers (two public members share the name).
    assert!(env.instance_data_member(widget, "Go").is_none());
}

#[test]
fn instance_data_member_resolves_own_member_over_incomplete_base() {
    // `DerivesGeneric : GenericBase<int>` — a closed generic base makes the chain
    // Incomplete. The receiver's OWN data member (`OwnField`) still resolves: it is
    // declared on the receiver, hides any inherited member, and needs no base walk (the
    // regression codex flagged). Its own method (`OwnMethod`) defers, since an inherited
    // same-arity overload from the unwalkable base can't be ruled out; and an inherited
    // member (`Stored`) defers.
    let env = fixture_env();
    let d = env
        .lookup_type(&ns(&["Demo"]), "DerivesGeneric", 0)
        .expect("DerivesGeneric");
    let (decl, _, ty) = env
        .instance_data_member(d, "OwnField")
        .expect("the receiver's own field resolves over an incomplete base");
    assert_eq!(render_type_ref(ty), "System.Int32");
    assert_eq!(decl, d, "resolved on the receiver itself");

    assert!(
        env.instance_method(d, "OwnMethod").is_none(),
        "a method call defers on an incomplete base (inherited overloads unknowable)"
    );
    assert!(
        env.instance_data_member(d, "Stored").is_none(),
        "an inherited member on an incomplete base defers"
    );
}

#[test]
fn base_walk_defers_generic_system_object_base() {
    // A base named `System.Object` but *generic* (`System.Object<T>` — not the
    // universal non-generic root; hand-written / corrupt metadata) must not cap the
    // chain as complete. With Gizmo's base mutated to a generic `System.Object`, its
    // own method `Ping` defers — the chain is Incomplete, not Object-capped.
    use borzoi_assembly::{NullableType, Primitive, TypeRef};
    let mut entities = fixture_entities();
    let gizmo = entities
        .iter_mut()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Gizmo")
        .expect("Gizmo");
    gizmo.base_type = Some(TypeRef::Named {
        assembly: None,
        namespace: ns(&["System"]),
        name: "Object".to_string(),
        type_args: vec![NullableType::oblivious(TypeRef::Primitive(Primitive::I4))],
        segment_arities: vec![1],
    });
    let env = AssemblyEnv::from_entities(entities);
    let gizmo = env.lookup_type(&ns(&["Demo"]), "Gizmo", 0).expect("Gizmo");
    assert!(
        env.instance_method(gizmo, "Ping").is_none(),
        "a generic System.Object base is not the universal root; the chain is incomplete"
    );
}

#[test]
fn base_walk_defers_when_a_skipped_member_may_hide() {
    // The reader drops undecodable members into `Entity::skipped_members`. A skipped
    // member of the name on a walked (nearer) level could hide or overload the
    // inherited one, so the walk — which scans only *decoded* members — must defer
    // rather than resolve a base member. Inject skipped `Inherited` / `BaseField` onto
    // `Derived` (which inherits both from `Base`); the lookups that resolved before now
    // defer.
    use borzoi_assembly::SkippedMember;
    let mut entities = fixture_entities();
    let derived = entities
        .iter_mut()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Derived")
        .expect("Derived");
    for name in ["Inherited", "BaseField"] {
        derived.skipped_members.push(SkippedMember {
            name: name.to_string(),
            reason: "test: undecodable".to_string(),
        });
    }
    let env = AssemblyEnv::from_entities(entities);
    let derived = env
        .lookup_type(&ns(&["Demo"]), "Derived", 0)
        .expect("Derived");
    assert!(
        env.instance_method(derived, "Inherited").is_none(),
        "a skipped nearer member may hide the inherited method — defer"
    );
    assert!(
        env.instance_data_member(derived, "BaseField").is_none(),
        "a skipped nearer member may hide the inherited field — defer"
    );
}

#[test]
fn instance_method_resolves_past_a_same_name_static_overload() {
    // `Pick` has an instance overload and a static overload on the same type. The
    // static coexists with the instance method (an overload set) rather than hiding
    // it, and F# ignores the static for an instance call — so the single instance
    // candidate resolves. A same-name static at the owning level must not cancel the
    // instance lookup (contrast `HideDerived`, whose static is the *only* member of
    // the name at its level).
    let env = fixture_env();
    let t = env
        .lookup_type(&ns(&["Demo"]), "StaticOverload", 0)
        .expect("StaticOverload");
    let (decl, _, ty, params) = env
        .instance_method(t, "Pick")
        .expect("a same-name static overload does not cancel the instance method");
    assert_eq!(render_type_ref(ty), "System.Int32");
    assert_eq!(params, 1, "the instance overload takes one parameter");
    assert_eq!(decl, t);
}

#[test]
fn base_walk_defers_when_a_static_hides_an_inherited_instance_member() {
    // Name hiding is by name across static/instance: `HideDerived`'s public *static*
    // `Prop` / `Meth` hide the inherited public *instance* members of `HideBase`, but
    // cannot be reached through a value receiver, so both a data access and a method
    // call must DEFER (FCS leaves them `obj`) — not fall through to the hidden base.
    let env = fixture_env();
    let base = env
        .lookup_type(&ns(&["Demo"]), "HideBase", 0)
        .expect("HideBase");
    let derived = env
        .lookup_type(&ns(&["Demo"]), "HideDerived", 0)
        .expect("HideDerived");

    // Control: on the base itself the instance members resolve normally.
    assert!(env.instance_data_member(base, "Prop").is_some());
    assert!(env.instance_method(base, "Meth").is_some());

    // On the derived, the same-name static hides them — both defer.
    assert!(
        env.instance_data_member(derived, "Prop").is_none(),
        "a derived static hides the inherited instance data member"
    );
    assert!(
        env.instance_method(derived, "Meth").is_none(),
        "a derived static hides the inherited instance method"
    );
}

/// A tiny canonical render of the `TypeRef`s this fixture uses (BCL primitives),
/// enough to assert the resolved member type without depending on the assembly
/// crate's private renderer.
fn render_type_ref(ty: &borzoi_assembly::TypeRef) -> &'static str {
    use borzoi_assembly::{Primitive, TypeRef};
    match ty {
        TypeRef::Primitive(Primitive::I4) => "System.Int32",
        TypeRef::Primitive(Primitive::String) => "System.String",
        TypeRef::Primitive(Primitive::Void) => "System.Void",
        other => panic!("unexpected fixture member type: {other:?}"),
    }
}

#[test]
fn absent_names_miss_rather_than_guess() {
    let env = fixture_env();
    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing");

    assert!(env.lookup_type(&ns(&["Demo"]), "Nope", 0).is_none());
    assert!(env.lookup_type(&ns(&["Nope"]), "Thing", 0).is_none());
    assert!(env.nested(thing, "Nope", 0).is_none());
    assert!(env.member(thing, "Nope").is_none());
}

#[test]
fn from_views_matches_from_entities() {
    let bytes = std::fs::read(ensure_fixture_built()).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("from_views");
    // The realistic shell path resolves the same known type.
    assert!(env.lookup_type(&ns(&["Demo"]), "Thing", 0).is_some());
    assert_eq!(env.len(), fixture_env().len());
}

/// Headline property: *every* entity `EcmaView` enumerates — top-level via the
/// `(namespace, name, arity)` index, nested via descent from its encloser — is
/// reachable, and the round-trip returns a type with the same identity
/// (namespace, name, and generic arity).
#[test]
fn every_enumerated_entity_is_reachable() {
    let entities = fixture_entities();
    let env = AssemblyEnv::from_entities(entities.clone());
    assert!(!entities.is_empty(), "fixture produced no types");

    for e in &entities {
        let handle = env
            .lookup_type(&e.namespace, &e.name, e.generic_parameters.len())
            .unwrap_or_else(|| panic!("top-level {:?}.{} not reachable", e.namespace, e.name));
        assert_same_identity(&env, handle, e);
        check_nested_reachable(&env, handle, &e.nested_types);
    }
}

fn check_nested_reachable(env: &AssemblyEnv, parent: EntityHandle, nested: &[Entity]) {
    for n in nested {
        let handle = env
            .nested(parent, &n.name, n.generic_parameters.len())
            .unwrap_or_else(|| panic!("nested type {} not reachable by descent", n.name));
        assert_same_identity(env, handle, n);
        check_nested_reachable(env, handle, &n.nested_types);
    }
}

fn assert_same_identity(env: &AssemblyEnv, handle: EntityHandle, expected: &Entity) {
    let got = env.entity(handle);
    assert_eq!(got.name, expected.name);
    assert_eq!(got.namespace, expected.namespace);
    assert_eq!(
        got.generic_parameters.len(),
        expected.generic_parameters.len(),
        "arity mismatch for {:?}.{}",
        expected.namespace,
        expected.name
    );
}

#[test]
fn open_static_entries_are_distinct_and_public_only() {
    // `Demo.Calc` has public statics `Zero`, `Answer`, and an overloaded `Twice`
    // (two public statics), plus an *internal* `Hush`. The opened-name set must
    // list each public static name exactly once — the unique ones carrying their
    // member index, the overloaded `Twice` carrying `None` (in scope, deferred) —
    // and omit the internal one: exactly what `open type Demo.Calc` brings into
    // scope.
    let env = fixture_env();
    let calc = env
        .lookup_type(&ns(&["Demo"]), "Calc", 0)
        .expect("Demo.Calc in env");
    let mut entries = env.open_static_entries(calc);
    entries.sort_unstable_by_key(|(name, _)| *name);
    let names: Vec<&str> = entries.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, vec!["Answer", "Twice", "Zero"]);
    let unique: Vec<bool> = entries.iter().map(|(_, idx)| idx.is_some()).collect();
    assert_eq!(
        unique,
        vec![true, false, true],
        "the overloaded `Twice` is not uniquely selectable; `Answer`/`Zero` are"
    );

    // The same names round-trip through the *qualified* lookups, which differ only
    // in their extension rules (`Demo.Calc` declares none).
    assert!(env.static_member(calc, "Zero").is_some());
    assert!(env.static_member(calc, "Answer").is_some());
    assert!(env.static_member(calc, "Twice").is_none());
    assert_eq!(
        env.static_lookup(calc, "Twice"),
        StaticLookup::Uncertain,
        "the overloaded `Twice` is occupied but not uniquely selectable"
    );
}

/// `static_lookup` answers the whole *qualified* channel — selection **and**
/// path-ownership — so a name a qualified path cannot select, but FCS's lookup still
/// reaches, is [`StaticLookup::Uncertain`], not [`StaticLookup::Absent`]. `Absent` is
/// reserved for "no member of this name is here at all", which is the only case where
/// a lower-priority tier may re-root the path (`resolve/assembly.rs`).
///
/// `Demo.Thing` has an instance method `Go` and an instance field `Value`, and no
/// statics: a type-qualified `Demo.Thing.Go` selects nothing, yet FCS's member lookup
/// is kind-agnostic — it *does* find the name, and errors on it rather than re-rooting
/// the path at a lower-priority reading (probed 2026-07-10, OV-7 review round 2). So
/// the path is occupied, and we defer rather than fall through.
#[test]
fn static_lookup_occupies_a_name_it_cannot_select() {
    let env = fixture_env();
    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing in env");

    for name in ["Go", "Value"] {
        assert_eq!(
            env.static_lookup(thing, name),
            StaticLookup::Uncertain,
            "`{name}` is an instance member: unselectable through a qualified path, \
             but it occupies the name, so the path is owned and defers"
        );
        assert!(
            env.static_member(thing, name).is_none(),
            "`{name}` is still not *selectable* as a static"
        );
    }

    assert_eq!(
        env.static_lookup(thing, "Nope"),
        StaticLookup::Absent,
        "a name on no member of the type — nor of its base chain — is genuinely absent, \
         and only that lets a lower-priority reading own the path"
    );

    // A CLASS receiver owns `Object`'s member names through the base chain: FCS's
    // type-qualified lookup is inheritance-aware, and it errors on (or resolves) an
    // inherited member rather than re-rooting the path (probed 2026-07-10). This is
    // exactly the rule that must NOT leak to module receivers (the test below).
    assert_eq!(
        env.static_lookup(thing, "ToString"),
        StaticLookup::Uncertain,
        "`ToString` is inherited from `Object`: a class-qualified path is occupied by it"
    );
}

/// A **module** receiver takes FCS's *module* lookup, not its type-member lookup:
/// `ResolveExprLongIdentInModuleOrNamespace` (NameResolution.fs) consults the
/// module's own contents — vals, exception constructors, union cases, nested
/// types, submodules — and never the compiled class's base chain, so `Object`'s
/// members do NOT occupy a module-qualified name. On no match FCS razes
/// `UndefinedName`, and `AtMostOneResultQuery` lets the *type* search re-root the
/// path — which is how `String.Equals` under `open System; open
/// Microsoft.FSharp.Core` is `System.String.Equals(…)`, not the FSharp.Core
/// `String` module (the `resolve_string_qualifier_repro` divergence: the old
/// base-chain rule made `Equals` "occupied" via `Object`, so the module reading
/// wrongly owned the path and the `open System` tier was never consulted).
#[test]
fn static_lookup_on_a_module_ignores_object_members() {
    let dll = crate::common::ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view))
        .expect("FSharp.Core must project end-to-end into an AssemblyEnv");
    let string_module = env
        .lookup_type(&ns(&["Microsoft", "FSharp", "Core"]), "String", 0)
        .expect("the FSharp.Core `String` module in env");
    assert_eq!(env.entity(string_module).kind, EntityKind::Module);

    // `Object`'s public members — instance (`Equals(obj)`, `ToString`,
    // `GetHashCode`, `GetType`) and static (`Equals(obj, obj)`,
    // `ReferenceEquals`) alike — are unreachable through a module qualifier, so
    // the name is genuinely absent and a lower-priority reading may own the path.
    for name in [
        "Equals",
        "ToString",
        "GetHashCode",
        "GetType",
        "ReferenceEquals",
    ] {
        assert_eq!(
            env.static_lookup(string_module, name),
            StaticLookup::Absent,
            "`{name}` is an `Object` member: FCS's in-module lookup cannot reach it, \
             so it must not occupy the module-qualified name"
        );
    }

    // The module's own vals still resolve (`String.length` — a
    // `[<CompiledName>]`-renamed method, so this also pins the source-name
    // matching) and still occupy names they cannot uniquely select.
    assert!(matches!(
        env.static_lookup(string_module, "length"),
        StaticLookup::Resolved(_)
    ));
    // The val's *source* name is `length`; its IL name `Length` is not an F#
    // name at this position (FCS's `AllValsByLogicalName` is keyed by logical
    // name), so it must be absent — the same rule that lets `String.Concat`
    // re-root to `System.String.Concat` past the module's `concat`.
    assert_eq!(
        env.static_lookup(string_module, "Length"),
        StaticLookup::Absent,
    );
}

fn entities_of(dll: &Path) -> Vec<Entity> {
    let bytes = std::fs::read(dll).expect("read dll");
    Ecma335Assembly::parse(&bytes)
        .expect("parse dll")
        .enumerate_type_defs()
        .expect("enumerate dll types")
}

#[test]
fn from_assemblies_tags_each_entity_with_its_source_dll() {
    // Two distinct assemblies in one env: every entity must report the DLL it
    // came from, and a nested type inherits its encloser's assembly.
    let asm1 = ensure_fixture_built();
    let asm2 = crate::common::ensure_autoopen_fixture_built();
    let env = AssemblyEnv::from_assemblies(vec![
        (asm1.to_path_buf(), entities_of(asm1)),
        (asm2.to_path_buf(), entities_of(asm2)),
    ]);

    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing (assembly 1)");
    assert_eq!(env.assembly_path(thing), Some(asm1));

    // A nested type reports its enclosing assembly, not `None`.
    let inner = env.nested(thing, "Inner", 0).expect("Thing.Inner");
    assert_eq!(env.assembly_path(inner), Some(asm1));

    let core_ops = env
        .lookup_type(&ns(&["Microsoft", "FSharp", "Core"]), "CoreOps", 0)
        .expect("Microsoft.FSharp.Core.CoreOps (assembly 2)");
    assert_eq!(env.assembly_path(core_ops), Some(asm2));
}

#[test]
fn from_entities_has_no_provenance() {
    // The provenance-free constructors leave every entity's source path unknown.
    let env = fixture_env();
    let thing = env
        .lookup_type(&ns(&["Demo"]), "Thing", 0)
        .expect("Demo.Thing");
    assert_eq!(env.assembly_path(thing), None);
}

// ===== Assembly-level AutoOpen deref is per-assembly (codex P2 on A3/S3) =====
//
// FCS dereferences an `[<assembly: AutoOpen("path")>]` within the
// *contributing* CCU and warns-and-skips when absent there
// (`ApplyAssemblyLevelAutoOpenAttributeToTcEnv`). An env-wide existence check
// would let a stale attribute in one assembly implicitly open a namespace
// that only *another* assembly declares — bare names from that namespace's
// auto-open modules would then wrongly resolve where FCS errors.

#[test]
fn stale_auto_open_path_naming_another_assemblys_namespace_is_dropped() {
    // The C# fixture (asm1) claims AutoOpen("Demo.Auto") — a namespace only
    // the autoopen fixture (asm2) declares. It must not survive processing.
    let asm1 = ensure_fixture_built();
    let asm2 = crate::common::ensure_autoopen_fixture_built();
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            asm1.to_path_buf(),
            entities_of(asm1),
            AbbreviationVisibility::Modelled,
            vec!["Demo.Auto".to_string()],
        ),
        (
            asm2.to_path_buf(),
            entities_of(asm2),
            AbbreviationVisibility::Modelled,
            vec![],
        ),
    ]);
    assert!(
        !env.implicit_open_namespace_paths()
            .contains(&ns(&["Demo", "Auto"])),
        "a stale AutoOpen must not deref in a sibling assembly (got {:?})",
        env.implicit_open_namespace_paths()
    );
}

#[test]
fn auto_open_path_derefs_within_its_own_assembly() {
    // The same path contributed by the assembly that DOES declare it survives.
    let asm1 = ensure_fixture_built();
    let asm2 = crate::common::ensure_autoopen_fixture_built();
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            asm1.to_path_buf(),
            entities_of(asm1),
            AbbreviationVisibility::Modelled,
            vec![],
        ),
        (
            asm2.to_path_buf(),
            entities_of(asm2),
            AbbreviationVisibility::Modelled,
            vec!["Demo.Auto".to_string()],
        ),
    ]);
    assert!(
        env.implicit_open_namespace_paths()
            .contains(&ns(&["Demo", "Auto"])),
        "an AutoOpen path declared by its own assembly must survive (got {:?})",
        env.implicit_open_namespace_paths()
    );
}

#[test]
fn duplicate_auto_open_path_moves_to_the_end() {
    // FCS applies every assembly-level AutoOpen in sequence, so a duplicate
    // re-establishes its namespace's latest-open precedence (fsi-verified:
    // with auto-open modules in A and B both exporting `marker`,
    // `AutoOpen("A"); AutoOpen("B")` binds B's, and
    // `AutoOpen("A"); AutoOpen("B"); AutoOpen("A")` binds A's). The processed
    // list must therefore keep the LAST occurrence's position, not the first
    // (codex P2, round 2).
    let asm = crate::common::ensure_autoopen_fixture_built();
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        asm.to_path_buf(),
        entities_of(asm),
        AbbreviationVisibility::Modelled,
        vec![
            "Demo.Auto".to_string(),
            "Sub".to_string(),
            "Demo.Auto".to_string(),
        ],
    )]);
    assert_eq!(
        env.implicit_open_namespace_paths(),
        [ns(&["Sub"]), ns(&["Demo", "Auto"])],
        "the duplicate Demo.Auto must re-establish latest precedence (after Sub)"
    );
}

#[test]
fn contested_auto_open_namespace_is_dropped_entirely() {
    // FCS opens the contributing CCU's namespace ENTITY — a sibling
    // assembly's same-named namespace stays closed (fsi-verified: a stand-in
    // `[<AutoOpen>]` module under `Microsoft.FSharp.Core` does not
    // bare-resolve next to real FSharp.Core). Our open machinery is
    // path-based, so a contested namespace cannot be applied faithfully and
    // is dropped entirely (codex P2, round 3) — deferrals where FCS
    // resolves, never a new wrong resolution.
    let asm = crate::common::ensure_autoopen_fixture_built();
    let entities = entities_of(asm);
    // The sibling: any public type retagged into the AutoOpen'd namespace
    // under a different assembly identity.
    let mut sibling = entities
        .iter()
        .find(|e| e.name == "ManifestOps")
        .expect("fixture declares ManifestOps")
        .clone();
    sibling.assembly.name = "Sibling".to_string();
    sibling.name = "SiblingMarker".to_string();
    sibling.is_auto_open = false;
    sibling.members = Vec::new();
    sibling.nested_types = Vec::new();
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            asm.to_path_buf(),
            entities,
            AbbreviationVisibility::Modelled,
            vec!["SemaAutoOpen.FromManifest".to_string()],
        ),
        (
            PathBuf::from("sibling.dll"),
            vec![sibling],
            AbbreviationVisibility::Modelled,
            vec![],
        ),
    ]);
    assert!(
        !env.implicit_open_namespace_paths()
            .contains(&ns(&["SemaAutoOpen", "FromManifest"])),
        "a namespace a sibling assembly also declares must not be implicitly opened (got {:?})",
        env.implicit_open_namespace_paths()
    );
}

// ── OV-0.5: the F#-native extension-member name index ────────────────────────

/// `(namespace, name, arity)` of the first fixture entity, after attaching a
/// synthetic extension-member index to it — so the query tests are robust to the
/// fixture's exact contents.
fn fixture_with_extension_index(names: &[&str]) -> (Vec<Entity>, Vec<String>, String, usize) {
    let mut entities = fixture_entities();
    entities[0].extension_member_names = names.iter().map(|s| s.to_string()).collect();
    let namespace = entities[0].namespace.clone();
    let name = entities[0].name.clone();
    let arity = entities[0].generic_parameters.len();
    (entities, namespace, name, arity)
}

#[test]
fn module_extension_members_known_exposes_the_index() {
    // A `Modelled`-visibility env (the `from_entities` default) surfaces the
    // module's `extension_member_names` verbatim as `Known`.
    let (entities, namespace, name, arity) = fixture_with_extension_index(&["Twice", "GenericExt"]);
    let env = AssemblyEnv::from_entities(entities);
    let handle = env
        .lookup_type(&namespace, &name, arity)
        .expect("entity[0]");
    match env.module_extension_members(handle) {
        ExtensionMembers::Known(members) => {
            assert_eq!(members, ["Twice".to_string(), "GenericExt".to_string()]);
        }
        ExtensionMembers::Unknowable => panic!("from_entities is Modelled ⇒ Known"),
    }
}

#[test]
fn module_extension_members_known_empty_for_a_module_with_none() {
    // The common case: a module that declares no extension members reports a
    // *known-empty* set — distinct from `Unknowable`, so the gate can proceed.
    let env = fixture_env();
    let handle = {
        let e = fixture_entities();
        env.lookup_type(&e[0].namespace, &e[0].name, e[0].generic_parameters.len())
            .expect("entity[0]")
    };
    assert!(matches!(
        env.module_extension_members(handle),
        ExtensionMembers::Known([])
    ));
}

#[test]
fn module_extension_members_unknowable_when_signature_data_is() {
    // Soundness: an `Unknowable`-visibility assembly (its pickle failed to
    // decode, or it embeds foreign CCUs) may declare extension members in
    // modules the host pickle never described, so the query must report
    // `Unknowable` — the gate defers — *even though* this entity happens to carry
    // a (necessarily partial) index. Trusting the partial list would let the gate
    // wrongly conclude "no extension member named M".
    let (entities, namespace, name, arity) = fixture_with_extension_index(&["Twice"]);
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("Ghost.dll"),
        entities,
        AbbreviationVisibility::Unknowable,
        Vec::new(),
    )]);
    let handle = env
        .lookup_type(&namespace, &name, arity)
        .expect("entity[0]");
    assert!(
        matches!(
            env.module_extension_members(handle),
            ExtensionMembers::Unknowable
        ),
        "an Unknowable-visibility assembly must not trust its partial index"
    );
}

/// The `Augmentation::Possible` path — the one the FCS matrix cannot reach, because
/// it needs an image whose pickle does *not* decode (the projector's IL dot-name
/// fallback).
///
/// fsc mangles an augmentation's compiled name to `Type.Member`, but
/// `[<CompiledName("A.B")>]` on an ordinary `let` produces the very same IL name —
/// legal, and FCS resolves it normally (fsi-verified). With no pickle we cannot tell
/// them apart, and *both* guesses are wrong resolutions: hiding loses a value FCS
/// resolves, surfacing gains a member FCS hides. So the member enters scope but names
/// no target — it shadows by position and resolves to nothing (codex review round 2).
#[test]
fn an_undecidable_dotted_module_member_defers_rather_than_hides_or_resolves() {
    let mut entities = fixture_entities();
    let template = entities
        .iter()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Calc")
        .and_then(|c| {
            c.members.iter().find_map(|m| match m {
                Member::Method(mm) if mm.name == "Zero" => Some(mm.clone()),
                _ => None,
            })
        })
        .expect("Demo.Calc.Zero template");

    let mut dotted = template.clone();
    dotted.name = "String.Mangled".to_string();
    dotted.source_name = Some("Mangled".to_string());
    dotted.augmentation = Augmentation::Possible;

    let mut module = entities
        .iter()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Calc")
        .cloned()
        .expect("Calc");
    module.name = "Undecidable".to_string();
    module.kind = EntityKind::Module;
    module.members = vec![Member::Method(dotted)];
    entities.push(module);

    let env = AssemblyEnv::from_entities(entities);
    let handle = env
        .lookup_type(&ns(&["Demo"]), "Undecidable", 0)
        .expect("Demo.Undecidable");

    // Bare scope: the name is present (so it shadows an earlier open by position)…
    let entries = env.open_static_entries(handle);
    let entry = entries
        .iter()
        .find(|(name, _)| *name == "Mangled")
        .expect("an undecidable member still occupies its bare name");
    // …but names no target.
    assert_eq!(
        entry.1, None,
        "an undecidable dotted member must defer, not resolve"
    );

    // Qualified: the name is OCCUPIED but has no nameable target — `Uncertain`, not
    // `Absent`. The distinction is load-bearing: reporting it absent would let a
    // lower-priority `open` re-root the path and resolve it to a *different*
    // module's same-named member (review round 3), a wrong target where the honest
    // answer is a deferral.
    assert_eq!(
        env.static_lookup(handle, "Mangled"),
        StaticLookup::Uncertain,
        "a module-qualified undecidable member is occupied-but-undecidable, not absent"
    );
    assert_eq!(env.static_member(handle, "Mangled"), None);
}

/// Review round 3 (Slice A): a **nested** module has no namespace of its own — a nested
/// ECMA TypeDef carries none — while a *dropped* nested type is recorded under the
/// top-level encloser's namespace. Asking `entity.namespace` (empty!) therefore declared
/// such a module "fully enumerable" even when projection had dropped one of its
/// children, so an earlier open's colliding value could resolve where FCS binds the
/// dropped type. The node now inherits its owning namespace, as it already does the
/// assembly's unknowable-pickle flag.
#[test]
fn a_nested_module_reads_dropped_types_from_its_owning_namespace() {
    let mut outer = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == ns(&["Demo"]) && e.name == "Calc")
        .expect("Demo.Calc");
    outer.name = "Outer".to_string();
    outer.kind = EntityKind::Module;
    outer.members = vec![];

    let mut inner = outer.clone();
    inner.name = "Inner".to_string();
    inner.namespace = vec![]; // a nested TypeDef declares no namespace of its own
    outer.nested_types = vec![inner];

    let mut env = AssemblyEnv::from_entities(vec![outer]);
    let outer_h = env
        .lookup_type(&ns(&["Demo"]), "Outer", 0)
        .expect("Demo.Outer");
    let inner_h = env.nested(outer_h, "Inner", 0).expect("Demo.Outer.Inner");

    // With nothing dropped, neither can hide a nested module.
    assert!(!env.module_may_hide_nested_modules(outer_h));
    assert!(!env.module_may_hide_nested_modules(inner_h));

    // A type dropped from the *top-level* namespace must make BOTH conservative — the
    // nested one included, which is the bug: its own `namespace` is empty.
    env.mark_namespace_dropped_type(ns(&["Demo"]));
    assert!(
        env.module_may_hide_nested_modules(outer_h),
        "the top-level module sees the drop"
    );
    assert!(
        env.module_may_hide_nested_modules(inner_h),
        "the NESTED module must see it too — it inherits its owning namespace"
    );
}
