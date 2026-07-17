//! Regenerate the categorised name-resolution divergence report — the
//! resolution analogue of `cst`'s `fcs_divergence.rs`.
//!
//! [`resolve_corpus_diff.rs`](crate) *asserts* a floor of matches, a ceiling of
//! divergences, and (now) a ceiling of B1 gaps over the corpus, but it only ever
//! prints a sample of sites. When you want to *triage* — "which lexical names
//! that FCS resolves do we still defer, and what are they?" — you need the full
//! categorised lists. This is the generator for those: it sweeps the same corpus,
//! classifies every FCS-resolved symbol *use* whose declaration is in the same
//! file, and writes one file per bucket under `resolve-divergence/` at the
//! workspace root (gitignored; override with `BORZOI_RESOLVE_DIVERGENCE_OUT`).
//!
//! The classification crosses **outcome** (did our resolver agree, disagree, or
//! decline?) with the [`Bucket`] taxonomy (what *machinery* the use needs), the
//! two halves that already live in `resolve_corpus_diff` and `uses_census`:
//!
//! * `divergence.txt` — FCS found an **in-file** binder but we gave a
//!   *differently-named* binder, an assembly `Entity`/`Member`, or `Unresolved`.
//!   The unambiguous faults (D5). `resolve_corpus_diff` gates the B1 slice of
//!   this to zero, so real entries here should be B1-free; sorted bucket-first so
//!   any B1 fault sorts to the top. `<bucket>/<tag>\t<path>:<range>\t<text>\t<ours>`.
//! * `alt_binder.txt` — a *same-named* in-file binder at a different range
//!   (OR-pattern canonicalisation / isolation-bias recovery — see
//!   `resolve_corpus_diff`). Reported, not a fault.
//! * `gap_b1.txt` — **the primary worklist.** FCS resolves it with *no inference*
//!   (bucket B1) and its declaration is in this file, yet we return
//!   `Deferred`/nothing. These are the pure-lexical names we ought to bind and
//!   don't. Sorted by sub-`tag` then path, so the dominant missing constructs
//!   group — the analogue of `fcs_divergence`'s `we_reject_fcs_accepts.txt`
//!   sorted by error message.
//! * `gap_b2.txt` / `gap_b3.txt` / `gap_other.txt` — declined uses that need
//!   shallow inference (a receiver type), the hard pile (overload / extension
//!   search), or fall outside the taxonomy. **Expected** until inference lands;
//!   listed for measurement, not a worklist.
//! * infra worklists — `our_parser_errors.txt`, `our_panics.txt`,
//!   `fcs_not_ok.txt`, `unreadable.txt`: one path per line, the files we could
//!   not compare.
//! * `summary.txt` — the per-bucket counts, the `gap_b1` sub-tag histogram
//!   (the actionable digest), and the coverage ratios.
//!
//! Matches are the headline success and are only counted, not listed.
//!
//! ## Scope — and what it deliberately does not cover
//!
//! Like `resolve_corpus_diff`, this runs each file **in isolation** with an
//! empty `AssemblyEnv`, so it can only adjudicate **in-file** declarations.
//! Uses FCS resolves into a referenced assembly (FSharp.Core / BCL / NuGet) have
//! no in-file declaration to compare against and no env for us to resolve them
//! through, so they are tallied under `out-of-file` and otherwise skipped — the
//! target-identity check for those is the *whole-project* differential
//! (`crates/lsp/tests/all/resolve_real_project_diff.rs` and the `corpus-diff`
//! crate), which drives the real assembly closure and the `uses-project` oracle.
//! Bringing assembly divergences into *this* report would mean extending the
//! `uses-census` oracle to emit each symbol's `(assembly, full name)`.
//!
//! `#[ignore]`d like the parser sweep (it type-checks a corpus sample and writes
//! files). Run under `nix develop` (which sets `BORZOI_CORPUS`):
//!
//! ```text
//! BORZOI_RESOLVE_DIVERGENCE_OUT=target/resolve-divergence \
//!   cargo test -p borzoi-sema --test all resolve_divergence:: -- --ignored --nocapture
//! ```
//!
//! Honours the same `BORZOI_RESOLVE_DIFF_STRIDE` (default 13) /
//! `BORZOI_RESOLVE_DIFF_LIMIT` sample controls as the gate, so the two see the
//! same files: this report's `gap_b1` count is exactly the gate's `tally.gaps`,
//! the denominator of its `MIN_B1_COVERAGE_PERMILLE` completeness ratchet.

