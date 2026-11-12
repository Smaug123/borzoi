//! Filesystem-backed MSBuild glob resolver — the imperative shell around
//! the pure `crate::glob` matcher.
//!
//! The evaluator never touches the filesystem to glob (gospel "dependency
//! rejection"): it routes any globbing/excluding item element through a
//! caller-supplied [`crate::GlobResolver`]. This module is the resolver
//! every caller in the workspace passes — the LSP, and the MSBuild
//! differentials that must test exactly what the LSP runs, the same way
//! [`crate::resolve_sdk`] is the shipped `SdkResolver`. For each wildcard
//! fragment it enumerates the
//! filesystem under the fragment's fixed (wildcard-free) prefix, drives
//! `crate::glob::select`, passes literal fragments through unchanged, and
//! joins everything in MSBuild's document order.
//!
//! ## Semantics (pinned against real `dotnet msbuild`)
//!
//! For each `Include` fragment, in document order:
//!
//! - Fragments arrive split and still escaped ([`SpecFragment`]). Whether
//!   one is a glob is MSBuild's classification of its escaped text
//!   ([`SpecFragment::is_wildcard`]), so an escaped `%2a` is a star in a
//!   filename and `a%2a*.fs` is the literal `a**.fs`; each is decoded here,
//!   after that, and never split again, so an escaped `%3b` is a `;` in a
//!   filename.
//! - A **literal** fragment is emitted regardless of whether the file
//!   exists (MSBuild passes literal includes through unconditionally); an
//!   `Exclude` that matches it still removes it.
//! - A **wildcard** fragment is expanded against the filesystem enumerated
//!   under its fixed (wildcard-free) prefix, with excludes applied, in
//!   MSBuild's `OrdinalIgnoreCase` order within the fragment (see the
//!   `crate::glob` module). The prefix may climb out of the project
//!   (`../shared/*.fs`) or be absolute (`/opt/lib/*.fs`).
//! - **Excludes** are matched in the same absolute frame as the candidates:
//!   a relative exclude is anchored at the project directory, an absolute
//!   one kept as-is. So a relative include with an absolute exclude (e.g.
//!   `$(MSBuildProjectDirectory)/Gen.fs`) — or the mirror — filters
//!   correctly rather than silently missing. An exclude carrying an escaped
//!   wildcard applies differently to the two kinds of include, as MSBuild's
//!   does: a name against a literal, a decoded pattern against a glob
//!   ([`SpecFragment::excludes_from_globs`]).
//!
//! Fragments are concatenated in document order and **not** deduplicated:
//! MSBuild keeps duplicates from overlapping fragments (e.g. `a.fs;*.fs`
//! lists `a.fs` twice), and so do we.
//!
//! ## Correctness envelope
//!
//! The resolver commits a list only when it is MSBuild's list; otherwise it
//! returns a [`GlobDecline`] and the evaluator marks the item set uncertain.
//!
//! - **An allow-list, not a deny-list.** MSBuild's matchers carry Windows
//!   filename rules, UTF-16 widths and .NET case folding that the plain
//!   matching here does not model. Rather than chase each, the resolver
//!   commits only when every spec it matches against, and every name a glob
//!   would match, is inside the shape the MSBuild differentials verify
//!   (`glob::unmodelled_shape`); anything else declines
//!   ([`GlobDecline::OutsideModelledShape`]), so a shape nobody has checked
//!   cannot commit a wrong list. Within it, the pattern rules MSBuild's
//!   source states are ported: legality ([`GlobDecline::IllegalFilespec`]),
//!   the all-files `*.*`, and how a literal include meets an exclude.
//! - Recursive (`**`) globs descend at most `MAX_GLOB_DEPTH` components
//!   to bound runaway enumeration; meeting a directory at that depth
//!   declines ([`GlobDecline::DepthLimit`]).
//! - Symlinked **files** and **directories** are followed, as MSBuild
//!   follows them (probed), and listed under the link's name. A link back to
//!   a directory already on the walk's path is a cycle, which declines
//!   ([`GlobDecline::SymlinkCycle`]) rather than guess what MSBuild does.
//! - Selection and order decline where ASCII case or a non-ASCII name would
//!   decide them (see `crate::glob`).

