//! Live MSBuild oracle for the phase-9b glob resolver.
//!
//! The `fsproj_msbuild_diff` test in the `msbuild` crate pins our parser
//! against `dotnet msbuild` over the *vendored* F# corpus — but that corpus
//! never globs its sources (every `<Compile>` is an explicit literal). This
//! test fills that gap: it builds **synthetic** globbing fixtures in a
//! tempdir and diffs [`borzoi::glob_resolver::resolve`] (wired into
//! [`parse_fsproj_with_imports`] exactly as the LSP wires it) against
//! `dotnet msbuild -getItem:Compile` for the same project.
//!
//! ## Why this is the faithfulness oracle for phase 9b
//!
//! Phase 9b-1's property test only proved the matcher agrees with an
//! independent naive matcher — both could share a wrong assumption about
//! MSBuild. This test is the thing that actually pins MSBuild semantics:
//! literal passthrough, recursive `**`, single-level `*`, exclude
//! application, and duplicate preservation across overlapping fragments.
//!
//! ## What is and isn't asserted (ordering)
//!
//! MSBuild's *within-glob* enumeration order is filesystem-dependent; our
//! resolver instead imposes a deterministic lexicographic order (see
//! [`borzoi`]'s `glob` module). To avoid a false alarm on a
//! platform whose filesystem hands MSBuild a different order, this oracle
//! compares the selected paths as a **sorted multiset** — it pins *which*
//! files are selected and *how many times* (the duplicate-preservation
//! decision), but deliberately not their order. Our deterministic ordering
//! is pinned separately by the unit tests in `glob_resolver`.
//!
//! ## Hermetic invocation
//!
//! Like `fsproj_msbuild_diff`, the `dotnet` child runs with a stripped
//! environment (only `PATH`/`HOME`/`TMPDIR` and `DOTNET_*`/`NUGET_*`) so an
//! inherited variable can't flip the evaluated item set. The fixture lives
//! in a fresh tempdir with no `global.json`/`Directory.Build.*`, so MSBuild
//! uses the host SDK and evaluates nothing but the project file itself.
//! `EnableDefaultCompileItems=false` keeps the SDK's own implicit `**/*.fs`
//! glob from masking the fixture's explicit one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use borzoi_spawn::BoundedCommand;

use borzoi::glob_resolver;
use borzoi_msbuild::{GlobResolver, ItemKind, parse_fsproj_with_imports};
use serde::Deserialize;
use tempfile::TempDir;

#[derive(Deserialize)]
struct MsbuildOutput {
    #[serde(rename = "Items")]
    items: MsbuildItems,
}

#[derive(Deserialize, Default)]
struct MsbuildItems {
    #[serde(default, rename = "Compile")]
    compile: Vec<MsbuildItem>,
}

#[derive(Deserialize)]
struct MsbuildItem {
    #[serde(rename = "FullPath")]
    full_path: String,
}

/// Create a fresh tempdir containing every relative path in `files` (with
/// parent directories), each an empty-ish F# source.
fn fixture(files: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    for rel in files {
        let full = tmp.path().join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, b"module M\n").unwrap();
    }
    tmp
}

/// Write a minimal globbing `.fsproj` into `dir` and return its path.
fn write_fsproj(dir: &Path, include: &str, exclude: Option<&str>) -> PathBuf {
    let excl = exclude
        .map(|e| format!(" Exclude=\"{e}\""))
        .unwrap_or_default();
    let source = format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  \
           <PropertyGroup>\n    \
             <TargetFramework>net10.0</TargetFramework>\n    \
             <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n  \
           </PropertyGroup>\n  \
           <ItemGroup>\n    \
             <Compile Include=\"{include}\"{excl} />\n  \
           </ItemGroup>\n\
         </Project>\n"
    );
    let path = dir.join("Test.fsproj");
    std::fs::write(&path, source).unwrap();
    path
}

/// Our pipeline: parse the project with the real glob resolver wired in
/// (sdk_resolver `None`, exactly as the LSP's no-SDK arm does), returning
/// the resolved `Compile` paths.
fn our_compile(fsproj: &Path) -> Vec<PathBuf> {
    let source = std::fs::read_to_string(fsproj).unwrap();
    let extras: HashMap<String, String> = HashMap::new();
    let glob: &GlobResolver<'_> = &glob_resolver::resolve;
    let project = parse_fsproj_with_imports(
        &source,
        fsproj,
        &extras,
        &oracle_environment(),
        None,
        Some(glob),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));
    project
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Compile)
        .map(|i| i.include.clone())
        .collect()
}

