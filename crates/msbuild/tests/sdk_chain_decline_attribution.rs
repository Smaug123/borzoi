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
//! So this file does not classify. It runs the census's own population twice —
//! once with the census seeds, once with the reserved names additionally
//! defined — and reports the **difference**. That is the lever, measured
//! directly, with no claim about the declines it does not move.
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

/// The reserved names a real walk seeds once an SDK resolves, with the values
/// it seeds them with (`seed_toolset_properties`, `properties::well_known`),
/// plus the four path-derivable ones `docs/msbuild-reserved-seeding-plan.md`
/// proposes adding. Realistic rather than uniform: the point is to model the
/// table seeding would produce, not to search for one that commits.
fn reserved_seed() -> Vec<(String, String)> {
    [
        ("MSBuildProjectDirectory", "/repo/proj"),
        ("MSBuildProjectFullPath", "/repo/proj/Demo.fsproj"),
        ("MSBuildProjectName", "Demo"),
        ("MSBuildProjectFile", "Demo.fsproj"),
        ("MSBuildProjectExtension", ".fsproj"),
        ("MSBuildProjectDirectoryNoRoot", "repo/proj"),
        ("MSBuildThisFile", "Demo.fsproj"),
        ("MSBuildThisFileDirectory", "/repo/proj/"),
        ("MSBuildThisFileFullPath", "/repo/proj/Demo.fsproj"),
        ("MSBuildThisFileName", "Demo"),
        ("MSBuildThisFileExtension", ".fsproj"),
        ("MSBuildToolsPath", "/usr/share/dotnet/sdk/10.0.301"),
        ("MSBuildBinPath", "/usr/share/dotnet/sdk/10.0.301"),
        ("MSBuildToolsVersion", "Current"),
        ("MSBuildRuntimeType", "Core"),
        ("MSBuildSDKsPath", "/usr/share/dotnet/sdk/10.0.301/Sdks"),
        ("MSBuildExtensionsPath", "/usr/share/dotnet/sdk/10.0.301/"),
        ("MSBuildExtensionsPath32", "/usr/share/dotnet/sdk/10.0.301"),
        ("MSBuildVersion", "18.6.4"),
        ("MSBuildAssemblyVersion", "18.0"),
        ("MSBuildProgramFiles32", "/Applications"),
        ("MSBuildNodeCount", "1"),
        ("MSBuildStartupDirectory", "/repo"),
        ("VisualStudioVersion", "18.0"),
        ("OS", "Unix"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
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

/// Seed every reserved name and count what that turns on.
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
fn seeding_every_reserved_name_moves_the_census_by_single_digits() {
    let sdk = sdk_dir();
    let files = msbuild_files(&sdk);
    let census_seeds = seeded_props();
    let reserved = reserved_seed();

    let base = table(&[&census_seeds]);
    let with_reserved = table(&[&census_seeds, &reserved]);

    let mut expressions: BTreeSet<String> = BTreeSet::new();
    let mut conditions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file) {
            extract_call_expressions(&text, &mut expressions);
            extract_conditions(&text, &mut conditions);
        }
    }

    let expr_base = expressions
        .iter()
        .filter(|e| expression_commits(e, &base))
        .count();
    let expr_seeded = expressions
        .iter()
        .filter(|e| expression_commits(e, &with_reserved))
        .count();
    let cond_base = conditions
        .iter()
        .filter(|c| condition_commits(c, &base))
        .count();
    let cond_seeded = conditions
        .iter()
        .filter(|c| condition_commits(c, &with_reserved))
        .count();

    eprintln!(
        "expressions: {expr_base} of {} committed, {expr_seeded} with every reserved name seeded \
         (+{})",
        expressions.len(),
        expr_seeded - expr_base
    );
    eprintln!(
        "conditions:  {cond_base} of {} committed, {cond_seeded} with every reserved name seeded \
         (+{})",
        conditions.len(),
        cond_seeded - cond_base
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

    // The finding. Seeding *every* reserved name — including the ones a real
    // walk already supplies, and the one that is unknowable in principle — is
    // an over-statement of the lever by construction, and it still moves the
    // census by single digits against populations of 396 and 2 758.
    assert_eq!(
        (expr_base, expr_seeded),
        (66, 78),
        "the reserved-seeding lever on call expressions moved; restate it with \
         today's date and say which way, rather than adjusting the tuple"
    );
    assert_eq!(
        (cond_base, cond_seeded),
        (139, 163),
        "the reserved-seeding lever on conditions moved; restate it with \
         today's date and say which way, rather than adjusting the tuple"
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
