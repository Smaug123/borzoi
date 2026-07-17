//! Run the preprocessor + lexer over a real F# source tree and report
//! failures.
//!
//! Walks the corpus rooted at `BORZOI_CORPUS` (the `fsharp-src` flake
//! input under `nix develop`), runs `lex_with_symbols(src, &HashSet::new())` on
//! every `.fs` / `.fsi` / `.fsx` file, and asserts zero errors — except
//! files matching `EXPECTED_FAILURE_SUBSTRINGS`, which are deliberately-
//! malformed fixtures whose contents aren't really F# code we'd be
//! expected to lex cleanly. Any *new* file failing → real regression.
//!
//! Using `lex_with_symbols` rather than bare `lex` means `#if NOTDEFINED`
//! blocks are skipped (closing the original `ifdef-plan.md` goal), so
//! fixtures whose inactive arms contain malformed `(* ... *)` /
//! `"...` literals (e.g. `ConditionalCompilation/InComment01.fs`) are
//! handled correctly.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use borzoi_cst::directives::{PreprocError, lex_with_symbols};

use crate::common::corpus_root;

/// Paths containing any of these substrings are allowed to fail. Documented
/// reason next to each so the list is auditable.
const EXPECTED_FAILURE_SUBSTRINGS: &[&str] = &[
    // F# fixtures named `E_<name>` are deliberately malformed source designed
    // to exercise the compiler's error reporting. Our lexer correctly reports
    // them as broken.
    "/E_",
    // Diagnostic-NONTERM fixtures share the same intent (a stray `\;;` etc.).
    "/Diagnostics/NONTERM/interactiveExprOrDefinitionsTerminator06",
    // SyntaxTree fixtures contain unfinished/exotic inputs (lone backticks,
    // U+FFFD replacement chars in identifier positions, etc.) used to drive
    // parser recovery — not real F# source.
    "/SyntaxTree/Expression/Id ",
    "/SyntaxTree/Expression/Unfinished escaped ident ",
    "/SyntaxTree/Type/Type 10.fs",
    // FCS's test driver compiles this file with `--define:DEFINED`
    // (see `ConditionalCompilation.fs:98` in the F# repo). Under that
    // symbol the inactive `#else` arm hides text that would otherwise
    // split a string literal across two conditional branches. We run
    // the corpus with an empty symbol set, so the file is genuinely
    // malformed here — re-include it once Stage 7/8 plumbs per-file
    // SCFLAGS.
    "/ConditionalCompilation/InStringLiteral02.fs",
];

fn is_expected_failure(path: &Path) -> bool {
    let p = path.to_string_lossy();
    EXPECTED_FAILURE_SUBSTRINGS.iter().any(|s| p.contains(s))
}

#[test]
fn lex_fsharp_corpus() {
    let root = corpus_root();

    let mut files = Vec::new();
    collect_fsharp_files(&root, &mut files);
    assert!(!files.is_empty(), "no .fs/.fsi/.fsx files under {root:?}");

    eprintln!("lexing {} files under {}", files.len(), root.display());

    let symbols: HashSet<String> = HashSet::new();
    let mut failures: Vec<FailureReport> = Vec::new();
    let mut total_tokens: u64 = 0;
    let mut total_bytes: u64 = 0;

    for path in &files {
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                // Skip non-UTF-8 files. F# source is expected to be UTF-8;
                // anything else isn't part of the corpus we care about.
                eprintln!("skip (not UTF-8): {} ({err})", path.display());
                continue;
            }
        };
        total_bytes += src.len() as u64;
        let mut errors: Vec<(PreprocError, usize)> = Vec::new();
        for (tok, span) in lex_with_symbols(&src, &symbols) {
            total_tokens += 1;
            if let Err(err) = tok {
                errors.push((err, span.start));
                if errors.len() >= 5 {
                    break;
                }
            }
        }
        if !errors.is_empty() {
            failures.push(FailureReport {
                path: path.clone(),
                errors,
            });
        }
    }

    eprintln!(
        "lexed {} tokens across {:.1} MiB",
        total_tokens,
        total_bytes as f64 / (1024.0 * 1024.0)
    );

    let (expected, unexpected): (Vec<_>, Vec<_>) = failures
        .into_iter()
        .partition(|f| is_expected_failure(&f.path));

    eprintln!(
        "{} expected failures (in {}-entry allow-list), {} unexpected",
        expected.len(),
        EXPECTED_FAILURE_SUBSTRINGS.len(),
        unexpected.len(),
    );

    if !unexpected.is_empty() {
        eprintln!(
            "\n{} files produced unexpected lex errors:",
            unexpected.len()
        );
        for f in &unexpected {
            eprintln!("  {}", f.path.display());
            for (err, offset) in &f.errors {
                eprintln!("    @ byte {offset}: {err:?}");
            }
        }
        panic!("{} files failed to lex cleanly", unexpected.len());
    }
}

