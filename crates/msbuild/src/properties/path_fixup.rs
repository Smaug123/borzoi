//! MSBuild's unix-only path fixup, `MaybeAdjustFilePath`.
//!
//! Stage P0 of `docs/msbuild-unix-path-fixup-plan.md`. On a non-Windows host
//! MSBuild passes every expanded value through `FileUtilities.MaybeAdjustFilePath`
//! (`FileUtilities.cs:608`): a backslash-bearing value is rewritten `\`→`/` (and
//! its slash runs collapsed) **iff** the rewritten first segment exists as a
//! directory relative to the MSBuild process's working directory — a
//! cwd-dependent decision we cannot make.
//!
//! The response (later stages) is to *bracket the two worlds*: the value MSBuild
//! commits is one of exactly two — the raw text, or the rewrite — so a consumer
//! that agrees under both can commit, and one that disagrees declines. This
//! module is the eligibility half: [`worlds`] says whether the fixup can fire
//! and, if so, what the rewrite is.
//!
//! Everything here is a faithful port of the source, unit-pinned and (in
//! `tests/path_fixup_diff.rs`) differentially bracketed against `dotnet msbuild`
//! run from two directories.

/// The **rewrite** the fixup would apply if it fires, or `None` when the fixup
/// cannot fire at all (so the value is unambiguous — MSBuild returns it raw).
///
/// `Some(rewrite)` means the value MSBuild commits is one of `{value, rewrite}`,
/// decided by a filesystem probe against the process cwd. `None` means MSBuild
/// commits `value` regardless.
///
/// Ported from `MaybeAdjustFilePath`: the fixup is skipped for an empty value,
/// a `$(`/`@(` reference, a `\\` network prefix, or a value with no backslash
/// (its `ConvertToUnixSlashes` is then the identity, so both worlds coincide).
/// Otherwise the rewrite is `convert_to_unix_slashes`. We do **not** here
/// evaluate `LooksLikeUnixFilePath`'s probe — that is the cwd-dependent part we
/// cannot decide, and bracketing `{value, rewrite}` is conservative whether the
/// probe would hit or miss.
pub fn worlds(value: &str) -> Option<String> {
    // `MaybeAdjustFilePath` returns early on Windows (`IsWindows`), where MSBuild
    // uses `\` separators and does not touch them — so on a Windows host every
    // value is committed verbatim, unambiguously.
    if cfg!(windows)
        || value.is_empty()
        || value.starts_with("$(")
        || value.starts_with("@(")
        || value.starts_with("\\\\")
        || !value.contains('\\')
    {
        return None;
    }
    let rewrite = convert_to_unix_slashes(value);
    // `MaybeAdjustFilePath` also gates on the rewrite containing a `/`; a value
    // with a backslash always yields one, so this holds whenever we reach here.
    debug_assert!(rewrite.contains('/'));
    Some(rewrite)
}

/// `\`→`/`, then collapse every run of slashes to one — a faithful port of
/// `ConvertToUnixSlashes`/`CollapseSlashes` (`Regex.Replace(s, "[\\/]+", "/")`).
/// MSBuild only reaches this when the value contains a backslash, so the
/// collapse (which the identity path skips) is always in effect here.
pub fn convert_to_unix_slashes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_slash = false;
    for ch in value.chars() {
        let is_slash = ch == '\\' || ch == '/';
        if !(is_slash && prev_slash) {
            out.push(if ch == '\\' { '/' } else { ch });
        }
        prev_slash = is_slash;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_backslash_is_unambiguous() {
        // The fixup's `ConvertToUnixSlashes` is the identity without a backslash,
        // so both worlds coincide — even a forward-slash path, and even a `//`
        // run (which is *not* collapsed absent a backslash).
        assert_eq!(worlds(""), None);
        assert_eq!(worlds("obj/Debug/"), None);
        assert_eq!(worlds("a//b"), None);
        assert_eq!(worlds("plain"), None);
    }

    #[test]
    fn references_and_network_prefixes_are_skipped() {
        // `$(`/`@(` references and a `\\` network prefix are excluded verbatim,
        // even though they contain a backslash.
        assert_eq!(worlds("$(Foo)\\bar"), None);
        assert_eq!(worlds("@(Item)\\bar"), None);
        assert_eq!(worlds("\\\\server\\share"), None);
    }

    #[test]
    #[cfg(windows)]
    fn the_fixup_is_inert_on_windows() {
        // MSBuild does not adjust separators on Windows, so every value is
        // unambiguous there.
        assert_eq!(worlds("obj\\Debug\\"), None);
        assert_eq!(worlds("\\x"), None);
    }

    #[test]
    #[cfg(not(windows))]
    fn a_backslash_value_brackets_the_rewrite() {
        // The canonical case: `obj\Debug\` could stay raw or become `obj/Debug/`
        // (cwd-dependent), so the two worlds are exactly those.
        assert_eq!(worlds("obj\\Debug\\").as_deref(), Some("obj/Debug/"));
        // A single leading backslash becomes an absolute `/…` (whose probe is
        // cwd-*independent*, but bracketing is still correct).
        assert_eq!(worlds("\\x").as_deref(), Some("/x"));
        // Slash runs collapse only once a backslash is present.
        assert_eq!(worlds("a\\\\b").as_deref(), Some("a/b"));
        assert_eq!(worlds("a\\/b").as_deref(), Some("a/b"));
        assert_eq!(worlds("a//\\b").as_deref(), Some("a/b"));
    }

    #[test]
    fn convert_matches_the_collapse_rule() {
        assert_eq!(convert_to_unix_slashes("a\\b\\c"), "a/b/c");
        assert_eq!(convert_to_unix_slashes("a\\\\\\b"), "a/b");
        assert_eq!(convert_to_unix_slashes("\\a"), "/a");
        assert_eq!(convert_to_unix_slashes("a\\"), "a/");
    }
}
