//! Differential sweep: **the tier ladder itself**, against FCS.
//!
//! Every other type-position differential in this crate probes one surface and
//! takes the ladder as given. That ladder — which scope wins when several make
//! the same bare name visible — has been restated three times (#181, #187, and
//! the contested arm this group's sibling commit fixes), each time because a
//! review round found a scope the previous statement had not considered. The
//! systematic answer is to stop asserting the ladder and start *measuring* it.
//!
//! The corpus ([`crate::common::tier_corpus`]) plants a distinct simple name
//! for every singleton and every unordered pair of [`Tier`]s, and the probe
//! template is uniform, so one case puts exactly two scopes in contention with
//! no third party to confound the answer. Each case runs under **both**
//! reference orders, because FCS has no fixed manifest-surface/root tier at
//! all: an assembly's root-namespace contents and its
//! `[<assembly: AutoOpen>]` targets both enter the name environment when that
//! assembly is imported, so reference order decides.
//!
//! The whole matrix repeats per generic-arity shape
//! ([`crate::common::tier_corpus::Arity`]), because the resolver's type-position
//! lookup is arity-keyed and FCS's arity preference is a *fallback rather than
//! a filter*: the tier that wins a name is not necessarily the tier that wins
//! it when nothing holds the written arity.
//!
//! Two properties ride on the same matrix. The first is the crate's usual
//! **certain-implies-exact**: whenever we commit an entity for the probed name,
//! FCS's `(assembly, full name)` must agree exactly.
//!
//! The second separates a deferral from a **denial**, which a
//! certain-implies-exact oracle alone cannot do: `resolve_type_path` records
//! nothing at all on a genuine no-match, and for a single segment that silence
//! is not an absence of opinion but the resolver's positive claim that *no
//! shadow is possible* — a signal downstream consumers read. So a bare
//! (one-segment) plant denied while FCS binds something is a divergence in its
//! own right. Without that property a whole class of branch is invisible here:
//! the resolver's arity-fallback arm exists precisely to turn such a no-match
//! into a deferral when a manifest surface holds the written name at another
//! arity, and deleting the arm outright moves not one case of the first
//! property.
//!
//! Known divergences are a **two-sided ratchet** ([`KNOWN_DIVERGENCES`] and
//! [`WRONG_ARITY_DENIALS`], one per property): a case in the table must still
//! diverge, and a case outside it must not. So fixing one of the modelling
//! errors it records fails this test until the entry is removed, and a
//! regression that reintroduces one fails it too.

use std::collections::{BTreeMap, BTreeSet};

use crate::common::tier_corpus::{self, Plant, Tier};
use crate::common::{
    ensure_tier_corpus_built, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// Which assembly the probe references first. FCS imports references in this
/// order and the name environment is last-write-wins, so it is a *dimension*,
/// not an implementation detail.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Order {
    ContributorFirst,
    DecoyFirst,
}

impl Order {
    const ALL: [Order; 2] = [Order::ContributorFirst, Order::DecoyFirst];

    fn label(self) -> &'static str {
        match self {
            Order::ContributorFirst => "contributor-first",
            Order::DecoyFirst => "decoy-first",
        }
    }
}

