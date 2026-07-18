//! Differential test for the assembly reader.
//!
//! Phase 1 has no real backend on either side. The harness is exercised
//! end-to-end by hand-building both inputs:
//!
//! - a `Vec<Entity>` standing in for what the Rust importer will produce
//!   in phase 2;
//! - the equivalent JSON, standing in for what `tools/fcs-dump` will
//!   produce in phase 2.
//!
//! Both project to the same [`NormalisedAssembly`] tree, which is the
//! contract the phase-2 work must keep.

use borzoi_assembly::test_support::{normalise_entities, parse_fcs_dump};
use borzoi_assembly::{
    Access, AssemblyIdentity, Augmentation, DefaultMember, Ecma335Assembly, EcmaView, Entity,
    EntityKind, Field, IndexParameter, Member, MethodLike, MethodSignature, Nullability,
    NullableType, ParamDefault, Parameter, Primitive, Property, ResourceKind, TypeParameter,
    TypeRef, Variance, Version,
};

use crate::common::{
    ensure_literal_consts_built, ensure_measure_attr_args_built, ensure_minilib_built,
    ensure_minilib_fs_built, ensure_minilib_fs_ext_built, invoke_fcs_dump,
};

/// `System.Object`, trimmed to the members that exercise the model:
/// a virtual instance method (`Equals`), a static method
/// (`ReferenceEquals`), an unnamed-parameter case, a parameterless ctor.
/// Pins the "both projectors agree" baseline.
#[test]
fn diff_assembly_system_object() {
    let rust = fixture_system_object();
    let json = fixture_system_object_json();

    let rust_norm = normalise_entities("mscorlib", &rust);
    let fcs_norm = parse_fcs_dump(json);

    assert_eq!(
        rust_norm, fcs_norm,
        "normalised assemblies diverge.\n  rust: {rust_norm:#?}\n  fcs:  {fcs_norm:#?}",
    );
}

/// End-to-end diff against a real CSC-built `.dll`: our [`Ecma335Assembly`]
/// vs `fcs-dump entities`, both reading the MiniLib fixture.
///
/// MiniLib is a one-class C# library (`namespace MiniLib; public class
/// Counter {}`). Phase 2 only enumerates the type skeleton — namespace,
/// name, kind, access, base, interfaces, nested types — so the diff
/// covers exactly what both projectors are supposed to agree on at this
/// stage. Members are deliberately empty on both sides; phase 3 will
/// grow that side of the assertion.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn diff_assembly_minilib_one_class() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");

    // Rust side: parse the bytes through `Ecma335Assembly` → owned Entity tree.
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let rust_entities = view.enumerate_type_defs().expect("enumerate MiniLib types");
    let rust_norm = normalise_entities(&view.identity().name, &rust_entities);

    // FCS side: shell out to `fcs-dump entities <path>` and parse the JSON
    // through the same normaliser the in-memory fixtures use.
    let fcs_json = invoke_fcs_dump("entities", dll_path);
    let fcs_norm = parse_fcs_dump(&fcs_json);

    // Plain `assert_eq!` would print both trees; this format keeps the
    // failure diff readable without separate-line interleaving from
    // pretty-printing two big values.
    assert_eq!(
        rust_norm,
        fcs_norm,
        "MiniLib normalised assemblies diverge.\n\
         rust ({} entities): {:#?}\n\
         fcs  ({} entities): {:#?}\n",
        rust_norm.entities.len(),
        rust_norm,
        fcs_norm.entities.len(),
        fcs_norm,
    );
}

/// Absolute pin for phase B1's indexer projection. The differential test
/// (`diff_assembly_minilib_one_class`) proves both projectors *agree*, but
/// it would stay green even if both sides dropped the index dimension — so
/// this asserts the concrete Rust-side shape against the real DLL: the
/// `IndexerHost` entity carries `DefaultMember::Named("Item")` (Roslyn's
/// auto-emit for any type declaring an indexer), and its `Item` property
/// carries the index parameter type rather than collapsing to a zero-arg
/// property.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_indexer_projects_index_parameter() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let host = entities
        .iter()
        .find(|e| e.name == "IndexerHost")
        .expect("MiniLib must expose the IndexerHost fixture");

    assert_eq!(
        host.default_member,
        Some(DefaultMember::Named("Item".to_string())),
        "Roslyn auto-emits [DefaultMember(\"Item\")] on a type declaring an indexer",
    );

    let item = host
        .members
        .iter()
        .find_map(|m| match m {
            Member::Property(p) if p.name == "Item" => Some(p),
            _ => None,
        })
        .expect("IndexerHost must expose the `Item` indexer property");

    assert_eq!(
        item.parameters,
        vec![IndexParameter {
            name: Some("i".to_string()),
            ty: NullableType::oblivious(TypeRef::Primitive(Primitive::I4)),
            is_param_array: false,
        }],
        "the `int this[int i]` indexer must project a single named Int32 index parameter",
    );
    assert_eq!(item.ty, TypeRef::Primitive(Primitive::I4));
    assert!(item.has_getter && !item.has_setter);
}

/// Overloaded indexers (`this[int]` plus `this[string]`) are legal: both
/// accessors compile to a method named `get_Item`, so the property must be
/// bound to its accessor by signature, not name. The Rust importer resolves
/// this via ECMA-335 MethodSemantics tokens, so both `Item` overloads project
/// with their own index dimension. (The differential test exercises the
/// matching fcs-dump path, whose name-only accessor lookup would otherwise
/// crash on the colliding `get_Item` methods.)
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_overloaded_indexer_projects_both_dimensions() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let host = entities
        .iter()
        .find(|e| e.name == "OverloadedIndexerHost")
        .expect("MiniLib must expose the OverloadedIndexerHost fixture");

    assert_eq!(
        host.default_member,
        Some(DefaultMember::Named("Item".to_string())),
    );

    let mut indexers: Vec<(TypeRef, TypeRef)> = host
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Property(p) if p.name == "Item" => {
                assert_eq!(p.parameters.len(), 1, "each overload takes one index");
                // Outside a `#nullable` scope, the index dimension is Oblivious.
                assert_eq!(p.parameters[0].ty.nullability, Nullability::Oblivious);
                Some((p.parameters[0].ty.ty.clone(), p.ty.clone()))
            }
            _ => None,
        })
        .collect();
    indexers.sort_by_key(|(idx, _)| format!("{idx:?}"));

    assert_eq!(
        indexers,
        vec![
            (
                TypeRef::Primitive(Primitive::I4),
                TypeRef::Primitive(Primitive::I4)
            ),
            (
                TypeRef::Primitive(Primitive::String),
                TypeRef::Primitive(Primitive::String)
            ),
        ],
        "both `this[int]` and `this[string]` overloads must project their own index dimension",
    );
}