use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use crate::common::{
    Bucket, FileCensus, LineIndex, classify, env_usize_or, invoke_fcs_dump_census,
    parse_census_jsonl,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// Where the report is written. Defaults to `resolve-divergence/` at the
/// **workspace root** — not the cwd, which is the crate directory when cargo runs
/// the test, so a bare relative default would litter `crates/sema/` with
/// untracked report files. Mirrors `fcs_divergence.rs`; the directory is
/// gitignored there. Override with `BORZOI_RESOLVE_DIVERGENCE_OUT`.
fn output_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BORZOI_RESOLVE_DIVERGENCE_OUT") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("resolve-divergence")
}

/// One adjudicated use, for a worklist line.
struct Site {
    /// `"B1"` / `"B2"` / `"B3"` / `"other"` — the machinery bucket.
    bucket: &'static str,
    /// The `classify` sub-tag (`"value:module-or-import"`, `"union-case"`, …).
    tag: &'static str,
    path: PathBuf,
    start: usize,
    end: usize,
    text: String,
    /// What we gave (divergence / alt-binder); empty for a gap.
    ours: String,
}

impl Site {
    fn line(&self) -> String {
        let loc = format!("{}:{}..{}", self.path.display(), self.start, self.end);
        if self.ours.is_empty() {
            format!("{}/{}\t{loc}\t{:?}", self.bucket, self.tag, self.text)
        } else {
            format!(
                "{}/{}\t{loc}\t{:?}\t{}",
                self.bucket, self.tag, self.text, self.ours
            )
        }
    }
}

#[derive(Default)]
struct Report {
    files_compared: usize,
    /// In-file, non-definition, non-zero-width uses adjudicated (the denominator).
    adjudicated: usize,
    /// FCS resolved these into a referenced assembly / FSharp.Core — no in-file
    /// declaration and no env in isolation, so not checkable here.
    out_of_file: usize,
    /// Exact matches, per machinery bucket. Counted, not listed.
    matches: BTreeMap<&'static str, usize>,
    gap_b1: Vec<Site>,
    gap_b2: Vec<Site>,
    gap_b3: Vec<Site>,
    gap_other: Vec<Site>,
    divergences: Vec<Site>,
    alt_binders: Vec<Site>,
    our_errors: Vec<PathBuf>,
    our_panics: Vec<PathBuf>,
    fcs_not_ok: Vec<PathBuf>,
    unreadable: Vec<PathBuf>,
}

fn bucket_name(b: Option<Bucket>) -> &'static str {
    match b {
        Some(Bucket::B1) => "B1",
        Some(Bucket::B2) => "B2",
        Some(Bucket::B3) => "B3",
        Some(Bucket::Other) | None => "other",
    }
}

#[test]
#[ignore = "categorised name-resolution divergence report (us + FCS type-check); run with --ignored under nix develop"]
fn regenerate_resolution_divergence_report() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!(
            "BORZOI_CORPUS unset; skipping resolution divergence report. Run under \
             `nix develop`, or point it at an F# checkout."
        );
        return;
    };
    let root = PathBuf::from(root);
    let out = output_dir();
    let stride = env_usize_or("BORZOI_RESOLVE_DIFF_STRIDE", 13).max(1);
    let limit = env_usize_or("BORZOI_RESOLVE_DIFF_LIMIT", usize::MAX);

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
        "resolve-divergence: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all_files.len()
    );

    let census: Vec<FileCensus> = parse_census_jsonl(&invoke_fcs_dump_census(&sample));

    let mut report = Report::default();
    {
        // Per-thread panic silence: our parser/resolver panics on a few unmodeled
        // constructs and we count those ourselves. Scoped to the sweep, not held
        // across the writes/asserts below.
        let _silence = silence_panics_here();
        for file in &census {
            sweep_file(file, &mut report);
        }
    }

    write_report(&out, &report);

    let total_gaps =
        report.gap_b1.len() + report.gap_b2.len() + report.gap_b3.len() + report.gap_other.len();
    let match_total: usize = report.matches.values().sum();
    eprintln!(
        "resolve-divergence: {} files | {} adjudicated | {} match | {} diverge | {} alt-binder | \
         {} gap (b1={}, b2={}, b3={}, other={}) | {} out-of-file | wrote report to {}",
        report.files_compared,
        report.adjudicated,
        match_total,
        report.divergences.len(),
        report.alt_binders.len(),
        total_gaps,
        report.gap_b1.len(),
        report.gap_b2.len(),
        report.gap_b3.len(),
        report.gap_other.len(),
        report.out_of_file,
        out.display(),
    );

    // A measurement, not a gate (the gate is `resolve_corpus_diff`): assert only
    // that the sweep was non-vacuous, so a broken oracle / empty corpus fails
    // loudly rather than silently writing empty worklists.
    assert!(
        report.files_compared > 0 && report.adjudicated > 0,
        "vacuous sweep: {} files compared, {} uses adjudicated — oracle or corpus problem",
        report.files_compared,
        report.adjudicated,
    );
}

