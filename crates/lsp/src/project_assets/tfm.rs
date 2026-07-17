//! Conversion between the long and short forms of a Target Framework
//! Moniker.
//!
//! `project.assets.json` carries the *long* form in
//! `targets[<consumer>][<lib>].framework` (e.g. `.NETStandard,Version=v2.0`)
//! whereas everywhere else in the file — keys in `project.frameworks`,
//! keys in `targets`, etc. — uses the *short* alias (`netstandard2.0`).
//! Downstream code (the LSP protocol, MSBuild dispatch, csproj
//! enumeration) speaks the short form, so we normalise on read.
//!
//! Unrecognised inputs pass through unchanged: assets files written by
//! NuGet versions or platforms we don't model would otherwise crash the
//! resolver for unrelated TFMs, and treating an unknown short alias as
//! already-canonical is the conservative behaviour.
//!
//! Cutoff rule for `.NETCoreApp,Version=vX.Y`: major >= 5 maps to
//! `netX.Y` (the unified `net5.0+` line); major < 5 stays
//! `netcoreappX.Y`. This mirrors NuGet's published moniker table.

/// Convert a long-form TFM moniker into its canonical short form.
///
/// Recognises:
/// - `.NETStandard,Version=vX.Y` → `netstandardX.Y`
/// - `.NETCoreApp,Version=vX.Y` with X >= 5 → `netX.Y`
/// - `.NETCoreApp,Version=vX.Y` with X < 5 → `netcoreappX.Y`
/// - `.NETFramework,Version=vA.B[.C]` → `net{A}{B}{C}` (digits concatenated)
///
/// A `,Profile=...` or other secondary clause after the version part
/// (e.g. `.NETFramework,Version=v4.0,Profile=Client` → short `net40-client`,
/// `Profile=CompactFramework` → `-cf`) is **not** modelled here. Inputs
/// carrying such a clause are passed through unchanged so the caller sees
/// the original string and downstream surfaces an unresolved-TFM error
/// instead of a silently-truncated short alias.
///
/// Anything else is returned unchanged so already-short inputs (and
/// inputs from monikers we don't yet model) round-trip.
pub fn long_to_short(moniker: &str) -> String {
    if let Some(rest) = strip_long_prefix(moniker, ".NETStandard,Version=v") {
        if has_profile_clause(rest) {
            return moniker.to_string();
        }
        return format!("netstandard{rest}");
    }
    if let Some(rest) = strip_long_prefix(moniker, ".NETCoreApp,Version=v") {
        if has_profile_clause(rest) {
            return moniker.to_string();
        }
        if let Some((major, _minor)) = split_major_minor(rest)
            && major >= 5
        {
            return format!("net{rest}");
        }
        return format!("netcoreapp{rest}");
    }
    if let Some(rest) = strip_long_prefix(moniker, ".NETFramework,Version=v") {
        if has_profile_clause(rest) {
            return moniker.to_string();
        }
        let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return format!("net{digits}");
        }
    }
    moniker.to_string()
}

/// `.NETFramework,Version=v4.0,Profile=Client` has a comma in the
/// version-tail that signals a secondary clause we don't model. The
/// long-form prefix already consumed one comma (the `,Version=` token),
/// so any remaining comma in `rest` means a profile/platform clause.
fn has_profile_clause(version_tail: &str) -> bool {
    version_tail.contains(',')
}

/// Split a short-form TFM into its base and optional platform suffix.
///
/// The suffix is everything after the *first* hyphen: NuGet's TFM
/// grammar is `<base>(-<platform>[<version>])?`, so further hyphens in
/// the tail belong to the platform name (e.g. an OS+version pair) and
/// stay together. Examples:
///
/// - `net8.0-windows7.0` → (`net8.0`, Some(`windows7.0`))
/// - `net8.0` → (`net8.0`, None)
/// - `netstandard2.0` → (`netstandard2.0`, None)
///
/// Lossless: joining the two halves with `-` reconstructs the input.
/// Inputs without a hyphen (the common case) round-trip as `(input, None)`.
pub fn split_platform(short_tfm: &str) -> (&str, Option<&str>) {
    match short_tfm.split_once('-') {
        Some((base, plat)) => (base, Some(plat)),
        None => (short_tfm, None),
    }
}