/// The environment snapshot `msbuild_compile`'s child runs under, as the map
/// our evaluator takes: both sides must see the same initial properties for
/// the differential comparison to be meaningful.
fn oracle_environment() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            env.insert(var.to_string(), value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            env.insert(key, value);
        }
    }
    env
}

/// MSBuild's reference `Compile` set for the same project.
fn msbuild_compile(fsproj: &Path) -> Vec<PathBuf> {
    let mut cmd = Command::new("dotnet");
    // Evaluate from the fixture dir: there is no `global.json` above it, so
    // MSBuild picks the host SDK rather than any pinned version.
    cmd.current_dir(fsproj.parent().unwrap());
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
    cmd.args(["msbuild", "-nologo", "-getItem:Compile"]);
    cmd.arg(fsproj);
    // An MSBuild evaluation can restore packages on the way, which is
    // legitimately minutes on a cold cache: the bound is there to stop a
    // *stalled* run (blocked on a NuGet lock held by a concurrent run in a
    // sibling worktree, say) from hanging the suite forever, not to police a
    // slow one.
    let out = BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(1800))
        .run_ok(format_args!("dotnet msbuild for {}", fsproj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    let parsed: MsbuildOutput = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "could not parse msbuild JSON for {}: {e}\n--- stdout ---\n{stdout}",
            fsproj.display()
        )
    });
    parsed
        .items
        .compile
        .into_iter()
        .map(|i| PathBuf::from(i.full_path))
        .collect()
}

/// Canonicalise (so `/var` vs `/private/var` etc. agree) and sort — the
/// sorted-multiset comparison the module docs describe. Every fixture used
/// here selects only files that exist on disk, so canonicalisation always
/// succeeds.
fn sorted_canon(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            std::fs::canonicalize(p).unwrap_or_else(|e| panic!("canonicalize {}: {e}", p.display()))
        })
        .collect();
    v.sort();
    v
}

fn assert_matches_msbuild(files: &[&str], include: &str, exclude: Option<&str>) {
    let tmp = fixture(files);
    let fsproj = write_fsproj(tmp.path(), include, exclude);
    let ours = sorted_canon(&our_compile(&fsproj));
    let theirs = sorted_canon(&msbuild_compile(&fsproj));
    assert_eq!(
        ours, theirs,
        "Compile set diverges from MSBuild for include={include:?} exclude={exclude:?}"
    );
}

const TREE: &[&str] = &[
    "a.fs",
    "b.fs",
    "m.fs",
    "z.fs",
    "sub/c.fs",
    "sub/deep/d.fs",
    "sub/e.fsi",
];

#[test]
fn recursive_glob() {
    assert_matches_msbuild(TREE, "**/*.fs", None);
}

#[test]
fn top_level_star() {
    assert_matches_msbuild(TREE, "*.fs", None);
}

#[test]
fn nested_single_level_star() {
    assert_matches_msbuild(TREE, "sub/*.fs", None);
}

#[test]
fn fsi_extension_distinguished() {
    assert_matches_msbuild(TREE, "**/*.fsi", None);
}

#[test]
fn literal_then_glob_keeps_duplicate() {
    assert_matches_msbuild(TREE, "a.fs;*.fs", None);
}

#[test]
fn two_overlapping_globs_keep_duplicates() {
    assert_matches_msbuild(TREE, "*.fs;?.fs", None);
}

#[test]
fn exclude_removes_subtree() {
    assert_matches_msbuild(TREE, "**/*.fs", Some("sub/**/*.fs"));
}

#[test]
fn exclude_single_file() {
    assert_matches_msbuild(TREE, "*.fs", Some("b.fs"));
}

#[test]
fn recursive_glob_absolute_exclude() {
    // A relative recursive Include with an *absolute* Exclude — the form
    // MSBuild produces from `$(MSBuildProjectDirectory)/sub/c.fs`. The
    // resolver enumerates candidates base-relative, so it must anchor the
    // absolute exclude into the same frame to agree with MSBuild.
    let tmp = fixture(TREE);
    let abs_excl = tmp.path().join("sub/c.fs");
    let fsproj = write_fsproj(tmp.path(), "**/*.fs", Some(&abs_excl.to_string_lossy()));
    let ours = sorted_canon(&our_compile(&fsproj));
    let theirs = sorted_canon(&msbuild_compile(&fsproj));
    assert_eq!(ours, theirs, "absolute Exclude diverges from MSBuild");
}

