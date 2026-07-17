//! Phase-3 scoping census (`#[ignore]`d corpus sweep). For every symbol use FCS
//! resolves over a corpus sample, bucket it by *what machinery a resolver needs*
//! (see [`common::classify`] for the taxonomy):
//!
//! - **B1 — lexical**: scope / import / path / assembly-index, **no inference**.
//! - **B2 — shallow inference**: a receiver type for a single-candidate member.
//! - **B3 — hard pile**: overload resolution or extension-member search.
//!
//! Prints a report (overall plus per source-area); it is a measurement, not a
//! gate, so it asserts only that it observed uses. Run with:
//!
//! ```text
//! cargo test -p borzoi-sema --test all uses_census:: -- --ignored --nocapture
//! ```
//!
//! Honours `BORZOI_CORPUS`; tune the sample with `BORZOI_CENSUS_STRIDE`
//! (default 13 — every 13th `.fs` file) and `BORZOI_CENSUS_LIMIT`.
//!
//! ## Two biases, stated so the numbers don't mislead
//!
//! 1. **Isolation bias.** `fcs-dump uses-census-batch` type-checks each file
//!    *alone*, so a name referencing an unresolved sibling type drops out of
//!    FCS's use list. The member-needing fraction (B2+B3) is therefore a **lower
//!    bound**: self-contained files (most of `tests/`) lose ~nothing;
//!    interconnected library files (`src/`) lose the most. `uses_census_project`
//!    quantifies that gap. The hardness split *within* resolved members —
//!    `B3 / (B2+B3)` — reads an intrinsic property of each symbol and is unbiased.
//! 2. **Corpus bias.** This corpus is the F# compiler's own repo: `tests/`
//!    (feature snippets) and `src/` (the compiler). Neither is typical
//!    application code, so the report breaks the distribution down by area.

use crate::common::{FileCensus, Tally, env_usize_or, invoke_fcs_dump_census, parse_census_jsonl};
use std::path::{Path, PathBuf};

/// Which top-level area of the F# repo a file belongs to, for the per-area
/// breakdown (the `tests/` snippets and the `src/` compiler differ sharply).
fn area_of(path: &str) -> &'static str {
    for (needle, label) in [
        ("/tests/", "tests"),
        ("/src/", "src"),
        ("/vsintegration/", "vsintegration"),
    ] {
        if path.contains(needle) {
            return label;
        }
    }
    "other"
}

/// Recursively collect `.fs` implementation files (not `.fsi`: signature files
/// are a different use population — annotations and member signatures), skipping
/// build/VCS output and symlinks.
fn collect_fs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                continue;
            }
            collect_fs(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("fs") {
            out.push(path);
        }
    }
}

/// Print one area's (or the whole sample's) bucket ratios and sub-tag histogram.
fn print_report(label: &str, files: usize, t: &Tally) {
    let nondef = t.nondef();
    println!("\n=== {label} === ({files} files, {nondef} non-definition uses)");
    if nondef == 0 {
        return;
    }
    let pct = |n: u64| 100.0 * n as f64 / nondef as f64;
    println!(
        "  B1 lexical (no inference) : {:7} ({:5.1}%)",
        t.buckets[0],
        pct(t.buckets[0])
    );
    println!(
        "  B2 shallow inference      : {:7} ({:5.1}%)",
        t.buckets[1],
        pct(t.buckets[1])
    );
    println!(
        "  B3 hard pile              : {:7} ({:5.1}%)",
        t.buckets[2],
        pct(t.buckets[2])
    );
    if t.buckets[3] > 0 {
        println!("  (unclassified)            : {:7}", t.buckets[3]);
    }
    let members = t.buckets[1] + t.buckets[2];
    if members > 0 {
        println!(
            "  -> needs inference (B2+B3): {members} ({:.1}% of non-def); \
             hard-pile share B3/(B2+B3) = {:.1}%",
            t.needs_inference_pct(),
            100.0 * t.buckets[2] as f64 / members as f64
        );
    }
    let mut sorted: Vec<_> = t
        .subtags
        .iter()
        .filter(|(k, _)| **k != "definition-occurrence")
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    println!("  sub-tags:");
    for (tag, n) in sorted {
        println!("    {n:7}  {tag}");
    }
}

#[test]
#[ignore = "corpus sweep: needs BORZOI_CORPUS + builds/JIT-warms fcs-dump"]
fn uses_bucket_census() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!(
            "BORZOI_CORPUS unset; skipping census. Run under `nix develop`, \
             or point it at an F# checkout."
        );
        return;
    };
    let root = PathBuf::from(root);
    let stride = env_usize_or("BORZOI_CENSUS_STRIDE", 13).max(1);
    let limit = env_usize_or("BORZOI_CENSUS_LIMIT", usize::MAX);

    let mut all_files = Vec::new();
    collect_fs(&root, &mut all_files);
    all_files.sort();
    let sample: Vec<PathBuf> = all_files
        .iter()
        .step_by(stride)
        .take(limit)
        .cloned()
        .collect();
    assert!(!sample.is_empty(), "no .fs files under {root:?}");
    eprintln!(
        "census: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all_files.len()
    );

    let census: Vec<FileCensus> = parse_census_jsonl(&invoke_fcs_dump_census(&sample));
    let ok: Vec<&FileCensus> = census.iter().filter(|f| f.ok).collect();
    println!(
        "FILES: {} sampled, {} type-checked Ok ({:.0}%)",
        census.len(),
        ok.len(),
        100.0 * ok.len() as f64 / census.len() as f64
    );

    let mut overall = Tally::default();
    overall.add(ok.iter().flat_map(|f| f.uses.iter()));
    print_report("ALL AREAS", ok.len(), &overall);

    for area in ["tests", "src", "vsintegration", "other"] {
        let area_files: Vec<&&FileCensus> =
            ok.iter().filter(|f| area_of(&f.path) == area).collect();
        if area_files.is_empty() {
            continue;
        }
        let mut t = Tally::default();
        t.add(area_files.iter().flat_map(|f| f.uses.iter()));
        print_report(&format!("AREA = {area}"), area_files.len(), &t);
    }

    assert!(
        overall.nondef() > 0,
        "census observed no non-definition uses"
    );
}
