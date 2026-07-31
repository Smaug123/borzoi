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
//! `BORZOI_USES_CENSUS_OUT=<dir>` additionally writes a `summary.json` to the
//! generator contract in `docs/continuous-measurements.md`, which is what lets
//! `stats.yml` carry this census as a series rather than as a number somebody
//! reads off a local terminal once.
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

use crate::common::{
    Bucket, CENSUS_TAGS, FileCensus, Tally, env_usize_or, invoke_fcs_dump_census,
    parse_census_jsonl,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The areas [`area_of`] can return. The published per-area breakdown emits
/// every one of them every run, zeros included: a map keyed by the areas that
/// *occurred* would drop `vsintegration` the moment the sample missed it, and
/// the dashboard reads an absent key exactly as it reads a null — the previous
/// run's value stays "Latest" and nobody learns the area went unmeasured.
const AREAS: [&str; 4] = ["tests", "src", "vsintegration", "other"];

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

#[derive(Serialize)]
struct Summary {
    schema_version: u32,
    measurement: &'static str,
    configuration: ConfigurationSummary,
    statistics: StatisticsSummary,
}

#[derive(Serialize)]
struct ConfigurationSummary {
    corpus: &'static str,
    file_extensions: [&'static str; 1],
    /// `"file-isolated"` — each file is type-checked alone, which is the whole
    /// reason the B2/B3 share reads as a lower bound (see the module docs).
    scope: &'static str,
    stride: usize,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct StatisticsSummary {
    files: FileSummary,
    uses: UseSummary,
    buckets: BucketSummary,
    needs_inference: RatioSummary,
    hard_pile_share: RatioSummary,
    by_tag: BTreeMap<&'static str, u64>,
    by_area: BTreeMap<&'static str, AreaSummary>,
}

#[derive(Serialize)]
struct FileSummary {
    sampled: usize,
    type_checked_ok: usize,
}

#[derive(Serialize)]
struct UseSummary {
    total: u64,
    non_definition: u64,
    definition_occurrences: u64,
}

#[derive(Serialize)]
struct BucketSummary {
    b1: u64,
    b2: u64,
    b3: u64,
    other: u64,
}

/// A ratio published as its two integer parts plus basis points, never as a
/// float and never as an `Option`. An empty denominator yields `0` basis points
/// rather than `null`, and `denominator` sits beside it so "0 of 0" stays
/// distinguishable from "0 of many".
#[derive(Serialize)]
struct RatioSummary {
    numerator: u64,
    denominator: u64,
    basis_points: u64,
}

impl RatioSummary {
    fn new(numerator: u64, denominator: u64) -> Self {
        // `checked_div` rather than a zero test: an empty denominator yields a
        // defined `0`, never a `null` the dashboard would skip.
        let basis_points = (numerator * 10_000).checked_div(denominator).unwrap_or(0);
        Self {
            numerator,
            denominator,
            basis_points,
        }
    }
}

#[derive(Serialize)]
struct AreaSummary {
    files: usize,
    non_definition: u64,
    buckets: BucketSummary,
    needs_inference: RatioSummary,
}

/// Project one [`Tally`] onto the published bucket counts.
fn bucket_summary(t: &Tally) -> BucketSummary {
    BucketSummary {
        b1: t.buckets[0],
        b2: t.buckets[1],
        b3: t.buckets[2],
        other: t.buckets[3],
    }
}

/// The count of uses `classify` set aside as defining occurrences — outside the
/// resolution denominator, but published so `total` reconciles.
fn definition_occurrences(t: &Tally) -> u64 {
    t.subtags.get("definition-occurrence").copied().unwrap_or(0)
}

/// Seed every tag in the taxonomy at zero, then fill in what occurred.
///
/// A tag `classify` produced that [`CENSUS_TAGS`] does not name would mint a
/// metric this run and no other, so it fails here rather than reaching the
/// recorder — the enumeration test that pins the list should make it
/// unreachable, and this is what says so if it ever is not.
fn by_tag(t: &Tally) -> BTreeMap<&'static str, u64> {
    let mut counts: BTreeMap<&'static str, u64> =
        CENSUS_TAGS.iter().map(|(tag, _)| (*tag, 0)).collect();
    for (tag, n) in &t.subtags {
        if *tag == "definition-occurrence" {
            continue;
        }
        let slot = counts.get_mut(tag).unwrap_or_else(|| {
            panic!(
                "sub-tag {tag:?} is not in common::CENSUS_TAGS, so it would mint a metric \
                 on the runs it happens to occur in; add it to the taxonomy list"
            )
        });
        *slot = *n;
    }
    counts
}

fn area_summary(files: usize, t: &Tally) -> AreaSummary {
    let members = t.buckets[1] + t.buckets[2];
    AreaSummary {
        files,
        non_definition: t.nondef(),
        buckets: bucket_summary(t),
        needs_inference: RatioSummary::new(members, t.nondef()),
    }
}

fn summary_json(
    sampled: usize,
    type_checked_ok: usize,
    overall: &Tally,
    per_area: &BTreeMap<&'static str, (usize, Tally)>,
    stride: usize,
    limit: Option<usize>,
) -> String {
    let members = overall.buckets[1] + overall.buckets[2];
    let summary = Summary {
        schema_version: 1,
        measurement: "uses-census",
        configuration: ConfigurationSummary {
            corpus: "fsharp-src",
            file_extensions: [".fs"],
            scope: "file-isolated",
            stride,
            limit,
        },
        statistics: StatisticsSummary {
            files: FileSummary {
                sampled,
                type_checked_ok,
            },
            uses: UseSummary {
                total: overall.nondef() + definition_occurrences(overall),
                non_definition: overall.nondef(),
                definition_occurrences: definition_occurrences(overall),
            },
            buckets: bucket_summary(overall),
            needs_inference: RatioSummary::new(members, overall.nondef()),
            // The hardness split *within* resolved members. Unlike the bucket
            // shares this one is unbiased by isolation (it reads an intrinsic
            // property of each symbol), so it is the per-run number worth
            // trending on its own.
            hard_pile_share: RatioSummary::new(overall.buckets[2], members),
            by_tag: by_tag(overall),
            by_area: per_area
                .iter()
                .map(|(area, (files, t))| (*area, area_summary(*files, t)))
                .collect(),
        },
    };
    serde_json::to_string_pretty(&summary).expect("summary serialises")
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

    // Every area is tallied, including the ones with no files. Printing skips
    // an empty area (an empty section is noise on a terminal); the published
    // summary must not, so the tally is built for all of them and the print
    // guard sits after it.
    let mut per_area: BTreeMap<&'static str, (usize, Tally)> = BTreeMap::new();
    for area in AREAS {
        let area_files: Vec<&&FileCensus> =
            ok.iter().filter(|f| area_of(&f.path) == area).collect();
        let mut t = Tally::default();
        t.add(area_files.iter().flat_map(|f| f.uses.iter()));
        if !area_files.is_empty() {
            print_report(&format!("AREA = {area}"), area_files.len(), &t);
        }
        per_area.insert(area, (area_files.len(), t));
    }

    if let Some(dir) = std::env::var_os("BORZOI_USES_CENSUS_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create uses-census output directory");
        let json = summary_json(
            sample.len(),
            ok.len(),
            &overall,
            &per_area,
            stride,
            (limit != usize::MAX).then_some(limit),
        );
        std::fs::write(dir.join("summary.json"), json).expect("write uses-census summary.json");
        eprintln!("wrote {}", dir.join("summary.json").display());
    }

    assert!(
        overall.nondef() > 0,
        "census observed no non-definition uses"
    );
}

/// The generator contract, checked on a **degenerate** run rather than on the
/// fields anybody thought to name: the shape that breaks the dashboard is the
/// one whose denominator is empty, and the key nobody remembers is exactly the
/// key that goes missing.
#[test]
fn summary_json_is_versioned_and_never_publishes_a_null() {
    fn assert_all_numbers(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, child) in fields {
                    assert_all_numbers(child, &format!("{path}.{key}"));
                }
            }
            serde_json::Value::Number(_) => {}
            other => panic!(
                "statistics{path} is {other}, not a number — the dashboard skips it exactly \
                 as it skips an absent key, and the previous run still reads as Latest"
            ),
        }
    }

    // Nothing observed at all: every ratio's denominator is zero.
    let empty = Tally::default();
    let empty_areas: BTreeMap<&'static str, (usize, Tally)> = AREAS
        .into_iter()
        .map(|area| (area, (0, Tally::default())))
        .collect();

    let populated = Tally {
        buckets: [7, 3, 2, 1],
        subtags: [
            ("value:local-or-param", 7),
            ("instance-member:simple", 3),
            ("definition-occurrence", 5),
        ]
        .into_iter()
        .collect(),
    };
    let populated_areas: BTreeMap<&'static str, (usize, Tally)> = AREAS
        .into_iter()
        .map(|area| {
            let is_src = area == "src";
            let t = Tally {
                buckets: if is_src { [7, 3, 2, 1] } else { [0; 4] },
                ..Tally::default()
            };
            (area, (if is_src { 4 } else { 0 }, t))
        })
        .collect();

    for (tally, areas, stride, limit) in [
        (&empty, &empty_areas, 13, None),
        (&populated, &populated_areas, 1, Some(50)),
    ] {
        let rendered = summary_json(0, 0, tally, areas, stride, limit);
        let json: serde_json::Value = serde_json::from_str(&rendered).expect("summary is JSON");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["measurement"], "uses-census");
        assert_all_numbers(&json["statistics"], "");

        // Every taxonomy key is present whether or not it occurred, so a
        // construct vanishing from the corpus reads as a zero rather than as a
        // retired metric.
        let by_tag = json["statistics"]["by_tag"]
            .as_object()
            .expect("by_tag is an object");
        assert_eq!(by_tag.len(), CENSUS_TAGS.len());
        for (tag, _) in CENSUS_TAGS {
            assert!(by_tag.contains_key(tag), "by_tag lost {tag}");
        }
        let by_area = json["statistics"]["by_area"]
            .as_object()
            .expect("by_area is an object");
        assert_eq!(by_area.len(), AREAS.len());
    }
}