#[test]
fn question_mark_single_char() {
    assert_matches_msbuild(TREE, "?.fs", None);
}

#[test]
fn parent_relative_glob() {
    // A glob rooted above the project directory (`../shared/*.fs`) must
    // enumerate the sibling directory exactly as MSBuild does, and must not
    // pick up the project-local decoy.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("proj");
    let shared = tmp.path().join("shared");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    for f in ["a.fs", "b.fs"] {
        std::fs::write(shared.join(f), b"module M\n").unwrap();
    }
    std::fs::write(proj.join("local.fs"), b"module M\n").unwrap();
    let fsproj = write_fsproj(&proj, "../shared/*.fs", None);
    let ours = sorted_canon(&our_compile(&fsproj));
    let theirs = sorted_canon(&msbuild_compile(&fsproj));
    assert_eq!(ours, theirs, "../shared/*.fs diverges from MSBuild");
}

/// Build the `proj`/`shared` sibling fixture used by the parent-relative
/// exclude oracle pair: `tmp/{proj, shared/{a.fs, b.fs}}`. Returns the
/// tempdir (keep alive) and the `proj` dir.
fn parent_relative_fixture() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("proj");
    let shared = tmp.path().join("shared");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    for f in ["a.fs", "b.fs"] {
        std::fs::write(shared.join(f), b"module M\n").unwrap();
    }
    (tmp, proj)
}

#[test]
fn parent_relative_glob_absolute_exclude_is_noop() {
    // Surprising-but-real MSBuild behaviour: an *absolute* Exclude does NOT
    // cross-match a `../shared/*.fs` include. MSBuild matches Exclude against
    // the Include's items in the Include's relative (`..`-preserving) frame,
    // so the absolute exclude — in a different frame — filters nothing and
    // both siblings survive. Pinned here so a future `..`-collapsing "fix"
    // (which would wrongly drop `a.fs`) can't silently diverge.
    let (tmp, proj) = parent_relative_fixture();
    let abs_excl = tmp.path().join("shared/a.fs");
    let fsproj = write_fsproj(&proj, "../shared/*.fs", Some(&abs_excl.to_string_lossy()));
    let ours = sorted_canon(&our_compile(&fsproj));
    let theirs = sorted_canon(&msbuild_compile(&fsproj));
    assert_eq!(
        ours, theirs,
        "absolute Exclude must not cross-match a `..` include (per MSBuild)"
    );
}

#[test]
fn parent_relative_glob_relative_exclude_filters() {
    // The contrast to the no-op case: a *relative* Exclude in the Include's
    // own frame (`../shared/a.fs`) does filter, dropping `a.fs`.
    let (_tmp, proj) = parent_relative_fixture();
    let fsproj = write_fsproj(&proj, "../shared/*.fs", Some("../shared/a.fs"));
    let ours = sorted_canon(&our_compile(&fsproj));
    let theirs = sorted_canon(&msbuild_compile(&fsproj));
    assert_eq!(
        ours, theirs,
        "relative Exclude in the include's frame should filter"
    );
}

// A `*` in a directory name is only a legal path component on Unix-like
// filesystems; on Windows `create_dir_all` would reject it before the
// resolver runs, so this oracle is Unix-gated.
#[cfg(unix)]
#[test]
fn base_dir_wildcard_is_literal_not_glob() {
    // A project directory whose *name* contains a glob metacharacter (`*`,
    // legal on Unix) must be treated as a literal path: MSBuild resolves
    // `Include="*.fs"` against the real `a*b/proj` directory and does NOT
    // let the `*` in the directory name match sibling dirs such as
    // `axb/proj`. Pinned here because folding the base directory into the
    // glob string reinterprets its `*` as a wildcard and wrongly pulls in
    // the sibling (verified against `dotnet msbuild`).
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("a*b/proj");
    let sibling = tmp.path().join("axb/proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(proj.join("real.fs"), b"module M\n").unwrap();
    std::fs::write(sibling.join("decoy.fs"), b"module M\n").unwrap();
    let fsproj = write_fsproj(&proj, "*.fs", None);
    let ours = sorted_canon(&our_compile(&fsproj));
    let theirs = sorted_canon(&msbuild_compile(&fsproj));
    assert_eq!(
        ours, theirs,
        "a `*` in the project directory name must be literal, not a glob"
    );
}
