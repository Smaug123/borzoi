//! What reserved-name seeding would actually buy on the SDK chain.
//!
//! [`sdk_chain_expression_census`](../sdk_chain_expression_census.rs) buckets
//! its declines by the *function being called*, which answers "what would I
//! have to model?". The census's own comments used it to answer a different
//! question — "what is blocking this?" — and concluded the declines were
//! dominated by undefined **reserved** receivers. That claim was load-bearing:
//! `docs/msbuild-value-carried-provenance-plan.md` deferred its P3 stage behind
//! "land trusted seeding, then re-price", so a wrong attribution had a real
//! stage waiting on a lever nobody had sized.
//!
//! ## Why this measures a delta rather than classifying declines
//!
//! Two earlier attempts here classified each decline by cause, and both were
//! wrong in the same way — they inferred a *universal* ("no property table
//! reaches this") from a *finite* probe:
//!
//! * Reading `Issue` tags does not work: `substitute` reports `Unsupported` and
//!   nothing else when a modelled function has an undefined operand, so
//!   `$([System.IO.Path]::Combine($(Undefined), 'x'))` is indistinguishable
//!   from a function we have never implemented.
//! * Re-probing with a fixed set of filler values does not work either: a
//!   condition like `'$(X)' != '' and Exists('$(X)')` commits when `X` is
//!   defined **empty**, and `$(Rid.Split('-')[1])` commits only for a
//!   hyphen-bearing value. No finite filler set proves the negative.
//!
//! So this file does not classify. It runs the census's own population under
//! three property tables — the census seeds, plus what a real walk already
//! seeds, plus the reserved names still left empty — and reports the steps
//! between them. The last step is what seeding *work* would buy, as opposed to
//! what reserved names are worth in total (most of which we already have).
//!
//! It also computes a **sound ceiling**: an item that reads no reserved name
//! cannot be turned on by any choice of reserved values, so the declines that
//! read one bound the lever from above. A measured delta is only ever a floor,
//! and "this lever is small" is an upper-bound claim — it needs the ceiling.
//!
//! **This runs without the oracle**, deliberately: the census cannot seed
//! reserved names on the MSBuild side at all (`-p:MSBuildProjectDirectory=…` is
//! rejected as reserved), which is why it leaves them out. Asking only what
//! *our* evaluator commits needs no second side. Certain-implies-exact remains
//! the census's job.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use borzoi_msbuild::test_support::{
    Outcome, PropertyMap, evaluate, is_toolset_initial_property_name, property_references,
    substitute,
};

use common::sdk_chain::{
    extract_call_expressions, extract_conditions, msbuild_files, sdk_dir, seeded_props,
};