/// Adjudicate one census file's in-file uses into `report`.
fn sweep_file(file: &FileCensus, report: &mut Report) {
    let path = PathBuf::from(&file.path);
    if !file.ok {
        report.fcs_not_ok.push(path);
        return;
    }
    let Ok(source) = std::fs::read_to_string(&path) else {
        report.unreadable.push(path);
        return;
    };

    let resolved = catch_unwind(AssertUnwindSafe(|| {
        let parsed = parse(&source);
        if !parsed.errors.is_empty() {
            return None; // "our errors" — signalled without panicking
        }
        let impl_file = ImplFile::cast(parsed.root)?;
        Some(resolve_file(
            &impl_file,
            &ProjectItems::default(),
            &AssemblyEnv::default(),
        ))
    }));
    let rf = match resolved {
        Ok(Some(rf)) => rf,
        Ok(None) => {
            report.our_errors.push(path);
            return;
        }
        Err(_) => {
            report.our_panics.push(path);
            return;
        }
    };

    report.files_compared += 1;
    let idx = LineIndex::new(&source);

    for u in &file.uses {
        if u.is_from_definition {
            continue;
        }
        let (bucket, tag) = classify(u);
        let (us, ue) = u.use_range_bytes(&idx);
        if us == ue {
            // The zero-width implicit anonymous-module symbol.
            continue;
        }
        let Some((ds, de)) = u.decl_range_bytes(&idx) else {
            // Declared in a referenced assembly (or nowhere in-file): out of the
            // isolation slice — see the module docs.
            report.out_of_file += 1;
            continue;
        };
        report.adjudicated += 1;

        let use_range = TextRange::new(
            u32::try_from(us).unwrap().into(),
            u32::try_from(ue).unwrap().into(),
        );
        let expected = TextRange::new(
            u32::try_from(ds).unwrap().into(),
            u32::try_from(de).unwrap().into(),
        );
        let text = source.get(us..ue).unwrap_or("").to_string();
        let bname = bucket_name(bucket);
        let site = |ours: String| Site {
            bucket: bname,
            tag,
            path: path.clone(),
            start: us,
            end: ue,
            text: text.clone(),
            ours,
        };

        match rf.resolution_at(use_range) {
            None | Some(Resolution::Deferred(_)) => {
                let s = site(String::new());
                match bucket {
                    Some(Bucket::B1) => report.gap_b1.push(s),
                    Some(Bucket::B2) => report.gap_b2.push(s),
                    Some(Bucket::B3) => report.gap_b3.push(s),
                    Some(Bucket::Other) | None => report.gap_other.push(s),
                }
            }
            Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => {
                match rf.resolved_def(res) {
                    Some(def) if def.range == expected => {
                        *report.matches.entry(bname).or_default() += 1;
                    }
                    Some(def) if def.name == text => report
                        .alt_binders
                        .push(site(format!("binder {:?} at {:?}", def.name, def.range))),
                    Some(def) => report
                        .divergences
                        .push(site(format!("binder {:?} at {:?}", def.name, def.range))),
                    None => report
                        .divergences
                        .push(site(format!("{res:?} (no in-file def)"))),
                }
            }
            // FCS found an in-file binder, but we resolved into an assembly or
            // called it unresolved: a soundness fault.
            Some(other) => report.divergences.push(site(format!("{other:?}"))),
        }
    }
}

/// Write every bucket file plus `summary.txt` under `out`.
fn write_report(out: &Path, r: &Report) {
    std::fs::create_dir_all(out).expect("create report dir");

    // Faults and gaps: `<bucket>/<tag>\t<loc>\t<text>[\t<ours>]`. Divergences and
    // alt-binders sort bucket-first (B1 faults to the top); gaps sort by sub-tag
    // so the dominant constructs group.
    write_sites(out, "divergence.txt", &r.divergences, SortKey::BucketTag);
    write_sites(out, "alt_binder.txt", &r.alt_binders, SortKey::BucketTag);
    write_sites(out, "gap_b1.txt", &r.gap_b1, SortKey::Tag);
    write_sites(out, "gap_b2.txt", &r.gap_b2, SortKey::Tag);
    write_sites(out, "gap_b3.txt", &r.gap_b3, SortKey::Tag);
    write_sites(out, "gap_other.txt", &r.gap_other, SortKey::Tag);

    write_paths(out, "our_parser_errors.txt", &r.our_errors);
    write_paths(out, "our_panics.txt", &r.our_panics);
    write_paths(out, "fcs_not_ok.txt", &r.fcs_not_ok);
    write_paths(out, "unreadable.txt", &r.unreadable);

    std::fs::write(out.join("summary.txt"), summary(r)).expect("write summary");
}

