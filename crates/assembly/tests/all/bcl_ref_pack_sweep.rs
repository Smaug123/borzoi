//! Exact projection record of the SDK's `Microsoft.NETCore.App.Ref` reference
//! pack — the ~170 reference assemblies every .NET build resolves against, and
//! so the exact real-world surface the LSP's assembly reader must hold.
//!
//! The projector's "bound uncertainty" posture drops-and-records what it
//! cannot model (`Entity::skipped_members`, the assembly-level dropped-type
//! list) instead of failing, which is right for the LSP — but it means coverage
//! can regress *silently*: a change that suddenly drops a thousand members
//! still enumerates `Ok`. The fixtures pin individual constructs; this sweep
//! pins the aggregate. Every pack DLL must parse and enumerate, no *type* may
//! be dropped (a hard assertion), and the rest — every kept type with its
//! member count, and every dropped member with its reason — is checked
//! **exactly** against a checked-in manifest, so a handful of members lost (or
//! gained) anywhere in the pack fails the run with a line diff naming them.
//!
//! # What pins the input
//!
//! The pack is whichever one the SDK on `PATH` ships ([`sdk_ref_pack_dir`]).
//! Under `nix develop` — the only way CI and the documented commands run this
//! — that SDK comes from the flake's pinned nixpkgs, so the pack is the same
//! on every machine and platform (reference assemblies are platform-neutral).
//! Outside the devshell it is whatever SDK is installed, so the manifest's
//! first entry names the pack version it describes: on another SDK the run
//! fails on that line first, rather than on a wall of member counts.
//!
//! An intended movement — a reader improvement, or a flake bump that moves
//! the SDK — is acknowledged by regenerating the manifest and committing the
//! diff; the failure message prints the command.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use std::path::{Path, PathBuf};

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};

use crate::common::sdk_ref_pack_dir;

/// The checked-in manifest `name` of this sweep.
fn manifest_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/manifests")
        .join(format!("{name}.txt"))
}

/// The command that regenerates the manifest of the test `test`.
fn regenerate(test: &str) -> String {
    format!(
        "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-assembly --test all \
         bcl_ref_pack_sweep::{test}"
    )
}

/// The pack's identity as a manifest entry: `(pack) <version>/ref/<tfm>`, the
/// last three components of [`sdk_ref_pack_dir`]. Sorts ahead of every DLL
/// entry.
fn pack_entry(dir: &Path) -> String {
    let tail: Vec<String> = dir
        .components()
        .rev()
        .take(3)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let tail: Vec<&str> = tail.iter().rev().map(String::as_str).collect();
    format!("(pack) {}", tail.join("/"))
}

/// Every `.dll` in the pack, sorted, with its file name.
fn pack_dlls(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut dlls: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read ref pack dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("dll path has a file name")
                .to_string_lossy()
                .into_owned();
            (name, p)
        })
        .collect();
    dlls.sort();
    assert!(!dlls.is_empty(), "no reference assemblies in {dir:?}");
    dlls
}

/// Parse and enumerate one pack DLL. Reference assemblies are ordinary managed
/// images; a parse or enumeration *error* on one is a plain regression — every
/// one of them projects `Ok`.
fn project(name: &str, path: &Path) -> (Vec<Entity>, borzoi_assembly::AssemblyProjectionSkips) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read ref-pack DLL {name}: {e}"));
    let view = Ecma335Assembly::parse(&bytes)
        .unwrap_or_else(|e| panic!("ref-pack DLL {name} failed to parse: {e}"));
    view.enumerate_type_defs_with_skips()
        .unwrap_or_else(|e| panic!("ref-pack DLL {name} failed to enumerate: {e}"))
}

/// Visit `entities` and their nested types, each with its key: the dotted
/// namespace and name, nested types joined by `/`, and a generic type's
/// parameter count after a backtick (`System.Action`2`), since the projected
/// name drops IL's arity suffix and `Action` and `Action<T>` would otherwise
/// share a key. A nested type counts its enclosing type's parameters too, as
/// metadata repeats them.
fn walk_types<'a>(
    entities: &'a [Entity],
    parent: Option<&str>,
    f: &mut impl FnMut(&str, &'a Entity),
) {
    for e in entities {
        let name = match e.generic_parameters.len() {
            0 => e.name.clone(),
            n => format!("{}`{n}", e.name),
        };
        let key = match parent {
            Some(p) => format!("{p}/{name}"),
            None if e.namespace.is_empty() => name,
            None => format!("{}.{name}", e.namespace.join(".")),
        };
        f(&key, e);
        walk_types(&e.nested_types, Some(&key), f);
    }
}

