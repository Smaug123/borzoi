//! Phase-3 *type* scoping census (`#[ignore]`d corpus sweep) — the type-side
//! sibling of [`uses_census`](../uses_census.rs). For every expression FCS
//! assigns a type over a corpus sample, bucket it by *what machinery a resolver
//! needs to assign that type* (see [`common::classify_expr`]):
//!
//! - **Lit** — a literal's primitive type. Reproducible by Phase 3.1 alone.
//! - **Spine** — the lexical / HM spine: value references, function & static
//!   calls, constructors, lambdas, control flow, tuples / records / unions.
//!   Typed by unification with **no type-directed member lookup**.
//! - **Member** — a single-candidate instance member / field; needs the
//!   *receiver* type first. The Phase 3.3 (`expr.Foo`) payoff.
//! - **Hard** — overloaded instance/static call, extension member, SRTP trait
//!   call. Needs overload resolution / constraint solving.
//!
//! Where the uses census measures the **name-resolution** axis (the hover/nav
//! currency when the target is a *name*), this measures the **expression-type**
//! axis (the hover currency for *any* expression, including literals and
//! compound expressions that are not name uses). Together they scope Phase 3:
//! the uses census found member access ≈ 7–12 % and overloads dominating the
//! hard pile on the name axis; this checks whether the *type* axis agrees.
//!
//! Prints a report (overall plus per source-area); a measurement, not a gate,
//! so it asserts only that it observed typed expressions. Run with:
//!
//! ```text
//! cargo test -p borzoi-sema --test all types_census:: -- --ignored --nocapture
//! ```
//!
//! Honours `BORZOI_CORPUS`; tune the sample with `BORZOI_CENSUS_STRIDE`
//! (default 13) and `BORZOI_CENSUS_LIMIT`.
//!
//! ## Three biases, stated so the numbers don't mislead
//!
//! 1. **Isolation bias.** `types-census-batch` type-checks each file *alone*, so
//!    a member access on an unresolved sibling type degrades (FCS leaves it a
//!    typar / a plain `call:function`) instead of becoming a `call:instance`.
//!    The **Member** + **Hard** fractions are therefore a **lower bound**, worst
//!    on interconnected `src/`, near-zero loss on self-contained `tests/`
//!    snippets — the same bias the uses census documents.
//! 2. **Elaboration bias.** The oracle walks FCS's *reduced* typed tree, not the
//!    source syntax: pattern matches are lowered to decision trees, and
//!    `inline` operators / pipelines / CEs are desugared. fcs-dump collapses
//!    nodes sharing an identical source range (keeping the outermost) so the
//!    `inline`-operator fan-out does not dominate, but the population is still
//!    *elaborated source spans*, not a 1:1 image of the CST.
//! 3. **Corpus bias.** Same corpus as the uses census (the F# compiler repo);
//!    the report breaks the distribution down by area.

use crate::common;

use crate::common::{
    FileTypeCensus, TypeBucket, classify_expr, env_usize_or, invoke_fcs_dump_types_census,
    parse_type_census_jsonl,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Which top-level area of the F# repo a file belongs to (the `tests/` snippets
/// and the `src/` compiler differ sharply). Duplicated from `uses_census.rs` —
/// each census test is its own binary and the helper is two lines.
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

/// Recursively collect `.fs` implementation files, skipping build/VCS output and
/// symlinks. Duplicated from `uses_census.rs` (separate test binaries).
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

/// Bucket counts (`[Lit, Spine, Member, Hard, Other]`) plus a per-kind histogram
/// and an "unground" count (nodes FCS itself left with a typar in their type —
/// an isolation-incompleteness signal).
#[derive(Default)]
struct Tally {
    buckets: [u64; 5],
    subtags: BTreeMap<String, u64>,
    unground: u64,
}

impl Tally {
    fn add<'a>(&mut self, exprs: impl Iterator<Item = &'a common::CensusExpr>) {
        for e in exprs {
            *self.subtags.entry(e.kind.clone()).or_default() += 1;
            // A `'` in FCS's rendered type means an unsolved/generic typar
            // remains — FCS could not fully ground it (common when a file is
            // checked without its siblings). Tracked as a quality signal.
            if e.ty.contains('\'') {
                self.unground += 1;
            }
            let idx = match classify_expr(&e.kind) {
                TypeBucket::Lit => 0,
                TypeBucket::Spine => 1,
                TypeBucket::Member => 2,
                TypeBucket::Hard => 3,
                TypeBucket::Other => 4,
            };
            self.buckets[idx] += 1;
        }
    }

    fn total(&self) -> u64 {
        self.buckets.iter().sum()
    }
}

