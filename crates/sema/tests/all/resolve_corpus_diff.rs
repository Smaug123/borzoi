//! Differential name-resolution sweep: our resolver vs FCS over the whole
//! corpus — the resolution analogue of `cst`'s `parser_corpus_diff.rs`.
//!
//! [`resolve_diff.rs`](crate) asserts the *strict* Stage C property (every
//! in-file use must resolve to the right binder) over a curated, fully-modeled
//! corpus. This sweep relaxes that to a *ratchet* so it can run over real,
//! partly-unmodeled F#: for every symbol use FCS resolves whose declaration is
//! in the same file and which is **lexical** (bucket B1 — no type inference), we
//! compare our resolution and bucket the outcome into four classes:
//!
//! * **match** — our `resolution_at(range)` is a `Local`/`Item` pointing at a
//!   binder whose range *equals* FCS's declaration range. The headline coverage,
//!   floored by [`MIN_RESOLUTION_MATCHES`] (only goes up).
//! * **divergence** (the gated fault) — we return `Unresolved`, resolve into an
//!   assembly `Entity`/`Member`, or point at a binder whose *name* differs from
//!   the use, all where FCS found an in-file binder. These are unambiguous
//!   soundness faults (D5: never `Unresolved`/out-of-file where resolvable).
//!   Ceilinged by [`MAX_RESOLUTION_DIVERGENCES`] (drive to zero); sites printed.
//! * **alt-binder** — we point at a *same-named* in-file binder at a *different*
//!   range than FCS. Over the wild corpus this is dominated not by bugs but by
//!   (a) OR-patterns, where FCS canonicalises a use to the *first* alternative's
//!   binder while we use the lexically-active one, and (b) isolation-bias
//!   recovery: checked alone, a pattern like `SynPat.Paren(p, _)` on an
//!   unresolved sibling type makes FCS *not* bind the inner `p` (so a body use
//!   falls back to an enclosing same-named binder), while our purely-lexical
//!   resolver binds it. So exact-range matching is too strict here; this class
//!   is reported and loosely ceilinged ([`MAX_ALT_BINDERS`]) to catch an
//!   explosion, not gated to zero. (Strict shadowing correctness is covered
//!   FCS-free by `resolve_scoping.rs` and exactly by `resolve_diff.rs`.)
//! * **gap** — we honestly return `Deferred`, or recorded nothing at that range
//!   (a construct we don't model yet, or a long-ident whose occurrence range we
//!   key differently). Expected and uninteresting; counted, not gated.
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
//! Tune the sample with `BORZOI_RESOLVE_DIFF_STRIDE` (default 13 — every
//! 13th `.fs` file) and `BORZOI_RESOLVE_DIFF_LIMIT`. The ratchet baselines
//! below are tied to the **default stride**; re-measure if you change it.

use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use crate::common::{
    Bucket, FileCensus, census_resolve_uses, env_usize_or, invoke_fcs_dump_census,
    parse_census_jsonl,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// Lower bound on in-file B1 uses where our resolution matches FCS exactly. Only
/// goes up — bump it after a phase lands. A drop is a regression. Tied to the
/// default stride; re-measure with `--ignored` if you change `*_STRIDE`.
///
/// Conservative: 12016 was measured 2026-06-29 (348 files compared, stride 13);
/// the floor sits a little under it to absorb the rare FCS isolation-check
/// flake, and parser improvements (which move files out of `our_errors` into the
/// compared set) only raise the true count, so re-tighten after they land.
const MIN_RESOLUTION_MATCHES: usize = 11_800;

/// Upper bound on unambiguous resolution faults (`Unresolved` / assembly entity
/// / wrong-named binder where FCS found an in-file binder). Gated to zero — each
/// is a soundness violation (D5). The sites are printed for triage.
const MAX_RESOLUTION_DIVERGENCES: usize = 0;

/// Upper bound on alt-binder disagreements (same-named in-file binder, different
/// range — OR-pattern canonicalisation / isolation-bias recovery, not bugs).
/// Loosely ceilinged to catch an explosion, not to drive to zero: 147 measured
/// 2026-06-29, with headroom for parser improvements surfacing more OR-patterns.
const MAX_ALT_BINDERS: usize = 220;

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
}

