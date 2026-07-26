//! Differential sweep: **which same-named entity a dotted head names**, against
//! FCS.
//!
//! A `(namespace, simple name)` in a referenced assembly can hold several
//! entities at once — a type and its companion module, and types at several
//! generic arities — so a path's head names a *candidate set*, and F# picks from
//! it by asking which candidate carries the **tail**. Our reading picked one
//! candidate up front (the first-wins arity-0 slot) and asked only it, which is
//! why `TypeInfo.NominallyEqual` and `MethodBody.Il` on `WoofWare.PawPrint`'s
//! main library bound `System.Reflection.*`: the companion module answered "no
//! such member", the reading became partial, and the higher-priority
//! `open System.Reflection`'s equally-partial reading won by tier.
//!
//! [`crate::common::companion_corpus`] plants one name per cell — holder shape ×
//! type arity × where the tail lives × what the type contributes it as ×
//! expression/pattern × with and without an outranking decoy — so a cell varies
//! exactly one thing and the fixture cannot drift from the matrix.
//!
//! Two properties ride on the matrix, both **certain-implies-exact**:
//!
//! - the **head**: whenever we commit an entity for the head segment, FCS's
//!   `(assembly, full name)` must agree exactly. This is where the candidate-set
//!   choice is observable, and where the PawPrint divergences were reported.
//! - the **leaf**: whenever we commit for the whole path, FCS must agree — with
//!   generic-arity markers stripped from both sides, since FCS renders a member's
//!   enclosing generic type as `Foo<_>.Tail` and our full names carry no marker
//!   (the same rendering difference corpus-diff normalises). The head comparison
//!   already pins *which* container the leaf came from, so the stripping cannot
//!   launder a container mix-up.
//!
//! Both are ratcheted, in **two** tables kept apart because the debts are not
//! alike: [`KNOWN_GAPS`] records a cell where we defer and FCS resolves (sound —
//! it claims nothing — so this is coverage), and [`KNOWN_WRONG_TARGETS`] records
//! a cell where we name the wrong symbol (unsound, and every row must carry the
//! defect it is a symptom of). Each ratchet is two-sided: an unlisted case
//! appearing fails, and a listed one that stopped behaving as recorded fails
//! too, so a fix lands with its row removed and a regression cannot be absorbed.
//!
//! A cell whose probe does not type-check ([`Plant::path_type_checks`]) makes no
//! head claim in either direction: FCS reports no symbol for an erroneous path,
//! so there is nothing for a commit there to contradict.

use std::collections::{BTreeMap, BTreeSet};

use crate::common::companion_corpus::{self, Plant};
use crate::common::{
    ensure_companion_corpus_built, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// Which span of the probe a case is about.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Site {
    /// The head segment — the candidate-set choice.
    Head,
    /// The whole path — the leaf the chosen candidate supplied.
    Leaf,
}

impl Site {
    const ALL: [Site; 2] = [Site::Head, Site::Leaf];

    fn label(self) -> &'static str {
        match self {
            Site::Head => "head",
            Site::Leaf => "leaf",
        }
    }
}

/// A **pattern** head is a lookup in F#'s constructor namespace, and
/// `assembly_case_pattern_records` declines outright at a prefix holding
/// anything of the name that is not a union declaring the case — the decoy class
/// here. F# instead keeps searching outward (a class can never supply a case), so
/// these are the conservative decline that walk documents, not a new error.
const PATTERN_DECOY: &str = "the case-pattern walk declines at a prefix holding a same-named non-union type instead of \
     reading past it";

/// A **nullary** union case compiles to a static property, which the F#-entity
/// projection drops (FCS surfaces the case, not the property), and it has no
/// nested carrier type either. So the case is *found* — `union_case_names` names
/// it, which is what lets the reading own the path — but there is no handle to
/// name it by.
const NULLARY_CASE: &str = "a nullary union case has neither a carrier type nor a projected member, so the reading owns \
     the path but can name no target";

/// A user-declared `static member` **property** on an F# record is dropped by
/// `project_fsharp_members` ("all other F#-kind properties are dropped: FCS
/// surfaces none"), which is true of the compiler's own generated properties and
/// wrong of this one. The tail then reads absent.
const DROPPED_STATIC_PROPERTY: &str = "the F#-entity projection drops a user-declared static property on a record, so the tail \
     reads as absent";

