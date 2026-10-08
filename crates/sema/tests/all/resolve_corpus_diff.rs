//! Differential name-resolution sweep: our resolver vs FCS over the whole
//! corpus — the resolution analogue of `cst`'s `parser_corpus_diff.rs`.
//!
//! [`resolve_diff.rs`](crate) asserts the *strict* Stage C property (every
//! in-file use must resolve to the right binder) over a curated, fully-modeled
//! corpus. This sweep runs over real, partly-unmodeled F#: for every symbol use
//! FCS resolves whose declaration is in the same file and which is **lexical**
//! (bucket B1 — no type inference), we compare our resolution and bucket the
//! outcome into four classes:
//!
//! * **match** — our `resolution_at(range)` is a `Local`/`Item` pointing at a
//!   binder whose range *equals* FCS's declaration range. The headline coverage.
//! * **divergence** — we return `Unresolved`, resolve into an assembly
//!   `Entity`/`Member`, or point at a binder whose *name* differs from the use,
//!   all where FCS found an in-file binder. These are unambiguous soundness
//!   faults (D5: never `Unresolved`/out-of-file where resolvable), gated to zero
//!   by an assertion of their own; sites printed.
//! * **alt-binder** — we point at a *same-named* in-file binder at a *different*
//!   range than FCS. In a file FCS's isolated check reports errors for, this is
//!   dominated by isolation-bias recovery: checked alone, a pattern like
//!   `SynPat.Paren(p, _)` on an unresolved sibling type makes FCS *not* bind the
//!   inner `p` (so a body use falls back to an enclosing same-named binder),
//!   while our purely-lexical resolver binds it. In a file FCS checks *cleanly*
//!   there is no recovery to blame, so an alt-binder there is a wrong answer;
//!   each entry says which kind of file it is in (`fcs-clean` /
//!   `fcs-check-errors`). (Strict shadowing correctness is covered FCS-free by
//!   `resolve_scoping.rs` and exactly by `resolve_diff.rs`.)
//! * **gap** — we honestly return `Deferred`, or recorded nothing at that range
//!   (a construct we don't model yet, or a long-ident whose occurrence range we
//!   key differently). The categorised worklist behind them — what these gaps
//!   are, by construct — is the sibling report generator `resolve_divergence.rs`
//!   (its `gap_b1.txt`).
//!
//! # The manifest
//!
//! The corpus is pinned by the flake and both sides are deterministic, so every
//! outcome is a fixed fact, and the sweep checks it **exactly** against
//! `tests/manifests/resolve_corpus_diff.txt` rather than through count bounds.
//! It holds one line per sampled file (whether it was compared or skipped, and
//! why; for a compared file, whether FCS's check errored and its match count)
//! and one line per non-match use (gap, alt-binder, divergence), keyed by
//! corpus-relative path and `line:col`. Matches are counted per file rather than
//! listed — there are ~22k of them — but since every gap, alt-binder and
//! divergence is listed by key, a use moving between match and any other bucket
//! moves a listed line, so the per-file count loses no movement.
//!
//! Any difference fails with a line diff. A movement in either direction is
//! signal: a lost match is a regression, a gained one an improvement, and both
//! are acknowledged by regenerating the manifest and committing the diff:
//!
//! ```text
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all resolve_corpus_diff:: -- --ignored
//! ```
//!
//! Regeneration cannot bless a divergence: that assertion runs first.
//!
//! Only the **B1 lexical** slice is checked: B2/B3 uses (`x.Length`, overloaded
//! members) need inference we do not do, so they are skipped — not divergences.
//! FCS type-checks each file *in isolation* (`uses-census-batch`). Our resolver
//! runs single-file with empty `ProjectItems` / `AssemblyEnv`, exactly as
//! `resolve_diff.rs` does.
//!
//! `#[ignore]`d like the parser sweep: it type-checks a corpus sample (slow).
//! Run under `nix develop` (which sets `BORZOI_CORPUS`):
//!
//! ```text
//! cargo test -p borzoi-sema --test all resolve_corpus_diff:: -- --ignored --nocapture
//! ```
//!
//! Tune the sample with `BORZOI_RESOLVE_DIFF_STRIDE` (default 13 — every 13th
//! `.fs` file) and `BORZOI_RESOLVE_DIFF_LIMIT`. The manifest describes the
//! default sample, so a run with either set checks only the divergence gate.