/// Print one area's (or the whole sample's) bucket ratios and kind histogram.
fn print_report(label: &str, files: usize, t: &Tally) {
    let total = t.total();
    println!("\n=== {label} === ({files} files, {total} typed expressions)");
    if total == 0 {
        return;
    }
    let pct = |n: u64| 100.0 * n as f64 / total as f64;
    let names = [
        "Lit  (literal)        ",
        "Spine (lexical/HM)    ",
        "Member (recv lookup)  ",
        "Hard  (overload/SRTP) ",
        "(unclassified)        ",
    ];
    for (i, name) in names.iter().enumerate() {
        if i == 4 && t.buckets[4] == 0 {
            continue;
        }
        println!("  {name}: {:8} ({:5.1}%)", t.buckets[i], pct(t.buckets[i]));
    }
    let inference = t.buckets[2] + t.buckets[3];
    println!(
        "  -> needs inference (Member+Hard): {inference} ({:.1}%); \
         hard-pile share Hard/(Member+Hard) = {:.1}%",
        pct(inference),
        if inference == 0 {
            0.0
        } else {
            100.0 * t.buckets[3] as f64 / inference as f64
        }
    );
    println!(
        "  unground (typar in type): {} ({:.1}%)",
        t.unground,
        pct(t.unground)
    );
    let mut sorted: Vec<_> = t.subtags.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    println!("  kinds:");
    for (tag, n) in sorted {
        println!("    {n:8}  {tag}");
    }
}

/// The areas [`area_of`] can return, emitted in full every run — an area that
/// stops occurring must read as zero rather than vanish (see
/// `docs/continuous-measurements.md`).
const AREAS: [&str; 4] = ["tests", "src", "vsintegration", "other"];

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
    scope: &'static str,
    stride: usize,
    limit: Option<usize>,
}

/// Note what is *not* here: the per-kind histogram the report prints.
///
/// `statistics` is a metric namespace, and its keys have to mean the same thing
/// in every run. The kind strings come from the oracle's walk of FCS's typed
/// tree, so their key space is open — a new FCS elaboration would mint a metric
/// on the run it first appears in, and `classify_expr` already funnels anything
/// it does not name into `Other`. The five buckets are the closed enumeration
/// underneath, so they are what gets published; the histogram stays in the
/// report, where an open key set costs nothing.
#[derive(Serialize)]
struct StatisticsSummary {
    files: FileSummary,
    expressions: u64,
    buckets: BucketSummary,
    needs_inference: RatioSummary,
    hard_pile_share: RatioSummary,
    /// Nodes FCS itself left carrying a typar. An isolation-incompleteness
    /// signal rather than a defect: it bounds how much of this sample the
    /// oracle could ground at all, so the bucket shares below are read against
    /// it.
    unground: RatioSummary,
    by_area: BTreeMap<&'static str, AreaSummary>,
}

#[derive(Serialize)]
struct FileSummary {
    sampled: usize,
    type_checked_ok: usize,
}

#[derive(Serialize)]
struct BucketSummary {
    lit: u64,
    spine: u64,
    member: u64,
    hard: u64,
    other: u64,
}

/// A ratio as two integers plus basis points — never a float, never an
/// `Option`. An empty denominator gives `0`, with `denominator` beside it so
/// "0 of 0" and "0 of many" stay distinguishable.
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
    expressions: u64,
    buckets: BucketSummary,
    needs_inference: RatioSummary,
    unground: RatioSummary,
}

fn bucket_summary(t: &Tally) -> BucketSummary {
    BucketSummary {
        lit: t.buckets[0],
        spine: t.buckets[1],
        member: t.buckets[2],
        hard: t.buckets[3],
        other: t.buckets[4],
    }
}

