//! Corpus sweep: the LSP parser path must survive real-world F# source.
//!
//! Runs [`parse_diagnostics`] over every `.fs` file in the F# compiler
//! checkout (`../fsharp`, per AGENTS.md). The parser is intentionally very
//! incomplete, so it produces many diagnostics and may even panic on some
//! constructs — that's expected. The load-bearing guarantee is that the
//! LSP wrapper *never* panics (it catches parser panics internally), so a
//! server stays alive whatever the user opens.
//!
//! `#[ignore]`d by default: it needs the external checkout and is slow.
//! Run with `cargo test -p borzoi -- --ignored`. Skips cleanly if
//! the checkout is absent, so CI without it stays green.

use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use borzoi::diagnostics::{SourceKind, parse_diagnostics};
use borzoi_cst::language_version::LanguageVersion;

/// `<repo>/crates/lsp/../../../fsharp/src` → `<repo>/../fsharp/src`, the
/// sibling F# compiler checkout AGENTS.md points at.
fn fsharp_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fsharp/src")
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
#[ignore = "needs the ../fsharp checkout; run with --ignored"]
fn parser_path_survives_fsharp_compiler_corpus() {
    let root = fsharp_src();
    if !root.is_dir() {
        eprintln!(
            "skipping corpus sweep: {} not found (clone the F# compiler next to this repo)",
            root.display()
        );
        return;
    }

    let mut files = Vec::new();
    collect_fs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "found no .fs files under {}",
        root.display()
    );

    let symbols = HashSet::from(["COMPILED".to_string()]);

    let mut raw_panics = Vec::new();
    let mut wrapper_panics = Vec::new();
    let mut total_diags = 0usize;
    let mut files_with_diags = 0usize;

    // Silence per-file panic backtraces; we count panics ourselves. Per-thread,
    // so a concurrent test's genuine panic still prints (see `panic_silence`).
    //
    // Scoped to the loop, and *not* held across the assertions below: a failing
    // one must keep its payload and backtrace. (The hook this replaced was
    // restored before them too.)
    let _silence = silence_panics_here();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };

        // Informational: the raw parser may panic on incomplete constructs.
        // This is *why* the wrapper catches — we count it, we don't fail on it.
        if catch_unwind(AssertUnwindSafe(|| borzoi_cst::parser::parse(&text))).is_err() {
            raw_panics.push(path.clone());
        }

        // The guarantee: the LSP path never panics, whatever the raw parser does.
        // The corpus is all `.fs` files, so parse under the implementation grammar.
        match catch_unwind(AssertUnwindSafe(|| {
            parse_diagnostics(
                &text,
                &symbols,
                SourceKind::Implementation,
                LanguageVersion::Preview,
            )
        })) {
            Ok(diags) => {
                if !diags.is_empty() {
                    files_with_diags += 1;
                }
                total_diags += diags.len();
            }
            Err(_) => wrapper_panics.push(path.clone()),
        }
    }
    drop(_silence);

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
