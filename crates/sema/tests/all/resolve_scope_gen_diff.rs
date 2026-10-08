//! The scope-graph generator (`common::scope_gen`) against our resolver and
//! against FCS.
//!
//! The generator plants name uses whose answer it knows by construction, in
//! programs that are well-typed by construction. Three properties follow, and
//! each covers what the others cannot:
//!
//! * **ours vs the generator** (FCS-free, so it runs at volume): every planted
//!   use we commit is the binder the generator planted, and a use the
//!   generator says FSharp.Core supplies is never committed in-file;
//! * **the generator vs FCS**: every program type-checks cleanly, and FCS
//!   reports every planted use at its range, declared where the generator
//!   says. This is what licenses trusting the generator's model in the first
//!   property — a model the compiler disagrees with is a generator bug, and
//!   fails here first;
//! * **ours vs FCS** over every use FCS reports, planted or not: anything we
//!   commit is FCS's declaration, and nothing FCS resolves outside the file is
//!   committed in-file.
//!
//! The census test pins that each construct the generator claims to cover is
//! actually emitted and planted at, and prints how often each kind of use is
//! committed — a property that holds because sema declines everything would
//! pass the first and third checks, so the census and the must-commit kinds
//! are what keep them from holding vacuously.
//!
//! One known wrong-answer class is classified rather than failed: sema does not
//! fold a same-file `[<AutoOpen>]` module's values back into its parent, so a
//! use FCS binds to the folded value binds an earlier same-named binder
//! instead. Open PR #233 fixes it. The classifier is exact — a planted
//! auto-opened use that hides a same-named binder — and its count over the
//! default seeds is pinned two-sided, so the fix landing fails the test until
//! the classifier is deleted.
//!
//! Default runtime is modest: a fixed handful of seeds against FCS and 256
//! FCS-free cases. For the soak, set `BORZOI_SCOPE_GEN_SEEDS=<n>` (seeds
//! against FCS) and `BORZOI_SCOPE_GEN_CASES=<n>` (FCS-free cases);
//! `BORZOI_SCOPE_GEN_DUMP=<seed>` prints that seed's program.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use proptest::prelude::*;
use rowan::TextRange;

use crate::common::scope_gen::{
    Expected, Form, Generated, PlantedRef, RefKind, generate, generate_seed,
};
use crate::common::{
    CensusDecl, census_resolve_uses, env_usize_or, invoke_fcs_dump_census, parse_census_jsonl,
    temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, ResolvedFile, SyntaxRecovery, resolve_file,
};

/// Seeds checked against FCS by default.
const DEFAULT_SEEDS: usize = 12;

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "generated program failed to parse: {:?}\n{src}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(
        &file,
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &recovery,
    )
}

/// Our answer at a planted use, graded against the generator's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grade {
    /// Committed to the planted binder.
    Agrees,
    /// Declined (or, for an external use, correctly not committed in-file).
    Declined,
}

/// The kinds of use sema resolves completely: every planted one must be
/// committed, not merely never contradicted. Without this, a resolver that
/// declined everything would pass both soundness properties. The rest —
/// qualified values, static members, constructions — are census-only, and an
/// external use must be declined.
const MUST_COMMIT: &[RefKind] = &[
    RefKind::ModuleValue,
    RefKind::LetRecSibling,
    RefKind::OpenedValue,
    RefKind::LocalLet,
    RefKind::FunctionParam,
    RefKind::LambdaParam,
    RefKind::MatchBinder,
    RefKind::OrAlias,
    RefKind::ForInVar,
    RefKind::ForToVar,
    RefKind::HandlerBinder,
    RefKind::UseBinder,
    RefKind::CtorParam,
    RefKind::ClassLet,
    RefKind::SelfIdentifier,
    RefKind::MemberParam,
    RefKind::StaticMemberParam,
    RefKind::UnionCase,
];

/// How many uses of the known fold-back class (below) the default seeds
/// produce. Two-sided: fewer means the defect is (partly) fixed — #233 has
/// landed — and the classifier should go.
const KNOWN_FOLD_BACK_AT_DEFAULT: usize = 6;

/// Whether a wrong answer at `r` is the known defect that open PR #233 fixes:
/// sema does not fold an in-file `[<AutoOpen>]` module's values back into its
/// parent, so a use that FCS binds to the folded value instead binds an
/// earlier same-named binder the fold hides.
fn is_known_auto_open_fold_back(r: &PlantedRef) -> bool {
    r.kind == RefKind::AutoOpenedValue && r.hides_same_name
}

