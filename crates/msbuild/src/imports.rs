//! Detection of MSBuild's well-known implicit-import files
//! (`Directory.Build.props`, `Directory.Build.targets`,
//! `Directory.Packages.props`) in the project's ancestor directories.
//!
//! Distinct from the pure-parsing core: this helper touches the
//! filesystem, while [`parse_fsproj`](super::parse_fsproj) does not.
//! The split exists so callers can decide whether to incur the IO and
//! so the snapshot tests for the parser stay reproducible (plan D8).
//!
//! ## What this is *not*
//!
//! - **Not an import follower.** We never open these files, never
//!   evaluate their content, never merge their properties or items
//!   into the result. Doing so is plan phase 7 and is far larger than
//!   detection (conditional logic across files,
//!   `$(MSBuildThisFileDirectory)`, `Sdk="…"` attribute resolution,
//!   recursion limits).
//! - **Not a property evaluator.** The fact that a
//!   `Directory.Build.props` exists doesn't tell us *what* properties
//!   it would set — we surface its existence, no more.
//!
//! ## MSBuild semantics we mirror
//!
//! MSBuild walks the project's directory and each ancestor, stopping
//! at the **first** `Directory.Build.props` (resp. `.targets`,
//! `Directory.Packages.props`) it finds. The discovered file is
//! responsible for importing further up the chain if it wants to;
//! MSBuild itself does not. We mirror this: at most one diagnostic
//! per implicit-import kind, naming the nearest match. (See
//! <https://learn.microsoft.com/visualstudio/msbuild/customize-by-directory>.)
//!
//! ## Order of returned diagnostics
//!
//! Diagnostics are appended as the walk discovers them, starting from
//! the project's own directory and moving up. For multiple kinds
//! found at the same depth, the order within that depth is
//! `DirectoryBuildProps`, then `DirectoryBuildTargets`, then
//! `DirectoryPackagesProps` — i.e., the declaration order of
//! [`ImplicitImportKind`]. The order is deterministic and stable
//! across runs on a given filesystem.

use std::path::{Component, Path, PathBuf};

use super::diagnostic::{Diagnostic, DiagnosticKind, DiagnosticOrigin, ImplicitImportKind};

/// Walk `project_path`'s ancestor directories and return one diagnostic
/// per implicit-import file kind that exists somewhere on the chain.
///
/// `project_path` does **not** need to exist on disk — only its lexical
/// ancestry is consulted (via [`Path::ancestors`], which is purely
/// string-based). Each candidate ancestor *is* stat-ed via
/// [`Path::is_file`] to confirm the discovered file actually exists
/// and is a regular file (not e.g. a directory of the same name).
///
/// **`project_path` must be rooted**: non-rooted paths return an
/// empty vector unconditionally. With a relative path,
/// `Path::parent()` returns `Some("")` for a bare filename or
/// `Some(".")` for a `./`-prefixed name; joining a well-known
/// filename onto either yields a relative path that `is_file`
/// resolves against the *process working directory*, which is not
/// what the caller asked us to probe. We refuse to guess. The
/// pure-parsing entry point ([`parse_fsproj`](super::parse_fsproj))
/// rejects non-rooted paths outright with a [`super::ParseError`];
/// this helper is more lenient (callers may probe paths without
/// satisfying `parse_fsproj`'s contract first), but it still won't
/// stat anything cwd-relative.
///
/// `..` and `.` components in `project_path` are collapsed lexically
/// before the walk so that we only probe directories that are *real*
/// ancestors of the project. [`Path::ancestors`] is purely string-based
/// and would otherwise walk through phantom parents — e.g. given
/// `/repo/a/../b/Demo.fsproj` it would visit `/repo/a` even though the
/// project actually lives in `/repo/b`. Collapse is lexical only: we
/// do not call `canonicalize`, so we never read the filesystem to
/// follow symlinks.
///
/// At most three diagnostics are returned (one per
/// [`ImplicitImportKind`]). Returns an empty vector when none of the
/// well-known files exist on the chain (the common case for projects
/// outside a configured MSBuild repository).
pub fn detect_implicit_imports(project_path: &Path) -> Vec<Diagnostic> {
    // Collapse `.` / `..` so the ancestor walk only visits real
    // parents (see doc above). Done before the root check so a
    // path like `/a/../foo.fsproj` is still recognised as rooted.
    let normalised = normalise(project_path);
    let project_dir = match normalised.parent() {
        Some(p) => p,
        // `parent()` returns `None` only for the filesystem root
        // itself (`/`) or for an empty path. Nothing useful to walk.
        None => return Vec::new(),
    };
    // Non-rooted project_dir would force every `is_file` probe to be
    // cwd-relative. Refuse, per the doc above.
    if !project_dir.has_root() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut found_props = false;
    let mut found_targets = false;
    let mut found_packages = false;

    for ancestor in project_dir.ancestors() {
        if !found_props {
            let candidate = ancestor.join("Directory.Build.props");
            if candidate.is_file() {
                out.push(make_diagnostic(
                    candidate,
                    ImplicitImportKind::DirectoryBuildProps,
                ));
                found_props = true;
            }
        }
        if !found_targets {
            let candidate = ancestor.join("Directory.Build.targets");
            if candidate.is_file() {
                out.push(make_diagnostic(
                    candidate,
                    ImplicitImportKind::DirectoryBuildTargets,
                ));
                found_targets = true;
            }
        }
        if !found_packages {
            let candidate = ancestor.join("Directory.Packages.props");
            if candidate.is_file() {
                out.push(make_diagnostic(
                    candidate,
                    ImplicitImportKind::DirectoryPackagesProps,
                ));
                found_packages = true;
            }
        }
        if found_props && found_targets && found_packages {
            break;
        }
    }

    out
}

/// Collapse `.` and `..` components lexically (no filesystem access).
/// `..` pops the last pushed component except across the path root,
/// which `PathBuf::pop` already protects: popping an empty buffer or
/// a buffer of just `/` is a no-op. That matches what we want — `..`
/// above the root has no meaning here.
///
/// We deliberately do *not* use [`std::fs::canonicalize`]: that would
/// touch the filesystem (and fail for paths that don't exist yet),
/// whereas we only need lexical ancestors.
pub(super) fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn make_diagnostic(path: std::path::PathBuf, kind: ImplicitImportKind) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::ImplicitImportPresent { path, kind },
        // No meaningful location in the project XML — the discovery is
        // out-of-band. Callers that surface these to an editor should
        // attribute them to the `<Project>` start tag or to the project
        // file as a whole.
        span: 0..0,
        // `detect_implicit_imports` is a pre-walk pass over the entry
        // project's directory tree — it never enters an imported file
        // — so every diagnostic it produces is `Buffer`.
        origin: DiagnosticOrigin::Buffer,
    }
}

#[cfg(test)]
mod tests;
