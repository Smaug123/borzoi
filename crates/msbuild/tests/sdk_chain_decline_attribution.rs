//! **Why** the SDK-chain census declines what it declines.
//!
//! [`sdk_chain_expression_census`](../sdk_chain_expression_census.rs) buckets
//! its declines by the *function being called*, which answers "what would I
//! have to model?" but not "what is actually blocking this?". The two are
//! different questions with different answers, and the census's own comments
//! answered the second one wrong: they said the declines were dominated by
//! undefined **reserved** receivers.
//!
//! That claim was load-bearing — `docs/msbuild-value-carried-provenance-plan.md`
//! deferred its P3 stage behind "land trusted seeding, then re-price", so a
//! wrong attribution had made a real stage wait on a lever nobody had sized.
//! Hence this file: the attribution is a number the machine keeps.
//!
//! **The classifier does not read issue tags**, because they do not carry the
//! distinction. `$([System.IO.Path]::Combine($(Undefined), 'x'))` reports
//! `Unsupported` and nothing else, even though `Combine` *is* modelled and the
//! real blocker is the operand — so charging `Unsupported` to "shape not
//! modelled" would silently absorb every operand-blocked call into the one
//! bucket the conclusion rests on. Instead each declined item is **re-evaluated
//! with every name it references defined**. If it commits then, the shape is
//! modelled and a richer property table would reach it; if it still declines,
//! no property table can.
//!
//! **This runs without the oracle.** It asks only what *our* evaluator does and
//! why, so it decomposes the census's decline column rather than adding a
//! second differential. Certain-implies-exact is the census's job.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use borzoi_msbuild::test_support::{
    Outcome, PropertyMap, evaluate, is_toolset_initial_property_name, property_references,
    substitute,
};

use common::sdk_chain::{
    extract_call_expressions, extract_conditions, msbuild_files, sdk_dir, seeded_props,
};

/// Plausible fillers for an otherwise-undefined name.
///
/// A single filler under-reports: `[System.IO.Path]::Combine` rejects `"x"` on
/// some hosts while accepting a path, and a version comparison rejects anything
/// that is not version-shaped. Trying several and counting the item as
/// *reachable* if **any** of them commits over-approximates "a richer table
/// would turn this on" — deliberately, because that over-approximation inflates
/// the seeding lever, which is the quantity this file is sceptical about. An
/// over-stated lever that still comes out small is a safe conclusion.
const FILLERS: &[&str] = &["x", "/tmp/probe", "1.0.0", "net10.0", "true"];

/// Would a richer property table let `render` commit? `references` are the
/// names it reads that the seeded table lacks.
fn reachable_with_operands_defined(
    references: &BTreeSet<String>,
    base: &[(String, String)],
    render: impl Fn(&PropertyMap) -> bool,
) -> bool {
    FILLERS.iter().any(|filler| {
        let mut map = PropertyMap::new();
        for (k, v) in base {
            map.insert(k.clone(), v.clone());
        }
        for name in references {
            map.insert(name.clone(), (*filler).to_string());
        }
        render(&map)
    })
}

#[derive(Default)]
struct Attribution {
    committed: usize,
    /// Declines no property table can reach: still declined with every name it
    /// references defined, under any filler. A property function we do not
    /// implement, or grammar outside the modelled subset.
    unreachable: usize,
    /// Declines a richer property table *would* reach.
    operand_blocked: usize,
    /// …of which the missing names are all reserved — an **upper bound** on
    /// the "trusted seeding" lever, not its value. The census seeds no reserved
    /// name at all, so this counts names a real walk already supplies
    /// (`MSBuildProjectDirectory`, `MSBuildToolsVersion`, `MSBuildRuntimeType`,
    /// …). The lever is smaller than the number printed here.
    operand_reserved_only: usize,
    operand_mixed: usize,
    operand_ordinary_only: usize,
    /// Which names block a reachable shape, most frequent first.
    blocking: BTreeMap<String, usize>,
}

impl Attribution {
    fn note_operand_blocked(&mut self, names: &BTreeSet<String>) {
        self.operand_blocked += 1;
        let reserved = names
            .iter()
            .filter(|n| is_toolset_initial_property_name(n))
            .count();
        if reserved == names.len() {
            self.operand_reserved_only += 1;
        } else if reserved > 0 {
            self.operand_mixed += 1;
        } else {
            self.operand_ordinary_only += 1;
        }
        for name in names {
            *self.blocking.entry(name.clone()).or_default() += 1;
        }
    }

    fn report(&self, title: &str, total: usize) {
        eprintln!("--- {title}: {total} distinct ---");
        eprintln!("  committed:                          {}", self.committed);
        eprintln!("  declined, no table can reach:       {}", self.unreachable);
        eprintln!(
            "  declined, operands missing:         {}",
            self.operand_blocked
        );
        eprintln!(
            "      purely reserved names:          {}",
            self.operand_reserved_only
        );
        eprintln!(
            "      reserved mixed with ordinary:   {}",
            self.operand_mixed
        );
        eprintln!(
            "      no reserved name involved:      {}",
            self.operand_ordinary_only
        );
        let mut rows: Vec<(&String, &usize)> = self.blocking.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        eprintln!("  top blocking names:");
        for (name, count) in rows.iter().take(15) {
            let tag = if is_toolset_initial_property_name(name) {
                "RESERVED"
            } else {
                "ordinary"
            };
            eprintln!("    {count:5}  {tag}  {name}");
        }
    }
}