/// Absolute pin for the `allows ref struct` typar anti-constraint. The
/// differential test (`diff_assembly_minilib_one_class`) proves both
/// projectors *agree*, but it would stay green even if both sides dropped
/// the bit (the pre-modelling state, where neither projector decoded it). So
/// this asserts the concrete Rust-side value against the real DLL: the
/// `AllowByRefLike` (`0x0020`) bit must reach `TypeParameter::allows_ref_struct`
/// for both a type-owned (`RefStructBox<T>`) and a method-owned
/// (`RefStructHost.Accept<T>`) typar, and no other special constraint may
/// be set (the anti-constraint stands alone). The normalised constraint set
/// must surface the `allows ref struct` token.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_projects_allows_ref_struct_typar() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    // Type-owned typar: `class RefStructBox<T> where T : allows ref struct`.
    let boxed = entities
        .iter()
        .find(|e| e.name == "RefStructBox")
        .expect("MiniLib must expose the RefStructBox fixture");
    assert_eq!(boxed.generic_parameters.len(), 1);
    let tp = &boxed.generic_parameters[0];
    assert!(
        tp.allows_ref_struct,
        "RefStructBox<T>'s typar must carry the AllowByRefLike bit",
    );
    assert!(!tp.reference_type_constraint);
    assert!(!tp.value_type_constraint);
    assert!(!tp.default_constructor_constraint);
    assert!(!tp.is_unmanaged);
    assert!(tp.type_constraints.is_empty());

    // Method-owned typar: `void Accept<T>(T) where T : allows ref struct`.
    let host = entities
        .iter()
        .find(|e| e.name == "RefStructHost")
        .expect("MiniLib must expose the RefStructHost fixture");
    let accept = host
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == "Accept" => Some(m),
            _ => None,
        })
        .expect("RefStructHost must expose the `Accept` method");
    assert_eq!(accept.generic_parameters.len(), 1);
    assert!(
        accept.generic_parameters[0].allows_ref_struct,
        "Accept<T>'s method typar must carry the AllowByRefLike bit",
    );
}

/// Absolute pin for the byref-like intrinsics. The differential test proves
/// both projectors agree, but it would stay green if both dropped these
/// members; this asserts the concrete Rust-side shape against the real DLL.
///
/// `System.TypedReference` reaches the signature as the token-free
/// `ELEMENT_TYPE_TYPEDBYREF` element, which projects to the `System.TypedReference`
/// value type (FCS's `ILType.Value(System.TypedReference)`). Two facts matter:
/// (1) the parameter is *not* byref — the intrinsic is passed by value, so
/// `is_byref` stays `false` (unlike a real `ref`/`out`); (2) the element names
/// no assembly, so it is attributed to MiniLib's core-library `AssemblyRef`
/// (a real cross-assembly reference), never same-assembly (`None`).
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_projects_typed_reference_parameter() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let host = entities
        .iter()
        .find(|e| e.name == "ByRefLikeIntrinsics")
        .expect("MiniLib must expose the ByRefLikeIntrinsics fixture");

    let param_of = |method: &str| -> Parameter {
        host.members
            .iter()
            .find_map(|m| match m {
                Member::Method(m) if m.name == method => Some(m.signature.parameters[0].clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("ByRefLikeIntrinsics must expose `{method}`"))
    };

    // All three intrinsics project as `System.<Name>` named value types passed
    // by value (never byref, despite FCS's byref-like classification). The
    // token-free TYPEDBYREF element is attributed to the core library like the
    // two token-referenced siblings, so all three carry a real cross-assembly
    // `AssemblyRef` (`assembly: Some(_)`) rather than the same-assembly `None`.
    for (method, name) in [
        ("TakeTypedRef", "TypedReference"),
        ("TakeArgIterator", "ArgIterator"),
        ("TakeArgHandle", "RuntimeArgumentHandle"),
    ] {
        let p = param_of(method);
        assert!(!p.is_byref, "`{method}` intrinsic parameter is by value");
        match &p.ty {
            TypeRef::Named {
                assembly,
                namespace,
                name: n,
                type_args,
                ..
            } => {
                assert_eq!(namespace.as_slice(), ["System"]);
                assert_eq!(n, name);
                assert!(type_args.is_empty(), "`{method}` is non-generic");
                assert!(
                    assembly.is_some(),
                    "`{method}`'s `{name}` is a corlib type, not a same-assembly TypeDef",
                );
            }
            other => panic!("`{method}` param should be a named value type, got {other:?}"),
        }
    }
}

/// Absolute pin for byref members. The differential diff proves both projectors
/// agree, but would stay green if both dropped these members; this asserts the
/// concrete Rust-side shape — `TypeRef::ByRef` over the referent — for a `ref`
/// field (in a `ref struct`), a `ref`-returning property, and a `ref`-returning
/// indexer. It also pins the referent's nullability, which rides on the *outer*
/// position (`Field`/`Property::nullability`) since the byref wrapper is never
/// annotable — so `ref string?` is `ByRef(String)` + `Annotated`, which the
/// normaliser renders `System.String&?` (suffix after the `&`).
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_projects_byref_members() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let entity = |name: &str| {
        entities
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("MiniLib must expose the {name} fixture"))
    };
    let byref = |inner: TypeRef| TypeRef::ByRef {
        inner: Box::new(inner),
        readonly: false,
    };
    let i4 = || TypeRef::Primitive(Primitive::I4);
    let string = || TypeRef::Primitive(Primitive::String);

    // `ref` fields in a `ref struct`.
    let host = entity("RefFieldHost");
    let field = |name: &str| {
        host.members
            .iter()
            .find_map(|m| match m {
                Member::Field(f) if f.name == name => Some(f),
                _ => None,
            })
            .unwrap_or_else(|| panic!("RefFieldHost must expose the `{name}` field"))
    };
    let slot = field("Slot"); // ref int
    assert_eq!(slot.ty, byref(i4()));
    assert_eq!(slot.nullability, Nullability::Oblivious);
    let name = field("Name"); // ref string?
    assert_eq!(name.ty, byref(string()));
    assert_eq!(
        name.nullability,
        Nullability::Annotated,
        "the ref referent's `?` rides on the outer field position",
    );

    // `ref`-returning property and indexer.
    let acc = entity("RefAccessorHost");
    let prop = |name: &str| {
        acc.members
            .iter()
            .find_map(|m| match m {
                Member::Property(p) if p.name == name => Some(p),
                _ => None,
            })
            .unwrap_or_else(|| panic!("RefAccessorHost must expose the `{name}` property"))
    };
    let first = prop("First"); // ref int (no index dimension)
    assert_eq!(first.ty, byref(i4()));
    assert!(first.parameters.is_empty());
    let first_name = prop("FirstName"); // ref string?
    assert_eq!(first_name.ty, byref(string()));
    assert_eq!(first_name.nullability, Nullability::Annotated);
    let item = prop("Item"); // ref int this[int i]
    assert_eq!(item.ty, byref(i4()));
    assert_eq!(
        item.parameters,
        vec![IndexParameter {
            name: Some("i".to_string()),
            ty: NullableType::oblivious(i4()),
            is_param_array: false,
        }],
    );
}