/// The reserved names a real walk **already** seeds once an SDK resolves:
/// `properties::well_known` for the path-derived group, `seed_toolset_properties`
/// for the toolset group, plus `OS` and the ChangeWaves threshold. Values are
/// the ones production actually uses.
fn already_seeded() -> Vec<(String, String)> {
    [
        ("MSBuildProjectDirectory", "/repo/proj"),
        ("MSBuildProjectFullPath", "/repo/proj/Demo.fsproj"),
        ("MSBuildProjectName", "Demo"),
        ("MSBuildProjectFile", "Demo.fsproj"),
        ("MSBuildProjectExtension", ".fsproj"),
        ("MSBuildThisFile", "Demo.fsproj"),
        ("MSBuildThisFileDirectory", "/repo/proj/"),
        ("MSBuildToolsPath", "/usr/share/dotnet/sdk/10.0.301"),
        ("MSBuildBinPath", "/usr/share/dotnet/sdk/10.0.301"),
        ("MSBuildToolsVersion", "Current"),
        ("MSBuildRuntimeType", "Core"),
        ("MSBuildSDKsPath", "/usr/share/dotnet/sdk/10.0.301/Sdks"),
        ("MSBuildExtensionsPath", "/usr/share/dotnet/sdk/10.0.301/"),
        ("MSBuildExtensionsPath32", "/usr/share/dotnet/sdk/10.0.301"),
        ("OS", "Unix"),
        // `999.999` when the environment is empty (`evaluator.rs`, probed).
        ("MSBuildDisableFeaturesFromVersion", "999.999"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The reserved names a real walk leaves empty — the ones seeding work would
/// have to add. Split by whether they can be known at all in
/// `docs/msbuild-reserved-seeding-plan.md`; here they are measured together, so
/// the figure is the *whole* remaining group's value, not one slice's.
fn not_yet_seeded() -> Vec<(String, String)> {
    [
        // (a) path-derivable
        ("MSBuildThisFileFullPath", "/repo/proj/Demo.fsproj"),
        ("MSBuildThisFileName", "Demo"),
        ("MSBuildThisFileExtension", ".fsproj"),
        ("MSBuildProjectDirectoryNoRoot", "repo/proj"),
        // (b) toolset-derivable
        ("MSBuildVersion", "18.6.4"),
        ("MSBuildAssemblyVersion", "18.0"),
        ("MSBuildProgramFiles32", "/Applications"),
        ("MSBuildNodeCount", "1"),
        ("VisualStudioVersion", "18.0"),
        ("MSBuildProjectDefaultTargets", "Build"),
        // (c) unknowable for an LSP, measured only to keep the figure an
        //     over-statement rather than an under-one
        ("MSBuildStartupDirectory", "/repo"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// How many declined items could *possibly* be turned on by seeding reserved
/// names: those that read at least one. A **sound ceiling** — an item reading no
/// reserved name cannot be affected by any choice of reserved values — and the
/// bound the conclusion needs, since a measured delta is only ever a floor.
fn could_be_reached_by_seeding<'a>(
    population: impl Iterator<Item = &'a String>,
    commits: impl Fn(&str) -> bool,
) -> usize {
    population
        .filter(|raw| !commits(raw))
        .filter(|raw| {
            property_references(raw)
                .into_iter()
                .any(|n| is_toolset_initial_property_name(&n.to_ascii_lowercase()))
        })
        .count()
}

fn table(entries: &[&[(String, String)]]) -> PropertyMap {
    let mut map = PropertyMap::new();
    for group in entries {
        for (k, v) in *group {
            map.insert(k.clone(), v.clone());
        }
    }
    map
}

fn expression_commits(raw: &str, map: &PropertyMap) -> bool {
    substitute(raw, map).1.is_empty()
}

fn condition_commits(raw: &str, map: &PropertyMap) -> bool {
    let eval = evaluate(raw, map);
    eval.outcome != Outcome::Unsupported && eval.undefined_properties.is_empty()
}

/// Every name in `population` that some item reads and `defined` lacks, counted
/// by how many items read it. Descriptive only — it says which names appear
/// undefined, not that supplying one would make anything commit.
fn undefined_name_census<'a>(
    population: impl Iterator<Item = &'a String>,
    defined: &PropertyMap,
) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for raw in population {
        let names: BTreeSet<String> = property_references(raw)
            .into_iter()
            .map(|n| n.to_ascii_lowercase())
            .filter(|n| defined.get(n).is_none())
            .collect();
        for name in names {
            *counts.entry(name).or_default() += 1;
        }
    }
    counts
}

fn report_names(title: &str, counts: &BTreeMap<String, usize>) {
    let mut rows: Vec<(&String, &usize)> = counts.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let reserved: usize = rows
        .iter()
        .filter(|(n, _)| is_toolset_initial_property_name(n))
        .map(|(_, c)| **c)
        .sum();
    let total: usize = rows.iter().map(|(_, c)| **c).sum();
    eprintln!("--- {title}: {total} undefined reads, {reserved} of them reserved ---");
    for (name, count) in rows.iter().take(12) {
        let tag = if is_toolset_initial_property_name(name) {
            "RESERVED"
        } else {
            "ordinary"
        };
        eprintln!("  {count:5}  {tag}  {name}");
    }
}

/// Three property tables, three commit counts: the census's own seeds, plus the
/// reserved names a real walk already supplies, plus the ones seeding work would
/// have to add. The **third minus the second** is what the work is worth.
///
/// The assertions are **two-sided**, for the same reason `parser_corpus`'s
/// `CLEAN_PARSES` is: the SDK is pinned and the evaluator deterministic, so
/// there is no drift for a one-sided bound to absorb. Improving the evaluator
/// is *supposed* to fail this test; update the numbers with the date and say
/// which way they moved.
///
/// Measured 2026-08-02 against the pinned SDK 10.0.301.
#[test]
#[ignore = "walks the whole pinned SDK chain; a measurement, run in CI's sweep lane"]
fn the_unseeded_reserved_names_are_worth_four_expressions_and_thirty_conditions() {
    let sdk = sdk_dir();
    let files = msbuild_files(&sdk);
    let census_seeds = seeded_props();
    let already = already_seeded();
    let remaining = not_yet_seeded();

    let mut expressions: BTreeSet<String> = BTreeSet::new();
    let mut conditions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file) {
            extract_call_expressions(&text, &mut expressions);
            extract_conditions(&text, &mut conditions);
        }
    }

    // Any reserved name the corpus reads that neither hand list names still
    // gets a value, so an omission cannot understate the lever — the direction
    // that would flatter this file's conclusion, and the one review has already
    // caught twice (`MSBuildDisableFeaturesFromVersion`,
    // `MSBuildProjectDefaultTargets`, both now named above).
    let named: BTreeSet<String> = already
        .iter()
        .chain(remaining.iter())
        .map(|(k, _)| k.to_ascii_lowercase())
        .collect();
    let derived: Vec<(String, String)> = expressions
        .iter()
        .chain(conditions.iter())
        .flat_map(|raw| property_references(raw))
        .map(|n| n.to_ascii_lowercase())
        .filter(|n| is_toolset_initial_property_name(n) && !named.contains(n))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .map(|n| (n, "1.0.0".to_string()))
        .collect();

    let base = table(&[&census_seeds]);
    let today = table(&[&census_seeds, &already]);
    let with_reserved = table(&[&census_seeds, &already, &remaining, &derived]);

    let expr_base = expressions
        .iter()
        .filter(|e| expression_commits(e, &base))
        .count();
    let expr_today = expressions
        .iter()
        .filter(|e| expression_commits(e, &today))
        .count();
    let expr_seeded = expressions
        .iter()
        .filter(|e| expression_commits(e, &with_reserved))
        .count();
    let cond_base = conditions
        .iter()
        .filter(|c| condition_commits(c, &base))
        .count();
    let cond_today = conditions
        .iter()
        .filter(|c| condition_commits(c, &today))
        .count();
    let cond_seeded = conditions
        .iter()
        .filter(|c| condition_commits(c, &with_reserved))
        .count();

    eprintln!(
        "expressions ({}): census seeds {expr_base} -> +what we already seed {expr_today} \
         -> +the names seeding work would add {expr_seeded}; the work is worth +{}",
        expressions.len(),
        expr_seeded - expr_today
    );
    eprintln!(
        "conditions ({}): census seeds {cond_base} -> +what we already seed {cond_today} \
         -> +the names seeding work would add {cond_seeded}; the work is worth +{}",
        conditions.len(),
        cond_seeded - cond_today
    );
    // The sound ceiling: no item that reads no reserved name can be turned on
    // by any choice of reserved values, whatever the seed table.
    let expr_ceiling =
        could_be_reached_by_seeding(expressions.iter(), |e| expression_commits(e, &base));
    let cond_ceiling =
        could_be_reached_by_seeding(conditions.iter(), |c| condition_commits(c, &base));
    eprintln!(
        "ceiling: at most {expr_ceiling} expression declines and {cond_ceiling} condition \
         withdrawals read any reserved name at all"
    );
    report_names(
        "expression reads still undefined after seeding",
        &undefined_name_census(
            expressions
                .iter()
                .filter(|e| !expression_commits(e, &with_reserved)),
            &with_reserved,
        ),
    );
    report_names(
        "condition reads still undefined after seeding",
        &undefined_name_census(
            conditions
                .iter()
                .filter(|c| !condition_commits(c, &with_reserved)),
            &with_reserved,
        ),
    );

    // The finding, bounded on both sides. The delta is a **floor** (one seed
    // table, measured), the ceiling is **sound** (an item reading no reserved
    // name cannot be reached by any table), and both are small against
    // populations of 396 and 2 758. The floor is itself an over-statement of
    // what real seeding would deliver: the table includes names a real walk
    // already supplies and one that cannot be known at all.
    assert_eq!(
        (expr_base, expr_today, expr_seeded),
        EXPRESSION_STEPS,
        "the reserved-seeding lever on call expressions moved; restate it with \
         today's date and say which way, rather than adjusting the tuple"
    );
    assert_eq!(
        (cond_base, cond_today, cond_seeded),
        CONDITION_STEPS,
        "the reserved-seeding lever on conditions moved; restate it with \
         today's date and say which way, rather than adjusting the tuple"
    );
    assert_eq!(
        (expr_ceiling, cond_ceiling),
        (CEILING_EXPRESSIONS, CEILING_CONDITIONS),
        "the reserved-seeding ceiling moved; restate it with today's date"
    );
    assert!(
        expr_seeded - expr_base <= expr_ceiling && cond_seeded - cond_base <= cond_ceiling,
        "a measured delta above the sound ceiling means one of them is computed \
         wrong: {} > {expr_ceiling} or {} > {cond_ceiling}",
        expr_seeded - expr_base,
        cond_seeded - cond_base
    );
    // Cross-check against the census, which computes its committed counts by a
    // different route (both property contexts, plus the oracle). If these
    // diverge, one of the two harnesses has drifted and neither can be trusted.
    assert_eq!(
        (expr_base, cond_base),
        (66, 139),
        "the base commit counts must equal the census's own — same seeds, same \
         extraction"
    );
}

/// `(census seeds, + what we already seed, + what seeding work would add)`.
/// The **third minus the second** is the value of the work; the second minus
/// the first is an artefact of the census not seeding reserved names at all.
/// Measured 2026-08-02, pinned SDK 10.0.301.
const EXPRESSION_STEPS: (usize, usize, usize) = (66, 76, 80);
/// As [`EXPRESSION_STEPS`], for `Condition` attributes.
const CONDITION_STEPS: (usize, usize, usize) = (139, 155, 185);
/// Declined call expressions that read at least one reserved name — the most
/// seeding could ever reach, whatever values it chose. Sound, unlike a measured
/// delta, which is only ever a floor.
const CEILING_EXPRESSIONS: usize = 52;
/// As [`CEILING_EXPRESSIONS`], for `Condition` attributes.
const CEILING_CONDITIONS: usize = 145;
