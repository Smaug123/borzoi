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
//! * **Graduating ratchets.** The parser is intentionally incomplete, so most
//!   real files still produce parse errors (and a few constructs still panic
//!   the raw parser). Rather than an unwieldy multi-thousand-entry allow-list,
//!   we ratchet on counts: at least [`MIN_CLEAN_PARSES`] files must parse with
//!   zero errors, and at most [`MAX_PANICS`] may panic. As the parser grows,
//!   bump the floor up / the ceiling down — the corpus is content-addressed
//!   (pinned in `flake.nix`), so these counts are stable across machines.
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

use crate::common::{
    catch_unwind_silent, collect_fsharp_corpus_files, corpus_root, read_corpus_source,
};

/// Lower bound on files that parse with **zero** errors and round-trip. The
/// parser only grows, so this only goes up — bump it after a phase lands.
/// A drop below this is a regression (some construct stopped parsing cleanly).
///
/// Measured 2026-06-19 against the pinned corpus: 3227 / 6367 (50.7%).
const MIN_CLEAN_PARSES: usize = 3227;

/// Upper bound on files whose raw parse **panics**. The LSP wraps the parser in
/// `catch_unwind` so a panic never kills the server (see
/// `crates/lsp/tests/all/parser_corpus_sweep.rs`), but a panic is still a latent
/// bug; this ceiling ratchets down as they are fixed.
///
/// Measured 2026-06-19 against the pinned corpus: 7 files.
const MAX_PANICS: usize = 7;

/// Upper bound on corpus files that are real F# fixtures but not UTF-8 source.
/// These are explicit skips because the CST parser takes `&str`; I/O failures
/// still panic instead of landing here.
///
/// Measured 2026-07-04 against the pinned corpus: 11 codepage / UTF-16
/// fixtures.
const MAX_NON_UTF8_SOURCES: usize = 11;

#[derive(Default)]
struct Tally {
    total: usize,
    panics: Vec<PathBuf>,
    roundtrip_failures: Vec<PathBuf>,
    files_with_errors: usize,
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

    let files = collect_fsharp_corpus_files(&root)
        .unwrap_or_else(|err| panic!("walk F# corpus under {}: {err}", root.display()));
    assert!(!files.is_empty(), "no .fs/.fsi/.fsx files under {root:?}");

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
            tally.files_with_errors += 1;
        }
    }

    eprintln!(
        "parsed {} files | {} clean ({:.1}%) | {} with errors | {} panics | \
         {} non-UTF-8 skipped | {} round-trip failures",
        tally.total,
        tally.clean,
        100.0 * tally.clean as f64 / tally.total.max(1) as f64,
        tally.files_with_errors,
        tally.panics.len(),
        tally.non_utf8.len(),
        tally.roundtrip_failures.len(),
    );

    // List the (few) panicking files so the known-broken set is auditable in
    // `--nocapture` output without having to trip the ceiling assertion.
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

    // --- Graduating ratchets ----------------------------------------------
    assert!(
        tally.non_utf8.len() <= MAX_NON_UTF8_SOURCES,
        "{} corpus sources were not UTF-8 (ceiling is MAX_NON_UTF8_SOURCES = {}). \
         These are skipped explicitly because the CST parser takes &str; \
         investigate new entries rather than silently dropping them.\n{}",
        tally.non_utf8.len(),
        MAX_NON_UTF8_SOURCES,
        tally
            .non_utf8
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(
        tally.panics.len() <= MAX_PANICS,
        "raw parser panicked on {} files (ceiling is MAX_PANICS = {}). New \
         panics regressed in; investigate or raise the ceiling deliberately.\n{}",
        tally.panics.len(),
        MAX_PANICS,
        tally
            .panics
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(
        tally.clean >= MIN_CLEAN_PARSES,
        "only {} files parsed cleanly (floor is MIN_CLEAN_PARSES = {}). Clean \
         parses regressed; some construct stopped parsing without errors.",
        tally.clean,
        MIN_CLEAN_PARSES,
    );
}
