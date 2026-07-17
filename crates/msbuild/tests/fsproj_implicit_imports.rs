//! Corpus assertion for [`borzoi_msbuild::detect_implicit_imports`].
//!
//! The pure-parsing tests in `tests/fsproj_corpus.rs` use a *copy* of
//! the relevant fsproj under `tests/fixtures/fsproj-corpus/`, in
//! isolation from the rest of the F# tree. That's deliberate — we
//! want snapshots that don't depend on whichever upstream Directory.*
//! files happen to exist around the source. The implicit-import
//! helper, however, is *only* interesting when there *are* surrounding
//! Directory.* files: so this test points it at the live F# corpus and
//! pins the well-known files we expect it to discover for each fsproj.
//!
//! Reads the corpus from `BORZOI_CORPUS` (see [`common::corpus_root`])
//! and panics if it is unset — same contract as the other corpus walkers.
//!
//! ## Path canonicalisation
//!
//! `detect_implicit_imports` refuses non-rooted project paths
//! (otherwise its `is_file` probes would silently resolve against
//! the process cwd; see `imports.rs`). `corpus_root()` honours
//! `BORZOI_CORPUS`, which is *documented* to accept relative
//! paths (e.g. `../fsharp`) — so we canonicalise the joined project
//! path before handing it over. This mirrors the same workaround in
//! `tests/fsproj_msbuild_diff.rs`.

mod common;

use std::path::{Path, PathBuf};

use borzoi_msbuild::{Diagnostic, DiagnosticKind, ImplicitImportKind, detect_implicit_imports};

/// Join `rel` onto the corpus root and canonicalise the result.
/// Canonicalisation is needed because `BORZOI_CORPUS` may be a
/// relative path — see the module-level note.
fn corpus_project(corpus: &Path, rel: &str) -> PathBuf {
    let joined = corpus.join(rel);
    std::fs::canonicalize(&joined)
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", joined.display()))
}

/// The canonicalised corpus root, for stripping in `simplify`.
fn canonical_corpus_root(corpus: &Path) -> PathBuf {
    std::fs::canonicalize(corpus)
        .unwrap_or_else(|e| panic!("canonicalize corpus root {}: {e}", corpus.display()))
}

#[test]
fn fcs_compiler_service_finds_nearest_props_and_targets() {
    let corpus = common::corpus_root();
    let proj = corpus_project(&corpus, "src/Compiler/FSharp.Compiler.Service.fsproj");
    let canon_root = canonical_corpus_root(&corpus);

    let got = detect_implicit_imports(&proj);
    let pairs = simplify(&got, &canon_root);

    // src/Compiler has its own Directory.Build.props. The targets
    // file isn't co-located there, so the walk falls through to
    // src/Directory.Build.targets. The repo has no
    // Directory.Packages.props in the chain.
    assert_eq!(
        pairs,
        vec![
            (
                ImplicitImportKind::DirectoryBuildProps,
                PathBuf::from("src/Compiler/Directory.Build.props")
            ),
            (
                ImplicitImportKind::DirectoryBuildTargets,
                PathBuf::from("src/Directory.Build.targets")
            ),
        ],
        "diagnostics: {got:#?}",
    );
}

#[test]
fn fcs_fsharp_core_finds_nearest_props_and_targets() {
    let corpus = common::corpus_root();
    let proj = corpus_project(&corpus, "src/FSharp.Core/FSharp.Core.fsproj");
    let canon_root = canonical_corpus_root(&corpus);

    let got = detect_implicit_imports(&proj);
    let pairs = simplify(&got, &canon_root);

    assert_eq!(
        pairs,
        vec![
            (
                ImplicitImportKind::DirectoryBuildProps,
                PathBuf::from("src/FSharp.Core/Directory.Build.props")
            ),
            (
                ImplicitImportKind::DirectoryBuildTargets,
                PathBuf::from("src/Directory.Build.targets")
            ),
        ],
        "diagnostics: {got:#?}",
    );
}

#[test]
fn fcs_assembly_check_falls_through_to_repo_root() {
    let corpus = common::corpus_root();
    let proj = corpus_project(&corpus, "buildtools/AssemblyCheck/AssemblyCheck.fsproj");
    let canon_root = canonical_corpus_root(&corpus);

    let got = detect_implicit_imports(&proj);
    let pairs = simplify(&got, &canon_root);

    // AssemblyCheck and buildtools/ have no Directory.* files; the
    // walk finds the repo-root copies.
    assert_eq!(
        pairs,
        vec![
            (
                ImplicitImportKind::DirectoryBuildProps,
                PathBuf::from("Directory.Build.props")
            ),
            (
                ImplicitImportKind::DirectoryBuildTargets,
                PathBuf::from("Directory.Build.targets")
            ),
        ],
        "diagnostics: {got:#?}",
    );
}

#[test]
fn fcs_fslex_falls_through_to_repo_root() {
    let corpus = common::corpus_root();
    let proj = corpus_project(&corpus, "buildtools/fslex/fslex.fsproj");
    let canon_root = canonical_corpus_root(&corpus);

    let got = detect_implicit_imports(&proj);
    let pairs = simplify(&got, &canon_root);

    assert_eq!(
        pairs,
        vec![
            (
                ImplicitImportKind::DirectoryBuildProps,
                PathBuf::from("Directory.Build.props")
            ),
            (
                ImplicitImportKind::DirectoryBuildTargets,
                PathBuf::from("Directory.Build.targets")
            ),
        ],
        "diagnostics: {got:#?}",
    );
}

/// Reduce each diagnostic to `(kind, path-relative-to-corpus-root)`.
/// The assertion only cares about the kind and the relative path —
/// the absolute path's prefix changes per developer / per CI host,
/// and the span is always `0..0`.
///
/// Filters out anything not under `corpus_root` defensively, in case
/// some unrelated `Directory.Build.props` exists in the test host's
/// real filesystem outside the submodule.
fn simplify(diags: &[Diagnostic], corpus_root: &Path) -> Vec<(ImplicitImportKind, PathBuf)> {
    diags
        .iter()
        .filter_map(|d| match &d.kind {
            DiagnosticKind::ImplicitImportPresent { path, kind } => path
                .strip_prefix(corpus_root)
                .ok()
                .map(|rel| (*kind, rel.to_path_buf())),
            other => panic!("unexpected diagnostic kind {other:?}"),
        })
        .collect()
}