/// The divergences this sweep records but does not fix, each with the scope
/// error it is a symptom of. Keyed `"<plant>/<order>"`.
///
/// Every row is independently FCS-verified with hand-built DLLs before being
/// recorded, so it states something about the compiler rather than rubber-stamps
/// whatever the suite happened to print.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    // `assembly_prefixes_by_priority` ranks ALL opens — the implicit ones
    // (FSharp.Core's, and namespace-shaped `[<assembly: AutoOpen>]`s) included —
    // above the enclosing namespace. FCS enters the enclosing namespace *after*
    // assembly import, so it shadows every implicit open; only explicit source
    // `open`s, which come later still, out-rank it. Order-independent, and not
    // specific to manifest surfaces: a bare `Option` from inside a namespace
    // declaring its own `Option` binds that one, not FSharp.Core's.
    //
    // **Reordering the walk alone does not fix this, and the cost was
    // measured.** Moving the enclosing namespace above the implicit opens
    // clears all six rows below and introduces no new divergence — and costs
    // `resolve_real_project_diff` 617 assembly resolutions on
    // WoofWare.PawPrint and 618 on WoofWare.PawPrint.Domain, every one a
    // deferral where FCS binds. The reason is that the walk's *shadow risks*
    // are keyed by the namespace prefix they live in but take their **rank**
    // from wherever that prefix happens to sit in the walk, so moving a tier
    // moves every veto attached to it. Four separate models are entangled with
    // the ladder, each measured on the way down:
    //
    //  1. `self_module_shadow_only` recognises a self-qualifier only in its
    //     *as-written* spelling, so the same reference reached under the
    //     enclosing-namespace prefix (`N.List.rev` for a written `List.rev`)
    //     is classified `ProjectShadowed` and preempts the opens the
    //     `module List` augmentation idiom relies on.
    //  2. `unmodelled_type_shadow_at`'s project-`[<AutoOpen>]` half is
    //     name-blind and `Preemptive`, so once the enclosing namespace is
    //     above the implicit opens it defers *every* bare annotation in any
    //     file whose namespace holds an auto-open module. Narrowing it by
    //     `project_type_named` — the file-global `TypeDefn` pre-scan already
    //     knows every project type's name — recovers 586 of Domain's 618.
    //  3. The **value** path cannot model FCS's per-member merge of a project
    //     module with an assembly namespace, so a project `module List` in the
    //     enclosing namespace preempts `Microsoft.FSharp.Collections`. 580 of
    //     PawPrint's 617; restricting the reorder to type position leaves 37.
    //  4. Those last 37 are all a bare `Result` vetoed by
    //     `auto_open_modules_in_namespace_shadow_type_named` at the enclosing
    //     prefix — but an assembly `[<AutoOpen>]` module enters scope at
    //     assembly-import time, *below* the enclosing namespace, so consulting
    //     it there is the rank/keying conflation in its purest form.
    //
    // So the fix is to give each shadow risk an explicit rank instead of
    // inheriting the prefix's position, and only then move the tier. Until
    // that lands these rows stay, and a reorder that clears them without
    // addressing (1)–(4) trades a rare wrong target for hundreds of lost ones.
    (
        "TEnNs/contributor-first",
        "implicit opens outrank the enclosing namespace in our ladder; FCS puts them below it",
    ),
    (
        "TEnNs/decoy-first",
        "implicit opens outrank the enclosing namespace in our ladder; FCS puts them below it",
    ),
    (
        "DEnNs/contributor-first",
        "as TEnNs, reached as a dotted head: the tier error is not form-specific",
    ),
    (
        "DEnNs/decoy-first",
        "as TEnNs, reached as a dotted head: the tier error is not form-specific",
    ),
    // We rank the implicit-open tier above root unconditionally. FCS has no
    // such tier: the root contents of an assembly and its manifest auto-opens
    // both enter the name environment at that assembly's import, so reference
    // order decides. The decoy-first twin of this case agrees only because our
    // fixed answer happens to be FCS's in that order.
    (
        "TNsRo/contributor-first",
        "implicit-open vs root is decided by reference order in FCS; our ladder fixes it",
    ),
    (
        "DNsRo/contributor-first",
        "as TNsRo, reached as a dotted head: the tier error is not form-specific",
    ),
    // The arity-1 twins of the two errors above. Both reproduce unchanged one
    // arity up, which is the evidence that they are tier errors rather than
    // anything the arity-keyed lookup introduces.
    (
        "GEnNs/contributor-first",
        "as TEnNs at arity 1: the enclosing-namespace error is not arity-specific",
    ),
    (
        "GEnNs/decoy-first",
        "as TEnNs at arity 1: the enclosing-namespace error is not arity-specific",
    ),
    (
        "GNsRo/contributor-first",
        "as TNsRo at arity 1: the reference-order error is not arity-specific",
    ),
];