fn area_summary(files: usize, t: &Tally) -> AreaSummary {
    let inference = t.buckets[2] + t.buckets[3];
    AreaSummary {
        files,
        expressions: t.total(),
        buckets: bucket_summary(t),
        needs_inference: RatioSummary::new(inference, t.total()),
        unground: RatioSummary::new(t.unground, t.total()),
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
    let inference = overall.buckets[2] + overall.buckets[3];
    let summary = Summary {
        schema_version: 1,
        measurement: "types-census",
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
            expressions: overall.total(),
            buckets: bucket_summary(overall),
            needs_inference: RatioSummary::new(inference, overall.total()),
            hard_pile_share: RatioSummary::new(overall.buckets[3], inference),
            unground: RatioSummary::new(overall.unground, overall.total()),
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
fn types_bucket_census() {
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
        "type census: {} of {} .fs files (stride {stride}); elaborating each in isolation…",
        sample.len(),
        all_files.len()
    );

    let census: Vec<FileTypeCensus> =
        parse_type_census_jsonl(&invoke_fcs_dump_types_census(&sample));
    let ok: Vec<&FileTypeCensus> = census.iter().filter(|f| f.ok).collect();
    println!(
        "FILES: {} sampled, {} produced a typed tree ({:.0}%)",
        census.len(),
        ok.len(),
        100.0 * ok.len() as f64 / census.len().max(1) as f64
    );

    let mut overall = Tally::default();
    overall.add(ok.iter().flat_map(|f| f.exprs.iter()));
    print_report("ALL AREAS", ok.len(), &overall);

    // Tallied for every area, printed only for the non-empty ones: an empty
    // section is noise on a terminal, but an absent key is a lost metric.
    let mut per_area: BTreeMap<&'static str, (usize, Tally)> = BTreeMap::new();
    for area in AREAS {
        let area_files: Vec<&&FileTypeCensus> =
            ok.iter().filter(|f| area_of(&f.path) == area).collect();
        let mut t = Tally::default();
        t.add(area_files.iter().flat_map(|f| f.exprs.iter()));
        if !area_files.is_empty() {
            print_report(&format!("AREA = {area}"), area_files.len(), &t);
        }
        per_area.insert(area, (area_files.len(), t));
    }

    if let Some(dir) = std::env::var_os("BORZOI_TYPES_CENSUS_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create types-census output directory");
        let json = summary_json(
            sample.len(),
            ok.len(),
            &overall,
            &per_area,
            stride,
            (limit != usize::MAX).then_some(limit),
        );
        std::fs::write(dir.join("summary.json"), json).expect("write types-census summary.json");
        eprintln!("wrote {}", dir.join("summary.json").display());
    }

    assert!(
        overall.total() > 0,
        "type census observed no typed expressions"
    );
}

/// The generator contract on a degenerate run — the shape whose denominators
/// are all zero, which is where a ratio turns into `null` and the dashboard
/// silently keeps showing the previous run's value.
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

    let empty_areas: BTreeMap<&'static str, (usize, Tally)> = AREAS
        .into_iter()
        .map(|area| (area, (0, Tally::default())))
        .collect();

    let populated = Tally {
        buckets: [11, 40, 6, 2, 1],
        unground: 3,
        subtags: [("call:instance".to_string(), 6)].into_iter().collect(),
    };

    for (tally, stride, limit) in [(&Tally::default(), 13, None), (&populated, 1, Some(250))] {
        let rendered = summary_json(0, 0, tally, &empty_areas, stride, limit);
        let json: serde_json::Value = serde_json::from_str(&rendered).expect("summary is JSON");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["measurement"], "types-census");
        assert_all_numbers(&json["statistics"], "");
        assert_eq!(
            json["statistics"]["by_area"]
                .as_object()
                .expect("by_area is an object")
                .len(),
            AREAS.len()
        );
    }
}

/// The published buckets must account for every expression the tally saw. The
/// kind histogram is deliberately unpublished (its key space is open), so this
/// total is the only thing tying `statistics` back to what was walked — if the
/// five buckets stopped partitioning the population, nothing else would say so.
#[test]
fn the_published_buckets_account_for_every_expression() {
    let tally = Tally {
        buckets: [11, 40, 6, 2, 1],
        ..Tally::default()
    };

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

    let buckets = &json["statistics"]["buckets"];
    let summed: u64 = ["lit", "spine", "member", "hard", "other"]
        .into_iter()
        .map(|k| buckets[k].as_u64().expect("count"))
        .sum();
    assert_eq!(json["statistics"]["expressions"].as_u64(), Some(summed));
    assert_eq!(summed, 60);

    // `needs_inference` is Member + Hard, and its denominator is the whole
    // population — not the inference-needing part, which would make it 1.0 by
    // construction and measure nothing.
    assert_eq!(json["statistics"]["needs_inference"]["numerator"], 8);
    assert_eq!(json["statistics"]["needs_inference"]["denominator"], 60);
    assert_eq!(json["statistics"]["hard_pile_share"]["numerator"], 2);
    assert_eq!(json["statistics"]["hard_pile_share"]["denominator"], 8);
}
