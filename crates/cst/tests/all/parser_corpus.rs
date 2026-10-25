//! Run the recursive-descent parser over a real F# source tree and report.
//!
//! Walks the corpus rooted at `BORZOI_CORPUS` (the `fsharp-src` flake
//! input under `nix develop`) and runs [`parse`] / [`parse_sig`] on every
//! `.fs` / `.fsx` / `.fsi` file. Two tiers of check:
//!
//! * **Universal hard invariant — lossless round-trip.** Whatever the parser
//!   does (success, recovery, or errors), the green tree must reproduce the
//!   source byte-for-byte (`root.text() == src`). A failure here is a real
//!   byte-dropping bug, never a "feature not implemented yet" gap, so it is a
//!   hard assertion for every file the parser returns from.
//! * **The manifest — every file's outcome, exactly.** The parser is
//!   intentionally incomplete, so some real files still produce parse errors.
//!   The corpus is content-addressed (pinned in `flake.nix`) and the parser is
//!   deterministic, so which files those are is a fixed fact, and the sweep
//!   checks it exactly against `tests/manifests/parser_corpus.txt`: one line per
//!   file that parses with errors or is skipped as non-UTF-8, and one summary
//!   line counting the clean parses. Every non-clean file is listed by path, so
//!   a file moving into or out of the clean set moves a listed line as well as
//!   the count.
//!
//!   The comparison is **two-sided**: improving the parser fails it just as a
//!   regression does. Parse one more file cleanly and it goes red until the
//!   manifest is regenerated and the diff committed, which is what keeps the
//!   record honest between measurements:
//!
//!   ```text
//!   BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-cst --test all parser_corpus:: -- --ignored
//!   ```
//!
//!   The alternative — a one-sided floor with slack nobody re-measures — is what
//!   let the clean-parse count sit 2,401 files below the truth while the sweep
//!   ran nowhere. Regeneration cannot bless a panic or a round-trip failure:
//!   those assertions run first.
//!
//! Symbols are empty (plain [`parse`]), matching the lexer corpus test: every
//! `#if <ident>` is false, so the `#else` / post-`#endif` branch is active.
//! Per-file SCFLAGS-aware symbol sets are future work (same caveat as the
//! lexer sweep).
//!
//! `#[ignore]`d by default: parsing all ~6.4k files takes a couple of minutes,
//! too slow for every `cargo test` run. Like the LSP sweep
//! (`crates/lsp/tests/all/parser_corpus_sweep.rs`), run it on demand with
//! `cargo test -p borzoi-cst --test all parser_corpus:: -- --ignored`
//! (under `nix develop`, which sets `BORZOI_CORPUS`).

use std::path::{Path, PathBuf};

use borzoi_cst::parser::{Parse, parse, parse_sig};
use borzoi_oracle_harness::manifest::Manifest;

use crate::common::corpus_manifest::{
    check_manifest, corpus_relative, regenerate_ignored, sort_by_corpus_key, summary_entry,
};
use crate::common::{
    catch_unwind_silent, collect_fsharp_corpus_files, corpus_root, read_corpus_source,
};

// Why a manifest and not a bound on the clean-parse count: a one-sided floor
// on it, measured as 3227 / 6367 in June 2026, was still 3227 on 2026-07-31
// when the truth was 5628 / 6344 — 2,401 files of slack, enough to un-parse a
// third of the corpus without failing, because nothing ran this sweep. Even an
// exact count cannot see one file gained while another is lost; the manifest
// pins the set of files, not only its size.
//
// The raw parser panicking on a corpus file used to be ratcheted (`MAX_PANICS`,
// 7 when it was last measured in June 2026). It reaches zero on the pinned
// corpus, so the ceiling became an invariant and is asserted as one below.
//
// The LSP wraps the parser in `catch_unwind` so a panic never kills the server
// (see `crates/lsp/tests/all/parser_corpus_sweep.rs`, which asserts exactly
// that); a panic here is still a latent bug worth failing on.