#[derive(Default)]
struct Tally {
    /// Files FCS type-checked Ok and we parsed cleanly (the comparable set).
    files_compared: usize,
    matches: usize,
    /// Unambiguous faults (gated to zero): `Unresolved`, assembly entity, or a
    /// wrong-*named* binder where FCS found an in-file binder.
    divergences: Vec<Site>,
    /// Same-named in-file binder at a different range (OR-pattern / isolation
    /// recovery): reported, loosely ceilinged.
    alt_binders: Vec<Site>,
    /// In-file B1 uses we left `Deferred` or recorded nothing at — modeling
    /// gaps, not bugs.
    gaps: usize,
    /// FCS reported the file as not Ok (a type error in isolation): skipped.
    fcs_not_ok: usize,
    /// Our parse produced errors, so its resolution isn't meaningful to diff.
    our_errors: usize,
    /// Our parse or resolve panicked (a construct not modeled yet).
    our_skipped: usize,
    unreadable: usize,
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
        "resolve-diff: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all_files.len()
    );

    let census: Vec<FileCensus> = parse_census_jsonl(&invoke_fcs_dump_census(&sample));

    let mut tally = Tally::default();
    {
        // Silence the per-panic backtraces from our parser/resolver on unmodeled
        // constructs (we count outcomes ourselves) — per-thread, so a concurrent
        // test's genuine panic still prints (see `panic_silence`).
        //
        // Scoped to the loop, and *not* held across the ratchet assertions below:
        // those are the point of the test, and a failing one must keep its payload
        // and backtrace. (The hook this replaced was restored before them too.)
        let _silence = silence_panics_here();

        for file in &census {
            compare_file(file, &mut tally);
        }
    }

    eprintln!(
        "resolve-diff: {} files compared | {} match | {} diverge | {} alt-binder | \
         {} gaps | {} fcs-not-ok | {} our-errors | {} our-skipped | {} unreadable",
        tally.files_compared,
        tally.matches,
        tally.divergences.len(),
        tally.alt_binders.len(),
        tally.gaps,
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

    // `<=` keeps this a ratchet ceiling that stays correct if the const is ever
    // raised; the lint only fires because the ceiling is currently zero.
    #[allow(clippy::absurd_extreme_comparisons)]
    {
        assert!(
            tally.divergences.len() <= MAX_RESOLUTION_DIVERGENCES,
            "{} in-file B1 uses are unambiguous faults (ceiling is \
             MAX_RESOLUTION_DIVERGENCES = {}). A resolver bug or soundness \
             violation regressed in.",
            tally.divergences.len(),
            MAX_RESOLUTION_DIVERGENCES,
        );
    }
    assert!(
        tally.alt_binders.len() <= MAX_ALT_BINDERS,
        "{} alt-binder disagreements (ceiling is MAX_ALT_BINDERS = {}). A \
         shadowing/OR-pattern change regressed in — inspect the printed sites.",
        tally.alt_binders.len(),
        MAX_ALT_BINDERS,
    );
    assert!(
        tally.matches >= MIN_RESOLUTION_MATCHES,
        "only {} in-file B1 uses match FCS exactly (floor is \
         MIN_RESOLUTION_MATCHES = {}). Resolution matches regressed.",
        tally.matches,
        MIN_RESOLUTION_MATCHES,
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
            "  {}:{:?} {:?} -> FCS decl {:?}, we gave {}",
            s.path.display(),
            s.range,
            s.text,
            s.expected,
            s.ours,
        );
    }
}

/// Compare one census file's in-file B1 uses against our resolution, folding the
/// outcome into `tally`.
fn compare_file(file: &FileCensus, tally: &mut Tally) {
    if !file.ok {
        tally.fcs_not_ok += 1;
        return;
    }
    let path = PathBuf::from(&file.path);
    let Ok(source) = std::fs::read_to_string(&path) else {
        tally.unreadable += 1;
        return;
    };

    // Our parser/resolver panics on a few unmodeled constructs; catch so one
    // file can't abort the sweep. Parse and resolve together — both can panic.
    let resolved = catch_unwind(AssertUnwindSafe(|| {
        let parsed = parse(&source);
        if !parsed.errors.is_empty() {
            return None; // signal "our errors" without panicking
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
            tally.our_errors += 1;
            return;
        }
        Err(_) => {
            tally.our_skipped += 1;
            return;
        }
    };

    tally.files_compared += 1;

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
        let site = |ours: String| Site {
            path: path.clone(),
            range: use_range,
            text: text.to_string(),
            expected,
            ours,
        };

        match rf.resolution_at(use_range) {
            // We recorded nothing here, or honestly deferred: a modeling gap,
            // not a disagreement (e.g. named-module headers we don't intern, or
            // a long-ident occurrence we key by a different range).
            None | Some(Resolution::Deferred(_)) => tally.gaps += 1,
            Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => {
                match rf.resolved_def(res) {
                    // Exact match — the headline coverage.
                    Some(def) if def.range == expected => tally.matches += 1,
                    // Same-named in-file binder, different range: OR-pattern
                    // canonicalisation or isolation-bias recovery, not a fault.
                    Some(def) if def.name == text => tally
                        .alt_binders
                        .push(site(format!("binder {:?} at {:?}", def.name, def.range))),
                    // A *differently-named* binder — we resolved to the wrong
                    // symbol entirely.
                    Some(def) => tally
                        .divergences
                        .push(site(format!("binder {:?} at {:?}", def.name, def.range))),
                    None => tally
                        .divergences
                        .push(site(format!("{res:?} (no in-file def)"))),
                }
            }
            // FCS resolved an in-file binder, but we point into an assembly or
            // claim the name is unresolved — an unambiguous soundness fault.
            Some(other) => tally.divergences.push(site(format!("{other:?}"))),
        }
    }
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
