//! Corpus sweep: the LSP parser path must survive real-world F# source.
//!
//! Runs [`parse_diagnostics`] over every `.fs` file in an F# source tree. The
//! parser is intentionally very incomplete, so it produces many diagnostics and
//! may even panic on some constructs — that's expected. The load-bearing
//! guarantee is that the LSP wrapper *never* panics (it catches parser panics
//! internally), so a server stays alive whatever the user opens.
//!
//! The tree is `BORZOI_CORPUS` — the pinned `fsharp-src` flake input under
//! `nix develop`, the same corpus every other sweep walks — falling back to the
//! sibling `../fsharp` checkout AGENTS.md points at. Preferring the pinned
//! input is what lets this run unattended: a runner has no sibling checkout, and
//! a sweep that skips itself when its corpus is absent reports the same green as
//! one that swept it.
//!
//! `#[ignore]`d by default: it is slow. Run with
//! `cargo test -p borzoi --test all parser_corpus_sweep:: -- --ignored`.

use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use borzoi::diagnostics::{SourceKind, parse_diagnostics};
use borzoi_cst::language_version::LanguageVersion;

/// Where the sweep is pointed, and how it was chosen — the caller reports the
/// choice, because "no corpus" and "a corpus with no files in it" have to be
/// told apart from a passing run.
enum SweepRoot {
    /// `BORZOI_CORPUS`: the pinned `fsharp-src` flake input under `nix develop`.
    Pinned(PathBuf),
    /// `<repo>/crates/lsp/../../../fsharp/src` → `<repo>/../fsharp/src`, the
    /// sibling F# compiler checkout AGENTS.md points at.
    SiblingCheckout(PathBuf),
    /// Neither is present, so there is nothing to sweep.
    Absent,
}

/// Resolve the tree to sweep, preferring the pinned corpus so an unattended run
/// walks the same source every other sweep does.
fn sweep_root() -> SweepRoot {
    if let Some(pinned) = std::env::var_os("BORZOI_CORPUS") {
        let pinned = PathBuf::from(pinned);
        assert!(
            pinned.is_dir(),
            "F# corpus root {pinned:?} (from BORZOI_CORPUS) is not a directory."
        );
        return SweepRoot::Pinned(pinned);
    }
    let sibling = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fsharp/src");
    if sibling.is_dir() {
        SweepRoot::SiblingCheckout(sibling)
    } else {
        SweepRoot::Absent
    }
}

fn collect_fs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("fs") {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "corpus sweep; run with --ignored under nix develop"]
fn parser_path_survives_fsharp_compiler_corpus() {
    let root = match sweep_root() {
        SweepRoot::Pinned(root) => {
            eprintln!("corpus sweep: BORZOI_CORPUS at {}", root.display());
            root
        }
        SweepRoot::SiblingCheckout(root) => {
            eprintln!("corpus sweep: sibling checkout at {}", root.display());
            root
        }
        SweepRoot::Absent => {
            eprintln!(
                "skipping corpus sweep: set BORZOI_CORPUS (run under `nix develop`) or clone \
                 the F# compiler next to this repo"
            );
            return;
        }
    };

    let mut files = Vec::new();
    collect_fs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "found no .fs files under {}",
        root.display()
    );

    let symbols = HashSet::from(["COMPILED".to_string()]);

    // Per file, whether the raw parser panicked and how many diagnostics the
    // LSP path produced (`Err` where a panic escaped it); `None` for a file
    // that does not read as UTF-8.
    let swept = borzoi_oracle_harness::par_map::par_map(files.len(), |i| {
        let text = std::fs::read_to_string(&files[i]).ok()?;
        // Silence per-file panic backtraces; we count panics ourselves.
        // Per-thread, so it is taken on the worker, and a concurrent test's
        // genuine panic still prints (see `panic_silence`). It is not held
        // across the assertions below: a failing one keeps its payload.
        let _silence = silence_panics_here();

        // Informational: the raw parser may panic on incomplete constructs.
        // This is *why* the wrapper catches — we count it, we don't fail on it.
        let raw_panicked =
            catch_unwind(AssertUnwindSafe(|| borzoi_cst::parser::parse(&text))).is_err();

        // The guarantee: the LSP path never panics, whatever the raw parser does.
        // The corpus is all `.fs` files, so parse under the implementation grammar.
        let diags = catch_unwind(AssertUnwindSafe(|| {
            parse_diagnostics(
                &text,
                &symbols,
                SourceKind::Implementation,
                LanguageVersion::Preview,
            )
            .len()
        }))
        .map_err(|_| ());
        Some((raw_panicked, diags))
    });

    let mut raw_panics = Vec::new();
    let mut wrapper_panics = Vec::new();
    let mut total_diags = 0usize;
    let mut files_with_diags = 0usize;
    for (path, outcome) in files.iter().zip(swept) {
        let Some((raw_panicked, diags)) = outcome else {
            continue;
        };
        if raw_panicked {
            raw_panics.push(path.clone());
        }
        match diags {
            Ok(n) => {
                if n > 0 {
                    files_with_diags += 1;
                }
                total_diags += n;
            }
            Err(()) => wrapper_panics.push(path.clone()),
        }
    }

    eprintln!(
        "corpus sweep: {} files | {} produced diagnostics | {} total diagnostics | {} raw-parser panics (caught)",
        files.len(),
        files_with_diags,
        total_diags,
        raw_panics.len(),
    );
    for p in &wrapper_panics {
        eprintln!("  WRAPPER PANIC ESCAPED: {}", p.display());
    }

    assert!(
        wrapper_panics.is_empty(),
        "parse_diagnostics let a panic escape on {} file(s) — the catch_unwind guard is not holding",
        wrapper_panics.len()
    );
}