use std::path::{Path, PathBuf};

use crate::{GlobRequest, SpecFragment, Unmodelled};

use crate::glob::{
    GlobDecline, Pattern, compares_unnormalised, has_wildcard_directory_part, is_legal_filespec,
    is_recursive_filename_pattern, select, split_filespec, split_glob_root, split_segments,
    unmodelled_glob_structure, unmodelled_shape,
};

/// Maximum number of path components a recursive (`**`) glob descends.
const MAX_GLOB_DEPTH: usize = 64;

/// Expand one [`GlobRequest`] into the ordered list of absolute paths to
/// splice as items — exactly MSBuild's list, or a [`GlobDecline`] saying why
/// it cannot be reproduced. Suitable as a [`crate::GlobResolver`].
pub fn resolve(req: &GlobRequest<'_>) -> Result<Vec<PathBuf>, GlobDecline> {
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

    // The allow-list: every spec the resolver matches anything against, and
    // every name a glob would match, must be in the shape it models
    // (`glob::unmodelled_shape`); anything else declines. A literal include
    // with no excludes is matched against nothing, so only it goes unchecked.
    for exclude in req.excludes {
        within_modelled_shape(&exclude.unescape(), &base_norm)?;
    }

    // A glob include is matched by MSBuild's file matcher, which is handed
    // the excludes `SpecFragment::excludes_from_globs` keeps, each decoded and
    // then read as a pattern (every decoded `*`/`?` a wildcard) with the
    // all-files reading of `*.*`. An absolute exclude keeps its path; a
    // relative one is rooted at the project directory with the base kept
    // literal. The list is consulted only by a glob include, so what it
    // cannot model declines only there: a filespec the matcher refuses, and
    // an exclude with a `.` segment, which the matcher compares by spelling
    // and `Pattern` would drop (`GlobDecline::CurDirSegmentExclude`).
    let glob_excludes: Result<Vec<Pattern>, GlobDecline> = req
        .excludes
        .iter()
        .filter(|e| e.excludes_from_globs())
        .map(|e| {
            let norm = e.unescape().replace('\\', "/");
            if !is_legal_filespec(&norm) {
                return Err(GlobDecline::IllegalFilespec { pattern: norm });
            }
            if has_cur_dir_segment(&norm) {
                return Err(GlobDecline::CurDirSegmentExclude { pattern: norm });
            }
            within_modelled_glob_structure(&norm)?;
            Ok(
                Pattern::with_literal_prefix(anchor_prefix(&base_segs, &norm), &norm)
                    .with_all_files_rule(),
            )
        })
        .collect();
    // The glob excludes with a wildcard directory part, each with whether it
    // is absolute. Such an exclude must be anchored as the glob include
    // it filters is (`Unmodelled::MixedAnchoring`).
    let glob_exclude_anchoring: Vec<(String, bool)> = req
        .excludes
        .iter()
        .filter(|e| e.excludes_from_globs())
        .map(|e| e.unescape().replace('\\', "/"))
        .filter(|norm| has_wildcard_directory_part(norm))
        .map(|norm| {
            let absolute = norm.starts_with('/');
            (norm, absolute)
        })
        .collect();

    let mut out: Vec<PathBuf> = Vec::new();
    // The evaluator already split, trimmed and $()-expanded each fragment
    // and stripped `@()`/`%()` references, so each is a literal or wildcard
    // path, classified on its escaped text and decoded here.
    for frag in req.include {
        let raw = frag.unescape();
        let norm = raw.replace('\\', "/");
        let absolute = norm.starts_with('/');
        if !frag.is_wildcard() {
            // Literal passthrough: MSBuild emits a literal include whether
            // or not the file exists; only a matching Exclude removes it. The
            // emitted path is `base_dir.join` (an absolute `norm` replaces the
            // base, a relative one extends it). Every character of `norm` is
            // data, a decoded `%2a` star included.
            if !req.excludes.is_empty() {
                within_modelled_shape(&raw, &base_norm)?;
            }
            if literal_survives_excludes(&raw, req.excludes, &base_segs)? {
                out.push(req.base_dir.join(&norm));
            }
            continue;
        }
        within_modelled_shape(&raw, &base_norm)?;
        if !is_legal_filespec(&norm) {
            return Err(GlobDecline::IllegalFilespec { pattern: norm });
        }
        let glob_excludes = glob_excludes.as_ref().map_err(Clone::clone)?;
        // A wildcard directory segment is verified on an include alone
        // (`*/*.fs`), not where it meets excludes.
        if !glob_excludes.is_empty() {
            within_modelled_glob_structure(&norm)?;
        }
        if let Some((exclude, _)) = glob_exclude_anchoring
            .iter()
            .find(|(_, exclude_absolute)| *exclude_absolute != absolute)
        {
            return Err(GlobDecline::OutsideModelledShape {
                spec: exclude.clone(),
                shape: Unmodelled::MixedAnchoring,
            });
        }
        if !glob_excludes.is_empty() && has_cur_dir_segment(&norm) {
            return Err(GlobDecline::CurDirSegmentExclude { pattern: norm });
        }
        // A glob: no escaped wildcard (that would have made it literal), so
        // every `*`/`?` in the decoded `norm` is a wildcard, while a decoded
        // `%3b` is a `;` in a filename. Split only the *fragment* at its first
        // wildcard, then prepend the literal base (for relative specs) to get
        // the walk root and key prefix. The base is never fed to
        // `split_glob_root`, so its `*`/`?` never act as a wildcard boundary.
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
        // fragment is parsed on its own (its own wildcards are wildcards). A
        // filename part of `*.*` matches every file, as MSBuild's does.
        let pat = Pattern::with_literal_prefix(anchor_prefix(&base_segs, &norm), &norm)
            .with_all_files_rule();
        // Root the walk at the literal anchor so `../shared/*.fs` enumerates
        // the sibling directory and `/opt/lib/*.fs` the absolute one.
        // `anchor_segs` is absolute (or empty → `/`).
        let walk_root = abs_from_segments(&anchor_segs);
        // A `**`-free tail matches files at one fixed depth below the walk
        // root, so there is no point descending further.
        let depth = frag_root.tail_depth.unwrap_or(MAX_GLOB_DEPTH);
        let recursive = frag_root.tail_depth.is_none();
        // Re-attach the literal prefix to each enumerated path so keys,
        // include pattern, and excludes all live in one frame. `select`
        // matches/orders them and returns leading-`/`-stripped absolute
        // strings (its `split_segments` drops the empty root segment), which
        // we re-root at `/`.
        let prefix = anchor_segs.join("/");
        // Every name the walk finds is held to the allow-list too, matched or
        // not: a name outside it could match where the plain reading says it
        // does not. A non-ASCII name is the exception, being common and
        // harmless to an ASCII pattern (which the specs are) except in two
        // ways: a `?` meeting a character two UTF-16 units wide, and
        // `OrdinalIgnoreCase` folding `ı` or `ſ` onto ASCII. So it is held
        // only where a `?` is in play, or it has one of those two.
        let question = norm.contains('?')
            || req
                .excludes
                .iter()
                .filter(|e| e.excludes_from_globs())
                .any(|e| e.unescape().contains('?'));
        let keys: Vec<String> = enumerate_files(&walk_root, depth, recursive)?
            .into_iter()
            .map(|rel| {
                let shape = match unmodelled_shape(&rel) {
                    Some(Unmodelled::NonAscii)
                        if !question && !rel.contains(['\u{131}', '\u{17f}']) =>
                    {
                        unmodelled_shape(&rel.replace(|c: char| !c.is_ascii(), "x"))
                    }
                    shape => shape,
                };
                if let Some(shape) = shape {
                    return Err(GlobDecline::OutsideModelledShape { spec: rel, shape });
                }
                Ok(if prefix.is_empty() {
                    rel
                } else {
                    format!("{prefix}/{rel}")
                })
            })
            .collect::<Result<_, _>>()?;
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        for key in select(&key_refs, std::slice::from_ref(&pat), glob_excludes)? {
            out.push(Path::new("/").join(&key));
        }
    }
    Ok(out)
}

