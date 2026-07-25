//! Differential sweep: the **module-shaped manifest AutoOpen surface** against
//! FCS — the systematic guard behind `decide_type_path`'s manifest veto.
//!
//! `[<assembly: AutoOpen("SemaAutoOpen.DirectOps")>]` opens a module whose
//! import surface FCS computes along several independent dimensions —
//! accessibility (private children are not imported), submodule opening (a
//! plain nested module is a dotted head only; an `[<AutoOpen>]` one opens
//! recursively), priority (below explicit source `open`s, above the
//! enclosing-namespace and root tiers), and position (a module never binds
//! type position). Each dimension the veto under-modelled was a separate
//! review round; this sweep replaces that reviewer intelligence with an
//! oracle.
//!
//! The cases are **generated, not curated**: every type name reachable in the
//! fixture's `DirectOps` tree, every global-namespace decoy, and every
//! `Module.Type` pair under `DirectOps`, each probed bare and under an
//! explicit `open` — so a fixture edit that adds a new surface shape is swept
//! automatically. The property is D5 soundness, certain-implies-exact in both
//! directions: whenever we commit an `Entity` at a range FCS also resolves
//! into the fixture, the `(assembly, full name)` must agree exactly; whenever
//! we commit into the fixture and FCS resolves that span elsewhere, we have
//! wrong-targeted. A deferral makes no claim (completeness is the FCS-free
//! tests' job — `resolve_autoopen.rs` asserts the specific commit/defer
//! verdicts per shape).

use std::collections::BTreeSet;