/// Grade one planted use, or describe the wrong answer.
fn grade(
    g: &Generated,
    rf: &ResolvedFile,
    range: TextRange,
    expected: Expected,
) -> Result<Grade, String> {
    let ours = rf.resolution_at(range);
    match (ours, expected) {
        (None | Some(Resolution::Deferred(_)), _) => Ok(Grade::Declined),
        (Some(res @ (Resolution::Local(_) | Resolution::Item(_))), Expected::Binder(uid)) => {
            let want = g.binder_ranges[&uid];
            match rf.resolved_def(res) {
                Some(def) if def.range == want => Ok(Grade::Agrees),
                Some(def) => Err(format!(
                    "committed to {:?} at {:?}, planted {want:?}",
                    def.name, def.range
                )),
                None => Err(format!("committed to {res:?}, which names no in-file def")),
            }
        }
        (Some(res @ (Resolution::Local(_) | Resolution::Item(_))), Expected::External) => {
            let def = rf.resolved_def(res).map(|d| (d.name.to_string(), d.range));
            Err(format!(
                "committed in-file to {def:?}, but FSharp.Core supplies it"
            ))
        }
        (Some(other), _) => Err(format!("committed to {other:?}")),
    }
}

/// The FCS-free property, over one program: every planted use is the planted
/// binder or declined. Returns the grades by kind.
fn check_against_generator(g: &Generated) -> Result<Vec<(RefKind, Grade)>, String> {
    let rf = resolve(&g.src);
    let mut grades = Vec::new();
    for r in &g.refs {
        match grade(g, &rf, r.range, r.expected) {
            Ok(Grade::Declined) if MUST_COMMIT.contains(&r.kind) => {
                return Err(format!(
                    "{:?} use {:?} at {:?} was declined; sema models this kind \
                     completely\n{}",
                    r.kind, &g.src[r.range], r.range, g.src
                ));
            }
            Ok(gr) => grades.push((r.kind, gr)),
            Err(_) if is_known_auto_open_fold_back(r) => {}
            Err(why) => {
                return Err(format!(
                    "{:?} use {:?} at {:?}: {why}\n{}",
                    r.kind, &g.src[r.range], r.range, g.src
                ));
            }
        }
    }
    Ok(grades)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: u32::try_from(env_usize_or("BORZOI_SCOPE_GEN_CASES", 256)).unwrap(),
        ..ProptestConfig::default()
    })]

    #[test]
    fn planted_uses_resolve_to_their_binders_or_decline(
        nums in prop::collection::vec(any::<u32>(), 200..4096)
    ) {
        let g = generate(nums);
        if let Err(why) = check_against_generator(&g) {
            return Err(TestCaseError::fail(why));
        }
    }
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Every generated program against FCS: clean, the generator's model
/// confirmed at every planted use, and ours never contradicting FCS.
#[test]
fn generated_programs_agree_with_fcs() {
    let seeds = env_usize_or("BORZOI_SCOPE_GEN_SEEDS", DEFAULT_SEEDS);
    let programs: Vec<Generated> = (0..seeds as u32).map(generate_seed).collect();
    if let Ok(seed) = std::env::var("BORZOI_SCOPE_GEN_DUMP") {
        let seed: u32 = seed.parse().expect("a seed number");
        eprintln!("{}", generate_seed(seed).src);
    }
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|g| temp_fs_file("scope_gen", &g.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let mut rejected = Vec::new();
    let mut model_wrong = Vec::new();
    let mut ours_wrong = Vec::new();
    let mut adjudicated: BTreeMap<RefKind, (usize, usize)> = BTreeMap::new();
    let mut fcs_uses_graded = 0usize;
    let mut known_fold_back = 0usize;
    let mut undercommitted = Vec::new();
    for (seed, (g, file)) in programs.iter().zip(&census).enumerate() {
        if !file.ok || file.has_check_errors {
            rejected.push(seed);
            continue;
        }
        let uses: Vec<_> = census_resolve_uses(file, &g.src)
            .into_iter()
            .filter(|u| !u.is_from_definition && u.start != u.end)
            .collect();
        let rf = resolve(&g.src);

        // The generator against FCS, at every planted use.
        for r in &g.refs {
            let at: Vec<_> = uses
                .iter()
                .filter(|u| range_of(u.start, u.end) == r.range)
                .collect();
            let confirmed = at.iter().any(|u| match (u.decl, r.expected) {
                (CensusDecl::InFile(s, e), Expected::Binder(uid)) => {
                    range_of(s, e) == g.binder_ranges[&uid]
                }
                (CensusDecl::OtherFile, Expected::External) => true,
                _ => false,
            });
            if !confirmed {
                model_wrong.push(format!(
                    "seed {seed}: {:?} use {:?} at {:?} planted {:?}, FCS says {:?}",
                    r.kind,
                    &g.src[r.range],
                    r.range,
                    r.expected,
                    at.iter().map(|u| u.decl).collect::<Vec<_>>()
                ));
                continue;
            }
            let entry = adjudicated.entry(r.kind).or_default();
            entry.0 += 1;
            match grade(g, &rf, r.range, r.expected) {
                Ok(Grade::Agrees) => entry.1 += 1,
                Ok(Grade::Declined) if MUST_COMMIT.contains(&r.kind) => {
                    undercommitted.push(format!(
                        "seed {seed}: {:?} use {:?} at {:?}",
                        r.kind, &g.src[r.range], r.range
                    ))
                }
                _ => {}
            }
        }

        // Ours against FCS, at every use FCS reports.
        let in_file_ranges: HashSet<(usize, usize)> = census_resolve_uses(file, &g.src)
            .iter()
            .filter(|u| matches!(u.decl, CensusDecl::InFile(..)))
            .map(|u| (u.start, u.end))
            .collect();
        for u in &uses {
            let range = range_of(u.start, u.end);
            let Some(res) = rf.resolution_at(range) else {
                continue;
            };
            let wrong = match (res, u.decl) {
                (Resolution::Deferred(_), _) => None,
                (Resolution::Local(_) | Resolution::Item(_), CensusDecl::InFile(s, e)) => rf
                    .resolved_def(res)
                    .filter(|d| d.range == range_of(s, e))
                    .is_none()
                    .then(|| format!("{res:?}, FCS declares at {:?}", range_of(s, e))),
                (Resolution::Local(_) | Resolution::Item(_), CensusDecl::OtherFile)
                    if !in_file_ranges.contains(&(u.start, u.end)) =>
                {
                    Some(format!("{res:?}, FCS declares outside the file"))
                }
                (Resolution::Unresolved | Resolution::Entity(_) | Resolution::Member { .. }, _) => {
                    Some(format!("{res:?}"))
                }
                _ => None,
            };
            fcs_uses_graded += 1;
            let planted = g.refs.iter().find(|r| r.range == range);
            if wrong.is_some() && planted.is_some_and(is_known_auto_open_fold_back) {
                known_fold_back += 1;
                continue;
            }
            if let Some(w) = wrong {
                ours_wrong.push(format!(
                    "seed {seed}: {:?} at {range:?}: we gave {w}",
                    &g.src[range]
                ));
            }
        }
    }

    eprintln!(
        "scope-gen: {} programs, {} rejected by FCS, {} FCS uses graded, {} wrong \
         answers of the known [<AutoOpen>] fold-back class (#233)",
        programs.len(),
        rejected.len(),
        fcs_uses_graded,
        known_fold_back,
    );
    for (kind, (n, committed)) in &adjudicated {
        eprintln!("  {kind:?}: {n} adjudicated, {committed} committed");
    }
    assert!(
        rejected.is_empty(),
        "FCS rejected the programs of seeds {rejected:?}: the generator must emit \
         only programs it accepts, or its answers are recovery"
    );
    assert!(
        model_wrong.is_empty(),
        "{} planted uses FCS resolves elsewhere — the generator's model is wrong:\n{}",
        model_wrong.len(),
        model_wrong.join("\n")
    );
    assert!(
        undercommitted.is_empty(),
        "{} planted uses of a must-commit kind were declined:\n{}",
        undercommitted.len(),
        undercommitted.join("\n")
    );
    if seeds == DEFAULT_SEEDS {
        assert_eq!(
            known_fold_back, KNOWN_FOLD_BACK_AT_DEFAULT,
            "the known [<AutoOpen>] fold-back class moved: fewer means #233 (or \
             another fix) landed, so delete `is_known_auto_open_fold_back`; more \
             means the generator or the resolver changed"
        );
    }
    assert!(
        ours_wrong.is_empty(),
        "{} uses committed to something FCS does not name:\n{}",
        ours_wrong.len(),
        ours_wrong.join("\n")
    );
}

/// Each form and each kind of planted use appears often enough over a fixed
/// sample that the properties above are sweeping it.
#[test]
fn the_scope_generator_emits_every_form_and_kind() {
    const SEEDS: u32 = 64;
    const FLOOR: usize = 10;
    let mut forms: BTreeMap<Form, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<RefKind, usize> = BTreeMap::new();
    for seed in 0..SEEDS {
        let g = generate_seed(seed);
        for (f, n) in &g.forms {
            *forms.entry(*f).or_default() += n;
        }
        for r in &g.refs {
            *kinds.entry(r.kind).or_default() += 1;
        }
    }
    let starved_forms: Vec<_> = Form::ALL
        .iter()
        .map(|f| (*f, forms.get(f).copied().unwrap_or(0)))
        .filter(|(_, n)| *n < FLOOR)
        .collect();
    let starved_kinds: Vec<_> = RefKind::ALL
        .iter()
        .map(|k| (*k, kinds.get(k).copied().unwrap_or(0)))
        .filter(|(_, n)| *n < FLOOR)
        .collect();
    assert!(
        starved_forms.is_empty() && starved_kinds.is_empty(),
        "under {FLOOR} occurrences over {SEEDS} seeds: forms {starved_forms:?}, \
         kinds {starved_kinds:?}\nforms: {forms:?}\nkinds: {kinds:?}"
    );
}
