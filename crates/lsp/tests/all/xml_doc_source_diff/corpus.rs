//! The source-doc differential over the pinned F# corpus: every Nth `.fs`
//! file, checked in isolation by FCS (`xmldoc-batch`, one path per request)
//! and by us (a one-file project, folded as the LSP folds it), every occurrence
//! our resolver records graded as in the generated cases.
//!
//! **The soundness gate** is zero divergences: wherever hover would show a doc,
//! FCS attaches exactly those lines (and elaborates them exactly as we do).
//! Declines make no claim; they are the census.
//!
//! **The manifest** (`tests/manifests/xml_doc_source_corpus.txt`) pins the
//! outcome exactly — the corpus is content-addressed and both sides are
//! deterministic: one line per sampled file (compared or skipped, and why;
//! whether FCS's check errored; its agreement counts, and how many "no doc"
//! answers FCS reported no use to pair with) and one line per other graded
//! occurrence (each decline with its cause and whether FCS had a doc there,
//! each unpaired doc, each oracle-ambiguous site). Agreements are counted per
//! file rather than listed; a site moving between agreement and anything else
//! moves a listed line. Regenerate with
//!
//! ```text
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi --test all xml_doc_source_diff::corpus:: -- --ignored
//! ```
//!
//! Regeneration cannot bless a divergence: that assertion runs first.
//! `BORZOI_XMLDOC_CORPUS_STRIDE` (default [`DEFAULT_STRIDE`]) and
//! `BORZOI_XMLDOC_CORPUS_LIMIT` tune the sample; the manifest describes the
//! default, so a run with either set checks only the gate.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use borzoi_oracle_harness::corpus_key::{Positions, corpus_relative, sort_by_corpus_key};
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};
use borzoi_oracle_harness::panic_silence::silence_panics_here;

use super::harness::{Graded, Verdict, census, check_paths, failures};

/// The sample the checked-in manifest describes: every `DEFAULT_STRIDE`th file.
const DEFAULT_STRIDE: usize = 13;

fn env_usize_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

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

/// The manifest spelling of a non-agreement verdict.
fn verdict_entry(g: &Graded) -> Option<String> {
    let fcs = match g.fcs_lines.as_deref() {
        Some(lines) if !lines.is_empty() => "fcs-doc",
        Some(_) => "fcs-none",
        None => "fcs-unpaired",
    };
    match &g.verdict {
        Verdict::Agree { .. } => None,
        Verdict::Declined(why) => Some(format!("declined {why:?} {fcs}")),
        // An unpaired "no doc" claims nothing hover would show; it is counted
        // per file. An unpaired doc is a commitment the oracle cannot check.
        Verdict::Unpaired => match g.ours.as_deref() {
            Some(lines) if !lines.is_empty() => Some("unpaired-doc".to_string()),
            _ => None,
        },
        Verdict::OracleAmbiguous => Some("oracle-ambiguous".to_string()),
        Verdict::Diverge { .. } | Verdict::ElaborationDiverges { .. } => {
            Some("DIVERGE".to_string())
        }
    }
}

#[test]
#[ignore = "corpus-wide source-doc differential (FCS type-checks each sampled file); run with --ignored under nix develop"]
fn source_docs_match_fcs_over_corpus() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping. Run under `nix develop`.");
        return;
    };
    let root = PathBuf::from(root);
    let stride = env_usize_or("BORZOI_XMLDOC_CORPUS_STRIDE", DEFAULT_STRIDE).max(1);
    let limit = env_usize_or("BORZOI_XMLDOC_CORPUS_LIMIT", usize::MAX);
    let mut all = Vec::new();
    collect_fs(&root, &mut all);
    sort_by_corpus_key(&root, &mut all);
    let sample: Vec<PathBuf> = all.iter().step_by(stride).take(limit).cloned().collect();
    assert!(!sample.is_empty(), "no .fs files under {}", root.display());
    eprintln!(
        "xmldoc-corpus: {} of {} .fs files (stride {stride})",
        sample.len(),
        all.len()
    );

    let mut manifest = Vec::new();
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    let mut divergences = Vec::new();
    let mut compared = 0usize;
    {
        let _silence = silence_panics_here();
        for path in &sample {
            let key = corpus_relative(&root, path);
            let Ok(text) = std::fs::read_to_string(path) else {
                manifest.push(format!("{key} skipped unreadable"));
                continue;
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                check_paths(std::slice::from_ref(path), vec![text.clone()], &[])
            }));
            let (graded, fcs) = match outcome {
                Ok(r) => r,
                Err(_) => {
                    manifest.push(format!("{key} skipped panicked"));
                    continue;
                }
            };
            let oracle = &fcs[0];
            if !oracle.ok {
                manifest.push(format!("{key} skipped fcs-not-ok"));
                continue;
            }
            compared += 1;
            let file_census = census(&graded);
            for (k, v) in &file_census {
                *totals.entry(k.clone()).or_default() += v;
            }
            let positions = Positions::new(&text);
            for g in &graded {
                if let Some(entry) = verdict_entry(g) {
                    manifest.push(format!(
                        "{key}:{} {} {:?} {} {entry}",
                        positions.at(usize::from(g.range.start())),
                        g.name.replace(char::is_whitespace, "_"),
                        g.def_kind,
                        if g.is_definition { "def" } else { "use" },
                    ));
                }
            }
            for g in failures(&graded) {
                divergences.push(format!(
                    "{key}:{} `{}` {:?}",
                    positions.at(usize::from(g.range.start())),
                    g.name,
                    g.verdict
                ));
            }
            let agree_doc = graded
                .iter()
                .filter(|g| matches!(g.verdict, Verdict::Agree { attached: true }))
                .count();
            let agree_none = graded
                .iter()
                .filter(|g| matches!(g.verdict, Verdict::Agree { attached: false }))
                .count();
            let unpaired_none = graded
                .iter()
                .filter(|g| matches!(g.verdict, Verdict::Unpaired) && verdict_entry(g).is_none())
                .count();
            manifest.push(format!(
                "{key} compared {} agree-doc={agree_doc} agree-none={agree_none} \
                 unpaired-none={unpaired_none}",
                if oracle.has_errors() {
                    "fcs-check-errors"
                } else {
                    "fcs-clean"
                }
            ));
        }
    }
    eprintln!("xmldoc-corpus: {compared} files compared; census {totals:#?}");
    assert!(
        divergences.is_empty(),
        "{} occurrence(s) where hover would show a doc FCS does not attach:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    if stride != DEFAULT_STRIDE || limit != usize::MAX {
        eprintln!("xmldoc-corpus: not comparing the manifest (non-default sample)");
        return;
    }
    let manifest = Manifest::from_counted(manifest).unwrap_or_else(|e| panic!("manifest: {e}"));
    check(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/manifests/xml_doc_source_corpus.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi --test all \
             xml_doc_source_diff::corpus:: -- --ignored"
        ),
    );
}