/// Absolute pin for `init`-only setters. The differential diff proves both
/// projectors agree, but would stay green if both dropped these; this asserts
/// the concrete Rust-side shape. A C# `init` property recovers as an ordinary
/// property carrying a setter — its `set_X` void return's
/// `modreq(IsExternalInit)` is recognised and projected as plain `void`, so the
/// property is *not* dropped and reads identically to a plain `get;set;`
/// (neither the model nor FCS's IL-property view distinguishes `init` from
/// `set`). Also checks a positional record's `init` properties.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_projects_init_setters() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("Ecma335Assembly::parse MiniLib");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let prop_of = |entity: &str, name: &str| -> Property {
        entities
            .iter()
            .find(|e| e.name == entity)
            .unwrap_or_else(|| panic!("MiniLib must expose the {entity} fixture"))
            .members
            .iter()
            .find_map(|m| match m {
                Member::Property(p) if p.name == name => Some(p.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{entity} must expose the `{name}` property"))
    };

    // `get; init;` recovers as a get+set property (init is projected as a plain
    // setter), identical in shape to a `get; set;`.
    let value = prop_of("InitHost", "Value");
    assert_eq!(value.ty, TypeRef::Primitive(Primitive::I4));
    assert!(
        value.has_getter && value.has_setter,
        "`init` recovers a setter"
    );

    let read_write = prop_of("InitHost", "ReadWrite");
    assert!(read_write.has_getter && read_write.has_setter);
    assert_eq!(
        (value.has_getter, value.has_setter),
        (read_write.has_getter, read_write.has_setter),
        "an `init` property is indistinguishable from a `set` one at this layer",
    );

    // A positional record's `X` compiles to a `get; init;` pair.
    let x = prop_of("PointRecord", "X");
    assert_eq!(x.ty, TypeRef::Primitive(Primitive::I4));
    assert!(x.has_getter && x.has_setter);
}

/// Sound invariant (B2): any type that *locally declares* an indexer — a
/// property with a non-empty index dimension — must carry `[DefaultMember]`
/// naming one of those indexers. Every real compiler (Roslyn, fsc) emits
/// this; it is the linkage phase-4n's DefaultMember decoder and B1's indexer
/// projection must keep in step. Runs across every enumerated MiniLib type,
/// so a future indexer fixture added without its own pin is still guarded.
///
/// The converse ("default_member names an indexer") is deliberately NOT
/// asserted: it is false in idiomatic C# — `[DefaultMember("CustomThing")]`
/// can name a parameterless property (the `ExplicitDefaultMember` fixture) —
/// so that direction would be unsound. Cross-projector agreement on
/// `default_member` itself is already covered by the differential test.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_indexer_types_carry_default_member() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let mut indexer_types_seen = 0usize;
    for e in &entities {
        let indexer_names: Vec<&str> = e
            .members
            .iter()
            .filter_map(|m| match m {
                Member::Property(p) if !p.parameters.is_empty() => Some(p.name.as_str()),
                _ => None,
            })
            .collect();
        if indexer_names.is_empty() {
            continue;
        }
        indexer_types_seen += 1;
        match &e.default_member {
            Some(DefaultMember::Named(n)) => assert!(
                indexer_names.contains(&n.as_str()),
                "type `{}` declares indexer(s) {indexer_names:?} but its default \
                 member `{n}` names none of them",
                e.name,
            ),
            other => panic!(
                "type `{}` declares indexer(s) {indexer_names:?} but carries \
                 default_member {other:?}; every compiler emits [DefaultMember] \
                 on an indexer-declaring type",
                e.name,
            ),
        }
    }

    // Guard against the check rotting into a vacuous no-op: MiniLib has
    // IndexerHost + OverloadedIndexerHost + NullableIndexerHost as
    // indexer-declaring types.
    assert!(
        indexer_types_seen >= 3,
        "expected IndexerHost, OverloadedIndexerHost and NullableIndexerHost \
         to declare indexers; saw {indexer_types_seen}",
    );
}

