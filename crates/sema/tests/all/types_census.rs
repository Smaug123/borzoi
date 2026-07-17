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

    for area in ["tests", "src", "vsintegration", "other"] {
        let area_files: Vec<&&FileTypeCensus> =
            ok.iter().filter(|f| area_of(&f.path) == area).collect();
        if area_files.is_empty() {
            continue;
        }
        let mut t = Tally::default();
        t.add(area_files.iter().flat_map(|f| f.exprs.iter()));
        print_report(&format!("AREA = {area}"), area_files.len(), &t);
    }

    assert!(
        overall.total() > 0,
        "type census observed no typed expressions"
    );
}
