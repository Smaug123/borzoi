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

    // Ours → FCS: every fixture resolution we made inside the probe's
    // ANNOTATION region must be confirmed by a covering FCS fixture use —
    // including when FCS reports no use there at all (an FS0039-erroring
    // annotation yields none, so a sema commitment on it is a divergence the
    // lenient "only if covered" form silently passed; codex P2, round 6).
    // The generated probes come from two templates — the type-position
    // `let f (x: N) = x` and the expression-position `let x = N ()` — so the
    // region is derivable from whichever one this case used.
    let (anno_start, anno_end) = match src.find("(x: ") {
        Some(at) => (
            at + "(x: ".len(),
            src.find(") = x").expect("type probe template"),
        ),
        None => {
            let at = src.rfind("let x = ").expect("expr probe template") + "let x = ".len();
            (at, src.len())
        }
    };
    for (range, res) in rf.resolutions() {
        if !matches!(res, Resolution::Entity(_) | Resolution::Member { .. }) {
            continue;
        }
        let (start, end) = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        if start < anno_start || end > anno_end {
            continue;
        }
        assert!(
            uses.iter().any(|u| u.start != u.end
                && u.start <= start
                && end <= u.end
                && u.assembly.as_deref() == Some(FIXTURE_ASM)),
            "{src:?}: we resolved {start}..{end} into the fixture ({:?}), but no FCS \
             fixture use covers that span — a wrong target or a commitment FCS errors on",
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

/// The **expression**-position twin of
/// [`manifest_module_surface_type_positions_are_sound_against_fcs`]: every name
/// the fixture's bare-visible surfaces could supply, used as a bare constructor
/// (`let x = <Name> ()`), checked certain-implies-exact against FCS.
///
/// This is the systematic backstop for
/// [`AssemblyEnv::assembly_bare_value_surface_could_supply`]. The type sweep
/// cannot stand in for it: the two positions ask different questions of the same
/// metadata — a value never shadows a type, so type position is *right* to
/// ignore module values and union cases, and expression position is precisely
/// where they win. Every arm of that query is therefore invisible to the sweep
/// above, and each omitted arm was a separate review finding until this existed.
///
/// The name set is enumerated from the assembly rather than hand-listed, so a
/// shape added to the fixture is probed automatically.
#[test]
fn bare_constructor_fallback_is_sound_over_every_bare_value_surface() {
    let fixture = ensure_autoopen_fixture_built();
    let bytes = std::fs::read(fixture).expect("read autoopen fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse autoopen fixture dll");
    use borzoi_assembly::EcmaView;
    let entities = view.enumerate_type_defs().expect("enumerate fixture");

    // Candidate names: everything under the manifest-opened module (its values,
    // nested types and submodule contents), every global-namespace public name,
    // and every union case anywhere — the three surfaces the value query walks.
    let mut names = BTreeSet::new();
    let direct_ops = entities
        .iter()
        .find(|e| e.namespace == ["SemaAutoOpen"] && e.name == "DirectOps")
        .expect("fixture must declare SemaAutoOpen.DirectOps");
    collect_names(direct_ops, &mut names);
    // The manifest-opened module's own `let` values — public statics, the
    // surface `lookup` cannot see and the reason this sweep exists.
    for m in &direct_ops.members {
        if plain_ident(member_name(m)) {
            names.insert(member_name(m).to_string());
        }
    }
    // The ROOT namespace: public names, the values of its modules, and its
    // unions' cases — all bare-visible with no `open` at all.
    //
    // Deliberately root-scoped, matching the query's arms. A namespace-scoped
    // union's cases need an `open` and are not this query's business, and
    // sweeping every assembly module's members would drag in the *implicit*
    // `Microsoft.FSharp.Core` open — a different mechanism, resolved through the
    // auto-open member path (a `Resolution::Member`, which the constructor
    // fallback cannot emit) and divergent from FCS for reasons predating it.
    for e in entities.iter().filter(|e| e.namespace.is_empty()) {
        if e.access != Access::Public {
            continue;
        }
        if plain_ident(source_name(e)) {
            names.insert(source_name(e).to_string());
        }
        if let Some(cases) = &e.union_case_names {
            for case in cases.iter().filter(|c| plain_ident(c)) {
                names.insert(case.clone());
            }
        }
        if e.kind == EntityKind::Module {
            for m in &e.members {
                if plain_ident(member_name(m)) {
                    names.insert(member_name(m).to_string());
                }
            }
        }
    }
    assert!(
        names.len() > 20,
        "expected a broad candidate set, got {}",
        names.len()
    );

    let mut cases = BTreeSet::new();
    for name in &names {
        cases.insert(format!("let x = {name} ()\n"));
        cases.insert(format!("let x = {name}\n"));
    }

    let (mut agreed, mut fixture_uses) = (0usize, 0usize);
    for case in &cases {
        let (a, f) = sweep_sound(case);
        agreed += a;
        fixture_uses += f;
    }
    // Non-vacuity, and the one thing this sweep *cannot* assert.
    //
    // FCS must actually bind fixture symbols across the sweep, or the oracle is
    // miswired. But we deliberately do NOT require any agreement: this fixture
    // declares `[<assembly: AutoOpen("SemaAutoOpen.NoSuchPath")>]`, an
    // unresolvable target, which makes the whole env's auto-open surface
    // `extension_surface_unknowable` — so an unseen auto-open really could
    // supply any name, and total deferral is the *correct* answer here, not a
    // disabled fallback. Requiring agreements would force the veto to be
    // narrower than the metadata justifies.
    //
    // Non-vacuity for the commit side therefore lives where the metadata is
    // knowable: `resolve_assembly_diff`'s
    // `bare_constructor_fallback_is_sound_over_every_demo_type`, whose C#
    // fixture has no manifest auto-opens at all, asserts the fallback resolves.
    assert!(
        fixture_uses >= cases.len() / 4,
        "sweep vacuous: only {fixture_uses} fixture uses across {} cases",
        cases.len()
    );
    eprintln!(
        "[value-surface-sweep] {agreed} agreed / {fixture_uses} fixture uses over {} cases",
        cases.len()
    );
}