/// Phase B3: index-parameter nullability flows from the getter parameter,
/// not the property signature. `NullableIndexerHost` declares two overloaded
/// indexers in a `#nullable enable` scope:
///
///   - `string? this[string? key]` — the index parameter's outer reference is
///     `Annotated` (from the getter's nullable scope; the property signature
///     carries no per-parameter nullness at all).
///   - `string this[List<string?> xs]` — the outer `List` is `NotAnnotated`
///     while its inner `string` arg is `Annotated`. That inner annotation is
///     only representable on the getter parameter type: the property-signature
///     type projects it `Oblivious`, so this pins the getter-sourced design.
///
/// One-sided pin reading the real DLL: the symmetric differential diff would
/// pass even if *both* projectors dropped the annotation, so the guard lives
/// here.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn minilib_nullable_index_parameter_projects_from_getter() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib types");

    let host = entities
        .iter()
        .find(|e| e.name == "NullableIndexerHost")
        .expect("MiniLib must expose the NullableIndexerHost fixture");

    let indexers: Vec<&borzoi_assembly::Property> = host
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Property(p) if p.name == "Item" => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(
        indexers.len(),
        2,
        "two overloaded nullable indexers expected"
    );

    // Scalar `string? this[string? key]`.
    let scalar = indexers
        .iter()
        .find(|p| {
            matches!(p.parameters.as_slice(), [np] if np.ty.ty == TypeRef::Primitive(Primitive::String))
        })
        .expect("scalar `string?` indexer");
    assert_eq!(
        scalar.parameters,
        vec![IndexParameter {
            name: Some("key".to_string()),
            ty: NullableType {
                ty: TypeRef::Primitive(Primitive::String),
                nullability: Nullability::Annotated,
            },
            is_param_array: false,
        }],
        "the `string? key` index parameter must carry its name and the Annotated reference annotation",
    );
    assert_eq!(scalar.ty, TypeRef::Primitive(Primitive::String));
    assert_eq!(scalar.nullability, Nullability::Annotated);

    // Composite `string this[List<string?> xs]`.
    let composite = indexers
        .iter()
        .find(|p| matches!(p.parameters.as_slice(), [np] if matches!(np.ty.ty, TypeRef::Named { .. })))
        .expect("composite `List<string?>` indexer");
    let idx = &composite.parameters[0];
    assert_eq!(
        idx.ty.nullability,
        Nullability::NotAnnotated,
        "the outer `List` reference is non-nullable under #nullable enable",
    );
    match &idx.ty.ty {
        TypeRef::Named {
            name, type_args, ..
        } => {
            assert_eq!(name, "List");
            assert_eq!(
                type_args,
                &vec![NullableType {
                    ty: TypeRef::Primitive(Primitive::String),
                    nullability: Nullability::Annotated,
                }],
                "the inner `string?` arg must carry Annotated — this nullness \
                 lives only on the getter parameter type",
            );
        }
        other => panic!("expected List<_> index parameter, got {other:?}"),
    }
}

/// End-to-end diff against a real `fsc`-built F# `.dll`: pins phase 4a's
/// `CompilationMappingAttribute` decoder + the resulting `EntityKind`
/// override. MiniLibFs covers each F#-specific kind (Module / Union /
/// Record / Exception) the ECMA-335 type flags alone can't distinguish
/// from a plain `Class`.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn diff_assembly_minilib_fs() {
    let dll_path = ensure_minilib_fs_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLibFs.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLibFs");
    let rust_entities = view
        .enumerate_type_defs()
        .expect("enumerate MiniLibFs types");
    let rust_norm = normalise_entities(&view.identity().name, &rust_entities);

    let fcs_json = invoke_fcs_dump("entities", dll_path);
    let fcs_norm = parse_fcs_dump(&fcs_json);

    assert_eq!(
        rust_norm,
        fcs_norm,
        "MiniLibFs normalised assemblies diverge.\n\
         rust ({} entities): {:#?}\n\
         fcs  ({} entities): {:#?}\n",
        rust_norm.entities.len(),
        rust_norm,
        fcs_norm.entities.len(),
        fcs_norm,
    );
}

/// Regression for the `Expr.Op` attribute-argument decoder slice. The
/// MeasureAttrArgs fixture carries both `Expr.Op` shapes FCS admits in
/// attribute position — an array literal (`[<Tags([| 1; 2; 3 |])>]` →
/// `TOp.Array`) and an `obj`-parameter coercion (`[<Boxed("hi")>]` →
/// `TOp.Coerce`) — alongside a `[<Measure>] type m`.
///
/// `enumerate_type_defs` decodes the host signature pickle (reaching both
/// `Expr.Op` expressions) and applies the measure overlay. Asserting `m`
/// recovers `EntityKind::Measure` is the whole end-to-end property in one
/// check: if either argument failed to decode, enumeration would record
/// skipped F# overlays and leave `m` as `Class` (its IL truth). So a green
/// assertion proves both that the real `Op` attribute-argument pickles
/// decode *and* that the overlay merged — the exact pair the divergence
/// degraded before this slice.
///
/// This fixture is deliberately not diffed against fcs-dump: the array
/// ctor parameter's element-nullness rendering diverges (an unrelated
/// member-signature gap), so we pin the measure kind alone.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn measure_overlay_survives_op_attribute_arguments() {
    let dll_path = ensure_measure_attr_args_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MeasureAttrArgs.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MeasureAttrArgs");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate MeasureAttrArgs types (Op attribute args must decode)");

    let m = entities
        .iter()
        .find(|e| e.name == "m")
        .expect("measure type `m` present in MeasureAttrArgs");
    assert_eq!(
        m.kind,
        EntityKind::Measure,
        "`m` must recover `Measure` with array + coercion attribute arguments present; \
         got {:?}. A `Class` here means an `Expr.Op` argument failed to decode and \
         the measure overlay was skipped.",
        m.kind,
    );
}

