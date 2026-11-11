//! MSBuild's path semantics for item *identities*: how a `Remove` or `Exclude`
//! decides that an item spec names an item, and which path the `->Exists()`
//! item function probes.
//!
//! Both are defined on the identity MSBuild records for an item — its
//! `Include` fragment, trimmed and decoded — and both are path questions rather
//! than string questions:
//!
//! - **Matching** (`FileSpecMatcherTester.IsMatch` for a non-wildcard spec)
//!   normalises each side with `FileUtilities.NormalizePathForComparisonNoThrow`
//!   — backslashes to the platform separator, trailing separators trimmed,
//!   `Path.Combine` onto the *entry* project's directory, `Path.GetFullPath` —
//!   and compares the results `OrdinalIgnoreCase`. So `./../a/A.FSPROJ` names
//!   `../A/A.fsproj`, and `../Nope/../A/A.fsproj` names it too, lexically.
//! - **Existence** (`Expander.IntrinsicItemFunctions.Exists`) hands
//!   `Path.Combine(projectDirectory, unescaped)` to `File.Exists ||
//!   Directory.Exists`, each of which runs `Path.GetFullPath` before asking the
//!   OS. So `..` collapses lexically here too — `../Nope/../A/A.fsproj` exists
//!   when `A/A.fsproj` does — but separators are not rewritten and case is the
//!   filesystem's business.
//!
//! Each function answers only inside a domain where we reproduce MSBuild
//! exactly, and returns `None` outside it; the caller declines.
//!
//! - **Unix only.** Windows path normalisation (drive roots, UNC prefixes, its
//!   invalid-character set) is a different function.
//! - **ASCII only**, project directory included. `OrdinalIgnoreCase` folds
//!   non-ASCII by .NET's simple uppercase mapping, which is not
//!   `to_ascii_lowercase`; on ASCII text the two agree.
//! - **No control characters**: `PathIsInvalid` (NUL on Unix) returns the spelling
//!   unnormalised.
//! - **No `*` or `?`.** MSBuild classifies a wildcard on the *escaped* spelling
//!   and then glob-matches; an identity reaches us decoded, so a literal star
//!   is indistinguishable from an escaped one.
//! - **Not only separators**: trimming would leave nothing, and
//!   `Path.Combine(dir, "")` is the directory itself.
//!
//! Existence adds three restrictions:
//!
//! - **No backslash.** MSBuild rewrites the backslashes in an evaluated value
//!   only when its first path segment exists relative to the *process working
//!   directory* (`FileUtilities.MaybeAdjustFilePath`), so which spelling
//!   `->Exists()` sees depends on where MSBuild runs.
//! - **No trailing separator.** `File.Exists` answers `false` for one outright
//!   and `Directory.Exists` does not, a split not worth reproducing.
//! - **No dangling symlink at the leaf.** .NET's Unix `File.Exists` reads the
//!   leaf with `lstat`, so a symlink to nothing may exist for it while `stat`
//!   says otherwise; [`probe_exists`] declines rather than pick.

use std::path::{Path, PathBuf};

/// Whether `identity` is inside the domain both functions share (see the
/// module docs).
fn in_domain(identity: &str) -> bool {
    cfg!(unix)
        && identity.is_ascii()
        && !identity
            .bytes()
            .any(|b| b.is_ascii_control() || b == b'*' || b == b'?')
        && !identity.trim_end_matches(['/', '\\']).is_empty()
}

/// The key two identities are compared by when a `Remove` or `Exclude`
/// matches: `NormalizePathForComparisonNoThrow(identity, project_dir)`,
/// case-folded. Two identities match exactly when their keys are equal.
///
/// `None` outside the domain.
pub(super) fn comparison_key(identity: &str, project_dir: &Path) -> Option<String> {
    if !in_domain(identity) {
        return None;
    }
    let spelled = identity.replace('\\', "/");
    let mut key = full_path(spelled.trim_end_matches('/'), project_dir)?;
    key.make_ascii_lowercase();
    Some(key)
}

/// `Path.GetFullPath(Path.Combine(project_dir, spelled))` on Unix: a rooted
/// `spelled` stands alone, empty and `.` segments vanish, `..` pops, and nothing
/// climbs above the root. `None` for a project directory outside the domain.
fn full_path(spelled: &str, project_dir: &Path) -> Option<String> {
    let dir = project_dir.to_str().filter(|dir| dir.is_ascii())?;
    let combined = if spelled.starts_with('/') {
        spelled.to_string()
    } else {
        format!("{dir}/{spelled}")
    };
    if !combined.starts_with('/') {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut full = String::with_capacity(combined.len());
    for segment in &segments {
        full.push('/');
        full.push_str(segment);
    }
    if full.is_empty() {
        full.push('/');
    }
    Some(full)
}

/// The path `@(…->Exists())` asks the filesystem about for an item with this
/// identity: `Path.GetFullPath(Path.Combine(project_dir, identity))`.
///
/// `None` outside the domain, and for an identity with a backslash or a
/// trailing separator.
pub(super) fn exists_probe(identity: &str, project_dir: &Path) -> Option<PathBuf> {
    if !in_domain(identity) || identity.contains('\\') || identity.ends_with('/') {
        return None;
    }
    full_path(identity, project_dir).map(PathBuf::from)
}

/// `File.Exists(path) || Directory.Exists(path)` for a path from
/// [`exists_probe`]: whether anything is there, following symlinks. `None` for a
/// symlink at the leaf whose target is missing, where .NET's `lstat` and our
/// `stat` may disagree.
pub(super) fn probe_exists(path: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Err(_) => Some(false),
        Ok(leaf) if leaf.file_type().is_symlink() => path.metadata().is_ok().then_some(true),
        Ok(_) => Some(true),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn key(identity: &str) -> Option<String> {
        comparison_key(identity, Path::new("/root/Proj"))
    }

    #[test]
    fn spellings_of_one_path_share_a_key() {
        let expected = Some("/root/a/a.fsproj".to_string());
        for spelling in [
            "../A/A.fsproj",
            "..\\A\\A.fsproj",
            "./../A/./A.fsproj",
            "../Nope/../a/A.FSPROJ",
            "../A//A.fsproj",
            "../A/A.fsproj/",
            "/root/A/A.fsproj",
            "/root/Proj/../../root/A/A.fsproj",
        ] {
            assert_eq!(key(spelling), expected, "{spelling}");
        }
    }

    #[test]
    fn climbing_past_the_root_stays_at_the_root() {
        assert_eq!(key("../../../../x"), Some("/x".to_string()));
        assert_eq!(key("../../.."), Some("/".to_string()));
    }

    #[test]
    fn identities_outside_the_domain_have_no_key() {
        for identity in ["a*b", "a?b", "é.fsproj", "a\u{0}b", "/", "\\\\", ""] {
            assert_eq!(key(identity), None, "{identity:?}");
        }
        assert_eq!(
            comparison_key("a.fsproj", Path::new("/rööt")),
            None,
            "a non-ASCII project directory folds by .NET's table, not ours"
        );
    }

    #[test]
    fn the_exists_probe_collapses_lexically_and_keeps_case() {
        let dir = Path::new("/root/Proj");
        assert_eq!(
            exists_probe("../Nope/../a/A.fsproj", dir),
            Some(PathBuf::from("/root/a/A.fsproj"))
        );
        assert_eq!(
            exists_probe("/abs/A.fsproj", dir),
            Some(PathBuf::from("/abs/A.fsproj"))
        );
        assert_eq!(exists_probe("..\\A\\A.fsproj", dir), None);
        assert_eq!(exists_probe("../Dir/", dir), None);
    }
}
