//! Tests for [`super::parse_fsproj_with_imports`] — the
//! filesystem-touching variant that follows explicit `<Import>` and
//! splices in the nearest `Directory.Build.props` / `.targets`.
//!
//! Each test builds a fresh [`tempfile::TempDir`], writes a small
//! cluster of fsproj/props files into it, and asserts on the merged
//! [`ParsedProject`]. The harness canonicalises every path it
//! constructs because on macOS `tempdir()` returns `/var/folders/...`
//! while [`std::fs::canonicalize`] (which the walker uses for
//! walked-file identity) returns `/private/var/folders/...` —
//! comparing the two directly would spuriously fail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;

mod cycles_and_failures;
mod directory_build;
mod explicit_imports;
mod fsharp_core_netsdk_props;
mod globs;
mod import_dag;
mod items_uncertain;
mod locators;
mod nested_sdk;
mod nuget_props_chain;
mod package_uncertain;
mod pass_ordering;
mod resolved_sdk_root;
mod sdk_custom_entry;
mod sdk_resolution;
mod spans;
mod this_file_directory;
mod treat_as_local;

/// Canonicalise `path` (must exist). Paths returned by the walker are
/// canonicalised; tests should canonicalise the expected paths too so
/// the macOS `/var` ↔ `/private/var` symlink doesn't desync them.
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("canonicalize {}: {e}", path.display()))
}

/// Write `contents` at `dir/name`, creating intermediate directories.
fn write_at(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("create_dir_all {}: {e}", parent.display()));
    }
    std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    path
}

/// Walk `source` as the body of `project_path`, canonicalising the
/// project path first so item include paths come out canonical too.
/// (The walker joins `Include` attributes onto the project's
/// directory verbatim — so tests that compare against canonical paths
/// need a canonical project path to match.)
fn parse(project_path: &Path, source: &str) -> ParsedProject {
    let canon_project = canon(project_path);
    parse_fsproj_with_imports(
        source,
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        None,
        None,
    )
    .expect("well-formed XML parses")
}

/// [`parse`], reading the body back from `project_path` on disk. Most
/// tests write the project with [`write_at`] (which returns the path)
/// and immediately walk it; this bundles the re-read so call sites
/// don't repeat `&std::fs::read_to_string(&project_path).unwrap()`.
fn parse_file(project_path: &Path) -> ParsedProject {
    parse(
        project_path,
        &std::fs::read_to_string(project_path).unwrap(),
    )
}

fn paths_of(items: &[ResolvedItem]) -> Vec<PathBuf> {
    items.iter().map(|i| i.include.clone()).collect()
}

/// Variant of [`parse`] that lets the caller supply
/// `extra_properties`. Used for opt-out gating tests where the
/// caller injects `ImportDirectoryBuildProps=false`.
fn parse_with_extras(
    project_path: &Path,
    source: &str,
    extras: HashMap<String, String>,
) -> ParsedProject {
    let canon_project = canon(project_path);
    parse_fsproj_with_imports(source, &canon_project, &extras, &HashMap::new(), None, None)
        .expect("well-formed XML parses")
}

/// [`parse_with_extras`], reading the body back from `project_path` on
/// disk (see [`parse_file`]).
fn parse_file_with_extras(project_path: &Path, extras: HashMap<String, String>) -> ParsedProject {
    parse_with_extras(
        project_path,
        &std::fs::read_to_string(project_path).unwrap(),
        extras,
    )
}

/// Materialise a synthetic SDK with caller-supplied props/targets
/// bodies. Returns the SDK root directory and the two file paths so
/// callers can hand them to a resolver closure.
fn write_synthetic_sdk(
    tmp: &Path,
    sdk_name: &str,
    props_body: &str,
    targets_body: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let sdk_dir = tmp.join("sdks").join(sdk_name);
    let props = write_at(&sdk_dir, "Sdk.props", props_body);
    let targets = write_at(&sdk_dir, "Sdk.targets", targets_body);
    (sdk_dir, props, targets)
}

/// Run `parse_fsproj_with_imports` with an SDK resolver in place. Keeps
/// the call shape uniform across the SDK tests below. The closure
/// returns plain [`SdkPaths`] (the shape every ordinary-SDK test wants);
/// [`parse_with_sdk_resolution`] is the locator-shaped variant.
fn parse_with_sdk<F>(project_path: &Path, source: &str, resolver: F) -> ParsedProject
where
    F: Fn(&str) -> Result<SdkPaths, SdkResolveError>,
{
    parse_with_sdk_resolution(project_path, source, |name| {
        resolver(name).map(SdkResolution::from)
    })
}

/// [`parse_with_sdk`] for resolvers that produce the full
/// [`SdkResolution`] (multi-root locator results included).
fn parse_with_sdk_resolution<F>(project_path: &Path, source: &str, resolver: F) -> ParsedProject
where
    F: Fn(&str) -> Result<SdkResolution, SdkResolveError>,
{
    let canon_project = canon(project_path);
    parse_fsproj_with_imports(
        source,
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .expect("well-formed XML parses")
}

/// [`parse_with_sdk`], reading the body back from `project_path` on disk
/// (see [`parse_file`]).
fn parse_file_with_sdk<F>(project_path: &Path, resolver: F) -> ParsedProject
where
    F: Fn(&str) -> Result<SdkPaths, SdkResolveError>,
{
    parse_with_sdk(
        project_path,
        &std::fs::read_to_string(project_path).unwrap(),
        resolver,
    )
}

/// [`parse_with_sdk_resolution`], reading the body back from
/// `project_path` on disk.
fn parse_file_with_sdk_resolution<F>(project_path: &Path, resolver: F) -> ParsedProject
where
    F: Fn(&str) -> Result<SdkResolution, SdkResolveError>,
{
    parse_with_sdk_resolution(
        project_path,
        &std::fs::read_to_string(project_path).unwrap(),
        resolver,
    )
}

/// [`parse_file_with_sdk_resolution`] with a caller-supplied environment
/// snapshot and globals. The toolset an SDK resolves to decides whether an
/// environment-supplied `MSBuildExtensionsPath` survives, so those tests need
/// all three inputs at once.
fn parse_file_with_sdk_env<F>(
    project_path: &Path,
    resolver: F,
    extras: HashMap<String, String>,
    environment: HashMap<String, String>,
) -> ParsedProject
where
    F: Fn(&str) -> Result<SdkResolution, SdkResolveError>,
{
    let canon_project = canon(project_path);
    let source = std::fs::read_to_string(project_path).unwrap();
    let resolver = |name: &str| resolver(name);
    parse_fsproj_with_imports(
        &source,
        &canon_project,
        &extras,
        &environment,
        Some(&resolver),
        None,
    )
    .expect("well-formed XML parses")
}

/// Materialise a synthetic SDK with the standard `Sdk.{props,targets}`
/// pair plus an arbitrary list of extra entry-point files. Returns the
/// SDK root directory and the two well-known file paths so callers can
/// hand them to a resolver closure; extras live alongside the standard
/// pair under the same root.
fn write_synthetic_sdk_with_extras(
    tmp: &Path,
    sdk_name: &str,
    props_body: &str,
    targets_body: &str,
    extras: &[(&str, &str)],
) -> (PathBuf, PathBuf, PathBuf) {
    let sdk_dir = tmp.join("sdks").join(sdk_name);
    let props = write_at(&sdk_dir, "Sdk.props", props_body);
    let targets = write_at(&sdk_dir, "Sdk.targets", targets_body);
    for (name, body) in extras {
        write_at(&sdk_dir, name, body);
    }
    (sdk_dir, props, targets)
}