/// Regression for the full `u_const` decoder (tags 0–17). The LiteralConsts
/// fixture carries one `[<Literal>]` of each previously-unsupported,
/// literal-expressible `u_const` tag (`sbyte`/`byte`/`int16`/`uint16`/
/// `uint32`/`int64`/`uint64`/`single`/`double`/`char`/`decimal`) plus a
/// constant `int64` attribute argument, alongside a `[<Measure>] type m`.
///
/// `enumerate_type_defs` decodes the host signature pickle — reaching every
/// literal value (`val.rs` / `repr.rs`) and the attribute argument
/// (`expr.rs`), all of which route through `read_const`. Asserting `m`
/// recovers `EntityKind::Measure` is the whole end-to-end property in one
/// check: before the full tag set, any single wide-typed literal or
/// attribute argument hard-errored, enumeration skipped the F# overlays, and
/// `m` stayed `Class` (its IL truth). A green assertion proves every
/// `u_const` tag in the assembly decoded and the overlay merged.
///
/// Like MeasureAttrArgs, this fixture is deliberately not diffed against
/// fcs-dump; we pin the measure kind alone.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn measure_overlay_survives_wide_literal_and_attribute_consts() {
    let dll_path = ensure_literal_consts_built();
    let dll_bytes = std::fs::read(dll_path).expect("read LiteralConsts.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse LiteralConsts");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate LiteralConsts types (every u_const literal must decode)");

    let m = entities
        .iter()
        .find(|e| e.name == "m")
        .expect("measure type `m` present in LiteralConsts");
    assert_eq!(
        m.kind,
        EntityKind::Measure,
        "`m` must recover `Measure` with wide-typed literals + an int64 attribute \
         argument present; got {:?}. A `Class` here means some `u_const` tag failed \
         to decode and the measure overlay was skipped.",
        m.kind,
    );
}

/// Phase 5: the F#-built MiniLibFs fixture exposes its signature and
/// optimisation data through manifest resources. Confirm both that we
/// classify the prefixes correctly and that decompression actually
/// produces a non-empty payload.
///
/// The exact resource set fsc emits is "whatever the current toolchain
/// chose for this build" — modern fsc (≥ 4.7) defaults to the
/// `Compressed` variants but a future toolchain could revert or split
/// further. The assertion is on the *set* of kinds present (sig + opt,
/// both for the assembly logical name `MiniLibFs`), not the exact count
/// or variant, so the test stays robust across F# minor versions while
/// still pinning the contract phase 6 needs ("we can find decompressed
/// signature bytes for an F# DLL").
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn fsharp_resources_minilib_fs() {
    let dll_path = ensure_minilib_fs_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLibFs.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLibFs");
    let resources = view
        .fsharp_resources()
        .expect("fsharp_resources on MiniLibFs");

    assert!(
        !resources.is_empty(),
        "MiniLibFs must expose at least one F# resource; got none",
    );

    // Every resource decompressed to a non-empty payload, every name
    // starts with the expected prefix, and every suffix is the assembly
    // logical name.
    for r in &resources {
        assert!(
            r.name.starts_with("FSharp"),
            "unexpected non-FSharp resource name: {}",
            r.name,
        );
        assert!(
            !r.payload.is_empty(),
            "decompressed payload for {} is empty — broken inflate",
            r.name,
        );
        assert!(
            r.name.ends_with(".MiniLibFs"),
            "expected suffix `.MiniLibFs` on {}",
            r.name,
        );
    }

    // The fixture must surface both a signature-flavoured kind and an
    // optimisation-flavoured kind. We don't care which exact variants
    // (compressed vs uncompressed, primary vs secondary) — that's a
    // function of the F# toolchain version. We do care that fsc emitted
    // *both* axes; without them phase 6's unpickler has nothing to chew
    // on.
    let has_signature = resources.iter().any(|r| {
        matches!(
            r.kind,
            ResourceKind::SignatureData
                | ResourceKind::SignatureDataB
                | ResourceKind::SignatureCompressedData
                | ResourceKind::SignatureCompressedDataB
                | ResourceKind::SignatureDataFSharpCore
        )
    });
    let has_optimization = resources.iter().any(|r| {
        matches!(
            r.kind,
            ResourceKind::OptimizationData
                | ResourceKind::OptimizationDataB
                | ResourceKind::OptimizationCompressedData
                | ResourceKind::OptimizationCompressedDataB
                | ResourceKind::OptimizationDataFSharpCore
        )
    });
    assert!(
        has_signature,
        "MiniLibFs must expose a signature resource; got {:?}",
        resources
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>(),
    );
    assert!(
        has_optimization,
        "MiniLibFs must expose an optimization resource; got {:?}",
        resources
            .iter()
            .map(|r| (&r.name, r.kind))
            .collect::<Vec<_>>(),
    );
}

/// Phase 5: the C#-built MiniLib fixture has zero F# resources and no
/// `FSharpInterfaceDataVersionAttribute`. The collector must return
/// `Ok(vec![])` — silently, without tripping the version-attribute
/// check on the empty-result path. Pins the negative end of the
/// contract: "no F# data" must not be conflated with "F# data exists
/// but is malformed".
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn fsharp_resources_minilib_csharp_empty() {
    let dll_path = ensure_minilib_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLib.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLib");
    let resources = view
        .fsharp_resources()
        .expect("fsharp_resources on MiniLib");

    assert!(
        resources.is_empty(),
        "C#-only MiniLib must expose no F# resources, got {resources:?}",
    );
}

