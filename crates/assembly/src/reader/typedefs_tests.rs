//! Stage 4 correctness oracles for the type-definition table walk.
//!
//! - **Property**: `enclosing`/`nested` are mutually consistent; `top_level` is
//!   exactly the no-encloser defs; every resolved handle indexes a live arena
//!   slot.
//! - **Fuzz**: arbitrary, truncated, and mutated inputs yield `Ok`/`Err`, never
//!   a panic.

use proptest::prelude::*;

use super::ids::{TypeDefId, TypeRefId};
use super::metadata::MetadataFile;
use super::model::{MemberHandle, RefScope};
use super::signature::{ModifiedType, TypeScope, TypeSig};
use super::tables;
use super::test_fixtures::all_dlls;
use super::typedefs::read_types;

// ============================================================================
// Structural property oracles (no external reference; durable)
// ============================================================================

#[test]
fn nesting_and_top_level_are_consistent() {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("read fixture dll");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let types = read_types(&md).expect("type-def walk");
        let n = types.type_defs.len();

        for (i, td) in types.type_defs.iter().enumerate() {
            // Every nested child points back via `enclosing`.
            for &TypeDefId(child) in &td.nested {
                assert_eq!(
                    types.type_defs[child as usize].enclosing,
                    Some(TypeDefId(i as u32)),
                    "child {child} of {i} does not point back in {}",
                    dll.display()
                );
            }
            // The encloser lists this type among its children.
            if let Some(TypeDefId(parent)) = td.enclosing {
                assert!(
                    types.type_defs[parent as usize]
                        .nested
                        .contains(&TypeDefId(i as u32)),
                    "parent {parent} of {i} omits it in {}",
                    dll.display()
                );
            }
        }

        // `top_level` is exactly the defs with no encloser, in arena order.
        let expected: Vec<TypeDefId> = (0..n)
            .filter(|&i| types.type_defs[i].enclosing.is_none())
            .map(|i| TypeDefId(i as u32))
            .collect();
        assert_eq!(types.top_level, expected, "top_level for {}", dll.display());
    }
}

#[test]
fn resolved_handles_are_in_range() {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("read fixture dll");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let types = read_types(&md).expect("type-def walk");
        let def_count = types.type_defs.len() as u32;
        let ref_count = types.type_refs.len() as u32;
        let asm_ref_count = md.rows[tables::table::ASSEMBLY_REF];
        let member_ref_count = md.rows[tables::table::MEMBER_REF];

        // Every TypeRef scope handle indexes a live slot.
        for tr in &types.type_refs {
            match tr.scope {
                RefScope::AssemblyRef(id) => assert!(id.0 < asm_ref_count, "asm ref handle"),
                RefScope::Nested(id) => assert!(id.0 < ref_count, "nested typeref handle"),
                RefScope::Module => {}
            }
        }

        for td in &types.type_defs {
            // enclosing / nested handles in range.
            if let Some(TypeDefId(p)) = td.enclosing {
                assert!(p < def_count, "enclosing handle");
            }
            for &TypeDefId(c) in &td.nested {
                assert!(c < def_count, "nested handle");
            }
            // extends / implements / constraint scope handles in range.
            if let Some(Ok(sig)) = &td.extends {
                check_scopes_in_range(sig, def_count, ref_count);
            }
            for sig in td.implements.iter().flatten() {
                check_scopes_in_range(sig, def_count, ref_count);
            }
            for gp in &td.generic_params {
                for sig in gp.constraints.iter().filter_map(|c| c.ty.as_ref().ok()) {
                    check_scopes_in_range(sig, def_count, ref_count);
                }
            }
            // Attribute constructor handles index live arenas.
            for attr in &td.attributes {
                match attr.ctor {
                    MemberHandle::MethodDef(TypeDefId(d), _) => {
                        assert!(d < def_count, "attr methoddef owner handle")
                    }
                    MemberHandle::MemberRef(id) => {
                        assert!(id.0 < member_ref_count, "attr memberref handle")
                    }
                }
            }
        }
    }
}

fn check_scopes_in_range(mt: &ModifiedType, def_count: u32, ref_count: u32) {
    let check = |scope: &TypeScope| match scope {
        TypeScope::Definition(TypeDefId(d)) => assert!(*d < def_count, "TypeDef scope in range"),
        TypeScope::Reference(TypeRefId(r)) => assert!(*r < ref_count, "TypeRef scope in range"),
    };
    // The modifier run names types too.
    for m in &mt.mods {
        check(&m.modifier);
    }
    match &mt.ty {
        TypeSig::Named { scope, .. } => check(scope),
        TypeSig::Generic { scope, args, .. } => {
            check(scope);
            for a in args {
                check_scopes_in_range(a, def_count, ref_count);
            }
        }
        TypeSig::SzArray(inner) | TypeSig::Array { element: inner, .. } | TypeSig::ByRef(inner) => {
            check_scopes_in_range(inner, def_count, ref_count)
        }
        TypeSig::Ptr(inner) => {
            if let Some(p) = inner {
                check_scopes_in_range(p, def_count, ref_count);
            }
        }
        TypeSig::Primitive(_)
        | TypeSig::TypeVar(_)
        | TypeSig::MethodVar(_)
        | TypeSig::TypedByRef => {}
    }
}

// ============================================================================
// Fuzz oracles: never panic
// ============================================================================

proptest! {
    /// Arbitrary bytes that survive the container parse never panic the walk.
    #[test]
    fn read_types_never_panics_on_arbitrary(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
        if let Ok(md) = MetadataFile::read(&bytes) {
            let _ = read_types(&md);
        }
    }

    /// Mutating a real assembly at arbitrary offsets never panics the walk:
    /// the container usually survives, driving the table walk with corrupted
    /// offsets, counts, and coded tokens.
    #[test]
    fn read_types_never_panics_on_mutated(
        which in 0usize..8,
        muts in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..64),
    ) {
        let dlls = all_dlls();
        let mut bytes = std::fs::read(&dlls[which % dlls.len()]).expect("fixture");
        for (off, val) in muts {
            if !bytes.is_empty() {
                let i = off % bytes.len();
                bytes[i] = val;
            }
        }
        if let Ok(md) = MetadataFile::read(&bytes) {
            let _ = read_types(&md);
        }
    }
}

/// Every prefix of a real assembly drives the walk without panic.
#[test]
fn read_types_never_panics_on_truncated() {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let step = (bytes.len() / 256).max(1);
        let mut len = 0;
        while len <= bytes.len() {
            if let Ok(md) = MetadataFile::read(&bytes[..len]) {
                let _ = read_types(&md);
            }
            len += step;
        }
    }
}