/// The cells where FCS resolves and we decline to name a target, each with the
/// modelling reason. Keyed `"<plant>/<site>"`.
///
/// A deferral is always sound — it claims nothing — so these are coverage
/// losses, not wrong answers. The ratchet is two-sided so that fixing one fails
/// this test until its row goes.
const KNOWN_GAPS: &[(&str, &str)] = &[
    ("CB0BFPD/head", PATTERN_DECOY),
    ("CB0BFPD/leaf", PATTERN_DECOY),
    ("CB0BUPD/head", PATTERN_DECOY),
    ("CB0BUPD/leaf", PATTERN_DECOY),
    ("CB0TFPD/head", PATTERN_DECOY),
    ("CB0TFPD/leaf", PATTERN_DECOY),
    ("CB0TUPD/head", PATTERN_DECOY),
    ("CB0TUPD/leaf", PATTERN_DECOY),
    ("CB1BFPD/head", PATTERN_DECOY),
    ("CB1BFPD/leaf", PATTERN_DECOY),
    ("CB1BUPD/head", PATTERN_DECOY),
    ("CB1BUPD/leaf", PATTERN_DECOY),
    ("CB1TFPD/head", PATTERN_DECOY),
    ("CB1TFPD/leaf", PATTERN_DECOY),
    ("CB1TUPD/head", PATTERN_DECOY),
    ("CB1TUPD/leaf", PATTERN_DECOY),
    ("CT0TFPD/head", PATTERN_DECOY),
    ("CT0TFPD/leaf", PATTERN_DECOY),
    ("CT0TUPD/head", PATTERN_DECOY),
    ("CT0TUPD/leaf", PATTERN_DECOY),
    ("CT1TFPD/head", PATTERN_DECOY),
    ("CT1TFPD/leaf", PATTERN_DECOY),
    ("CT1TUPD/head", PATTERN_DECOY),
    ("CT1TUPD/leaf", PATTERN_DECOY),
    ("CB0BUPX/leaf", NULLARY_CASE),
    ("CB0TUED/leaf", NULLARY_CASE),
    ("CB0TUEX/leaf", NULLARY_CASE),
    ("CB0TUPX/leaf", NULLARY_CASE),
    ("CB1BUPX/leaf", NULLARY_CASE),
    ("CB1TUED/leaf", NULLARY_CASE),
    ("CB1TUEX/leaf", NULLARY_CASE),
    ("CB1TUPX/leaf", NULLARY_CASE),
    ("CT0TUED/leaf", NULLARY_CASE),
    ("CT0TUEX/leaf", NULLARY_CASE),
    ("CT0TUPX/leaf", NULLARY_CASE),
    ("CT1TUED/leaf", NULLARY_CASE),
    ("CT1TUEX/leaf", NULLARY_CASE),
    ("CT1TUPX/leaf", NULLARY_CASE),
    ("CB0TRED/leaf", DROPPED_STATIC_PROPERTY),
    ("CB0TREX/leaf", DROPPED_STATIC_PROPERTY),
    ("CB1TRED/leaf", DROPPED_STATIC_PROPERTY),
    ("CB1TREX/leaf", DROPPED_STATIC_PROPERTY),
    ("CT0TRED/leaf", DROPPED_STATIC_PROPERTY),
    ("CT0TREX/leaf", DROPPED_STATIC_PROPERTY),
    ("CT1TRED/leaf", DROPPED_STATIC_PROPERTY),
    ("CT1TREX/leaf", DROPPED_STATIC_PROPERTY),
];

/// The cells where we name the **wrong** target, each with the defect it is a
/// symptom of. Same two-sided ratchet as [`KNOWN_GAPS`], and deliberately a
/// separate table: a wrong answer is a different kind of debt from a deferral,
/// and it must be impossible to add one by relaxing a gap row.
///
/// All four are the same cause — [`DROPPED_STATIC_PROPERTY`] — seen with a decoy
/// present: the plant's own reading cannot see its static property, so the tail
/// reads absent, the reading becomes partial, and the outranking decoy's equally
/// partial reading wins on tier. The projection is where it must be fixed; no
/// candidate ordering in the resolver can see a member that is not there.
const KNOWN_WRONG_TARGETS: &[(&str, &str)] = &[
    ("CB0TRED/head", DROPPED_STATIC_PROPERTY),
    ("CB1TRED/head", DROPPED_STATIC_PROPERTY),
    ("CT0TRED/head", DROPPED_STATIC_PROPERTY),
    ("CT1TRED/head", DROPPED_STATIC_PROPERTY),
];