use crate::common::{
    ensure_autoopen_fixture_built, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::{Access, Ecma335Assembly, Entity, EntityKind, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// The fixture's `<AssemblyName>`, as FCS reports declaring assemblies.
const FIXTURE_ASM: &str = "SemaAutoOpenFixture";

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn member_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => &x.name,
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

/// The F# source name of an entity (the name a probe would write).
fn source_name(e: &Entity) -> &str {
    e.source_name.as_deref().unwrap_or(e.name.as_str())
}

/// Whether `n` is a plain identifier a generated probe can write bare —
/// filters compiler-generated (`<StartupCode$…>`) and backtick-mangled names.
fn plain_ident(n: &str) -> bool {
    let mut chars = n.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every public descendant's source name under `e` (any depth — deliberately
/// deeper than the imported surface, so the sweep also probes names the veto
/// must NOT defer, like a plain submodule's contents).
fn collect_names(e: &Entity, out: &mut BTreeSet<String>) {
    for child in &e.nested_types {
        if child.access == Access::Public && plain_ident(source_name(child)) {
            out.insert(source_name(child).to_string());
        }
        collect_names(child, out);
    }
}

/// The `(assembly, full name)` our resolution names, for comparison with FCS.
/// The namespace-joined form is exact for **top-level** entities — the only
/// ones the resolver commits here (manifest-surface nested entities are
/// deferred, never committed) — and a nested commit would fail the diff
/// loudly rather than silently pass.
fn our_assembly_full(env: &AssemblyEnv, res: Resolution) -> (String, String) {
    fn full(ns: &[String], name: &str) -> String {
        if ns.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", ns.join("."), name)
        }
    }
    match res {
        Resolution::Entity(h) => {
            let e = env.entity(h);
            (e.assembly.name.clone(), full(&e.namespace, &e.name))
        }
        Resolution::Member { parent, idx } => {
            let e = env.entity(parent);
            (
                e.assembly.name.clone(),
                format!(
                    "{}.{}",
                    full(&e.namespace, &e.name),
                    member_name(env.member_at(parent, idx))
                ),
            )
        }
        _ => unreachable!("only Entity/Member reach here"),
    }
}

/// Assert the D5 soundness property for one probe source, both directions
/// (the shape of `resolve_assembly_diff::sweep_sound`, over the autoopen
/// fixture). Returns `(agreed, fixture_uses)` for the sweep's non-vacuity
/// floor.
fn sweep_sound(src: &str) -> (usize, usize) {
    let fixture = ensure_autoopen_fixture_built();
    let bytes = std::fs::read(fixture).expect("read autoopen fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse autoopen fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &env);

    let path = temp_fs_file("manifest_surface", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, &[fixture]);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, src);

    // FCS → ours: every use FCS resolves into the fixture is matched exactly
    // or honestly deferred/unrecorded — never a different entity.
    let (mut agreed, mut fixture_uses) = (0usize, 0usize);
    for u in &uses {
        if u.start == u.end || u.assembly.as_deref() != Some(FIXTURE_ASM) {
            continue;
        }
        fixture_uses += 1;
        match rf.resolution_at(span(u.start, u.end)) {
            None | Some(Resolution::Deferred(_)) => {}
            Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                let (asm, full) = our_assembly_full(&env, res);
                assert_eq!(
                    Some(asm.as_str()),
                    u.assembly.as_deref(),
                    "{src:?} use {:?}: assembly",
                    u.name
                );
                assert_eq!(
                    Some(full.as_str()),
                    u.full_name.as_deref(),
                    "{src:?} use {:?}: full name",
                    u.name
                );
                agreed += 1;
            }
            other => panic!(
                "{src:?} use {:?} at {}..{} resolves into the fixture, but we gave {other:?}",
                u.name, u.start, u.end
            ),
        }
    }

    // Ours → FCS: a fixture resolution we made that FCS resolves *elsewhere*
    // (no covering fixture use) is a wrong target.
    for (range, res) in rf.resolutions() {
        if !matches!(res, Resolution::Entity(_) | Resolution::Member { .. }) {
            continue;
        }
        let (start, end) = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let covering = uses
            .iter()
            .filter(|u| u.start != u.end && u.start <= start && end <= u.end);
        let (mut any_covering, mut any_fixture) = (false, false);
        for u in covering {
            any_covering = true;
            if u.assembly.as_deref() == Some(FIXTURE_ASM) {
                any_fixture = true;
            }
        }
        assert!(
            !any_covering || any_fixture,
            "{src:?}: we resolved {start}..{end} into the fixture ({:?}), but FCS resolves \
             that span elsewhere (no covering fixture use) — a wrong target",
            our_assembly_full(&env, *res),
        );
    }
    (agreed, fixture_uses)
}

#[test]
fn manifest_module_surface_type_positions_are_sound_against_fcs() {
    let fixture = ensure_autoopen_fixture_built();
    let bytes = std::fs::read(fixture).expect("read autoopen fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse autoopen fixture dll");
    use borzoi_assembly::EcmaView;
    let entities = view.enumerate_type_defs().expect("enumerate fixture");

    let direct_ops = entities
        .iter()
        .find(|e| e.namespace == ["SemaAutoOpen"] && e.name == "DirectOps")
        .expect("fixture must declare SemaAutoOpen.DirectOps");

    // Every type name anywhere under the manifest-opened module — including
    // shapes the veto must NOT defer (private children, plain-submodule
    // contents) — plus every global-namespace decoy.
    let mut names = BTreeSet::new();
    collect_names(direct_ops, &mut names);
    for e in &entities {
        if e.namespace.is_empty() && e.access == Access::Public && plain_ident(source_name(e)) {
            names.insert(source_name(e).to_string());
        }
    }

    // Every `Module.Type` dotted pair under `DirectOps` — the dotted-head
    // channel (a nested module roots a path whether or not it is auto-open).
    let mut dotted = BTreeSet::new();
    for child in &direct_ops.nested_types {
        if child.kind == EntityKind::Module && child.access == Access::Public {
            for inner in &child.nested_types {
                if inner.access == Access::Public && plain_ident(source_name(inner)) {
                    dotted.insert(format!("{}.{}", source_name(child), source_name(inner)));
                }
            }
        }
    }

    let mut cases = BTreeSet::new();
    for name in names.iter().chain(dotted.iter()) {
        cases.insert(format!("let f (x: {name}) = x\n"));
        // The explicit-open stratum: an explicit source `open` outranks the
        // manifest surface, so every name is probed under one too.
        cases.insert(format!(
            "open SemaAutoOpen.ExplicitBeats\nlet f (x: {name}) = x\n"
        ));
    }

    let (mut agreed, mut fixture_uses) = (0usize, 0usize);
    for case in &cases {
        let (a, f) = sweep_sound(case);
        agreed += a;
        fixture_uses += f;
    }
    // Non-vacuity: the sweep must both see fixture bindings (FCS resolves the
    // decoys and surface types) and agree on some (the decoy commits) — a
    // silent all-deferral or an oracle wiring failure would zero these.
    assert!(
        fixture_uses >= cases.len() / 2,
        "sweep vacuous: only {fixture_uses} fixture uses across {} cases",
        cases.len()
    );
    assert!(
        agreed >= 4,
        "sweep vacuous: only {agreed} agreements — the decoy commits should agree"
    );
}