/// The literal segments a decoded spec is anchored under: none for an
/// absolute spec, the project directory's for a relative one.
fn anchor_prefix<'b>(base_segs: &'b [&'b str], norm: &str) -> &'b [&'b str] {
    if norm.starts_with('/') {
        &[]
    } else {
        base_segs
    }
}

/// The allow-list gate for one decoded spec (`glob::unmodelled_shape`). An
/// absolute spec is checked below the project directory, so spelling a file
/// as `$(MSBuildProjectDirectory)/a.fs` in a checkout under a non-ASCII path
/// is still modelled: that prefix is a literal the two sides share.
fn within_modelled_shape(spec: &str, base_norm: &str) -> Result<(), GlobDecline> {
    let norm = spec.replace('\\', "/");
    let base = base_norm.trim_end_matches('/');
    let checked = norm
        .strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(&norm);
    match unmodelled_shape(checked) {
        Some(shape) => Err(GlobDecline::OutsideModelledShape {
            spec: spec.to_string(),
            shape,
        }),
        None => Ok(()),
    }
}

/// The allow-list's structural gate for a decoded spec read as a glob
/// (`glob::unmodelled_glob_structure`).
fn within_modelled_glob_structure(norm: &str) -> Result<(), GlobDecline> {
    match unmodelled_glob_structure(norm) {
        Some(shape) => Err(GlobDecline::OutsideModelledShape {
            spec: norm.to_string(),
            shape,
        }),
        None => Ok(()),
    }
}

/// Whether a decoded spec has a `.` (current-directory) path segment.
fn has_cur_dir_segment(norm: &str) -> bool {
    norm.split(['/', '\\']).any(|seg| seg == ".")
}

/// A spelling as `Path.GetFullPath(Path.Combine(base, spelling))` writes it,
/// for comparison: rooted at the project directory unless already absolute,
/// `.` segments dropped and `..` segments resolved lexically (at the root, a
/// `..` stays at the root), with `/` separators and no leading `/`.
fn lexical_full_path(base_segs: &[&str], spelling: &str) -> String {
    let norm = spelling.replace('\\', "/");
    let mut segs: Vec<&str> = anchor_prefix(base_segs, &norm).to_vec();
    for seg in split_segments(&norm) {
        if seg == ".." {
            segs.pop();
        } else {
            segs.push(seg);
        }
    }
    segs.join("/")
}

/// Whether a **literal** include survives the `Exclude` fragments, as
/// MSBuild's `FileSpecMatcherTester` decides it against an include it does
/// not glob — the reading that differs from a glob include's
/// ([`SpecFragment::excludes_from_globs`]). `item` is the include's decoded
/// spelling.
///
/// - An exclude MSBuild classifies as a glob ([`SpecFragment::is_wildcard`])
///   is a pattern over the item's full path, with the all-files reading of
///   `*.*` except in the filename-only branch
///   ([`is_recursive_filename_pattern`]). One the file matcher would refuse
///   declines.
/// - Any other exclude is a name. Two names are compared as full paths
///   (`./a.fs` is `a.fs`), unless either is a spelling MSBuild does not
///   normalise ([`compares_unnormalised`]), which is compared as written.
///
/// Comparison ignores case in MSBuild. Where folding ASCII case would change
/// the verdict, the host decides, so it declines
/// ([`GlobDecline::CaseAmbiguous`]).
fn literal_survives_excludes(
    item: &str,
    excludes: &[SpecFragment],
    base_segs: &[&str],
) -> Result<bool, GlobDecline> {
    let item_full = lexical_full_path(base_segs, item);
    for exclude in excludes {
        let spelling = exclude.unescape();
        let (exact, folded) = if exclude.is_wildcard() {
            let norm = spelling.replace('\\', "/");
            within_modelled_glob_structure(&norm)?;
            if !is_legal_filespec(&norm) {
                return Err(GlobDecline::IllegalFilespec { pattern: norm });
            }
            // `CreateGlobOrFilenamePattern` takes the fixed directory through
            // `GetFullPath`, so against a literal include `sub/../*.fs` is
            // `*.fs` (unlike the file matcher, which compares by spelling).
            // The wildcard part cannot hold a `..`: that is illegal.
            let (fixed, wildcard_dir, filename) = split_filespec(&norm);
            let fixed_full =
                lexical_full_path(base_segs, if fixed.is_empty() { "." } else { fixed });
            let fixed_segs = split_segments(&fixed_full);
            let mut pattern =
                Pattern::with_literal_prefix(&fixed_segs, &format!("{wildcard_dir}{filename}"));
            if !is_recursive_filename_pattern(&norm) {
                pattern = pattern.with_all_files_rule();
            }
            (
                pattern.matches(&item_full),
                pattern.matches_ignoring_ascii_case(&item_full),
            )
        } else {
            // Each operand is normalised, or not, on its own spelling
            // (`NormalizePathForComparisonNoThrow` is applied to each side).
            let operand = |spelling: &str| {
                if compares_unnormalised(spelling) {
                    spelling.to_string()
                } else {
                    format!("/{}", lexical_full_path(base_segs, spelling))
                }
            };
            let (ours, theirs) = (operand(item), operand(&spelling));
            (ours == theirs, ours.eq_ignore_ascii_case(&theirs))
        };
        if exact != folded {
            return Err(GlobDecline::CaseAmbiguous {
                path: item.to_string(),
            });
        }
        if exact {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Build an absolute path from a glob root's leading wildcard-free segments
/// (separators already split out). An empty prefix yields the filesystem
/// root; `..` segments are pushed literally, not resolved.
///
/// The walk root is rooted at the POSIX filesystem root `/`. Drive-letter /
/// UNC Windows roots are deliberately out of scope: the LSP targets
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
/// is included only when `d <= max_depth`. Symlinked files and directories
/// are followed, as MSBuild follows them, and listed under the link's own
/// name; unreadable directories are skipped. Declines where the walk would
/// miss files MSBuild might see: a directory left unvisited at the depth
/// bound of a `recursive` walk, or a symlink cycle (see the module's
/// correctness envelope).
fn enumerate_files(
    base: &Path,
    max_depth: usize,
    recursive: bool,
) -> Result<Vec<String>, GlobDecline> {
    let mut out = Vec::new();
    let mut ancestors: Vec<PathBuf> = std::fs::canonicalize(base).into_iter().collect();
    walk(base, "", 0, max_depth, recursive, &mut ancestors, &mut out)?;
    Ok(out)
}

/// One directory of [`enumerate_files`]. `ancestors` holds the canonical path
/// of every directory on the way down to `dir`, `dir` included, so a link
/// back to one of them is recognised as a cycle.
fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    max_depth: usize,
    recursive: bool,
    ancestors: &mut Vec<PathBuf>,
    out: &mut Vec<String>,
) -> Result<(), GlobDecline> {
    if depth >= max_depth {
        if recursive {
            return Err(GlobDecline::DepthLimit {
                dir: dir.to_string_lossy().into_owned(),
            });
        }
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type` does not follow symlinks; `fs::metadata` does.
        let is_dir = match entry.file_type() {
            Ok(t) if t.is_file() => false,
            Ok(t) if t.is_dir() => true,
            Ok(t) if t.is_symlink() => match std::fs::metadata(entry.path()) {
                Ok(m) if m.is_file() => false,
                Ok(m) if m.is_dir() => true,
                _ => continue,
            },
            _ => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let rel = if prefix.is_empty() {
            name.into_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if !is_dir {
            out.push(rel);
            continue;
        }
        // Only a directory the walk will actually enter can close a cycle.
        if depth + 1 >= max_depth && !recursive {
            continue;
        }
        let path = entry.path();
        let Ok(canonical) = std::fs::canonicalize(&path) else {
            continue;
        };
        if ancestors.contains(&canonical) {
            return Err(GlobDecline::SymlinkCycle {
                path: path.to_string_lossy().into_owned(),
            });
        }
        ancestors.push(canonical);
        let walked = walk(&path, &rel, depth + 1, max_depth, recursive, ancestors, out);
        ancestors.pop();
        walked?;
    }
    Ok(())
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
        let include = SpecFragment::split_escaped(include);
        let excludes: Vec<SpecFragment> = excludes
            .iter()
            .flat_map(|s| SpecFragment::split_escaped(s))
            .collect();
        let req = GlobRequest {
            base_dir: base,
            include: &include,
            excludes: &excludes,
        };
        resolve(&req)
            .expect("the fixture is decidable")
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

    /// Resolve with `include`/`excludes` split as escaped specs, returning the
    /// result rather than asserting it is decidable.
    fn try_resolve(base: &Path, include: &str, excludes: &[&str]) -> Result<usize, GlobDecline> {
        let include = SpecFragment::split_escaped(include);
        let excludes: Vec<SpecFragment> = excludes
            .iter()
            .flat_map(|s| SpecFragment::split_escaped(s))
            .collect();
        resolve(&GlobRequest {
            base_dir: base,
            include: &include,
            excludes: &excludes,
        })
        .map(|paths| paths.len())
    }

    /// A literal include survives an exclude regardless of case, unless case
    /// is the only difference — then the host decides, and it declines.
    #[test]
    fn a_literal_exclude_differing_only_in_case_declines() {
        let tmp = fixture();
        assert_eq!(try_resolve(tmp.path(), "a.fs", &["b.fs"]), Ok(1));
        assert!(matches!(
            try_resolve(tmp.path(), "a.fs", &["A.fs"]),
            Err(GlobDecline::CaseAmbiguous { .. })
        ));
    }

    /// Against a literal include, ordinary names compare as full paths and a
    /// name holding a `*` compares as written (`FileSpecMatcherTester`).
    #[test]
    fn literal_names_compare_as_msbuild_compares_them() {
        let tmp = fixture();
        assert_eq!(
            run_rel(tmp.path(), "./a.fs;sub/../b.fs", &["a.fs", "b.fs"]),
            Vec::<String>::new()
        );
        let abs = format!("{}/a%2ab.fs", tmp.path().display());
        assert_eq!(run_rel(tmp.path(), "a%2ab.fs", &[&abs]), ["a*b.fs"]);
        assert_eq!(try_resolve(tmp.path(), "./a%2ab.fs", &["a%2ab.fs"]), Ok(1));
        assert_eq!(
            run_rel(tmp.path(), "a%2ab.fs", &["a%2ab.fs"]),
            Vec::<String>::new()
        );
    }

    /// The shapes MSBuild's file matcher does not read the plain way decline
    /// by name; `*.*` is every file.
    #[test]
    fn file_matcher_shapes_decline_or_read_as_msbuild() {
        let tmp = fixture();
        fs::write(tmp.path().join("noext"), b"").unwrap();
        assert!(matches!(
            try_resolve(tmp.path(), "a**b.fs", &[]),
            Err(GlobDecline::IllegalFilespec { .. })
        ));
        assert!(matches!(
            try_resolve(tmp.path(), "*.fs", &["a%2a%2ab.fs"]),
            Err(GlobDecline::IllegalFilespec { .. })
        ));
        // A live illegal exclude declines against a literal include too; one
        // whose stars are decoded is not a glob there, only a name.
        assert!(matches!(
            try_resolve(tmp.path(), "a.fs", &["a**b.fs"]),
            Err(GlobDecline::IllegalFilespec { .. })
        ));
        assert_eq!(try_resolve(tmp.path(), "a.fs;z.fs", &["x%2a%2a.fs"]), Ok(2));
        assert!(matches!(
            try_resolve(tmp.path(), "*.fs", &["./a.fs"]),
            Err(GlobDecline::CurDirSegmentExclude { .. })
        ));
        assert!(matches!(
            try_resolve(tmp.path(), "./*.fs", &["a.fs"]),
            Err(GlobDecline::CurDirSegmentExclude { .. })
        ));
        assert_eq!(try_resolve(tmp.path(), "./*.fs", &[]), Ok(4));
        assert_eq!(run_rel(tmp.path(), "*.*", &["*.fs"]), ["noext"]);
        assert_eq!(run_rel(tmp.path(), "*", &["%2a.%2a"]), Vec::<String>::new());
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
        let excludes: Vec<SpecFragment> = Vec::new();
        let req = GlobRequest {
            base_dir: tmp.path(),
            include: &SpecFragment::split_escaped("a.fs"),
            excludes: &excludes,
        };
        assert_eq!(
            resolve(&req).expect("the fixture is decidable"),
            vec![tmp.path().join("a.fs")]
        );
    }

    // ----- Globs rooted outside the project directory -----

    fn canon(p: impl AsRef<Path>) -> PathBuf {
        fs::canonicalize(p.as_ref())
            .unwrap_or_else(|e| panic!("canonicalize {}: {e}", p.as_ref().display()))
    }

    /// Resolve and canonicalise the results, so `..`/`/private` differences
    /// don't defeat equality when the glob root is outside `base`.
    fn resolve_canon(base: &Path, include: &str, excludes: &[&str]) -> Vec<PathBuf> {
        let include = SpecFragment::split_escaped(include);
        let excludes: Vec<SpecFragment> = excludes
            .iter()
            .flat_map(|s| SpecFragment::split_escaped(s))
            .collect();
        let req = GlobRequest {
            base_dir: base,
            include: &include,
            excludes: &excludes,
        };
        resolve(&req)
            .expect("the fixture is decidable")
            .into_iter()
            .map(canon)
            .collect()
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
        // survive. This is verified against real MSBuild by the
        // parent-relative corners of `tests/glob_msbuild_diff.rs`; the contrast (a same-frame relative
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
        // over-match the sibling. Pinned against real MSBuild by the
        // literal-base corner of `tests/glob_msbuild_diff.rs`.
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
                // MSBuild's `OrdinalIgnoreCase`; the generated names are
                // ASCII and case-distinct, so this is total and decidable.
                m.sort_by_key(|p| p.to_ascii_uppercase());
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
                include: &SpecFragment::split_escaped(&sc.include),
                excludes: &sc
                    .excludes
                    .iter()
                    .flat_map(|e| SpecFragment::split_escaped(e))
                    .collect::<Vec<_>>(),
            };
            let got = resolve(&req).expect("the fixture is decidable");
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

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_followed_under_the_links_name() {
        // Probed (dotnet 10.0.301): MSBuild's `**` descends into a symlinked
        // directory and reports its files under the link's path.
        let ext = TempDir::new().unwrap();
        fs::write(ext.path().join("hidden.fs"), b"// f\n").unwrap();
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        std::os::unix::fs::symlink(ext.path(), base.join("linkdir")).unwrap();
        fs::write(base.join("a.fs"), b"// f\n").unwrap();
        assert_eq!(run_rel(base, "**/*.fs", &[]), ["a.fs", "linkdir/hidden.fs"]);
        assert_eq!(run_rel(base, "*/*.fs", &[]), ["linkdir/hidden.fs"]);
        // An excluded link costs nothing.
        assert_eq!(run_rel(base, "**/*.fs", &["linkdir/**"]), ["a.fs"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_declines() {
        let tmp = fixture();
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("sub/back")).unwrap();
        let excludes: Vec<SpecFragment> = Vec::new();
        let req = GlobRequest {
            base_dir: tmp.path(),
            include: &SpecFragment::split_escaped("**/*.fs"),
            excludes: &excludes,
        };
        assert!(matches!(
            resolve(&req),
            Err(GlobDecline::SymlinkCycle { .. })
        ));
        // A single-level glob never descends, so the cycle decides nothing.
        let req = GlobRequest {
            base_dir: tmp.path(),
            include: &SpecFragment::split_escaped("*.fs"),
            excludes: &excludes,
        };
        assert_eq!(resolve(&req).unwrap().len(), 4);
    }

    #[test]
    fn a_recursive_walk_that_reaches_the_depth_bound_declines() {
        let tmp = TempDir::new().unwrap();
        let mut deep = tmp.path().to_path_buf();
        for _ in 0..=MAX_GLOB_DEPTH {
            deep.push("d");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("x.fs"), b"// f\n").unwrap();
        let excludes: Vec<SpecFragment> = Vec::new();
        let req = GlobRequest {
            base_dir: tmp.path(),
            include: &SpecFragment::split_escaped("**/*.fs"),
            excludes: &excludes,
        };
        assert!(matches!(resolve(&req), Err(GlobDecline::DepthLimit { .. })));
    }
}
