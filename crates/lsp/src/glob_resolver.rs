//! Filesystem-backed MSBuild glob resolver — the imperative shell around
//! the pure `crate::glob` matcher.
//!
//! `borzoi-msbuild` stays filesystem-free (gospel "dependency
//! rejection"): its evaluator routes any globbing/excluding item element
//! through a caller-supplied [`borzoi_msbuild::GlobResolver`]. This module *is* that
//! resolver for the LSP — for each wildcard fragment it enumerates the
//! filesystem under the fragment's fixed (wildcard-free) prefix, drives
//! `crate::glob::select`, passes literal fragments through unchanged, and
//! joins everything in MSBuild's document order.
//!
//! ## Semantics (pinned against real `dotnet msbuild`)
//!
//! For each `;`-separated `Include` fragment, in document order:
//!
//! - A **literal** fragment is emitted regardless of whether the file
//!   exists (MSBuild passes literal includes through unconditionally); an
//!   `Exclude` that matches it still removes it.
//! - A **wildcard** fragment is expanded against the filesystem enumerated
//!   under its fixed (wildcard-free) prefix, with excludes applied, sorted
//!   lexicographically within the fragment (our deterministic stand-in for
//!   MSBuild's filesystem enumeration order — see the `crate::glob` module).
//!   The prefix may climb out of the project (`../shared/*.fs`) or be
//!   absolute (`/opt/lib/*.fs`).
//! - **Excludes** are matched in the same absolute frame as the candidates:
//!   a relative exclude is anchored at the project directory, an absolute
//!   one kept as-is. So a relative include with an absolute exclude (e.g.
//!   `$(MSBuildProjectDirectory)/Gen.fs`) — or the mirror — filters
//!   correctly rather than silently missing.
//!
//! Fragments are concatenated in document order and **not** deduplicated:
//! MSBuild keeps duplicates from overlapping fragments (e.g. `a.fs;*.fs`
//! lists `a.fs` twice), and so do we.
//!
//! ## Correctness envelope
//!
//! - Recursive (`**`) globs descend at most `MAX_GLOB_DEPTH` components
//!   to bound runaway enumeration; deeper files are not matched.
//! - Symlinked **files** are enumerated (MSBuild includes them); symlinked
//!   **directories** are not recursed into (bounds symlink-cycle blowups).

use std::path::{Path, PathBuf};

use borzoi_msbuild::GlobRequest;

use crate::glob::{Pattern, select, split_glob_root, split_segments};

/// Maximum number of path components a recursive (`**`) glob descends.
const MAX_GLOB_DEPTH: usize = 64;

