//! OV-0.5: the F#-native instance extension-member **name index**
//! (`Entity::extension_member_names`), read from the host signature pickle's
//! `IsExtensionMember ∧ IsInstance` bit per val.
//!
//! The point of the index is to be a *no-false-negative* signal for the overload
//! extension-absence gate, unlike the per-method `is_extension_method` flag,
//! which OV-0 found under-flags generic-method, optional-parameter, and
//! same-arity-collision instance extensions (plan §6.1(b)). These fixtures pin
//! that the index names every instance extension — including the shapes the flag
//! misses — and excludes static extensions and plain `let`s.

use std::path::Path;

use borzoi_assembly::{Augmentation, Ecma335Assembly, EcmaView, Entity, Member};

use crate::common::{ensure_fs_ext_index_built, ensure_minilib_fs_ext_built};

fn load(dll: &Path) -> Vec<Entity> {
    let bytes = std::fs::read(dll).expect("read fixture dll");
    Ecma335Assembly::parse(&bytes)
        .expect("parse fixture dll")
        .enumerate_type_defs()
        .expect("enumerate fixture types")
}

/// The module entity named `name`, searched recursively (a module may be nested).
fn module_named<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    fn find<'a>(entities: &'a [Entity], name: &str) -> Option<&'a Entity> {
        for e in entities {
            if e.name == name {
                return Some(e);
            }
            if let Some(found) = find(&e.nested_types, name) {
                return Some(found);
            }
        }
        None
    }
    find(entities, name).unwrap_or_else(|| panic!("module {name:?} not found"))
}

#[test]
fn extension_index_names_the_shapes_the_per_method_flag_misses() {
    let entities = load(ensure_fs_ext_index_built());
    let extensions = module_named(&entities, "Extensions");
    let mut names = extensions.extension_member_names.clone();
    names.sort();
    assert_eq!(
        names,
        vec![
            "GenericExt".to_string(),
            "OptExt".to_string(),
            "Twice".to_string(),
        ],
        "the index must name the generic-method (`GenericExt`) and \
         optional-parameter (`OptExt`) instance extensions the per-method flag \
         under-flags, plus the plain `Twice`, and exclude `StaticExt` (static) \
         and `notExtension` (not an extension)"
    );
}

#[test]
fn every_augmentation_carries_the_name_resolution_flag() {
    // The per-member `is_fsharp_extension_member` flag: set for **every** F#-native
    // augmentation (instance *and* static), and clear for a plain `let`. It is what
    // sema's bare/qualified-name filter reads — a static augmentation is equally
    // unreachable by name (FS0039), yet the *surface* `is_extension_method` flag
    // deliberately excludes it (FCS's `IsInstanceMember` gate, which the overload
    // engine reads as "instance-callable").
    //
    // Per member, not per name: F# permits a `let M` beside an augmentation `M`, and
    // only the augmentation is hidden (codex review, PR #916).
    let entities = load(ensure_fs_ext_index_built());
    let extensions = module_named(&entities, "Extensions");
    let flagged = |name: &str| -> bool {
        extensions.members.iter().any(|m| match m {
            Member::Method(mm) => {
                mm.source_name.as_deref().unwrap_or(&mm.name) == name
                    && mm.augmentation == Augmentation::Certain
            }
            _ => false,
        })
    };
    for name in ["Twice", "GenericExt", "OptExt", "StaticExt"] {
        assert!(flagged(name), "{name} is an F#-native augmentation");
    }
    assert!(
        !flagged("notExtension"),
        "a plain module `let` is not an augmentation"
    );
}

#[test]
fn extension_index_excludes_static_extension_and_plain_let() {
    // The exact-set assertion above already implies these, but pin the two
    // exclusions on their own so a regression names the culprit directly.
    let entities = load(ensure_fs_ext_index_built());
    let extensions = module_named(&entities, "Extensions");
    assert!(
        !extensions
            .extension_member_names
            .iter()
            .any(|n| n == "StaticExt"),
        "a static extension is not an *instance* extension member"
    );
    assert!(
        !extensions
            .extension_member_names
            .iter()
            .any(|n| n == "notExtension"),
        "a plain module `let` is not an extension member"
    );
}

#[test]
fn extension_index_on_minilib_fs_ext_names_only_the_instance_augmentation() {
    // MiniLibFsExt augments `MiniLib.Counter` with `Tripled` (instance ext) and
    // `Make` (static ext), plus two plain `let`s (`A.B` and a `Counter.Tripled`
    // arity-2 clash). The index names only the instance augmentation `Tripled`,
    // by its logical member name — not the mangled compiled name.
    let entities = load(ensure_minilib_fs_ext_built());
    let extensions = module_named(&entities, "Extensions");
    assert_eq!(
        extensions.extension_member_names,
        vec!["Tripled".to_string()],
        "only the instance augmentation is an instance extension member"
    );
}

// ── EX-0: the STATIC extension name list ────────────────────────────────────
//
// `extension_member_names` is instance-only *by design* (FCS filters a value
// receiver's extension candidates by `MethInfo.IsInstance`). But a
// `type T with static member M` in an opened module joins a **type-qualified
// static** call's group — probed 2026-07-12 through `fcs-dump overloads`:
//
//     module Ext = type System.String with static member Compare (x: int) = 3.0
//     open Ext
//     System.String.Compare 1     ⇒ call:extension, P2.Ext, Double
//
// so the OV-7 static path's extension gate needs the static names too. While the
// gate is *presence*-based this is harmless (the module's presence defers the
// call whatever its names), but a name-keyed gate reading an instance-only index
// would report "no extension named `Compare`" for a module that declares exactly
// that, and commit an intrinsic FCS might not have chosen. Hence a second,
// parallel list — and the two pin each other: `StaticExt` is in this one and
// asserted *absent* from the instance one above.
#[test]
fn static_extension_index_names_the_static_extensions() {
    let entities = load(ensure_fs_ext_index_built());
    let extensions = module_named(&entities, "Extensions");
    let mut names = extensions.static_extension_member_names.clone();
    names.sort();
    assert_eq!(
        names,
        vec!["StaticExt".to_string()],
        "the static index must name `StaticExt` (the one static extension) and \
         exclude every instance extension (`Twice`/`GenericExt`/`OptExt`) and the \
         plain `notExtension` `let`"
    );
    // The two lists are disjoint: an extension val is instance xor static.
    for n in &extensions.static_extension_member_names {
        assert!(
            !extensions.extension_member_names.contains(n),
            "`{n}` appears in both the instance and static extension indexes"
        );
    }
}