/// The shared reason for [`WRONG_ARITY_DENIALS`], stated once because every
/// row is the same error at a different tier.
const WRONG_ARITY_DENIAL: &str = "with the name's only occupants at another arity and no manifest surface among them, the \
     arity-keyed walk reports a genuine no-match and we record nothing — but FCS's arity \
     preference is a fallback, not a filter, so it binds the wrong-arity type (with an arity \
     error) and our silence wrongly says no shadow is possible";

/// Divergences of the **second** property: cases where we deny that anything
/// could bind and FCS binds something.
///
/// One error, enumerated per case rather than derived from a predicate — a
/// generated table would quietly re-fit itself around a partial fix, and the
/// ratchet's job is to make each case an individual commitment.
///
/// `decide_type_path` turns a no-match into a deferral when a *manifest
/// surface* holds the written name at another arity; the `W` cases whose
/// contenders include `ModAuto` or `Contested` are absent from this list for
/// exactly that reason. The same fallback happens with no manifest surface in
/// sight, and there the walk still denies.
const WRONG_ARITY_DENIALS: &[&str] = &[
    "WEn/contributor-first",
    "WEn/decoy-first",
    "WEnNs/contributor-first",
    "WEnNs/decoy-first",
    "WEnRo/contributor-first",
    "WEnRo/decoy-first",
    "WEx/contributor-first",
    "WEx/decoy-first",
    "WExEn/contributor-first",
    "WExEn/decoy-first",
    "WExNs/contributor-first",
    "WExNs/decoy-first",
    "WExRo/contributor-first",
    "WExRo/decoy-first",
    "WNs/contributor-first",
    "WNs/decoy-first",
    "WNsRo/contributor-first",
    "WNsRo/decoy-first",
    "WRo/contributor-first",
    "WRo/decoy-first",
];

/// Every recorded divergence, from both tables, as `(key, reason)`.
fn known_divergences() -> BTreeMap<&'static str, &'static str> {
    KNOWN_DIVERGENCES
        .iter()
        .copied()
        .chain(WRONG_ARITY_DENIALS.iter().map(|k| (*k, WRONG_ARITY_DENIAL)))
        .collect()
}

/// The `(assembly, full name)` currency both oracles report in.
type Target = (String, String);

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Name the tier a reported target came from, for a readable failure. `None`
/// when the target is not one of the plant's own declarations — which is
/// itself the interesting case, so the caller prints the raw target too.
fn tier_of(plant: &Plant, target: &Target) -> Option<Tier> {
    plant
        .tiers
        .iter()
        .copied()
        .find(|&t| plant.declaration(t) == (target.0.as_str(), target.1.clone()))
}

fn describe(plant: &Plant, target: &Target) -> String {
    match tier_of(plant, target) {
        Some(t) => format!("{t:?}"),
        None => format!("<not a plant declaration: {}/{}>", target.0, target.1),
    }
}

/// Our verdict for one probe. The three cases are genuinely different claims,
/// and collapsing the last two — as "an `Option<Target>`" does — is what hides
/// the arity-fallback branch from this sweep.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ours {
    /// We committed this entity. Bound by certain-implies-exact.
    Entity(Target),
    /// We recorded a deferral: something may bind here, but we cannot say
    /// what. Makes no claim, and is always sound.
    Deferred,
    /// We denied that anything can bind. For a single-segment name, recording
    /// nothing is not an absence of opinion but an opinion — the resolver's
    /// "no shadow is possible" signal — and [`Resolution::Unresolved`] is the
    /// same claim made explicitly. Either is a claim FCS can contradict.
    Denied,
}