/// End-to-end diff against MiniLibFsExt, the F# fixture that augments
/// the C# `MiniLib.Counter` class with both an instance F#-native
/// extension (`type Counter with member this.Tripled`) and a static
/// F#-native extension (`type Counter with static member Make`).
/// Pins phase 4c:
///
///   - The IL MethodDef for `Tripled` has the receiver as its first
///     parameter and lives on a synthetic `Extensions` module class.
///     The F# compiler does NOT emit `[ExtensionAttribute]` for
///     `type T with member …` augmentations (it only does so for
///     methods explicitly decorated with `[<Extension>]`), so the
///     projection cannot pick the flag up from the IL attribute
///     table. Instead it recovers the bit structurally from the IL
///     name mangling (`Counter.Tripled` — a `.` is illegal in F#
///     identifiers, so its presence on a Module-kind class is the
///     unique marker for an F#-native augmentation).
///
///     FCS reports `Tripled` with `IsExtensionMember = true` and
///     `CurriedParameterGroups` stripping the compiled receiver, so
///     the fcs-dump-side projector must re-prepend it and emit the
///     `extension` flag derived from `IsExtensionMember &&
///     IsInstanceMember`.
///
///   - `Make` is a static augmentation — its mangled name carries the
///     `.Static` suffix (`Counter.Make.Static`). The projection's
///     heuristic explicitly excludes that suffix so the `extension`
///     flag stays off; the fcs-dump side already excludes it via
///     `m.IsInstanceMember = false`.
///
///   - The IL name is mangled (`Counter.Tripled`,
///     `Counter.Make.Static`); fcs-dump uses `CompiledName` rather
///     than `LogicalName` for extension members to match.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn diff_assembly_minilib_fs_ext() {
    let dll_path = ensure_minilib_fs_ext_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLibFsExt.dll");

    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLibFsExt");
    let rust_entities = view
        .enumerate_type_defs()
        .expect("enumerate MiniLibFsExt types");
    let rust_norm = normalise_entities(&view.identity().name, &rust_entities);

    let fcs_json = invoke_fcs_dump("entities", dll_path);
    let fcs_norm = parse_fcs_dump(&fcs_json);

    assert_eq!(
        rust_norm,
        fcs_norm,
        "MiniLibFsExt normalised assemblies diverge.\n\
         rust ({} entities): {:#?}\n\
         fcs  ({} entities): {:#?}\n",
        rust_norm.entities.len(),
        rust_norm,
        fcs_norm.entities.len(),
        fcs_norm,
    );
}

/// The normaliser's member sort must include generic arity / declarations
/// as tie-breakers. Without that, an entity carrying both `void M()` and
/// `void M<T>()` overloads would normalise differently depending on the
/// projector's emission order (their `kind` / `name` / `signature` are
/// identical), and the diff would become input-order-sensitive again.
#[test]
fn sort_tiebreaks_on_generic_arity() {
    let non_generic = MethodLike {
        definition_range: None,
        name: "M".into(),
        access: Access::Public,
        signature: MethodSignature {
            parameters: vec![],
            return_type: void(),
            return_nullability: Nullability::Oblivious,
        },
        arg_group_count: Some(1),
        is_static: false,
        is_virtual: false,
        is_abstract: false,
        is_constructor: false,
        module_value: None,
        is_module_value_binding: false,
        is_extension_method: false,
        augmentation: Augmentation::No,
        is_final: false,
        is_newslot: false,
        is_hide_by_sig: false,
        generic_parameters: vec![],
        obsolete: None,
        experimental: None,
        sets_required_members: false,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        metadata_token: 0,
        implements: Vec::new(),
        unclassified_impls: Vec::new(),
    };
    let generic = MethodLike {
        definition_range: None,
        generic_parameters: vec![TypeParameter {
            name: "T".into(),
            variance: Variance::Invariant,
            reference_type_constraint: false,
            value_type_constraint: false,
            default_constructor_constraint: false,
            is_unmanaged: false,
            allows_ref_struct: false,
            nullability: Nullability::Oblivious,
            type_constraints: vec![],
        }],
        ..non_generic.clone()
    };

    let host_a = Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        assembly: my_lib(),
        namespace: vec!["MyLib".into()],
        name: "Host".into(),
        kind: EntityKind::Class,
        access: Access::Public,
        generic_parameters: vec![],
        base_type: None,
        interfaces: vec![],
        method_def_tokens: vec![],
        is_sealed: false,
        skipped_members: vec![],
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        abbreviation_target: None,
        definition_range: None,
        members: vec![
            Member::Method(non_generic.clone()),
            Member::Method(generic.clone()),
        ],
    };
    let mut host_b = host_a.clone();
    host_b.members = vec![Member::Method(generic), Member::Method(non_generic)];

    assert_eq!(
        normalise_entities("MyLib", &[host_a]),
        normalise_entities("MyLib", &[host_b]),
        "member sort must be input-order-insensitive even when only generic_parameters differs",
    );
}

/// Same shape as `sort_tiebreaks_on_generic_arity` but at the entity
/// level. `Foo` and `Foo<T>` share an FQN (the arity suffix is stripped),
/// so without tie-breaking on `generic_parameters` two assemblies with
/// the entities in opposite enumeration order would normalise unequal.
#[test]
fn entity_sort_tiebreaks_on_generic_arity() {
    let bare = Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        assembly: my_lib(),
        namespace: vec!["MyLib".into()],
        name: "Foo".into(),
        kind: EntityKind::Class,
        access: Access::Public,
        generic_parameters: vec![],
        base_type: None,
        interfaces: vec![],
        method_def_tokens: vec![],
        is_sealed: false,
        skipped_members: vec![],
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        abbreviation_target: None,
        definition_range: None,
        members: vec![],
    };
    let generic = Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        name: "Foo".into(),
        generic_parameters: vec![TypeParameter {
            name: "T".into(),
            variance: Variance::Invariant,
            reference_type_constraint: false,
            value_type_constraint: false,
            default_constructor_constraint: false,
            is_unmanaged: false,
            allows_ref_struct: false,
            nullability: Nullability::Oblivious,
            type_constraints: vec![],
        }],
        ..bare.clone()
    };

    let a = normalise_entities("MyLib", &[bare.clone(), generic.clone()]);
    let b = normalise_entities("MyLib", &[generic, bare]);
    assert_eq!(
        a, b,
        "entity sort must be input-order-insensitive for overloads sharing fqn",
    );
}