/// The `(assembly, full name)` currency both oracles report in.
type Target = (String, String);

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Strip generic-arity markers (`Foo<_>.Tail` → `Foo.Tail`) so the leaf
/// comparison is about the *symbol*, not about how each side renders its
/// enclosing type's arity.
fn strip_arity(full: &str) -> String {
    let mut out = String::with_capacity(full.len());
    let mut depth = 0usize;
    for ch in full.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Our verdict at one site.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ours {
    /// We committed this target. Bound by certain-implies-exact.
    Committed(Target),
    /// We recorded a deferral, or nothing at all. A dotted path records nothing
    /// on a genuine no-match *and* nothing at some deferrals, so — unlike the
    /// single-segment tier probes — silence here is not a distinguishable claim;
    /// both are "no target named".
    NoClaim,
}

/// Our verdicts for one cell, by site.
fn our_targets(env: &AssemblyEnv, src: &str, plant: &Plant) -> BTreeMap<Site, Ours> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "companion probe {} does not parse: {:?}",
        plant.name,
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("probe is an impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), env);
    let render = |res: Option<Resolution>| match res {
        Some(Resolution::Entity(h)) => {
            Ours::Committed((env.entity(h).assembly.name.clone(), env.entity_full_name(h)))
        }
        Some(Resolution::Member { parent, idx }) => Ours::Committed((
            env.entity(parent).assembly.name.clone(),
            format!(
                "{}.{}",
                env.entity_full_name(parent),
                env.member_display_name(parent, idx)
            ),
        )),
        None | Some(Resolution::Deferred(_)) | Some(Resolution::Unresolved) => Ours::NoClaim,
        // The probe declares nothing of the plant's name, folds no preceding
        // file and binds no local, so an in-file verdict here means the walk
        // wrong-targeted a project binder — a distinct bug that must not be
        // laundered into "no claim".
        Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => panic!(
            "companion probe {}: assembly path resolved to {res:?}",
            plant.name
        ),
    };
    let (hs, he) = plant.head_span(src);
    let (ps, pe) = plant.path_span(src);
    BTreeMap::from([
        (Site::Head, render(rf.resolution_at(span(hs, he)))),
        (Site::Leaf, render(rf.resolution_at(span(ps, pe)))),
    ])
}

/// FCS's verdicts for one cell, by site. The head use is the one FCS reports at
/// the head *segment*; the leaf use is the one whose name is the probed leaf.
/// Matching on the reported name rather than on an exact span keeps the pairing
/// robust against FCS's choice of range for each half of a long identifier.
fn fcs_targets(
    refs: &[&std::path::Path],
    src: &str,
    plant: &Plant,
) -> BTreeMap<Site, Option<Target>> {
    let path = temp_fs_file("companion_head", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, refs);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, src);
    let (hs, he) = plant.head_span(src);
    let pick = |want: &str, start: usize, end: usize| -> Option<Target> {
        uses.iter()
            .find(|u| !u.is_from_definition && u.name == want && u.start <= start && end <= u.end)
            .and_then(|u| Some((u.assembly.clone()?, u.full_name.clone()?)))
    };
    let (ps, pe) = plant.path_span(src);
    BTreeMap::from([
        (Site::Head, pick(&plant.name, hs, he)),
        (Site::Leaf, pick(companion_corpus::TAIL, ps, pe)),
    ])
}

/// One observed case: the two sides' verdicts at one site, plus what the corpus
/// knows about the cell that the verdicts alone cannot say.
struct Observation {
    site: Site,
    ours: Ours,
    fcs: Option<Target>,
    /// [`Plant::path_type_checks`] — `false` means FCS reports no symbol at
    /// *either* site because the path is an error, so the case constrains only
    /// that we do not claim to own the path.
    type_checks: bool,
}

/// Every cell's `(key, ours, fcs)`.
fn observe() -> BTreeMap<String, Observation> {
    let fixture = ensure_companion_corpus_built();
    let bytes = std::fs::read(fixture).expect("read companion fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse companion fixture dll");
    let env = AssemblyEnv::from_views(&[view]).expect("build companion AssemblyEnv");
    let refs = vec![fixture];

    let mut out = BTreeMap::new();
    for plant in companion_corpus::corpus() {
        let src = plant.probe_source();
        let ours = our_targets(&env, &src, &plant);
        let fcs = fcs_targets(&refs, &src, &plant);
        for site in Site::ALL {
            out.insert(
                format!("{}/{}", plant.key(), site.label()),
                Observation {
                    site,
                    ours: ours[&site].clone(),
                    fcs: fcs[&site].clone(),
                    type_checks: plant.path_type_checks(),
                },
            );
        }
    }
    out
}

/// Whether the two sides name the same symbol at `site`.
fn agrees(site: Site, ours: &Target, fcs: &Target) -> bool {
    match site {
        Site::Head => ours == fcs,
        Site::Leaf => ours.0 == fcs.0 && strip_arity(&ours.1) == strip_arity(&fcs.1),
    }
}

