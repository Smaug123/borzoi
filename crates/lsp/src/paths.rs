//! Lexical path helpers shared across the LSP. No filesystem access — these
//! operate purely on the path's text, so they stay usable in pure planners
//! ([`crate::publish`]) and in code that must compare paths to files that may
//! not exist on disk ([`crate::workspace`]).

use std::path::{Component, Path, PathBuf};

/// Collapse `.` and `..` path segments lexically, without touching the
/// filesystem. Unlike [`std::fs::canonicalize`] this does no IO and does not
/// resolve symlinks (the target file need not even exist). A `..` pops a
/// preceding normal segment; at the root it is dropped (`/.. == /`); a `..`
/// with no normal segment to pop in a relative remainder is kept so it still
/// climbs.
///
/// Two uses rely on this:
/// - [`crate::publish`] keeps a `#line "../Lexer.fsl"` from a generated file in
///   `obj/` resolving to the same `file:///proj/Lexer.fsl` URI the editor
///   opened, rather than a distinct `…/obj/../Lexer.fsl` resource that would
///   never union or clear with it.
/// - [`crate::workspace`] compares an open file's path against a project's
///   resolved `<Compile>` includes, which the msbuild parser passes through
///   literally (so they may not exist on disk and `canonicalize` is
///   unavailable).
pub fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // At a root/prefix, `..` has nowhere to go; otherwise (a
                // relative remainder) keep it so it still climbs.
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(Component::ParentDir),
            },
            other => out.push(other),
        }
    }
    out
}

/// Path equality honouring the platform's *default* filesystem case
/// sensitivity: case-insensitive (ASCII) on Windows and macOS,
/// case-sensitive elsewhere (Linux). This matches MSBuild/F# file identity
/// on the common configurations. Inputs are expected pre-normalised (see
/// [`lexically_normalize`]); equality then differs from `==` only in case.
///
/// It's a platform-*default* heuristic, not a per-volume probe: a
/// case-sensitive volume on macOS, or a case-insensitive mount on Linux, is
/// mishandled. Closing that needs a filesystem probe and is not worth the
/// complexity here. Folding is ASCII-only — full Unicode case folding is out
/// of scope and beyond what F# project filenames need in practice.
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    } else {
        a == b
    }
}

/// A hashable canonical key for a *pre-normalised* path that agrees with
/// [`paths_equal`]: two paths share a key **iff** `paths_equal` holds. The key
/// is the path's text, ASCII-lowercased on case-insensitive platforms
/// (Windows/macOS) and left as-is elsewhere (Linux). Use it to drive a
/// `HashSet`/`HashMap` dedup where an O(n²) pairwise `paths_equal` scan would be
/// too slow — e.g. de-duplicating the workspace-wide file set
/// ([`crate::handlers::workspace_diagnostic`]).
///
/// Like `paths_equal`, folding is ASCII-only and the platform-default
/// case-sensitivity heuristic applies. The text is taken via
/// `to_string_lossy`, so a path with invalid UTF-8 keys on its lossy form;
/// for the F# project paths this serves that never arises in practice.
pub fn path_dedup_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(any(windows, target_os = "macos")) {
        text.to_ascii_lowercase()
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defining property: `path_dedup_key` agrees with `paths_equal` for
    /// every pair — same key exactly when the paths are equal under the
    /// platform's case rules.
    #[test]
    fn dedup_key_agrees_with_paths_equal() {
        let paths = [
            "/proj/Lib.fs",
            "/proj/lib.fs",
            "/proj/LIB.fs",
            "/proj/Other.fs",
            "/proj/sub/Lib.fs",
        ];
        for a in paths {
            for b in paths {
                let pa = Path::new(a);
                let pb = Path::new(b);
                assert_eq!(
                    path_dedup_key(pa) == path_dedup_key(pb),
                    paths_equal(pa, pb),
                    "key/equality disagree for {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn dedup_key_folds_case_per_platform() {
        let upper = path_dedup_key(Path::new("/proj/Lib.fs"));
        let lower = path_dedup_key(Path::new("/proj/lib.fs"));
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(upper, lower, "case-insensitive platforms fold the key");
        } else {
            assert_ne!(upper, lower, "case-sensitive platforms keep distinct keys");
        }
    }
}