/// Expand one [`GlobRequest`] into the ordered list of absolute paths to
/// splice as items. Suitable as a [`borzoi_msbuild::GlobResolver`].
pub fn resolve(req: &GlobRequest<'_>) -> Vec<PathBuf> {
    // The base directory is a *literal* filesystem path (it may legitimately
    // contain `*`/`?` on a case-sensitive Unix filesystem), so it is split
    // into literal segments once and never parsed as a glob. Every relative
    // fragment is matched as `<base literal segments> ++ <fragment glob>`;
    // an absolute fragment ignores the base entirely. All matching then
    // happens in this single absolute frame, so a relative include with an
    // absolute exclude — e.g. `Include="**/*.fs"
    // Exclude="$(MSBuildProjectDirectory)/Gen.fs"` — and its mirror compare
    // correctly, while a literal `*` in the base path cannot turn into a
    // wildcard that matches sibling directories.
    let base_norm = req.base_dir.to_string_lossy().replace('\\', "/");
    let base_segs = split_segments(&base_norm);

    // Anchor every exclude into that frame: an absolute exclude keeps its
    // path (its own wildcards remain wildcards), a relative one is rooted at
    // the project directory with the base kept literal.
    let excludes: Vec<Pattern> = req
        .excludes
        .iter()
        .map(|e| {
            let norm = e.replace('\\', "/");
            if norm.starts_with('/') {
                Pattern::parse(&norm)
            } else {
                Pattern::with_literal_prefix(&base_segs, &norm)
            }
        })
        .collect();

    let mut out: Vec<PathBuf> = Vec::new();
    // The evaluator already trimmed/$()-expanded each fragment and stripped
    // `@()`/`%()` references, so each is a literal or wildcard path.
    for frag in req
        .include
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let norm = frag.replace('\\', "/");
        let absolute = norm.starts_with('/');
        // Split only the *fragment* at its first wildcard, then prepend the
        // literal base (for relative specs) to get the walk root and key
        // prefix. The base is never fed to `split_glob_root`, so its `*`/`?`
        // never act as a wildcard boundary.
        let frag_root = split_glob_root(&norm);
        let anchor_segs: Vec<String> = if absolute {
            frag_root.prefix.clone()
        } else {
            base_segs
                .iter()
                .map(|s| s.to_string())
                .chain(frag_root.prefix.iter().cloned())
                .collect()
        };
        // The pattern keeps the base literal for relative specs; an absolute
        // fragment is parsed on its own (its own wildcards are wildcards).
        let pat = if absolute {
            Pattern::parse(&norm)
        } else {
            Pattern::with_literal_prefix(&base_segs, &norm)
        };
        if pat.is_glob() {
            // Root the walk at the literal anchor so `../shared/*.fs`
            // enumerates the sibling directory and `/opt/lib/*.fs` the
            // absolute one. `anchor_segs` is absolute (or empty → `/`).
            let walk_root = abs_from_segments(&anchor_segs);
            // A `**`-free tail matches files at one fixed depth below the
            // walk root, so there is no point descending further.
            let depth = frag_root.tail_depth.unwrap_or(MAX_GLOB_DEPTH);
            // Re-attach the literal prefix to each enumerated path so keys,
            // include pattern, and excludes all live in one frame. `select`
            // matches/orders them and returns leading-`/`-stripped absolute
            // strings (its `split_segments` drops the empty root segment),
            // which we re-root at `/`.
            let prefix = anchor_segs.join("/");
            let keys: Vec<String> = enumerate_files(&walk_root, depth)
                .into_iter()
                .map(|rel| {
                    if prefix.is_empty() {
                        rel
                    } else {
                        format!("{prefix}/{rel}")
                    }
                })
                .collect();
            let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            for key in select(&key_refs, std::slice::from_ref(&pat), &excludes) {
                out.push(Path::new("/").join(&key));
            }
        } else {
            // Literal passthrough: MSBuild emits a literal include whether
            // or not the file exists; only a matching Exclude removes it. The
            // exclude check runs against the same absolute-frame string
            // (`anchor_segs` joined); the emitted path reproduces it as a
            // `PathBuf` via `base_dir.join` (an absolute `norm` replaces the
            // base, a relative one extends it).
            let key = anchor_segs.join("/");
            if !excludes.iter().any(|e| e.matches(&key)) {
                out.push(req.base_dir.join(&norm));
            }
        }
    }
    out
}

/// Build an absolute path from a glob root's leading wildcard-free segments
/// (separators already split out). An empty prefix yields the filesystem
/// root; `..` segments are pushed literally, not resolved.
///
/// The walk root is rooted at the POSIX filesystem root `/`. Drive-letter /
/// UNC Windows roots are deliberately out of scope: this LSP targets
/// case-sensitive Unix-like agent environments (see the `glob` module), so a
/// `C:`-style prefix segment would be re-rooted under `/` rather than
/// preserved. Supporting Windows roots would mean modelling drive/UNC path
/// semantics we have no environment to validate against.
fn abs_from_segments(prefix: &[String]) -> PathBuf {
    let mut p = PathBuf::from("/");
    p.extend(prefix);
    p
}

/// Recursively list files under `base`, returning their `/`-joined paths
/// relative to `base`. A file at depth `d` (i.e. with `d` path segments)
/// is included only when `d <= max_depth`. Symlinked files are enumerated
/// but directory symlinks are not recursed into, and unreadable directories
/// are skipped (see the module's correctness envelope).
fn enumerate_files(base: &Path, max_depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    walk(base, "", 0, max_depth, &mut out);
    out
}