#[derive(Default)]
struct Tally {
    total: usize,
    panics: Vec<PathBuf>,
    roundtrip_failures: Vec<PathBuf>,
    files_with_errors: Vec<PathBuf>,
    clean: usize,
    non_utf8: Vec<PathBuf>,
}

fn run_parse(path: &Path, src: &str) -> Parse {
    let is_sig = path.extension().and_then(|s| s.to_str()) == Some("fsi");
    if is_sig { parse_sig(src) } else { parse(src) }
}

#[test]
#[ignore = "full-corpus parse (~2 min); run with --ignored under nix develop"]
fn parse_fsharp_corpus() {
    let root = corpus_root();

    let mut files = collect_fsharp_corpus_files(&root)
        .unwrap_or_else(|err| panic!("walk F# corpus under {}: {err}", root.display()));
    assert!(!files.is_empty(), "no .fs/.fsi/.fsx files under {root:?}");
    // Host-independent order, and a loud failure if two files share a key.
    sort_by_corpus_key(&root, &mut files);

    eprintln!("parsing {} files under {}", files.len(), root.display());

    let mut tally = Tally::default();

    for path in &files {
        let src = match read_corpus_source(path) {
            Ok(src) => src,
            Err(err) if err.is_non_utf8() => {
                tally.non_utf8.push(path.clone());
                continue;
            }
            Err(err) => panic!("{err}"),
        };
        tally.total += 1;

        let parsed = match catch_unwind_silent(|| run_parse(path, &src)) {
            Ok(p) => p,
            Err(_) => {
                tally.panics.push(path.clone());
                continue;
            }
        };

        if parsed.root.text() != src.as_str() {
            tally.roundtrip_failures.push(path.clone());
        }

        if parsed.errors.is_empty() {
            tally.clean += 1;
        } else {
            tally.files_with_errors.push(path.clone());
        }
    }

    eprintln!(
        "parsed {} files | {} clean ({:.1}%) | {} with errors | {} panics | \
         {} non-UTF-8 skipped | {} round-trip failures",
        tally.total,
        tally.clean,
        100.0 * tally.clean as f64 / tally.total.max(1) as f64,
        tally.files_with_errors.len(),
        tally.panics.len(),
        tally.non_utf8.len(),
        tally.roundtrip_failures.len(),
    );

    // List the panicking files so they are auditable in `--nocapture` output
    // alongside the counts.
    if !tally.panics.is_empty() {
        eprintln!("raw-parser panics ({}):", tally.panics.len());
        for p in &tally.panics {
            eprintln!("  {}", p.display());
        }
    }
    if !tally.non_utf8.is_empty() {
        eprintln!("non-UTF-8 corpus sources ({}):", tally.non_utf8.len());
        for p in &tally.non_utf8 {
            eprintln!("  {}", p.display());
        }
    }

    // --- Universal hard invariant: losslessness ---------------------------
    if !tally.roundtrip_failures.is_empty() {
        eprintln!(
            "\n{} files did not round-trip (lossless invariant violated):",
            tally.roundtrip_failures.len()
        );
        for p in &tally.roundtrip_failures {
            eprintln!("  {}", p.display());
        }
        panic!(
            "{} files failed the lossless round-trip; the parser dropped or \
             rewrote source bytes",
            tally.roundtrip_failures.len()
        );
    }

    // --- Hard invariant: no panics -----------------------------------------
    assert!(
        tally.panics.is_empty(),
        "raw parser panicked on {} files. The corpus panics none of it today, so \
         this is a construct that regressed in — investigate rather than \
         restoring a ceiling.\n{}",
        tally.panics.len(),
        tally
            .panics
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // --- Every file's outcome, exactly ----------------------------------------
    let entries = std::iter::once(summary_entry("clean", tally.clean))
        .chain(
            tally
                .files_with_errors
                .iter()
                .map(|p| format!("{} errors", corpus_relative(&root, p))),
        )
        .chain(
            tally
                .non_utf8
                .iter()
                .map(|p| format!("{} non-utf8", corpus_relative(&root, p))),
        );
    let manifest =
        Manifest::from_entries(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "parser_corpus",
        &manifest,
        &regenerate_ignored("parser_corpus"),
    );
}