/// Names `raw` reads that `seeded` does not define.
fn missing_references(raw: &str, seeded: &[(String, String)]) -> BTreeSet<String> {
    let defined: BTreeSet<String> = seeded.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
    property_references(raw)
        .into_iter()
        .map(|n| n.to_ascii_lowercase())
        .filter(|n| !defined.contains(n))
        .collect()
}

/// The decline column of both censuses, decomposed by cause.
///
/// The assertions are **two-sided** over the *whole* partition, for the same
/// reason `parser_corpus`'s `CLEAN_PARSES` is: the SDK is pinned and the
/// evaluator deterministic, so there is no drift for a one-sided bound to
/// absorb, and pinning only the headline subset would let work move items
/// between the other buckets without ever restating the figures that priced
/// the plan. Improving the evaluator is *supposed* to fail this test; update
/// the numbers with the date and say which way they moved.
///
/// Measured 2026-08-02 against the pinned SDK 10.0.301.
#[test]
#[ignore = "walks the whole pinned SDK chain; a measurement, run in CI's sweep lane"]
fn the_declines_are_dominated_by_shapes_no_property_table_can_reach() {
    let sdk = sdk_dir();
    let files = msbuild_files(&sdk);
    let seeded = seeded_props();
    let mut seeded_map = PropertyMap::new();
    for (k, v) in &seeded {
        seeded_map.insert(k.clone(), v.clone());
    }

    // ---- expressions ----
    let mut expressions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file) {
            extract_call_expressions(&text, &mut expressions);
        }
    }
    let mut expr = Attribution::default();
    for expression in &expressions {
        let (_, issues) = substitute(expression, &seeded_map);
        if issues.is_empty() {
            expr.committed += 1;
            continue;
        }
        let missing = missing_references(expression, &seeded);
        let reachable = !missing.is_empty()
            && reachable_with_operands_defined(&missing, &seeded, |map| {
                substitute(expression, map).1.is_empty()
            });
        if reachable {
            expr.note_operand_blocked(&missing);
        } else {
            expr.unreachable += 1;
        }
    }
    expr.report("SDK-chain call expressions", expressions.len());

    // ---- conditions ----
    let mut conditions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file) {
            extract_conditions(&text, &mut conditions);
        }
    }
    let mut cond = Attribution::default();
    for condition in &conditions {
        let eval = evaluate(condition, &seeded_map);
        if eval.outcome != Outcome::Unsupported && eval.undefined_properties.is_empty() {
            cond.committed += 1;
            continue;
        }
        let missing = missing_references(condition, &seeded);
        let reachable = !missing.is_empty()
            && reachable_with_operands_defined(&missing, &seeded, |map| {
                let e = evaluate(condition, map);
                e.outcome != Outcome::Unsupported && e.undefined_properties.is_empty()
            });
        if reachable {
            cond.note_operand_blocked(&missing);
        } else {
            cond.unreachable += 1;
        }
    }
    cond.report("SDK-chain conditions", conditions.len());

    // The finding, pinned as a whole partition. Reserved-name seeding is a
    // *small* lever, and the population it cannot reach at all is the large one.
    assert_eq!(
        (
            expr.committed,
            expr.unreachable,
            expr.operand_blocked,
            expr.operand_reserved_only,
            expr.operand_mixed,
            expr.operand_ordinary_only,
        ),
        EXPRESSION_ATTRIBUTION,
        "the SDK-chain expression attribution moved; restate it with today's \
         date and say which way, rather than adjusting the tuple to match"
    );
    assert_eq!(
        (
            cond.committed,
            cond.unreachable,
            cond.operand_blocked,
            cond.operand_reserved_only,
            cond.operand_mixed,
            cond.operand_ordinary_only,
        ),
        CONDITION_ATTRIBUTION,
        "the SDK-chain condition attribution moved; restate it with today's \
         date and say which way, rather than adjusting the tuple to match"
    );
    // Non-vacuity: the decomposition must partition the census's own population,
    // or a broken extractor would make every bucket agreeably small.
    assert_eq!(
        expr.committed + expr.unreachable + expr.operand_blocked,
        expressions.len(),
        "the expression attribution must partition the census population"
    );
    assert_eq!(
        cond.committed + cond.unreachable + cond.operand_blocked,
        conditions.len(),
        "the condition attribution must partition the census population"
    );
    assert_eq!(
        (expr.committed, cond.committed),
        (66, 139),
        "the attribution's committed counts must equal the census's own \
         (same seeds, same extraction) — if they diverge, one of the two \
         harnesses has drifted and neither number can be trusted"
    );
}

/// `(committed, unreachable, operand_blocked, reserved_only, mixed, ordinary_only)`
/// over the pinned SDK 10.0.301, measured 2026-08-02.
const EXPRESSION_ATTRIBUTION: (usize, usize, usize, usize, usize, usize) =
    (66, 193, 137, 13, 13, 111);
/// As [`EXPRESSION_ATTRIBUTION`], for `Condition` attributes.
const CONDITION_ATTRIBUTION: (usize, usize, usize, usize, usize, usize) =
    (139, 157, 2462, 42, 65, 2355);