use borzoi_oracle_harness::manifest::Manifest;
use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use crate::common::corpus_manifest::{
    Positions, check_manifest, corpus_relative, regenerate_ignored,
};
use crate::common::{
    Bucket, FileCensus, census_resolve_uses, env_usize_or, invoke_fcs_dump_census,
    parse_census_jsonl,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use rowan::TextRange;

/// The sample the checked-in manifest describes: every `DEFAULT_STRIDE`th file.
const DEFAULT_STRIDE: usize = 13;

/// How many sites of each kind to print for investigation.
const SAMPLE: usize = 40;

/// One disagreement between our resolution and FCS, for the printed sample.
struct Site {
    path: PathBuf,
    range: TextRange,
    text: String,
    /// FCS's declaration range for this use (the binder we *should* point at).
    expected: TextRange,
    /// What we said (FCS resolved an in-file binder here).
    ours: String,
    /// Whether FCS's isolated check of the file reported an error.
    fcs_check_errors: bool,
}

#[derive(Default)]
struct Tally {
    /// Files FCS type-checked Ok and we parsed cleanly (the comparable set).
    files_compared: usize,
    matches: usize,
    /// Unambiguous faults (gated to zero): `Unresolved`, assembly entity, or a
    /// wrong-*named* binder where FCS found an in-file binder.
    divergences: Vec<Site>,
    /// Same-named in-file binder at a different range.
    alt_binders: Vec<Site>,
    /// In-file B1 uses we left `Deferred` or recorded nothing at — modeling
    /// gaps, not bugs.
    gaps: usize,
    /// FCS reported the file as not Ok (its check aborted or threw): skipped.
    fcs_not_ok: usize,
    /// Our parse produced errors, so its resolution isn't meaningful to diff.
    our_errors: usize,
    /// Our parse or resolve panicked (a construct not modeled yet).
    our_skipped: usize,
    unreadable: usize,
    /// One entry per sampled file and per non-match use (see the module docs).
    manifest: Vec<String>,
}

#[test]
#[ignore = "full-corpus differential resolution (us + FCS type-check); run with --ignored under nix develop"]
fn resolution_matches_fcs_over_corpus() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!(
            "BORZOI_CORPUS unset; skipping resolution sweep. Run under \
             `nix develop`, or point it at an F# checkout."
        );
        return;
    };
    let root = PathBuf::from(root);
    let stride = env_usize_or("BORZOI_RESOLVE_DIFF_STRIDE", DEFAULT_STRIDE).max(1);
    let limit = env_usize_or("BORZOI_RESOLVE_DIFF_LIMIT", usize::MAX);

    let mut all_files = Vec::new();
    collect_fs(&root, &mut all_files);
    crate::common::corpus_manifest::sort_by_corpus_key(&root, &mut all_files);
    let sample: Vec<PathBuf> = all_files
        .iter()
        .step_by(stride)
        .take(limit)
        .cloned()
        .collect();
    assert!(!sample.is_empty(), "no .fs files under {root:?}");
    eprintln!(
        "resolve-diff: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all_files.len()
    );

    let census: Vec<FileCensus> = parse_census_jsonl(&invoke_fcs_dump_census(&sample));
    assert_eq!(
        census.len(),
        sample.len(),
        "one census line per sampled file"
    );

    let mut tally = Tally::default();
    {
        // Silence the per-panic backtraces from our parser/resolver on unmodeled
        // constructs (we count outcomes ourselves) — per-thread, so a concurrent
        // test's genuine panic still prints (see `panic_silence`).
        //
        // Scoped to the loop, and *not* held across the assertions below: those
        // are the point of the test, and a failing one must keep its payload and
        // backtrace.
        let _silence = silence_panics_here();

        for file in &census {
            compare_file(&root, file, &mut tally);
        }
    }

    let b1_seen = tally.matches + tally.gaps;
    eprintln!(
        "resolve-diff: {} files compared | {} match | {} diverge | {} alt-binder \
         ({} in files FCS checked cleanly) | {} gaps | in-file B1 coverage {}‰ | \
         {} fcs-not-ok | {} our-errors | {} our-skipped | {} unreadable",
        tally.files_compared,
        tally.matches,
        tally.divergences.len(),
        tally.alt_binders.len(),
        tally
            .alt_binders
            .iter()
            .filter(|s| !s.fcs_check_errors)
            .count(),
        tally.gaps,
        (tally.matches * 1000).checked_div(b1_seen).unwrap_or(0),
        tally.fcs_not_ok,
        tally.our_errors,
        tally.our_skipped,
        tally.unreadable,
    );

    print_sites("divergences (gated faults)", &tally.divergences);
    print_sites(
        "alt-binders (same name, different range)",
        &tally.alt_binders,
    );

    // The soundness gate: wrong answers, which no manifest regeneration may
    // bless.
    assert!(
        tally.divergences.is_empty(),
        "{} in-file B1 uses are unambiguous faults (`Unresolved`, an assembly \
         entity, or a differently-named binder where FCS found an in-file \
         binder). A resolver bug or soundness violation regressed in.",
        tally.divergences.len(),
    );

    if stride != DEFAULT_STRIDE || limit != usize::MAX {
        eprintln!(
            "resolve-diff: NOT comparing the manifest — it describes the default \
             sample (stride {DEFAULT_STRIDE}, no limit), and this run sampled \
             stride {stride}, limit {limit}."
        );
        return;
    }
    let manifest =
        Manifest::from_counted(tally.manifest).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "resolve_corpus_diff",
        &manifest,
        &regenerate_ignored("resolve_corpus_diff"),
    );
}