#[test]
fn ref_pack_projection_matches_the_manifest() {
    let dir = sdk_ref_pack_dir();
    let mut entries = vec![pack_entry(&dir)];
    let mut type_drops: Vec<String> = Vec::new();
    let (mut total_types, mut total_members, mut total_member_drops) = (0usize, 0usize, 0usize);

    for (dll, path) in pack_dlls(&dir) {
        let (entities, skips) = project(&dll, &path);
        type_drops.extend(
            skips
                .dropped_types
                .iter()
                .map(|t| format!("{dll} {}: {}", t.name, t.reason)),
        );
        walk_types(&entities, None, &mut |key, e| {
            total_types += 1;
            total_members += e.members.len();
            total_member_drops += e.skipped_members.len();
            entries.push(format!("{dll} {key} members={}", e.members.len()));
            entries.extend(
                e.skipped_members
                    .iter()
                    .map(|m| format!("{dll} {key} dropped-member {}: {}", m.name, m.reason)),
            );
        });
    }
    eprintln!(
        "[bcl_ref_pack_sweep] {}: {total_types} types, {total_members} members kept, \
         {total_member_drops} member drops, {} type drops",
        pack_entry(&dir),
        type_drops.len(),
    );

    // **Zero**, asserted before the manifest so regeneration cannot bless one.
    // A whole-type drop is a different grade of loss from a member drop: it also
    // makes the type's namespace unknowable to the overload engine's extension
    // gate, so one stale metadata assumption costs overload coverage in every
    // file that opens that namespace. (Refusing the `[Nullable]` that the BCL
    // puts on constraint rows such as `where TSelf : IParsable<TSelf>` would
    // drop 38 types from `System.Runtime` alone.)
    assert!(
        type_drops.is_empty(),
        "{} types dropped from the reference pack:\n{}",
        type_drops.len(),
        type_drops.join("\n"),
    );

    // A type's overloads can share a name and a drop reason, so entries are a
    // multiset.
    let manifest =
        Manifest::from_counted(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check(
        &manifest_path("bcl_ref_pack_projection"),
        &manifest,
        &regenerate("ref_pack_projection_matches_the_manifest"),
    );
}

/// `(kind, IL name, is_static, has explicit-interface entries)` for every
/// member of `e` that can carry them.
fn member_explicit_flags(e: &Entity) -> Vec<(&'static str, &str, bool, bool)> {
    e.members
        .iter()
        .filter_map(|m| match m {
            borzoi_assembly::Member::Method(m) => Some((
                "method",
                m.name.as_str(),
                m.is_static,
                !m.implements.is_empty(),
            )),
            borzoi_assembly::Member::Property(p) => Some((
                "property",
                p.name.as_str(),
                p.is_static,
                !p.implements.is_empty(),
            )),
            borzoi_assembly::Member::Event(ev) => Some((
                "event",
                ev.name.as_str(),
                ev.is_static,
                !ev.implements.is_empty(),
            )),
            borzoi_assembly::Member::Field(_) => None,
        })
        .collect()
}

#[test]
fn explicit_impl_classification_agrees_with_roslyn_convention_across_the_pack() {
    // Corpus-wide differential between two independent notions of "implements
    // an interface member":
    //
    // - ours, classified from the `MethodImpl` declaration target (interface
    //   vs base class) — see `reader/members.rs::apply_method_impls`;
    // - Roslyn's, readable off the member name: Roslyn name-mangles exactly
    //   the *explicit* interface implementations (`IFace<…>.Member`, hence a
    //   `.`) and nothing else it emits into a reference assembly.
    //
    // Over ~170 real assemblies:
    //
    // - every dotted member must be flagged (a miss is a wrongly-skipped row
    //   or a regressed interface decode) — both instance and static;
    // - a flagged *instance* member must be dotted (a plain-named one means a
    //   base-class override was misclassified as an interface impl: instance
    //   `MethodImpl` rows exist only for explicit impls, which Roslyn always
    //   mangles);
    // - a flagged plain-named *static* member is correct and expected: static
    //   interface members have no vtable slot, so even *implicit* impls (C#11
    //   generic math — `NFloat` satisfying `INumberBase<NFloat>.Parse`) are
    //   wired through `MethodImpl`. The 10.0 pack carries ~1,100 of them.
    //
    // The two oracles share no code — the name is never consulted by the
    // classifier — so agreement here is evidence, not tautology. (The shapes
    // where the notions *deliberately* diverge are compiler-unreachable;
    // `methodimpl_classification.rs` pins them from fabricated IL.)
    //
    // The convention cannot see a flag lost from a plain-named static member —
    // that is an implicit impl going unrecognised, which the convention allows
    // either way. So every type's flagged and implicit-static counts are pinned
    // exactly by a manifest: a classifier that silently dropped some (or all)
    // of a category fails it, naming the types.
    let dir = sdk_ref_pack_dir();

    let mut violations: Vec<String> = Vec::new();
    let mut entries = vec![pack_entry(&dir)];
    let (mut flagged_total, mut implicit_static_total) = (0usize, 0usize);
    for (dll, path) in pack_dlls(&dir) {
        let (entities, _skips) = project(&dll, &path);
        walk_types(&entities, None, &mut |key, e| {
            let (mut flagged, mut implicit_static) = (0usize, 0usize);
            for (kind, name, is_static, is_flagged) in member_explicit_flags(e) {
                let is_ctor = name == ".ctor" || name == ".cctor";
                let dotted = name.contains('.') && !is_ctor;
                if is_flagged {
                    flagged += 1;
                }
                let violation = if is_flagged && !dotted && is_static {
                    implicit_static += 1; // implicit static impl — expected
                    false
                } else {
                    is_flagged != dotted
                };
                if violation {
                    violations.push(format!(
                        "{dll}: {kind} `{key}::{name}` — dotted={dotted}, static={is_static}, \
                         flagged={is_flagged}",
                    ));
                }
            }
            if flagged > 0 {
                entries.push(format!(
                    "{dll} {key} interface-impls={flagged} implicit-static={implicit_static}"
                ));
            }
            flagged_total += flagged;
            implicit_static_total += implicit_static;
        });
    }

    assert!(
        violations.is_empty(),
        "{} classification/convention disagreements across the pack:\n{}",
        violations.len(),
        violations.join("\n"),
    );
    eprintln!(
        "[bcl_ref_pack_sweep] {flagged_total} interface-member impls recognised \
         across the pack ({implicit_static_total} implicit static)"
    );
    let manifest =
        Manifest::from_counted(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check(
        &manifest_path("bcl_ref_pack_interface_impls"),
        &manifest,
        &regenerate("explicit_impl_classification_agrees_with_roslyn_convention_across_the_pack"),
    );
}