/// A second fixture exercising fields and a non-`System` namespace, so the
/// projector's field/method discrimination and FQN handling are pinned by
/// a test that isn't `System.Object`-shaped.
#[test]
fn diff_assembly_fields_and_namespaces() {
    let rust = fixture_my_lib();
    let json = fixture_my_lib_json();

    let rust_norm = normalise_entities("MyLib", &rust);
    let fcs_norm = parse_fcs_dump(json);

    assert_eq!(
        rust_norm, fcs_norm,
        "normalised assemblies diverge.\n  rust: {rust_norm:#?}\n  fcs:  {fcs_norm:#?}",
    );
}

// ============================================================================
// Fixtures
// ============================================================================

fn mscorlib() -> AssemblyIdentity {
    AssemblyIdentity {
        name: "mscorlib".into(),
        version: Version {
            major: 4,
            minor: 0,
            build: 0,
            revision: 0,
        },
        public_key_token: Some([0xb7, 0x7a, 0x5c, 0x56, 0x19, 0x34, 0xe0, 0x89]),
    }
}

fn my_lib() -> AssemblyIdentity {
    AssemblyIdentity {
        name: "MyLib".into(),
        version: Version {
            major: 1,
            minor: 0,
            build: 0,
            revision: 0,
        },
        public_key_token: None,
    }
}

fn obj() -> TypeRef {
    TypeRef::Primitive(Primitive::Object)
}
fn boolean() -> TypeRef {
    TypeRef::Primitive(Primitive::Bool)
}
fn int32() -> TypeRef {
    TypeRef::Primitive(Primitive::I4)
}
fn string() -> TypeRef {
    TypeRef::Primitive(Primitive::String)
}
fn void() -> TypeRef {
    TypeRef::Primitive(Primitive::Void)
}

fn param(name: &str, ty: TypeRef) -> Parameter {
    Parameter {
        name: Some(name.into()),
        ty,
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    }
}

fn unnamed(ty: TypeRef) -> Parameter {
    Parameter {
        name: None,
        ty,
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    }
}

fn fixture_system_object() -> Vec<Entity> {
    vec![Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        assembly: mscorlib(),
        namespace: vec!["System".into()],
        name: "Object".into(),
        kind: EntityKind::Class,
        access: Access::Public,
        generic_parameters: vec![],
        base_type: None,
        interfaces: vec![],
        method_def_tokens: vec![],
        is_sealed: false,
        skipped_members: vec![],
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        abbreviation_target: None,
        definition_range: None,
        members: vec![
            Member::Method(MethodLike {
                definition_range: None,
                name: ".ctor".into(),
                access: Access::Public,
                signature: MethodSignature {
                    parameters: vec![],
                    return_type: void(),
                    return_nullability: Nullability::Oblivious,
                },
                arg_group_count: Some(1),
                is_static: false,
                is_virtual: false,
                is_abstract: false,
                is_constructor: true,
                module_value: None,
                is_module_value_binding: false,
                is_extension_method: false,
                augmentation: Augmentation::No,
                is_final: false,
                is_newslot: false,
                is_hide_by_sig: false,
                generic_parameters: vec![],
                obsolete: None,
                experimental: None,
                sets_required_members: false,
                compiler_feature_required: vec![],
                source_name: None,
                custom_attrs: vec![],
                metadata_token: 0,
                implements: Vec::new(),
                unclassified_impls: Vec::new(),
            }),
            Member::Method(MethodLike {
                definition_range: None,
                name: "Equals".into(),
                access: Access::Public,
                signature: MethodSignature {
                    parameters: vec![param("obj", obj())],
                    return_type: boolean(),
                    return_nullability: Nullability::Oblivious,
                },
                arg_group_count: Some(1),
                is_static: false,
                is_virtual: true,
                is_abstract: false,
                is_constructor: false,
                module_value: None,
                is_module_value_binding: false,
                is_extension_method: false,
                augmentation: Augmentation::No,
                is_final: false,
                is_newslot: false,
                is_hide_by_sig: false,
                generic_parameters: vec![],
                obsolete: None,
                experimental: None,
                sets_required_members: false,
                compiler_feature_required: vec![],
                source_name: None,
                custom_attrs: vec![],
                metadata_token: 0,
                implements: Vec::new(),
                unclassified_impls: Vec::new(),
            }),
            Member::Method(MethodLike {
                definition_range: None,
                name: "GetHashCode".into(),
                access: Access::Public,
                signature: MethodSignature {
                    parameters: vec![],
                    return_type: int32(),
                    return_nullability: Nullability::Oblivious,
                },
                arg_group_count: Some(1),
                is_static: false,
                is_virtual: true,
                is_abstract: false,
                is_constructor: false,
                module_value: None,
                is_module_value_binding: false,
                is_extension_method: false,
                augmentation: Augmentation::No,
                is_final: false,
                is_newslot: false,
                is_hide_by_sig: false,
                generic_parameters: vec![],
                obsolete: None,
                experimental: None,
                sets_required_members: false,
                compiler_feature_required: vec![],
                source_name: None,
                custom_attrs: vec![],
                metadata_token: 0,
                implements: Vec::new(),
                unclassified_impls: Vec::new(),
            }),
            Member::Method(MethodLike {
                definition_range: None,
                name: "ReferenceEquals".into(),
                access: Access::Public,
                signature: MethodSignature {
                    // Two unnamed `obj` parameters — exercise the
                    // "parameter without a name" path through the
                    // renderer.
                    parameters: vec![unnamed(obj()), unnamed(obj())],
                    return_type: boolean(),
                    return_nullability: Nullability::Oblivious,
                },
                arg_group_count: Some(1),
                is_static: true,
                is_virtual: false,
                is_abstract: false,
                is_constructor: false,
                module_value: None,
                is_module_value_binding: false,
                is_extension_method: false,
                augmentation: Augmentation::No,
                is_final: false,
                is_newslot: false,
                is_hide_by_sig: false,
                generic_parameters: vec![],
                obsolete: None,
                experimental: None,
                sets_required_members: false,
                compiler_feature_required: vec![],
                source_name: None,
                custom_attrs: vec![],
                metadata_token: 0,
                implements: Vec::new(),
                unclassified_impls: Vec::new(),
            }),
        ],
    }]
}