#[test]
fn companion_head_choice_is_sound_against_fcs() {
    let observations = observe();
    let known: BTreeMap<&str, &str> = KNOWN_GAPS.iter().copied().collect();

    let mut diverged: BTreeMap<String, String> = BTreeMap::new();
    let mut gaps: BTreeSet<String> = BTreeSet::new();
    let mut agreed = 0usize;
    let mut both_silent = 0usize;
    let mut ill_typed = 0usize;
    for (key, obs) in &observations {
        match (&obs.ours, &obs.fcs) {
            (Ours::NoClaim, None) => both_silent += 1,
            (Ours::NoClaim, Some(_)) => {
                gaps.insert(key.clone());
            }
            (Ours::Committed(o), Some(f)) if agrees(obs.site, o, f) => agreed += 1,
            (Ours::Committed(o), Some(f)) => {
                diverged.insert(
                    key.clone(),
                    format!("we bound {}/{} but FCS binds {}/{}", o.0, o.1, f.0, f.1),
                );
            }
            // FCS reports no symbol for a path that does not type-check, so a
            // commit there contradicts nothing.
            (Ours::Committed(_), None) if !obs.type_checks => ill_typed += 1,
            (Ours::Committed(o), None) => {
                diverged.insert(
                    key.clone(),
                    format!(
                        "we bound {}/{} but FCS resolves the span to nothing at all",
                        o.0, o.1
                    ),
                );
            }
        }
    }

    // Non-vacuity: a fixture that stopped building, or a probe template that
    // stopped resolving, must not pass by committing nothing anywhere.
    assert!(
        agreed > 0,
        "no cell agreed at all — fixture or probe template broken?"
    );

    let known_wrong: BTreeMap<&str, &str> = KNOWN_WRONG_TARGETS.iter().copied().collect();
    // Each ratchet is two-sided: an unlisted case appearing fails, and a listed
    // one that stopped behaving as recorded fails too, so a fix cannot land
    // without its row.
    let two_sided = |observed: BTreeSet<&str>, expected: &BTreeMap<&str, &str>| {
        let listed: BTreeSet<&str> = expected.keys().copied().collect();
        let unexpected: Vec<String> = observed.difference(&listed).map(|k| (*k).into()).collect();
        let stale: Vec<String> = listed
            .difference(&observed)
            .map(|k| {
                format!(
                    "  {k}: recorded ({}), but it no longer behaves that way",
                    expected[*k]
                )
            })
            .collect();
        (unexpected, stale)
    };
    let (new_gaps, stale_gaps) = two_sided(gaps.iter().map(String::as_str).collect(), &known);
    let (new_wrong, stale_wrong) =
        two_sided(diverged.keys().map(String::as_str).collect(), &known_wrong);
    let stale: Vec<String> = stale_gaps.into_iter().chain(stale_wrong).collect();

    assert!(
        new_wrong.is_empty() && new_gaps.is_empty() && stale.is_empty(),
        "companion-head choice diverges from FCS.\n\
         WRONG TARGETS (fix these, or — only with a named cause — record in \
         KNOWN_WRONG_TARGETS):\n{}\n\
         NEW GAPS (a deferral where FCS resolves — fix, or record in KNOWN_GAPS with a \
         reason):\n{}\n\
         STALE entries (remove them; both ratchets are two-sided):\n{}\n\
         ({agreed} agreed, {} gaps, {both_silent} silent on both sides, {ill_typed} ill-typed, \
         {} cases)",
        if new_wrong.is_empty() {
            "  (none)".to_string()
        } else {
            new_wrong
                .iter()
                .map(|key| format!("  {key}: {}", diverged[key.as_str()]))
                .collect::<Vec<_>>()
                .join("\n")
        },
        if new_gaps.is_empty() {
            "  (none)".to_string()
        } else {
            new_gaps
                .iter()
                .map(|key| format!("  {key}: FCS resolves it and we name nothing"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        if stale.is_empty() {
            "  (none)".to_string()
        } else {
            stale.join("\n")
        },
        gaps.len(),
        observations.len(),
    );
}

/// The measurement behind the sweep, printed rather than asserted: which
/// candidate FCS picks for every cell. Run it when a candidate-set question
/// comes up instead of reasoning about the walk from its comments.
#[test]
#[ignore = "report generator; run explicitly with --ignored --nocapture"]
fn report_companion_head_choice() {
    for (key, obs) in observe() {
        let show = |t: &Option<Target>| match t {
            Some((asm, full)) => format!("{asm}/{full}"),
            None => "-".to_string(),
        };
        let show_ours = match &obs.ours {
            Ours::Committed(t) => show(&Some(t.clone())),
            Ours::NoClaim => "(no claim)".to_string(),
        };
        println!("{key:<24} fcs={:<58} ours={show_ours}", show(&obs.fcs));
    }
}
