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
//! The property is the crate's usual **certain-implies-exact**: whenever we
//! commit an entity for the probed name, FCS's `(assembly, full name)` must
//! agree exactly. A deferral makes no claim.
//!
//! Known divergences are a **two-sided ratchet** ([`KNOWN_DIVERGENCES`]): a
//! case in the table must still diverge, and a case outside it must not. So
//! fixing one of the modelling errors it records fails this test until the
//! entry is removed, and a regression that reintroduces one fails it too.

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
];

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

/// Our verdict for one probe: the entity we commit at the plant's span, or
/// `None` for any deferral / no-record (which makes no claim).
fn our_target(env: &AssemblyEnv, src: &str, plant: &Plant) -> Option<Target> {
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
        Some(Resolution::Entity(h)) => {
            let e = env.entity(h);
            let full = if e.namespace.is_empty() {
                e.name.clone()
            } else {
                format!("{}.{}", e.namespace.join("."), e.name)
            };
            Some((e.assembly.name.clone(), full))
        }
        // A `Member` here would mean we bound a *value* for a type-position
        // name; loud rather than silently "no claim".
        Some(res @ Resolution::Member { .. }) => {
            panic!(
                "tier probe {}: type position resolved to {res:?}",
                plant.name
            )
        }
        _ => None,
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
fn observe() -> BTreeMap<String, (Option<Target>, Option<Target>)> {
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
            (None, _) => deferred += 1,
            (Some(o), Some(f)) if o == f => agreed += 1,
            (Some(o), Some(f)) => {
                diverged.insert(
                    key.clone(),
                    format!(
                        "we bound {} but FCS binds {}",
                        describe(plant, o),
                        describe(plant, f)
                    ),
                );
            }
            (Some(o), None) => {
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
    // surface is never committed, only deferred to, which is the design.)
    for control in ["TEx", "TEn", "TNs", "TRo", "DEx", "DEn", "DNs", "DRo"] {
        for order in Order::ALL {
            let key = format!("{control}/{}", order.label());
            let (ours, fcs) = &observations[&key];
            assert_eq!(
                ours.as_ref(),
                fcs.as_ref(),
                "{key}: an uncontested name must resolve, and to what FCS says",
            );
            assert!(
                fcs.is_some(),
                "{key}: FCS resolved nothing — corpus broken?"
            );
        }
    }

    let expected: BTreeSet<&str> = KNOWN_DIVERGENCES.iter().map(|(k, _)| *k).collect();
    let observed: BTreeSet<&str> = diverged.keys().map(String::as_str).collect();

    let unexpected: Vec<String> = observed
        .difference(&expected)
        .map(|k| format!("  {k}: {}", diverged[*k]))
        .collect();
    let stale: Vec<String> = expected
        .difference(&observed)
        .map(|k| {
            let why = KNOWN_DIVERGENCES
                .iter()
                .find(|(e, _)| e == k)
                .map(|(_, w)| *w)
                .unwrap_or("");
            format!("  {k}: recorded as diverging ({why}), but it now agrees or defers")
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
        let show = |t: &Option<Target>| match t {
            Some(t) => describe(plant, t),
            None => "-".to_string(),
        };
        println!(
            "{key:<40} contenders={:?}  fcs={:<12} ours={}",
            plant.tiers,
            show(&fcs),
            show(&ours)
        );
    }
}