fn fixture_system_object_json() -> &'static str {
    r#"{
      "Assembly": "mscorlib",
      "Entities": [
        {
          "Fqn": "System.Object",
          "Kind": "Class",
          "Access": "Public",
          "BaseType": null,
          "Interfaces": [],
          "Members": [
            { "Kind": "Method", "Name": ".ctor",
              "Signature": "() -> System.Void",
              "Access": "Public",
              "Flags": ["constructor", "instance"] },
            { "Kind": "Method", "Name": "Equals",
              "Signature": "(System.Object) -> System.Boolean",
              "Access": "Public",
              "Flags": ["instance", "virtual"] },
            { "Kind": "Method", "Name": "GetHashCode",
              "Signature": "() -> System.Int32",
              "Access": "Public",
              "Flags": ["instance", "virtual"] },
            { "Kind": "Method", "Name": "ReferenceEquals",
              "Signature": "(System.Object, System.Object) -> System.Boolean",
              "Access": "Public",
              "Flags": ["static"] }
          ],
          "NestedTypes": []
        }
      ]
    }"#
}

fn fixture_my_lib() -> Vec<Entity> {
    vec![Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        assembly: my_lib(),
        namespace: vec!["MyLib".into(), "Inner".into()],
        name: "Counter".into(),
        kind: EntityKind::Class,
        access: Access::Public,
        generic_parameters: vec![],
        base_type: Some(obj()),
        interfaces: vec![],
        method_def_tokens: vec![],
        is_sealed: false,
        skipped_members: vec![],
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        abbreviation_target: None,
        definition_range: None,
        members: vec![
            Member::Field(Field {
                name: "count".into(),
                access: Access::Private,
                ty: int32(),
                is_static: false,
                is_init_only: false,
                is_volatile: false,
                is_literal: false,
                is_required: false,
                compiler_feature_required: vec![],
                nullability: Nullability::Oblivious,
                custom_attrs: vec![],
            }),
            Member::Field(Field {
                name: "Tag".into(),
                access: Access::Public,
                ty: string(),
                is_static: false,
                is_init_only: true,
                is_volatile: false,
                is_literal: false,
                is_required: false,
                compiler_feature_required: vec![],
                nullability: Nullability::Oblivious,
                custom_attrs: vec![],
            }),
            Member::Method(MethodLike {
                definition_range: None,
                name: "Increment".into(),
                access: Access::Public,
                signature: MethodSignature {
                    parameters: vec![],
                    return_type: void(),
                    return_nullability: Nullability::Oblivious,
                },
                arg_group_count: Some(1),
                is_static: false,
                is_virtual: false,
                is_abstract: false,
                is_constructor: false,
                module_value: None,
                is_module_value_binding: false,
                is_extension_method: false,
                augmentation: Augmentation::No,
                is_final: false,
                is_newslot: false,
                is_hide_by_sig: false,
                generic_parameters: vec![],
                obsolete: None,
                experimental: None,
                sets_required_members: false,
                compiler_feature_required: vec![],
                source_name: None,
                custom_attrs: vec![],
                metadata_token: 0,
                implements: Vec::new(),
                unclassified_impls: Vec::new(),
            }),
        ],
    }]
}

fn fixture_my_lib_json() -> &'static str {
    // Notice the absence of the private `count` field. `fcs-dump`
    // enumerates through FCS's `MembersFunctionsAndValues`, which applies
    // `AccessibleFromSomeFSharpCode` and drops private/internal members.
    // The Rust-side fixture above still includes `count` (that's what the
    // raw IL importer sees); the normaliser drops it before comparison so
    // both sides agree.
    r#"{
      "Assembly": "MyLib",
      "Entities": [
        {
          "Fqn": "MyLib.Inner.Counter",
          "Kind": "Class",
          "Access": "Public",
          "BaseType": "System.Object",
          "Interfaces": [],
          "Members": [
            { "Kind": "Field", "Name": "Tag",
              "Signature": "System.String",
              "Access": "Public",
              "Flags": ["init_only", "instance"] },
            { "Kind": "Method", "Name": "Increment",
              "Signature": "() -> System.Void",
              "Access": "Public",
              "Flags": ["instance"] }
          ],
          "NestedTypes": []
        }
      ]
    }"#
}

/// Robustness pin for the fcs-dump oracle: a generic module `let` whose typar
/// carries an **IL-visible** constraint (the flexible `#seq` parameter — the
/// `array2D` shape that used to abort `entities` on real FSharp.Core) is
/// *elided* on both sides rather than crashing the dump or one-sidedly
/// diverging. The MiniLibFs diff above only proves agreement; this asserts
/// the owned model still *keeps* the member, constraint and all — the
/// elision is the differential's, not the projection's.
#[test]
fn constrained_generic_let_is_kept_in_the_owned_model() {
    let dll_path = ensure_minilib_fs_built();
    let dll_bytes = std::fs::read(dll_path).expect("read MiniLibFs.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("Ecma335Assembly::parse MiniLibFs");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate MiniLibFs types");
    let flatten = entities
        .iter()
        .filter(|e| matches!(e.kind, EntityKind::Module))
        .flat_map(|e| &e.members)
        .find_map(|m| match m {
            Member::Method(m) if m.name == "constrainedFlatten" => Some(m),
            _ => None,
        })
        .expect("MiniLibFs projects constrainedFlatten");
    assert_eq!(flatten.generic_parameters.len(), 1);
    assert!(
        !flatten.generic_parameters[0].type_constraints.is_empty(),
        "the flexible #seq parameter compiles to an IL coercion-constraint row"
    );
}
