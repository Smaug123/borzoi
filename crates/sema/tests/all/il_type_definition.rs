//! `AssemblyEnv::il_type_definition`: binding an IL type reference the way a
//! metadata consumer does, over the real FSharp.Core + System.Runtime +
//! `netstandard` facade.
//!
//! The references are taken from the projected metadata itself wherever the
//! shape occurs there (FSharp.Core's bases name `[netstandard]System.Object`,
//! a forwarder away from its definition), and built by hand only for the
//! shapes nothing in these three DLLs writes.

use borzoi_assembly::{AssemblyIdentity, TypeRef, Version};
use borzoi_sema::{AssemblyEnv, EntityHandle, IlTypeDefinition};

use crate::common::full_bcl_env;

fn top(env: &AssemblyEnv, ns: &str, name: &str, arity: usize, assembly: &str) -> EntityHandle {
    let ns: Vec<String> = ns.split('.').map(str::to_string).collect();
    env.top_level_handles()
        .iter()
        .copied()
        .find(|&h| {
            let e = env.entity(h);
            e.namespace == ns
                && e.name == name
                && e.generic_parameters.len() == arity
                && e.assembly.name == assembly
        })
        .unwrap_or_else(|| panic!("{assembly}: no {name}`{arity}"))
}

/// A reference in the projection's shape, from a metadata name: each
/// `/`-segment's `` `n `` suffix stripped and recorded as its arity.
fn named(assembly: Option<&str>, ns: &str, metadata_name: &str) -> TypeRef {
    let (segments, segment_arities): (Vec<&str>, Vec<usize>) = metadata_name
        .split('/')
        .map(|seg| match seg.split_once('`') {
            Some((bare, n)) => (bare, n.parse().unwrap()),
            None => (seg, 0),
        })
        .unzip();
    let name = segments.join("/");
    TypeRef::Named {
        assembly: assembly.map(|a| AssemblyIdentity {
            name: a.to_string(),
            version: Version {
                major: 0,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        }),
        namespace: ns.split('.').map(str::to_string).collect(),
        name,
        type_args: Vec::new(),
        segment_arities,
    }
}

/// FSharp.Core is compiled against `netstandard`, whose `System.Object` is a
/// forwarder: the base binds to System.Runtime's definition.
#[test]
fn a_reference_through_a_facade_follows_its_forwarder() {
    let env = full_bcl_env();
    let object = top(env, "System", "Object", 0, "System.Runtime");
    let func = top(env, "Microsoft.FSharp.Core", "FSharpFunc", 2, "FSharp.Core");
    let base = env
        .entity(func)
        .base_type
        .clone()
        .expect("FSharpFunc has a base");
    assert!(
        matches!(&base, TypeRef::Named { assembly: Some(a), .. } if a.name == "netstandard"),
        "the shape under test: {base:?}"
    );
    assert_eq!(
        env.il_type_definition(func, &base),
        IlTypeDefinition::Resolved(object)
    );
}

/// A generic reference through the facade: the forwarder is keyed by the
/// raw metadata name (``IEnumerable`1``), which the projected reference
/// spells as a bare name and an arity.
#[test]
fn a_generic_reference_through_a_facade_follows_its_forwarder() {
    let env = full_bcl_env();
    let enumerable = top(
        env,
        "System.Collections.Generic",
        "IEnumerable",
        1,
        "System.Runtime",
    );
    let list = top(
        env,
        "Microsoft.FSharp.Collections",
        "FSharpList",
        1,
        "FSharp.Core",
    );
    let reference = env
        .entity(list)
        .interfaces
        .iter()
        .find(|i| matches!(i, TypeRef::Named { name, type_args, .. } if name == "IEnumerable" && type_args.len() == 1))
        .expect("FSharpList implements IEnumerable<T>")
        .clone();
    assert!(
        matches!(&reference, TypeRef::Named { assembly: Some(a), .. } if a.name == "netstandard"),
        "the shape under test: {reference:?}"
    );
    assert_eq!(
        env.il_type_definition(list, &reference),
        IlTypeDefinition::Resolved(enumerable)
    );
}

/// `assembly: None` is the referencing type's own module.
#[test]
fn a_same_module_reference_binds_in_the_referencing_assembly() {
    let env = full_bcl_env();
    let int32 = top(env, "System", "Int32", 0, "System.Runtime");
    let value_type = top(env, "System", "ValueType", 0, "System.Runtime");
    assert_eq!(
        env.il_type_definition(int32, &named(None, "System", "ValueType")),
        IlTypeDefinition::Resolved(value_type)
    );
    // The same name read from FSharp.Core's module is FSharp.Core's — which
    // declares no `System.ValueType`.
    let func = top(env, "Microsoft.FSharp.Core", "FSharpFunc", 2, "FSharp.Core");
    assert_eq!(
        env.il_type_definition(func, &named(None, "System", "ValueType")),
        IlTypeDefinition::NotFound
    );
}

/// The raw name's arity suffix selects between same-named types:
/// `System.Nullable` (a static class) and ``System.Nullable`1``.
#[test]
fn the_arity_suffix_is_part_of_the_name() {
    let env = full_bcl_env();
    let int32 = top(env, "System", "Int32", 0, "System.Runtime");
    let plain = top(env, "System", "Nullable", 0, "System.Runtime");
    let generic = top(env, "System", "Nullable", 1, "System.Runtime");
    assert_eq!(
        env.il_type_definition(int32, &named(None, "System", "Nullable")),
        IlTypeDefinition::Resolved(plain)
    );
    assert_eq!(
        env.il_type_definition(int32, &named(None, "System", "Nullable`1")),
        IlTypeDefinition::Resolved(generic)
    );
    assert_eq!(
        env.il_type_definition(int32, &named(None, "System", "Nullable`2")),
        IlTypeDefinition::NotFound
    );
}

#[test]
fn a_nested_reference_descends_from_its_encloser() {
    let env = full_bcl_env();
    let int32 = top(env, "System", "Int32", 0, "System.Runtime");
    let environment = top(env, "System", "Environment", 0, "System.Runtime");
    let folder = env
        .children(environment)
        .iter()
        .copied()
        .find(|&c| env.entity(c).name == "SpecialFolder")
        .expect("Environment.SpecialFolder");
    assert_eq!(
        env.il_type_definition(
            int32,
            &named(
                Some("System.Runtime"),
                "System",
                "Environment/SpecialFolder"
            )
        ),
        IlTypeDefinition::Resolved(folder)
    );
    assert_eq!(
        env.il_type_definition(
            int32,
            &named(Some("System.Runtime"), "System", "Environment/NoSuchNested")
        ),
        IlTypeDefinition::NotFound
    );
}

#[test]
fn an_assembly_nothing_loaded_carries_is_not_found() {
    let env = full_bcl_env();
    let int32 = top(env, "System", "Int32", 0, "System.Runtime");
    assert_eq!(
        env.il_type_definition(int32, &named(Some("NoSuchAssembly"), "System", "Object")),
        IlTypeDefinition::NotFound
    );
    assert_eq!(
        env.il_type_definition(
            int32,
            &TypeRef::Var {
                index: 0,
                is_method: false
            }
        ),
        IlTypeDefinition::NotNamed
    );
}

/// Assembly simple names compare case-insensitively when a binder resolves a
/// reference: one naming `system.runtime` may bind to the loaded
/// `System.Runtime`, so it is neither that assembly's type for certain nor
/// "no loaded assembly has the name".
#[test]
fn a_reference_naming_a_loaded_assembly_in_another_case_is_ambiguous() {
    let env = full_bcl_env();
    let func = top(env, "Microsoft.FSharp.Core", "FSharpFunc", 2, "FSharp.Core");
    assert_eq!(
        env.il_type_definition(func, &named(Some("system.runtime"), "System", "Object")),
        IlTypeDefinition::Ambiguous
    );
    let object = top(env, "System", "Object", 0, "System.Runtime");
    assert_eq!(
        env.il_type_definition(func, &named(Some("System.Runtime"), "System", "Object")),
        IlTypeDefinition::Resolved(object)
    );
}