/// Extract the platform-name portion of a platform suffix: the leading
/// run of ASCII alphabetics before any digit or other character. The
/// remainder is the platform's version, which NuGet's compatibility
/// rules treat as orderable within a platform family.
///
/// - `windows` → `windows`
/// - `windows7.0` → `windows`
/// - `windows10.0.19041.0` → `windows`
/// - `android` → `android`
///
/// Same-family comparison is enough for the consumer-side gating in
/// `pick_producer_tfm`: callers that need to *order* versions must do
/// their own parsing.
pub fn platform_family(suffix: &str) -> &str {
    let end = suffix
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(suffix.len());
    &suffix[..end]
}

fn strip_long_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.strip_prefix(prefix)
}

fn split_major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn netstandard_long_to_short() {
        assert_eq!(long_to_short(".NETStandard,Version=v2.0"), "netstandard2.0");
        assert_eq!(long_to_short(".NETStandard,Version=v2.1"), "netstandard2.1");
        assert_eq!(long_to_short(".NETStandard,Version=v1.6"), "netstandard1.6");
    }

    #[test]
    fn netcoreapp_below_five_keeps_netcoreapp_prefix() {
        assert_eq!(long_to_short(".NETCoreApp,Version=v3.1"), "netcoreapp3.1");
        assert_eq!(long_to_short(".NETCoreApp,Version=v2.1"), "netcoreapp2.1");
        assert_eq!(long_to_short(".NETCoreApp,Version=v1.0"), "netcoreapp1.0");
    }

    #[test]
    fn netcoreapp_five_plus_drops_to_net() {
        assert_eq!(long_to_short(".NETCoreApp,Version=v5.0"), "net5.0");
        assert_eq!(long_to_short(".NETCoreApp,Version=v8.0"), "net8.0");
        assert_eq!(long_to_short(".NETCoreApp,Version=v10.0"), "net10.0");
    }

    #[test]
    fn netframework_concatenates_digits() {
        assert_eq!(long_to_short(".NETFramework,Version=v4.0"), "net40");
        assert_eq!(long_to_short(".NETFramework,Version=v4.7.2"), "net472");
        assert_eq!(long_to_short(".NETFramework,Version=v3.5"), "net35");
    }

    #[test]
    fn already_short_passes_through() {
        // Idempotent on canonical short forms: needed because the same
        // helper runs over both forms downstream (some assets files store
        // the short form even in `framework`).
        for s in [
            "netstandard2.0",
            "netstandard2.1",
            "net8.0",
            "net10.0",
            "netcoreapp3.1",
            "net472",
        ] {
            assert_eq!(long_to_short(s), s, "short form must round-trip: {s}");
        }
    }

    #[test]
    fn profile_qualified_moniker_passes_through_unchanged() {
        // `.NETFramework,Version=v4.0,Profile=Client` previously collapsed
        // to `net40` because the digit-filter stripped the Profile clause.
        // The correct short alias is `net40-client` (or, for CompactFramework,
        // `net45-cf`). We don't model those mappings here, so the safest
        // behaviour is pass-through so downstream surfaces the original
        // moniker rather than a silently-truncated short alias.
        assert_eq!(
            long_to_short(".NETFramework,Version=v4.0,Profile=Client"),
            ".NETFramework,Version=v4.0,Profile=Client"
        );
        assert_eq!(
            long_to_short(".NETFramework,Version=v4.5,Profile=CompactFramework"),
            ".NETFramework,Version=v4.5,Profile=CompactFramework"
        );
        // Same defensiveness on the other long-form families, even though
        // profile-qualified netstandard / netcoreapp aren't common in the
        // wild.
        assert_eq!(
            long_to_short(".NETStandard,Version=v2.0,Profile=Foo"),
            ".NETStandard,Version=v2.0,Profile=Foo"
        );
        assert_eq!(
            long_to_short(".NETCoreApp,Version=v8.0,Profile=Foo"),
            ".NETCoreApp,Version=v8.0,Profile=Foo"
        );
    }

    #[test]
    fn unknown_input_passes_through() {
        // Conservative: don't crash on monikers we don't model yet
        // (e.g. `MonoAndroid,Version=v9.0`). Returning the input as-is
        // means downstream code at worst encounters an unrecognised
        // short alias, which surfaces as a clearer error than
        // "framework field could not be normalised".
        assert_eq!(
            long_to_short("MonoAndroid,Version=v9.0"),
            "MonoAndroid,Version=v9.0"
        );
        assert_eq!(long_to_short(""), "");
        assert_eq!(
            long_to_short(".NETFramework,Version=v"),
            ".NETFramework,Version=v"
        );
    }

    #[test]
    fn split_platform_no_suffix() {
        assert_eq!(split_platform("net8.0"), ("net8.0", None));
        assert_eq!(split_platform("netstandard2.0"), ("netstandard2.0", None));
        assert_eq!(split_platform("netcoreapp3.1"), ("netcoreapp3.1", None));
        assert_eq!(split_platform("net472"), ("net472", None));
        assert_eq!(split_platform(""), ("", None));
    }

    #[test]
    fn split_platform_with_suffix() {
        assert_eq!(
            split_platform("net8.0-windows"),
            ("net8.0", Some("windows"))
        );
        assert_eq!(
            split_platform("net8.0-windows7.0"),
            ("net8.0", Some("windows7.0"))
        );
        assert_eq!(
            split_platform("net8.0-android"),
            ("net8.0", Some("android"))
        );
        // Only the *first* hyphen splits — RIDs and OS versions all live
        // in the suffix as a single opaque tail. That matches NuGet's
        // moniker grammar: `<base>(-<platform>[<version>])?`.
        assert_eq!(
            split_platform("net8.0-windows-extra"),
            ("net8.0", Some("windows-extra"))
        );
    }

    #[test]
    fn platform_family_extracts_alpha_prefix() {
        assert_eq!(platform_family("windows"), "windows");
        assert_eq!(platform_family("windows7.0"), "windows");
        assert_eq!(platform_family("windows10.0.19041.0"), "windows");
        assert_eq!(platform_family("android"), "android");
        assert_eq!(platform_family("ios13.0"), "ios");
        assert_eq!(platform_family(""), "");
        // Leading non-alpha: empty family. Defensive — real NuGet
        // suffixes start with a platform name, but the helper is
        // total over arbitrary strings.
        assert_eq!(platform_family("7.0"), "");
    }

    proptest! {
        // For every netstandard major.minor we expect the conversion to
        // round-trip through the obvious format: `.NETStandard,Version=vX.Y`
        // → `netstandardX.Y`. A property test over the major/minor space
        // catches off-by-one parsing bugs that a few hand-picked cases
        // might miss (we've been burned by silently passing an unrecognised
        // input through when the prefix matcher had a typo).
        #[test]
        fn netstandard_property(major in 0u32..10, minor in 0u32..20) {
            let long = format!(".NETStandard,Version=v{major}.{minor}");
            let short = format!("netstandard{major}.{minor}");
            prop_assert_eq!(long_to_short(&long), short);
        }

        #[test]
        fn netcoreapp_property(major in 0u32..15, minor in 0u32..20) {
            let long = format!(".NETCoreApp,Version=v{major}.{minor}");
            let expected = if major >= 5 {
                format!("net{major}.{minor}")
            } else {
                format!("netcoreapp{major}.{minor}")
            };
            prop_assert_eq!(long_to_short(&long), expected);
        }

        #[test]
        fn netframework_property(a in 1u32..6, b in 0u32..10, c in 0u32..10) {
            let long = format!(".NETFramework,Version=v{a}.{b}.{c}");
            let expected = format!("net{a}{b}{c}");
            prop_assert_eq!(long_to_short(&long), expected);
        }

        // Joining the base with the suffix (via `-`) must reproduce the
        // input exactly. Catches any future cleverness that strips or
        // normalises components of the suffix.
        #[test]
        fn split_platform_roundtrips(
            base in "[a-z]+[0-9]+\\.[0-9]+",
            suffix in proptest::option::of("[a-z]+[0-9.]*")
        ) {
            let s = match &suffix {
                Some(p) => format!("{base}-{p}"),
                None => base.clone(),
            };
            let (got_base, got_suffix) = split_platform(&s);
            prop_assert_eq!(got_base, &base);
            prop_assert_eq!(got_suffix, suffix.as_deref());
        }
    }
}