/// Our verdict for one probe, at the plant's span.
fn our_target(env: &AssemblyEnv, src: &str, plant: &Plant) -> Ours {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "tier probe for {} does not parse: {:?}",
        plant.name,
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("probe is an impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), env);
    let (start, end) = tier_corpus::probe_use_span(src, plant);
    match rf.resolution_at(span(start, end)) {
        // `entity_full_name` is the currency `fcs-dump` was taught to report
        // in: nesting-aware, and named from `source_name` so a generic's
        // compiled backtick arity never reaches the comparison.
        Some(Resolution::Entity(h)) => {
            Ours::Entity((env.entity(h).assembly.name.clone(), env.entity_full_name(h)))
        }
        Some(Resolution::Deferred(_)) => Ours::Deferred,
        Some(Resolution::Unresolved) => Ours::Denied,
        None => Ours::Denied,
        // Nothing else is reachable from this corpus, and each would be a
        // distinct bug rather than a deferral: the probe declares no type of
        // the plant's name, is resolved against an empty `ProjectItems`, and
        // binds no local — so a `Local`/`Item` verdict means the walk
        // wrong-targeted an in-file or project binder, and a `Member` one
        // means it bound a *value* for a type-position name. A catch-all arm
        // would launder all three into "no claim", which is exactly the
        // collapse this sweep exists to prevent.
        Some(res @ (Resolution::Local(_) | Resolution::Item(_) | Resolution::Member { .. })) => {
            panic!(
                "tier probe {}: type position resolved to {res:?}",
                plant.name
            )
        }
    }
}

/// FCS's verdict for one probe.
fn fcs_target(refs: &[&std::path::Path], src: &str, plant: &Plant) -> Option<Target> {
    let (start, end) = tier_corpus::probe_use_span(src, plant);
    let path = temp_fs_file("tier_order", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, refs);
    let _ = std::fs::remove_file(&path);
    parse_fcs_uses(&json, src)
        .into_iter()
        .find(|u| {
            !u.is_from_definition
                && u.start <= start
                && end <= u.end
                && u.name == plant.probed_ident()
        })
        .and_then(|u| Some((u.assembly?, u.full_name?)))
}

/// Every case's `(key, ours, fcs)`, one reference order at a time so the two
/// `AssemblyEnv`s and the two `-r` orders stay in lock step.
fn observe() -> BTreeMap<String, (Ours, Option<Target>)> {
    let (contributor, decoy) = ensure_tier_corpus_built();
    let contributor_bytes = std::fs::read(contributor).expect("read tier contributor dll");
    let decoy_bytes = std::fs::read(decoy).expect("read tier decoy dll");
    let plants = tier_corpus::corpus();

    let mut out = BTreeMap::new();
    for order in Order::ALL {
        let views = match order {
            Order::ContributorFirst => [&contributor_bytes, &decoy_bytes],
            Order::DecoyFirst => [&decoy_bytes, &contributor_bytes],
        }
        .map(|b| Ecma335Assembly::parse(b).expect("parse tier fixture dll"));
        let refs: Vec<&std::path::Path> = match order {
            Order::ContributorFirst => vec![contributor, decoy],
            Order::DecoyFirst => vec![decoy, contributor],
        };
        let env = AssemblyEnv::from_views(&views).expect("build tier AssemblyEnv");

        for plant in &plants {
            let src = tier_corpus::probe_source(plant);
            let ours = our_target(&env, &src, plant);
            let fcs = fcs_target(&refs, &src, plant);
            out.insert(plant.key(order.label()), (ours, fcs));
        }
    }
    out
}

#[test]
fn tier_ladder_is_sound_against_fcs() {
    let plants: BTreeMap<String, Plant> = tier_corpus::corpus()
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    let observations = observe();

    let mut diverged: BTreeMap<String, String> = BTreeMap::new();
    let mut agreed = 0usize;
    let mut deferred = 0usize;
    for (key, (ours, fcs)) in &observations {
        let plant = &plants[key.split('/').next().expect("keyed <plant>/<order>")];
        match (ours, fcs) {
            (Ours::Deferred, _) => deferred += 1,
            // Recording nothing is a claim only where the resolver makes one:
            // a single-segment deferral records the shadowable marker, but a
            // dotted one records nothing either, so a `DottedHead` plant's
            // silence is indistinguishable from a deferral and asserts nothing.
            (Ours::Denied, _) if plant.form == tier_corpus::Form::DottedHead => deferred += 1,
            (Ours::Denied, None) => deferred += 1,
            (Ours::Denied, Some(f)) => {
                diverged.insert(
                    key.clone(),
                    format!(
                        "we deny that anything can bind — the \"no shadow is possible\" signal — \
                         but FCS binds {}",
                        describe(plant, f)
                    ),
                );
            }
            (Ours::Entity(o), Some(f)) if o == f => agreed += 1,
            (Ours::Entity(o), Some(f)) => {
                diverged.insert(
                    key.clone(),
                    format!(
                        "we bound {} but FCS binds {}",
                        describe(plant, o),
                        describe(plant, f)
                    ),
                );
            }
            (Ours::Entity(o), None) => {
                diverged.insert(
                    key.clone(),
                    format!(
                        "we bound {} but FCS resolves the span to nothing at all",
                        describe(plant, o)
                    ),
                );
            }
        }
    }

    // Non-vacuity, per case rather than by a count: a corpus that stopped
    // building, or a probe template that stopped resolving, would otherwise
    // pass this test by committing nothing at all. The floor is the
    // **uncontested** plants at the tiers the resolver is supposed to commit —
    // nothing contends for those names in either order, so anything but exact
    // agreement is a bug. (`TCo` and `TMo` are deliberately absent: a manifest
    // surface is never committed, only deferred to, which is the design. So is
    // every `W`: with no exact-arity occupant anywhere, a *deferral* is the
    // right answer even uncontested, so it cannot serve as a floor.)
    for control in [
        "TEx", "TEn", "TNs", "TRo", //
        "DEx", "DEn", "DNs", "DRo", //
        "GEx", "GEn", "GNs", "GRo",
    ] {
        for order in Order::ALL {
            let key = format!("{control}/{}", order.label());
            let (ours, fcs) = &observations[&key];
            let fcs = fcs.as_ref().unwrap_or_else(|| {
                panic!("{key}: FCS resolved nothing — corpus broken?");
            });
            assert_eq!(
                ours,
                &Ours::Entity(fcs.clone()),
                "{key}: an uncontested name must resolve, and to what FCS says",
            );
        }
    }

    let known = known_divergences();
    let expected: BTreeSet<&str> = known.keys().copied().collect();
    let observed: BTreeSet<&str> = diverged.keys().map(String::as_str).collect();

    let unexpected: Vec<String> = observed
        .difference(&expected)
        .map(|k| format!("  {k}: {}", diverged[*k]))
        .collect();
    let stale: Vec<String> = expected
        .difference(&observed)
        .map(|k| {
            format!(
                "  {k}: recorded as diverging ({}), but it now agrees or defers",
                known[k]
            )
        })
        .collect();

    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "tier ladder diverges from FCS.\n\
         NEW divergences (a wrong target — fix, or record in KNOWN_DIVERGENCES with an \
         FCS-verified reason):\n{}\n\
         STALE entries (remove them; the ratchet is two-sided so a fix must land with its \
         entry):\n{}\n\
         ({agreed} agreed, {deferred} deferred, {} cases total)",
        if unexpected.is_empty() {
            "  (none)".to_string()
        } else {
            unexpected.join("\n")
        },
        if stale.is_empty() {
            "  (none)".to_string()
        } else {
            stale.join("\n")
        },
        observations.len(),
    );
}

/// The measurement behind the ladder, printed rather than asserted: which tier
/// FCS picks for every contest, in both reference orders. Run it when a scope
/// question comes up instead of reasoning about the ladder from the comments.
///
/// `#[ignore]`d — it is a report, and its value is the output, not a verdict.
#[test]
#[ignore = "report generator; run explicitly with --ignored --nocapture"]
fn report_tier_ladder() {
    let plants: BTreeMap<String, Plant> = tier_corpus::corpus()
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    for (key, (ours, fcs)) in observe() {
        let plant = &plants[key.split('/').next().expect("keyed <plant>/<order>")];
        let show_fcs = match &fcs {
            Some(t) => describe(plant, t),
            None => "-".to_string(),
        };
        let show_ours = match &ours {
            Ours::Entity(t) => describe(plant, t),
            Ours::Deferred => "(defer)".to_string(),
            Ours::Denied => "(denied)".to_string(),
        };
        println!(
            "{key:<40} {:<13} contenders={:<30} fcs={:<12} ours={show_ours}",
            format!("{:?}", plant.arity),
            format!("{:?}", plant.tiers),
            show_fcs,
        );
    }
}