/// Tighter-scope sweep of `Conformance/LexicalAnalysis/ConditionalCompilation/`.
/// The broad corpus walk already touches these, but this test is a focused
/// regression net for the directive layer: it enumerates the directory
/// directly, asserts it is non-empty (so a renamed/moved corpus is loud),
/// and reports failures one fixture at a time rather than batched.
///
/// `E_*` fixtures are skipped — they are deliberately malformed and
/// belong to the directive-error allow-list. Other allow-list entries
/// living under this directory (e.g. `InStringLiteral02.fs`, which FCS
/// gates on `--define:DEFINED`) are skipped via the shared
/// [`is_expected_failure`] check.
#[test]
fn lex_conditional_compilation_fixtures() {
    let dir = corpus_root()
        .join("tests")
        .join("FSharp.Compiler.ComponentTests")
        .join("resources")
        .join("tests")
        .join("Conformance")
        .join("LexicalAnalysis")
        .join("ConditionalCompilation");
    assert!(
        dir.is_dir(),
        "ConditionalCompilation fixture directory {dir:?} not found — \
         was the corpus moved? Update this test or the resolver in \
         common::corpus_root."
    );

    let mut files = Vec::new();
    collect_fsharp_files(&dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "ConditionalCompilation directory {dir:?} produced no .fs files \
         — the test below would silently pass against an empty set."
    );

    let symbols: HashSet<String> = HashSet::new();
    let mut failures: Vec<FailureReport> = Vec::new();

    for path in &files {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // Deliberately-malformed and SCFLAGS-gated fixtures are handled
        // by the broad allow-list rather than re-listed here.
        if name.starts_with("E_") || is_expected_failure(path) {
            continue;
        }
        let src =
            fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        let mut errors: Vec<(PreprocError, usize)> = Vec::new();
        for (tok, span) in lex_with_symbols(&src, &symbols) {
            if let Err(err) = tok {
                errors.push((err, span.start));
                if errors.len() >= 5 {
                    break;
                }
            }
        }
        if !errors.is_empty() {
            failures.push(FailureReport {
                path: path.clone(),
                errors,
            });
        }
    }

    if !failures.is_empty() {
        eprintln!(
            "\n{} ConditionalCompilation fixtures failed to lex cleanly:",
            failures.len()
        );
        for f in &failures {
            eprintln!("  {}", f.path.display());
            for (err, offset) in &f.errors {
                eprintln!("    @ byte {offset}: {err:?}");
            }
        }
        panic!("{} fixtures failed", failures.len());
    }
}

struct FailureReport {
    path: PathBuf,
    errors: Vec<(PreprocError, usize)>,
}

fn collect_fsharp_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            // Skip `.git` and build output directories to keep the walk cheap.
            if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                continue;
            }
            collect_fsharp_files(&path, out);
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("fs" | "fsi" | "fsx")
        ) {
            out.push(path);
        }
    }
}
