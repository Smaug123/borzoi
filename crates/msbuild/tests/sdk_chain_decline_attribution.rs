//! **Why** the SDK-chain census declines what it declines.
//!
//! [`sdk_chain_expression_census`](../sdk_chain_expression_census.rs) buckets
//! its declines by the *function being called*, which answers "what would I
//! have to model?" but not "what is actually blocking this?". The two are
//! different questions and they have different answers, and for a year the
//! census's own comments answered the second one wrong: they said the declines
//! were dominated by undefined **reserved** receivers, which the numbers below
//! refute by two orders of magnitude.
//!
//! That claim was load-bearing — `docs/msbuild-value-carried-provenance-plan.md`
//! deferred its P3 stage behind "land trusted seeding, then re-price", so a
//! wrong attribution had made a real stage wait on a lever that does not exist.
//! Hence this file: the attribution is now a number the machine keeps, not a
//! sentence in a comment.
//!
//! **This runs without the oracle.** It asks only what *our* evaluator does and
//! why, so it is a decomposition of the census's decline column rather than a
//! second differential. Certain-implies-exact is the census's job.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use borzoi_msbuild::test_support::{Issue, Outcome, PropertyMap, evaluate, substitute};

use common::sdk_chain::{
    extract_call_expressions, extract_conditions, msbuild_files, sdk_dir, seeded_props,
};

/// Names MSBuild computes for itself and refuses to let a project write —
/// the set "trusted seeding" would have to supply. Deliberately the same
/// predicate the evaluator uses (`is_toolset_initial_property_name`), so this
/// measurement and the walker agree on what "reserved" means.
fn is_reserved(lower: &str) -> bool {
    lower.starts_with("msbuild")
        || matches!(
            lower,
            "visualstudioversion" | "roslyntargetspath" | "os" | "dotnet_host_path"
        )
}

#[derive(Default)]
struct Attribution {
    committed: usize,
    /// The shape itself is outside the modelled grammar — a property function
    /// we do not implement. Seeding cannot help these *at all*: the expression
    /// does not reduce even with every operand defined.
    unmodelled_shape: usize,
    /// Every operand is a name we simply do not have. These are the ones a
    /// richer property table could turn on.
    undefined_only: usize,
    /// …and of those, the ones blocked *purely* by reserved names, which is the
    /// population the "trusted seeding" lever addresses.
    undefined_reserved_only: usize,
    undefined_mixed: usize,
    undefined_ordinary_only: usize,
    /// Which names block an otherwise-reducible shape, most frequent first.
    blocking: BTreeMap<String, usize>,
}

impl Attribution {
    fn note_undefined(&mut self, names: &BTreeSet<String>) {
        self.undefined_only += 1;
        let reserved = names.iter().filter(|n| is_reserved(n)).count();
        if reserved == names.len() {
            self.undefined_reserved_only += 1;
        } else if reserved > 0 {
            self.undefined_mixed += 1;
        } else {
            self.undefined_ordinary_only += 1;
        }
        for name in names {
            *self.blocking.entry(name.clone()).or_default() += 1;
        }
    }

    fn report(&self, title: &str, total: usize) {
        eprintln!("--- {title}: {total} distinct ---");
        eprintln!("  committed:                          {}", self.committed);
        eprintln!(
            "  declined, shape not modelled:       {}",
            self.unmodelled_shape
        );
        eprintln!(
            "  declined, undefined operands only:  {}",
            self.undefined_only
        );
        eprintln!(
            "      purely reserved names:          {}",
            self.undefined_reserved_only
        );
        eprintln!(
            "      reserved mixed with ordinary:   {}",
            self.undefined_mixed
        );
        eprintln!(
            "      no reserved name involved:      {}",
            self.undefined_ordinary_only
        );
        let mut rows: Vec<(&String, &usize)> = self.blocking.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        eprintln!("  top blocking names:");
        for (name, count) in rows.iter().take(15) {
            let tag = if is_reserved(name) {
                "RESERVED"
            } else {
                "ordinary"
            };
            eprintln!("    {count:5}  {tag}  {name}");
        }
    }
}

/// The decline column of both censuses, decomposed by cause.
///
/// The assertions are **two-sided**, for the same reason `parser_corpus`'s
/// `CLEAN_PARSES` is: the SDK is pinned and the evaluator deterministic, so
/// there is no drift for a one-sided bound to absorb, and a one-sided bound on
/// "reserved-blocked declines are few" would silently become a rubber stamp
/// the moment someone made it false. Improving the evaluator is *supposed* to
/// fail this test; update the numbers with the date and say which way they
/// moved.
///
/// Measured 2026-08-02 against the pinned SDK 10.0.301.
#[test]
#[ignore = "walks the whole pinned SDK chain; a measurement, run in CI's sweep lane"]
fn the_declines_are_dominated_by_unmodelled_shapes_not_reserved_names() {
    let sdk = sdk_dir();
    let files = msbuild_files(&sdk);
    let seeded = seeded_props();
    let mut map = PropertyMap::new();
    for (k, v) in &seeded {
        map.insert(k.clone(), v.clone());
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
        let (_, issues) = substitute(expression, &map);
        if issues.is_empty() {
            expr.committed += 1;
            continue;
        }
        if issues
            .iter()
            .any(|i| matches!(i, Issue::Unsupported { .. }))
        {
            expr.unmodelled_shape += 1;
            continue;
        }
        let names: BTreeSet<String> = issues
            .iter()
            .filter_map(|i| match i {
                Issue::Undefined { name } => Some(name.to_ascii_lowercase()),
                Issue::Unsupported { .. } => None,
            })
            .collect();
        expr.note_undefined(&names);
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
        let eval = evaluate(condition, &map);
        if eval.outcome == Outcome::Unsupported {
            cond.unmodelled_shape += 1;
            continue;
        }
        if eval.undefined_properties.is_empty() {
            cond.committed += 1;
            continue;
        }
        let names: BTreeSet<String> = eval
            .undefined_properties
            .iter()
            .map(|n| n.to_ascii_lowercase())
            .collect();
        cond.note_undefined(&names);
    }
    cond.report("SDK-chain conditions", conditions.len());

    // The finding, pinned. Reserved-name seeding is a *small* lever: it can
    // reach only the shapes below, and nothing in the far larger
    // `unmodelled_shape` column, which does not reduce however many operands
    // are defined.
    assert_eq!(
        (expr.undefined_reserved_only, expr.undefined_mixed),
        (6, 0),
        "expression declines blocked by reserved names moved; \
         {} of {} declines are unmodelled shapes, which seeding cannot reach",
        expr.unmodelled_shape,
        expressions.len() - expr.committed
    );
    assert_eq!(
        (cond.undefined_reserved_only, cond.undefined_mixed),
        (40, 14),
        "condition withdrawals blocked by reserved names moved; \
         {} of {} withdrawals have no reserved name involved at all",
        cond.undefined_ordinary_only,
        conditions.len() - cond.committed
    );
    // Non-vacuity: the decomposition must add up to the census's own totals, or
    // a broken extractor would make every bucket agreeably small.
    assert_eq!(
        expr.committed + expr.unmodelled_shape + expr.undefined_only,
        expressions.len(),
        "the expression attribution must partition the census population"
    );
    assert_eq!(
        cond.committed + cond.unmodelled_shape + cond.undefined_only,
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