fn walk(dir: &Path, prefix: &str, depth: usize, max_depth: usize, out: &mut Vec<String>) {
    if depth >= max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type` does not follow symlinks.
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let rel = if prefix.is_empty() {
            name.into_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if file_type.is_file() {
            out.push(rel);
        } else if file_type.is_dir() {
            walk(&entry.path(), &rel, depth + 1, max_depth, out);
        } else if file_type.is_symlink() {
            // MSBuild's wildcard expansion includes symlinked *files*, so
            // follow the link (`fs::metadata`, unlike `file_type`, resolves
            // it) and emit it when the target is a file. Symlinked
            // directories are deliberately *not* recursed into — that bounds
            // symlink-cycle blowups (see the module's correctness envelope).
            if std::fs::metadata(entry.path())
                .map(|m| m.is_file())
                .unwrap_or(false)
            {
                out.push(rel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;
    use std::collections::BTreeSet;
    use std::fs;
    use tempfile::TempDir;

    /// Create the file tree used by the probe-derived unit tests, mirroring
    /// the layout I measured `dotnet msbuild` against:
    /// `a.fs b.fs m.fs z.fs sub/c.fs sub/deep/d.fs sub/e.fsi`.
    fn fixture() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        for f in ["a.fs", "b.fs", "m.fs", "z.fs"] {
            fs::write(base.join(f), b"// f\n").unwrap();
        }
        fs::create_dir_all(base.join("sub/deep")).unwrap();
        fs::write(base.join("sub/c.fs"), b"// f\n").unwrap();
        fs::write(base.join("sub/deep/d.fs"), b"// f\n").unwrap();
        fs::write(base.join("sub/e.fsi"), b"// f\n").unwrap();
        tmp
    }

    /// Run the resolver against a base dir with the given include spec and
    /// excludes, returning paths relative to `base` as `/`-joined strings
    /// for compact assertions.
    fn run_rel(base: &Path, include: &str, excludes: &[&str]) -> Vec<String> {
        let excludes: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();
        let req = GlobRequest {
            base_dir: base,
            include,
            excludes: &excludes,
        };
        resolve(&req)
            .into_iter()
            .map(|p| {
                p.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    // ----- Probe-derived ground-truth unit tests (expected values are the
    // observed `dotnet msbuild -getItem:Compile` outputs) -----

    #[test]
    fn recursive_glob_matches_all_fs_sorted() {
        let tmp = fixture();
        assert_eq!(
            run_rel(tmp.path(), "**/*.fs", &[]),
            ["a.fs", "b.fs", "m.fs", "sub/c.fs", "sub/deep/d.fs", "z.fs"]
        );
    }

    #[test]
    fn top_level_star_is_not_recursive() {
        let tmp = fixture();
        assert_eq!(
            run_rel(tmp.path(), "*.fs", &[]),
            ["a.fs", "b.fs", "m.fs", "z.fs"]
        );
    }

    #[test]
    fn nested_star_is_single_level() {
        let tmp = fixture();
        // `sub/*.fs` is depth-2: it matches `sub/c.fs` but not the
        // depth-3 `sub/deep/d.fs`.
        assert_eq!(run_rel(tmp.path(), "sub/*.fs", &[]), ["sub/c.fs"]);
    }

    #[test]
    fn fsi_is_not_matched_by_fs_glob() {
        let tmp = fixture();
        assert_eq!(run_rel(tmp.path(), "sub/*.fsi", &[]), ["sub/e.fsi"]);
        // and *.fs must not pick up the .fsi
        assert!(!run_rel(tmp.path(), "**/*.fs", &[]).contains(&"sub/e.fsi".to_string()));
    }

    #[test]
    fn literal_then_glob_keeps_duplicate() {
        let tmp = fixture();
        // MSBuild: `a.fs;*.fs` -> [a.fs, a.fs, b.fs, m.fs, z.fs].
        assert_eq!(
            run_rel(tmp.path(), "a.fs;*.fs", &[]),
            ["a.fs", "a.fs", "b.fs", "m.fs", "z.fs"]
        );
    }

    #[test]
    fn two_overlapping_globs_keep_duplicates() {
        let tmp = fixture();
        // MSBuild: `*.fs;?.fs` -> both expansions concatenated.
        assert_eq!(
            run_rel(tmp.path(), "*.fs;?.fs", &[]),
            [
                "a.fs", "b.fs", "m.fs", "z.fs", "a.fs", "b.fs", "m.fs", "z.fs"
            ]
        );
    }

    #[test]
    fn literal_passthrough_does_not_check_existence() {
        let tmp = fixture();
        // MSBuild includes a non-existent literal verbatim.
        assert_eq!(run_rel(tmp.path(), "ghost.fs", &[]), ["ghost.fs"]);
    }

    #[test]
    fn glob_with_no_match_is_empty() {
        let tmp = fixture();
        assert!(run_rel(tmp.path(), "ghost*.fs", &[]).is_empty());
    }

    #[test]
    fn exclude_filters_literal_but_not_ghost() {
        let tmp = fixture();
        // MSBuild: `a.fs;b.fs;ghost.fs` Exclude `a.fs` -> [b.fs, ghost.fs].
        assert_eq!(
            run_rel(tmp.path(), "a.fs;b.fs;ghost.fs", &["a.fs"]),
            ["b.fs", "ghost.fs"]
        );
    }

    #[test]
    fn exclude_glob_removes_subtree() {
        let tmp = fixture();
        // MSBuild: `**/*.fs` Exclude `sub/**/*.fs` -> top-level only.
        assert_eq!(
            run_rel(tmp.path(), "**/*.fs", &["sub/**/*.fs"]),
            ["a.fs", "b.fs", "m.fs", "z.fs"]
        );
    }

    #[test]
    fn paths_are_joined_onto_base_dir() {
        let tmp = fixture();
        let excludes: Vec<String> = Vec::new();
        let req = GlobRequest {
            base_dir: tmp.path(),
            include: "a.fs",
            excludes: &excludes,
        };
        assert_eq!(resolve(&req), vec![tmp.path().join("a.fs")]);
    }

    // ----- Globs rooted outside the project directory -----

    fn canon(p: impl AsRef<Path>) -> PathBuf {
        fs::canonicalize(p.as_ref())
            .unwrap_or_else(|e| panic!("canonicalize {}: {e}", p.as_ref().display()))
    }

    /// Resolve and canonicalise the results, so `..`/`/private` differences
    /// don't defeat equality when the glob root is outside `base`.
    fn resolve_canon(base: &Path, include: &str, excludes: &[&str]) -> Vec<PathBuf> {
        let excludes: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();
        let req = GlobRequest {
            base_dir: base,
            include,
            excludes: &excludes,
        };
        resolve(&req).into_iter().map(canon).collect()
    }

    #[test]
    fn parent_relative_glob_resolves_sibling_directory() {
        // tmp/{proj, shared/{x.fs, y.fs}}; base = proj; `../shared/*.fs`
        // must enumerate the sibling `shared` dir, not the project dir.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        let shared = tmp.path().join("shared");
        fs::create_dir_all(&proj).unwrap();
        fs::create_dir_all(&shared).unwrap();
        for f in ["x.fs", "y.fs"] {
            fs::write(shared.join(f), b"// f\n").unwrap();
        }
        assert_eq!(
            resolve_canon(&proj, "../shared/*.fs", &[]),
            [canon(shared.join("x.fs")), canon(shared.join("y.fs"))]
        );
    }

    #[test]
    fn parent_relative_glob_honours_exclude() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        let shared = tmp.path().join("shared");
        fs::create_dir_all(&proj).unwrap();
        fs::create_dir_all(&shared).unwrap();
        for f in ["x.fs", "y.fs"] {
            fs::write(shared.join(f), b"// f\n").unwrap();
        }
        assert_eq!(
            resolve_canon(&proj, "../shared/*.fs", &["../shared/y.fs"]),
            [canon(shared.join("x.fs"))]
        );
    }

    #[test]
    fn parent_relative_glob_absolute_exclude_does_not_cross_match() {
        // MSBuild matches an Exclude against the Include's items in the
        // Include's own (project-relative, `..`-preserving) frame, *not* by
        // collapsing to a canonical absolute path. So an absolute exclude
        // does NOT cross-match a `../shared/*.fs` include — both siblings
        // survive. This is verified against real `dotnet msbuild` by
        // `parent_relative_glob_absolute_exclude_is_noop` in the
        // glob_msbuild_diff oracle; the contrast (a same-frame relative
        // exclude *does* filter) is `parent_relative_glob_honours_exclude`
        // above.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        let shared = tmp.path().join("shared");
        fs::create_dir_all(&proj).unwrap();
        fs::create_dir_all(&shared).unwrap();
        for f in ["x.fs", "y.fs"] {
            fs::write(shared.join(f), b"// f\n").unwrap();
        }
        let abs_excl = shared.join("y.fs").to_string_lossy().into_owned();
        assert_eq!(
            resolve_canon(&proj, "../shared/*.fs", &[&abs_excl]),
            [canon(shared.join("x.fs")), canon(shared.join("y.fs"))]
        );
    }

    // A `*` in a directory name is only legal on Unix-like filesystems, so
    // this fixture (and its oracle mirror) is gated accordingly.
    #[cfg(unix)]
    #[test]
    fn base_dir_wildcard_is_literal() {
        // A project directory whose name contains a glob metacharacter (`*`,
        // legal on Unix) must be treated literally: `*.fs` enumerates the
        // real `a*b` directory and must not let the `*` in the name match a
        // sibling `axb`. Folding base_dir into the glob string would
        // over-match the sibling. Pinned against `dotnet msbuild` by
        // `base_dir_wildcard_is_literal_not_glob` in the glob_msbuild_diff
        // oracle.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("a*b");
        let sibling = tmp.path().join("axb");
        fs::create_dir_all(&proj).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        fs::write(proj.join("real.fs"), b"// f\n").unwrap();
        fs::write(sibling.join("decoy.fs"), b"// f\n").unwrap();
        assert_eq!(
            resolve_canon(&proj, "*.fs", &[]),
            [canon(proj.join("real.fs"))]
        );
    }

    #[cfg(unix)]
    #[test]
    fn absolute_glob_resolves_against_filesystem_root() {
        // An absolute include glob is rooted at the filesystem root, not the
        // project directory.
        let src = TempDir::new().unwrap();
        for f in ["a.fs", "b.fs"] {
            fs::write(src.path().join(f), b"// f\n").unwrap();
        }
        let proj = TempDir::new().unwrap();
        let include = format!("{}/*.fs", src.path().display());
        assert_eq!(
            resolve_canon(proj.path(), &include, &[]),
            [
                canon(src.path().join("a.fs")),
                canon(src.path().join("b.fs"))
            ]
        );
    }

    // ----- Symlinks -----

    #[cfg(unix)]
    #[test]
    fn symlinked_source_file_is_included() {
        // MSBuild's wildcard expansion includes a symlinked file; the walk
        // must follow the link to classify the target as a file.
        use std::os::unix::fs::symlink;
        let target_dir = TempDir::new().unwrap();
        let target = target_dir.path().join("real.fs");
        fs::write(&target, b"// f\n").unwrap();
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        symlink(&target, base.join("linked.fs")).unwrap();
        fs::write(base.join("a.fs"), b"// f\n").unwrap();
        assert_eq!(run_rel(base, "*.fs", &[]), ["a.fs", "linked.fs"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_is_not_recursed() {
        // Directory symlinks are deliberately not recursed (cycle
        // protection), so a recursive glob does not pull files through them.
        use std::os::unix::fs::symlink;
        let ext = TempDir::new().unwrap();
        fs::write(ext.path().join("hidden.fs"), b"// f\n").unwrap();
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        symlink(ext.path(), base.join("linkdir")).unwrap();
        fs::write(base.join("a.fs"), b"// f\n").unwrap();
        assert_eq!(run_rel(base, "**/*.fs", &[]), ["a.fs"]);
    }

    // ----- Exclude anchoring (relative vs absolute frames) -----

    #[test]
    fn relative_glob_absolute_exclude_is_honoured() {
        // A relative recursive Include with an *absolute* Exclude — exactly
        // what MSBuild hands us from `$(MSBuildProjectDirectory)/sub/c.fs`.
        // The candidates are enumerated base-relative, so the exclude must be
        // anchored into the same (absolute) frame or it silently fails to
        // filter.
        let tmp = fixture();
        let abs_excl = tmp.path().join("sub/c.fs").to_string_lossy().into_owned();
        assert_eq!(
            run_rel(tmp.path(), "**/*.fs", &[&abs_excl]),
            ["a.fs", "b.fs", "m.fs", "sub/deep/d.fs", "z.fs"]
        );
    }

    #[test]
    fn absolute_glob_relative_exclude_is_honoured() {
        // The mirror image: an absolute Include glob with a relative Exclude.
        // MSBuild anchors the relative exclude at the project directory, so
        // it only filters when the absolute glob root *is* that directory.
        let tmp = fixture();
        let include = format!("{}/*.fs", tmp.path().display());
        assert_eq!(
            run_rel(tmp.path(), &include, &["b.fs"]),
            ["a.fs", "m.fs", "z.fs"]
        );
    }

    // ----- Independent naive reference for the property test -----

    fn split_norm(s: &str) -> Vec<String> {
        s.split(['/', '\\'])
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_string)
            .collect()
    }

    fn seg_naive(pat: &[char], s: &[char]) -> bool {
        match pat.split_first() {
            None => s.is_empty(),
            Some((&'*', r)) => (0..=s.len()).any(|i| seg_naive(r, &s[i..])),
            Some((&'?', r)) => !s.is_empty() && seg_naive(r, &s[1..]),
            Some((&c, r)) => s.first() == Some(&c) && seg_naive(r, &s[1..]),
        }
    }

    fn path_naive(pat: &[String], path: &[String]) -> bool {
        match pat.split_first() {
            None => path.is_empty(),
            Some((p, r)) if p == "**" => (0..=path.len()).any(|i| path_naive(r, &path[i..])),
            Some((p, r)) => {
                !path.is_empty() && {
                    let pc: Vec<char> = p.chars().collect();
                    let sc: Vec<char> = path[0].chars().collect();
                    seg_naive(&pc, &sc) && path_naive(r, &path[1..])
                }
            }
        }
    }

    fn naive_match(pattern: &str, path: &str) -> bool {
        path_naive(&split_norm(pattern), &split_norm(path))
    }

    /// Independent reference resolver over a *known* file set (not the
    /// filesystem walk), using the naive matcher. Agreement with `resolve`
    /// validates the filesystem enumeration, ordering, dedup, literal
    /// passthrough, and exclude application together.
    fn reference(
        base: &Path,
        known: &BTreeSet<String>,
        include: &str,
        excludes: &[String],
    ) -> Vec<PathBuf> {
        let is_glob = |f: &str| f.contains('*') || f.contains('?');
        let excluded = |path: &str| excludes.iter().any(|e| naive_match(e, path));
        let mut out = Vec::new();
        for frag in include.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            if is_glob(frag) {
                let mut m: Vec<String> = known
                    .iter()
                    .filter(|p| naive_match(frag, p) && !excluded(p))
                    .map(|p| split_norm(p).join("/"))
                    .collect();
                m.sort();
                out.extend(m.into_iter().map(|p| base.join(p)));
            } else {
                let n = frag.replace('\\', "/");
                if !excluded(&n) {
                    out.push(base.join(&n));
                }
            }
        }
        out
    }

    // ----- Generators -----

    /// A relative file path: 1–3 directory levels over {d,e}, file named
    /// from {a,b,c} with a {.fs,.fsi} extension.
    fn rel_path() -> impl Strategy<Value = String> {
        (
            prop::collection::vec(prop::sample::select(vec!["d", "e"]), 0..=2),
            prop::sample::select(vec!["a", "b", "c"]),
            prop::sample::select(vec![".fs", ".fsi"]),
        )
            .prop_map(|(dirs, name, ext)| {
                let mut parts = dirs;
                parts.push(name);
                let mut p = parts.join("/");
                p.push_str(ext);
                p
            })
    }

    /// Pool of glob fragments exercising `*`, `?`, `**`, and nesting.
    fn glob_frag() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "*.fs",
            "*.fsi",
            "?.fs",
            "**/*.fs",
            "**/*",
            "d/*.fs",
            "d/**/*.fs",
            "e/*.fsi",
        ])
        .prop_map(str::to_string)
    }

    /// A ghost literal naming a file that will not exist in the tree.
    fn ghost_frag() -> impl Strategy<Value = String> {
        prop::sample::select(vec!["ghost.fs", "no/such.fs", "d/missing.fsi"])
            .prop_map(str::to_string)
    }

    // ----- Property test -----

    /// One generated scenario: a file tree, an include spec, and excludes.
    #[derive(Debug)]
    struct Scenario {
        files: BTreeSet<String>,
        include: String,
        excludes: Vec<String>,
    }

    fn scenario() -> impl Strategy<Value = Scenario> {
        prop::collection::vec(rel_path(), 1..=8).prop_flat_map(|paths| {
            let files: BTreeSet<String> = paths.into_iter().collect();
            let known: Vec<String> = files.iter().cloned().collect();
            // include fragment pool: known literal, ghost literal, or glob.
            let known_for_incl = known.clone();
            let incl_frag = prop_oneof![
                3 => glob_frag(),
                2 => prop::sample::select(known_for_incl),
                1 => ghost_frag(),
            ];
            let include = prop::collection::vec(incl_frag, 1..=3).prop_map(|fs| fs.join(";"));
            let known_for_excl = known.clone();
            let excl_frag = prop_oneof![
                2 => glob_frag(),
                2 => prop::sample::select(known_for_excl),
            ];
            let excludes = prop::collection::vec(excl_frag, 0..=2);
            (Just(files), include, excludes).prop_map(|(files, include, excludes)| Scenario {
                files,
                include,
                excludes,
            })
        })
    }

    /// Materialise a scenario's files into a fresh tempdir.
    fn materialise(files: &BTreeSet<String>) -> TempDir {
        let tmp = TempDir::new().unwrap();
        for f in files {
            let full = tmp.path().join(f);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, b"// f\n").unwrap();
        }
        tmp
    }

    #[test]
    fn resolver_agrees_with_reference_over_random_trees() {
        let mut runner = TestRunner::default();
        let strat = scenario();
        let n = 256;
        let (mut nonempty, mut excl_removed, mut had_dup, mut ghost_passed) = (0, 0, 0, 0);
        for _ in 0..n {
            let sc = strat.new_tree(&mut runner).unwrap().current();
            let tmp = materialise(&sc.files);
            let req = GlobRequest {
                base_dir: tmp.path(),
                include: &sc.include,
                excludes: &sc.excludes,
            };
            let got = resolve(&req);
            let want = reference(tmp.path(), &sc.files, &sc.include, &sc.excludes);
            assert_eq!(
                got, want,
                "include={:?} excludes={:?} files={:?}",
                sc.include, sc.excludes, sc.files
            );

            // Instrumentation.
            if !got.is_empty() {
                nonempty += 1;
            }
            let without = reference(tmp.path(), &sc.files, &sc.include, &[]);
            if without.len() > want.len() {
                excl_removed += 1;
            }
            let unique: BTreeSet<&PathBuf> = got.iter().collect();
            if unique.len() < got.len() {
                had_dup += 1;
            }
            if sc.include.split(';').any(|f| f == "ghost.fs")
                && got.iter().any(|p| p.ends_with("ghost.fs"))
            {
                ghost_passed += 1;
            }
        }
        // The agreement check is only meaningful if the generator explores
        // each interesting regime. Thresholds chosen far below the expected
        // counts so the false-positive rate is well under 1e-11.
        assert!(nonempty >= 60, "too few non-empty results: {nonempty}/{n}");
        assert!(
            excl_removed >= 20,
            "excludes rarely removed anything: {excl_removed}/{n}"
        );
        assert!(had_dup >= 10, "duplicates rarely arose: {had_dup}/{n}");
        assert!(
            ghost_passed >= 5,
            "ghost literals rarely passed through: {ghost_passed}/{n}"
        );
    }
}