enum SortKey {
    /// Bucket first, then sub-tag, then location (faults: surface B1 first).
    BucketTag,
    /// Sub-tag first, then location (gaps: group dominant constructs).
    Tag,
}

fn write_sites(out: &Path, name: &str, sites: &[Site], key: SortKey) {
    let mut sorted: Vec<&Site> = sites.iter().collect();
    sorted.sort_by(|a, b| match key {
        SortKey::BucketTag => a
            .bucket
            .cmp(b.bucket)
            .then(a.tag.cmp(b.tag))
            .then(a.path.cmp(&b.path))
            .then(a.start.cmp(&b.start)),
        SortKey::Tag => a
            .tag
            .cmp(b.tag)
            .then(a.path.cmp(&b.path))
            .then(a.start.cmp(&b.start)),
    });
    let body: String = sorted.iter().map(|s| format!("{}\n", s.line())).collect();
    std::fs::write(out.join(name), body).unwrap_or_else(|e| panic!("write {name}: {e}"));
}

fn write_paths(out: &Path, name: &str, paths: &[PathBuf]) {
    let mut sorted: Vec<&PathBuf> = paths.iter().collect();
    sorted.sort();
    let body: String = sorted
        .iter()
        .map(|p| format!("{}\n", p.display()))
        .collect();
    std::fs::write(out.join(name), body).unwrap_or_else(|e| panic!("write {name}: {e}"));
}

/// The digest: per-bucket counts, the `gap_b1` sub-tag histogram (what to work
/// on next), and coverage ratios.
fn summary(r: &Report) -> String {
    let mut s = String::new();
    let match_total: usize = r.matches.values().sum();
    let gap_total = r.gap_b1.len() + r.gap_b2.len() + r.gap_b3.len() + r.gap_other.len();
    let _ = writeln!(s, "files compared:        {}", r.files_compared);
    let _ = writeln!(s, "uses adjudicated:      {}", r.adjudicated);
    let _ = writeln!(s, "out-of-file (skipped): {}", r.out_of_file);
    let _ = writeln!(s, "matches:               {match_total}");
    let _ = writeln!(s, "divergences (faults):  {}", r.divergences.len());
    let _ = writeln!(s, "alt-binders:           {}", r.alt_binders.len());
    let _ = writeln!(
        s,
        "gaps:                  {gap_total} (b1={}, b2={}, b3={}, other={})",
        r.gap_b1.len(),
        r.gap_b2.len(),
        r.gap_b3.len(),
        r.gap_other.len(),
    );
    // The completeness ratio the B1-gap ratchet pins: of the B1 in-file uses we
    // could either match or defer, how many did we bind?
    let b1_match = r.matches.get("B1").copied().unwrap_or(0);
    let b1_seen = b1_match + r.gap_b1.len();
    if b1_seen > 0 {
        let _ = writeln!(
            s,
            "B1 in-file coverage:   {b1_match}/{b1_seen} = {:.1}% (gap_b1 is the worklist)",
            100.0 * b1_match as f64 / b1_seen as f64,
        );
    }
    let _ = writeln!(s, "\nmatches by bucket:");
    for (bucket, n) in &r.matches {
        let _ = writeln!(s, "  {bucket}: {n}");
    }

    let _ = writeln!(s, "\ngap_b1 by sub-tag (the primary worklist, most first):");
    let mut hist: BTreeMap<&str, usize> = BTreeMap::new();
    for site in &r.gap_b1 {
        *hist.entry(site.tag).or_default() += 1;
    }
    let mut rows: Vec<(&&str, &usize)> = hist.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (tag, n) in rows {
        let _ = writeln!(s, "  {n:>6}  {tag}");
    }
    s
}

/// Recursively collect `.fs` implementation files (not `.fsi`), skipping
/// build/VCS output and symlinks. Mirrors `resolve_corpus_diff.rs`'s collector.
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