/// Print up to [`SAMPLE`] sites of one kind for triage.
fn print_sites(label: &str, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    eprintln!(
        "\nresolution {label} ({}, showing up to {SAMPLE}):",
        sites.len()
    );
    for s in sites.iter().take(SAMPLE) {
        eprintln!(
            "  {}:{:?} {:?} -> FCS decl {:?}, we gave {}{}",
            s.path.display(),
            s.range,
            s.text,
            s.expected,
            s.ours,
            if s.fcs_check_errors {
                " [FCS check errored]"
            } else {
                ""
            },
        );
    }
}

/// Compare one census file's in-file B1 uses against our resolution, folding the
/// outcome into `tally`.
fn compare_file(root: &Path, file: &FileCensus, tally: &mut Tally) {
    let path = PathBuf::from(&file.path);
    let rel = corpus_relative(root, &path);
    if !file.ok {
        tally.fcs_not_ok += 1;
        tally.manifest.push(format!("{rel} fcs-not-ok"));
        return;
    }
    let Ok(source) = std::fs::read_to_string(&path) else {
        tally.unreadable += 1;
        tally.manifest.push(format!("{rel} unreadable"));
        return;
    };

    // Our parser/resolver panics on a few unmodeled constructs; catch so one
    // file can't abort the sweep. Parse and resolve together — both can panic.
    let resolved = catch_unwind(AssertUnwindSafe(|| {
        let parsed = parse(&source);
        if !parsed.errors.is_empty() {
            return None; // signal "our errors" without panicking
        }
        let recovery = SyntaxRecovery::of(&parsed);
        let impl_file = ImplFile::cast(parsed.root)?;
        Some(resolve_file(
            &impl_file,
            &ProjectItems::default(),
            &AssemblyEnv::default(),
            &recovery,
        ))
    }));
    let rf = match resolved {
        Ok(Some(rf)) => rf,
        Ok(None) => {
            tally.our_errors += 1;
            tally.manifest.push(format!("{rel} our-parse-errors"));
            return;
        }
        Err(_) => {
            tally.our_skipped += 1;
            tally.manifest.push(format!("{rel} our-panic"));
            return;
        }
    };

    tally.files_compared += 1;
    let positions = Positions::new(&source);
    let check = if file.has_check_errors {
        "fcs-check-errors"
    } else {
        "fcs-clean"
    };
    let mut file_matches = 0usize;

    for u in census_resolve_uses(file, &source) {
        // A definition is not a name to resolve; the implicit anonymous-module
        // symbol is reported at a zero-width range; only in-file declarations
        // are in this slice; only the lexical (B1) bucket is reproducible
        // without inference.
        if u.is_from_definition || u.start == u.end || u.bucket != Some(Bucket::B1) {
            continue;
        }
        let Some((ds, de)) = u.decl else {
            continue;
        };
        let use_range = TextRange::new(
            u32::try_from(u.start).unwrap().into(),
            u32::try_from(u.end).unwrap().into(),
        );
        let expected = TextRange::new(
            u32::try_from(ds).unwrap().into(),
            u32::try_from(de).unwrap().into(),
        );

        let text = source.get(u.start..u.end).unwrap_or("");
        let key = format!("{rel}:{} {text:?}", positions.at(u.start));
        let site = |ours: String| Site {
            path: path.clone(),
            range: use_range,
            text: text.to_string(),
            expected,
            ours,
            fcs_check_errors: file.has_check_errors,
        };
        let at = |r: TextRange| positions.at(usize::from(r.start()));

        match rf.resolution_at(use_range) {
            // We recorded nothing here, or honestly deferred: a modeling gap,
            // not a disagreement (e.g. named-module headers we don't intern, or
            // a long-ident occurrence we key by a different range).
            None | Some(Resolution::Deferred(_)) => {
                tally.gaps += 1;
                tally.manifest.push(format!("{key} gap"));
            }
            Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => {
                match rf.resolved_def(res) {
                    // Exact match — the headline coverage, counted per file.
                    Some(def) if def.range == expected => {
                        tally.matches += 1;
                        file_matches += 1;
                    }
                    // Same-named in-file binder, different range.
                    Some(def) if def.name == text => {
                        tally.manifest.push(format!(
                            "{key} alt-binder fcs={} ours={} {check}",
                            at(expected),
                            at(def.range)
                        ));
                        tally
                            .alt_binders
                            .push(site(format!("binder {:?} at {:?}", def.name, def.range)));
                    }
                    // A *differently-named* binder — we resolved to the wrong
                    // symbol entirely.
                    Some(def) => {
                        tally.manifest.push(format!(
                            "{key} divergence binder={:?} at {}",
                            def.name,
                            at(def.range)
                        ));
                        tally
                            .divergences
                            .push(site(format!("binder {:?} at {:?}", def.name, def.range)));
                    }
                    None => {
                        tally
                            .manifest
                            .push(format!("{key} divergence no-in-file-def"));
                        tally
                            .divergences
                            .push(site(format!("{res:?} (no in-file def)")));
                    }
                }
            }
            // FCS resolved an in-file binder, but we point into an assembly or
            // claim the name is unresolved — an unambiguous soundness fault.
            Some(other) => {
                tally.manifest.push(format!("{key} divergence out-of-file"));
                tally.divergences.push(site(format!("{other:?}")));
            }
        }
    }
    tally
        .manifest
        .push(format!("{rel} compared {check} match={file_matches}"));
}

/// Recursively collect `.fs` implementation files (not `.fsi`), skipping
/// build/VCS output and symlinks. Mirrors `uses_census.rs`'s collector.
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
