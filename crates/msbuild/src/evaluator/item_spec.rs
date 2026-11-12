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
//! - **No backslash.** `->Exists()` reads the identity verbatim, and MSBuild
//!   rewrites the backslashes of an `Include` only when its first path segment
//!   exists relative to the *process working directory*
//!   (`FileUtilities.MaybeAdjustFilePath`). [`include_spelling`] applies that
//!   rewrite when capturing an `Include` wherever the working directory cannot
//!   change it — `..\X\X.fsproj` is captured as `../X/X.fsproj` — so a
//!   backslash that survives into an identity is one whose spelling depends on
//!   where MSBuild runs, or an escaped `%5c`.
//! - **No trailing separator.** `File.Exists` answers `false` for one outright
//!   and `Directory.Exists` does not, a split not worth reproducing.
//! - **No dangling symlink at the leaf.** .NET's Unix `File.Exists` reads the
//!   leaf with `lstat`, so a symlink to nothing may exist for it while `stat`
//!   says otherwise; [`probe_exists`] declines rather than pick.

use std::borrow::Cow;
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

/// The spelling MSBuild gives an item spec once it has expanded it:
/// `FileUtilities.MaybeAdjustFilePath`, which `ExpandIntoStringLeaveEscaped`
/// applies to the *whole* expanded spec (escaped text, before the `;` split),
/// so the item identities split from it inherit one decision.
///
/// On Unix that function collapses every run of `/` and `\` into one `/`, but
/// only when the spec's first path segment (`'`/`"` quotes stripped) is a
/// directory under the process working directory — or, for a rooted spec, when
/// the root holds it. We commit only where the answer cannot turn on the
/// filesystem:
///
/// - no backslash, or a spec MSBuild never touches (`$(`, `@(`, `\\`): as is;
/// - first segment `.` or `..`, which name directories under any working
///   directory that exists: respelled;
/// - no separator after the first character, unrooted: as is.
///
/// `None` for any other spelling with a backslash, and for one whose `raw`
/// text holds a `$(` (each spliced piece is respelled on its own by the same
/// rule, before the whole is). `raw` is the attribute text, `expanded` its
/// escaped expansion.
pub(super) fn include_spelling<'a>(raw: &str, expanded: &'a str) -> Option<Cow<'a, str>> {
    let unchanged = Some(Cow::Borrowed(expanded));
    if cfg!(windows) || !expanded.contains('\\') {
        return unchanged;
    }
    if raw.contains("$(") {
        return None;
    }
    if ["$(", "@(", "\\\\"].iter().any(|p| expanded.starts_with(p)) {
        return unchanged;
    }
    let mut collapsed = String::with_capacity(expanded.len());
    for c in expanded.chars() {
        let slash = matches!(c, '/' | '\\');
        if !(slash && collapsed.ends_with('/')) {
            collapsed.push(if slash { '/' } else { c });
        }
    }
    let unquoted = ['\'', '"']
        .iter()
        .find_map(|q| {
            collapsed
                .strip_prefix(*q)
                .and_then(|rest| rest.strip_suffix(*q))
                .filter(|inner| !inner.is_empty())
        })
        .unwrap_or(&collapsed);
    let first = unquoted.chars().next().map_or(0, char::len_utf8);
    match unquoted[first..].find('/') {
        Some(at) => match &unquoted[..first + at] {
            "." | ".." => Some(Cow::Owned(collapsed)),
            _ => None,
        },
        None if unquoted.starts_with('/') => None,
        None => unchanged,
    }
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

    /// `FileUtilities.MaybeAdjustFilePath` on Unix, transcribed, with the one
    /// question that reaches outside the string left to the caller: whether a
    /// first path segment names a directory (`dir_exists`) or, for a rooted
    /// spelling with no second segment, anything (`exists`). Both are asked
    /// of `Path.Combine(cwd, segment)`.
    fn maybe_adjust_file_path(
        value: &str,
        dir_exists: &dyn Fn(&str) -> bool,
        exists: &dyn Fn(&str) -> bool,
    ) -> String {
        if value.is_empty()
            || value.starts_with("$(")
            || value.starts_with("@(")
            || value.starts_with("\\\\")
        {
            return value.to_string();
        }
        let mut new_value = String::new();
        if value.contains('\\') {
            for c in value.chars() {
                let slash = c == '/' || c == '\\';
                if !(slash && (new_value.ends_with('/'))) {
                    new_value.push(if slash { '/' } else { c });
                }
            }
        } else {
            new_value = value.to_string();
        }
        let chars: Vec<char> = new_value.chars().collect();
        let unquoted: &[char] = if chars.len() > 2
            && ((chars[0] == '\'' && chars[chars.len() - 1] == '\'')
                || (chars[0] == '"' && chars[chars.len() - 1] == '"'))
        {
            &chars[1..chars.len() - 1]
        } else {
            &chars
        };
        let looks_like_path = {
            let directory_length = unquoted
                .iter()
                .skip(1)
                .position(|&c| c == '/')
                .map_or(0, |at| at + 1);
            if directory_length != 0 {
                dir_exists(&unquoted[..directory_length].iter().collect::<String>())
            } else {
                unquoted.first() == Some(&'/') && exists(&unquoted.iter().collect::<String>())
            }
        };
        if new_value.contains('/') && looks_like_path {
            new_value
        } else {
            value.to_string()
        }
    }

    /// Every string up to `len` characters over `alphabet`.
    fn all_strings(alphabet: &[char], len: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut layer = vec![String::new()];
        for _ in 0..len {
            layer = layer
                .iter()
                .flat_map(|prefix| {
                    alphabet.iter().map(move |c| {
                        let mut s = prefix.clone();
                        s.push(*c);
                        s
                    })
                })
                .collect();
            all.extend(layer.iter().cloned());
        }
        all
    }

    /// `include_spelling` commits only to a spelling MSBuild produces whatever
    /// the working directory holds — the process's working directory exists,
    /// so `.` and `..` name directories under it and every other relative
    /// segment may or may not — and it does commit whenever that is so and
    /// the answer turns on no rooted path.
    #[test]
    fn include_spelling_is_maybe_adjust_file_path_wherever_the_cwd_cannot_matter() {
        let alphabet = ['.', '\\', '/', 'a', ';', '\'', '$', '(', '@', '%'];
        let mut committed_respellings = 0;
        for value in all_strings(&alphabet, 6) {
            let under = |cwd_holds_it: bool, root_holds_it: bool| {
                let dir_exists = move |segment: &str| {
                    if segment.starts_with('/') {
                        root_holds_it
                    } else {
                        matches!(segment, "." | "..") || cwd_holds_it
                    }
                };
                let exists = move |_: &str| root_holds_it;
                maybe_adjust_file_path(&value, &dir_exists, &exists)
            };
            let answers: Vec<String> = [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .map(|(cwd, root)| under(cwd, root))
                .collect();
            let settled = answers.iter().all(|a| a == &answers[0]);
            match include_spelling(&value, &value) {
                Some(spelling) => {
                    assert!(settled, "{value:?} turns on the filesystem: {answers:?}");
                    assert_eq!(spelling, answers[0], "{value:?}");
                    if spelling != value {
                        committed_respellings += 1;
                    }
                }
                None => assert!(
                    !settled || value.contains("$("),
                    "{value:?} is settled as {:?} but declined",
                    answers[0]
                ),
            }
        }
        assert!(committed_respellings > 1000, "{committed_respellings}");
    }

    #[test]
    fn include_spelling_declines_a_spliced_backslash() {
        // Each piece of a `$(…)` expansion is respelled on its own before the
        // whole is, and that pass is not modelled. (Here the whole-spec pass
        // happens to settle it; the decline is ours, not MSBuild's.)
        assert_eq!(include_spelling("$(Dir)/A.fsproj", "..\\X/A.fsproj"), None);
        assert_eq!(
            include_spelling("$(Dir)/A.fsproj", "../X/A.fsproj"),
            Some(Cow::Borrowed("../X/A.fsproj"))
        );
        assert_eq!(
            include_spelling(
                "..\\A\\A.fsproj;Sub\\B.fsproj",
                "..\\A\\A.fsproj;Sub\\B.fsproj"
            ),
            Some(Cow::Owned("../A/A.fsproj;Sub/B.fsproj".to_string())),
            "the whole spec is respelled by its first segment"
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