/// The bucket totals and the tag histogram are two views of one classification,
/// so they must reconcile: filing a tag under the wrong bucket in
/// [`CENSUS_TAGS`] would move the census's headline B1/B2/B3 split while every
/// key stayed present and the total stayed put.
#[test]
fn the_tag_histogram_reconciles_with_the_bucket_totals() {
    let mut tally = Tally::default();
    for (tag, bucket) in CENSUS_TAGS {
        let n = 1 + (tag.len() as u64 % 5);
        tally.subtags.insert(tag, n);
        let slot = match bucket {
            Bucket::B1 => 0,
            Bucket::B2 => 1,
            Bucket::B3 => 2,
            Bucket::Other => 3,
        };
        tally.buckets[slot] += n;
    }

    let json: serde_json::Value = serde_json::from_str(&summary_json(
        1,
        1,
        &tally,
        &AREAS
            .into_iter()
            .map(|area| (area, (0, Tally::default())))
            .collect(),
        13,
        None,
    ))
    .expect("summary is JSON");

    for (bucket, key) in [
        (Bucket::B1, "b1"),
        (Bucket::B2, "b2"),
        (Bucket::B3, "b3"),
        (Bucket::Other, "other"),
    ] {
        let from_tags: u64 = CENSUS_TAGS
            .into_iter()
            .filter(|(_, b)| *b == bucket)
            .map(|(tag, _)| json["statistics"]["by_tag"][tag].as_u64().expect("count"))
            .sum();
        assert_eq!(
            json["statistics"]["buckets"][key].as_u64().expect("count"),
            from_tags,
            "bucket {key} disagrees with the tags CENSUS_TAGS files under it"
        );
    }
}
