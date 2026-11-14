//! [`LanguageVersion`] — the F# language surface a parse targets.
//!
//! Mirrors FCS's `LanguageVersion`
//! (`../fsharp/src/Compiler/Facilities/LanguageFeatures.fs`): the same set of
//! recognised `<LangVersion>` strings and the same alias resolution. In
//! particular FCS sets `latestVersion = latestMajorVersion = defaultVersion =
//! 10.0`, so `default`, `latest`, and `latestmajor` all resolve to 10.0 today;
//! 11.0 is reachable only by writing `11.0`/`11` explicitly (it is gated on a
//! preview SDK), and `preview` is a sentinel above every numbered version
//! (FCS's `previewVersion = 9999`). See [`docs/ast-versioning-plan.md`] D3.
//!
//! Beyond faithful `<LangVersion>` parsing, the type answers each FCS
//! `SupportsFeature` question the lexer, lex-filter and parser ask:
//! [`LanguageVersion::supports`] over [`LanguageFeature`], which is generated
//! from the feature table `crates/cst/language-features.tsv` (see
//! [`docs/ast-versioning-plan.md`], "The feature table").
//!
//! [`docs/ast-versioning-plan.md`]: ../../../docs/ast-versioning-plan.md

/// An F# language version, as selected by `<LangVersion>` in a project file.
///
/// Ordering is by language surface, with [`Preview`](LanguageVersion::Preview)
/// above every numbered version (FCS models it as `9999`). The derived
/// [`Ord`] follows declaration order, which is ascending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LanguageVersion {
    /// F# 4.6.
    V4_6,
    /// F# 4.7.
    V4_7,
    /// F# 5.0.
    V5_0,
    /// F# 6.0.
    V6_0,
    /// F# 7.0.
    V7_0,
    /// F# 8.0.
    V8_0,
    /// F# 9.0.
    V9_0,
    /// F# 10.0 — FCS's `defaultVersion`, and what `default`/`latest`/
    /// `latestmajor` resolve to.
    V10_0,
    /// F# 11.0 — in FCS's set of valid versions, but *not* the default/latest;
    /// reachable only by an explicit `11.0`/`11` pin (gated on a preview SDK).
    V11_0,
    /// `<LangVersion>preview</LangVersion>` — every preview feature on. FCS's
    /// `previewVersion = 9999`, ordered above all numbered versions.
    Preview,
}

impl LanguageVersion {
    /// The version an unspecified project uses — FCS's `defaultVersion` (10.0),
    /// which `default`/`latest`/`latestmajor` all resolve to, and what the LSP
    /// parses a project at when it sets no `<LangVersion>`.
    pub const DEFAULT: LanguageVersion = LanguageVersion::V10_0;

    /// The bottom of the ladder — the least surface this parser models.
    pub const MIN: LanguageVersion = LanguageVersion::V4_6;

    /// The top of the ladder: every implemented feature on.
    ///
    /// [`MIN`](Self::MIN) and `MAX` bracket every version-gated decision the
    /// parser makes. Each such gate is a *monotone threshold* — a feature
    /// becomes available at some version and stays available — so a producer
    /// that agrees at both ends agrees at every version between them. That is
    /// what lets
    /// [`diagnostics_are_version_invariant`](crate::parser::diagnostics_are_version_invariant)
    /// be decided by two evaluations instead of nine.
    pub const MAX: LanguageVersion = LanguageVersion::Preview;

    /// Whether `feature` is available at this version (FCS's
    /// `LanguageVersion.SupportsFeature`).
    pub fn supports(self, feature: LanguageFeature) -> bool {
        self >= feature.introduced()
    }

    /// One version from each band of the gates that shape the lex-filter's
    /// token stream — `RelaxWhitespace2` (F# 6.0) and `StrictIndentation`
    /// (F# 8.0) — ascending. Two versions in the same band filter every source
    /// to the same stream, so comparing parses at these decides whether a
    /// parse flagged
    /// [`shape_depends_on_language_version`](crate::parser::Parse::shape_depends_on_language_version)
    /// really differs between any two versions.
    pub const SHAPE_REPRESENTATIVES: [LanguageVersion; 3] = [
        LanguageVersion::V5_0,
        LanguageVersion::V7_0,
        LanguageVersion::DEFAULT,
    ];

    /// The member of [`SHAPE_REPRESENTATIVES`](Self::SHAPE_REPRESENTATIVES)
    /// sharing this version's band, i.e. the version whose token stream this
    /// one's always equals.
    pub fn shape_representative(self) -> LanguageVersion {
        match (
            self.supports(LanguageFeature::RelaxWhitespace2),
            self.supports(LanguageFeature::StrictIndentation),
        ) {
            (false, _) => LanguageVersion::V5_0,
            (true, false) => LanguageVersion::V7_0,
            (true, true) => LanguageVersion::DEFAULT,
        }
    }

    /// Every numbered version, ascending. Excludes
    /// [`Preview`](LanguageVersion::Preview) (it is not a numbered surface).
    /// Primarily a test/iteration aid.
    pub const NUMBERED: [LanguageVersion; 9] = [
        LanguageVersion::V4_6,
        LanguageVersion::V4_7,
        LanguageVersion::V5_0,
        LanguageVersion::V6_0,
        LanguageVersion::V7_0,
        LanguageVersion::V8_0,
        LanguageVersion::V9_0,
        LanguageVersion::V10_0,
        LanguageVersion::V11_0,
    ];

    /// Resolve a `<LangVersion>` string exactly as FCS's `getVersionFromString`
    /// does: case-insensitive, with the `preview`/`default`/`latest`/
    /// `latestmajor` aliases and the `N` / `N.0` numeric forms. Aliases collapse
    /// to concrete versions (FCS compares on the resolved value, not the
    /// original text).
    ///
    /// Returns `None` for anything FCS maps to its `0m` "invalid" sentinel —
    /// including `"?"`, `"5.00"`, out-of-range numbers, and empty input — so a
    /// caller can diagnose an unrecognised pin rather than silently choosing a
    /// version.
    pub fn from_lang_version_text(text: &str) -> Option<LanguageVersion> {
        // Mirrors getVersionFromString in LanguageFeatures.fs. `<LangVersion>`
        // values are ASCII, so ASCII-uppercasing matches FCS's
        // `ToUpperInvariant` without locale surprises.
        Some(match text.to_ascii_uppercase().as_str() {
            "PREVIEW" => LanguageVersion::Preview,
            // FCS: latestVersion = latestMajorVersion = defaultVersion = 10.0.
            "DEFAULT" | "LATEST" | "LATESTMAJOR" => LanguageVersion::DEFAULT,
            "4.6" => LanguageVersion::V4_6,
            "4.7" => LanguageVersion::V4_7,
            "5.0" | "5" => LanguageVersion::V5_0,
            "6.0" | "6" => LanguageVersion::V6_0,
            "7.0" | "7" => LanguageVersion::V7_0,
            "8.0" | "8" => LanguageVersion::V8_0,
            "9.0" | "9" => LanguageVersion::V9_0,
            "10.0" | "10" => LanguageVersion::V10_0,
            "11.0" | "11" => LanguageVersion::V11_0,
            _ => return None,
        })
    }
}

pub use crate::syntax::features::{FcsEffect, FcsStage, LanguageFeature};

impl LanguageFeature {
    /// FCS's FS3350 message for using the feature at `lang`
    /// (`chkFeatureNotLanguageSupported`). FCS spells a preview feature's
    /// version `'PREVIEW'` (`LanguageVersion.GetFeatureVersionString`).
    pub fn not_available_message(self, lang: LanguageVersion) -> String {
        let introduced = match self.introduced() {
            LanguageVersion::Preview => "'PREVIEW'".to_string(),
            version => version.to_string(),
        };
        format!(
            "Feature '{}' is not available in F# {lang}. Please use language version {introduced} or greater.",
            self.display_name(),
        )
    }
}

impl std::fmt::Display for LanguageVersion {
    /// The canonical `<LangVersion>` spelling: `"4.6"` … `"11.0"`, and
    /// `"preview"`. Round-trips through [`from_lang_version_text`] for every
    /// variant.
    ///
    /// [`from_lang_version_text`]: LanguageVersion::from_lang_version_text
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LanguageVersion::V4_6 => "4.6",
            LanguageVersion::V4_7 => "4.7",
            LanguageVersion::V5_0 => "5.0",
            LanguageVersion::V6_0 => "6.0",
            LanguageVersion::V7_0 => "7.0",
            LanguageVersion::V8_0 => "8.0",
            LanguageVersion::V9_0 => "9.0",
            LanguageVersion::V10_0 => "10.0",
            LanguageVersion::V11_0 => "11.0",
            LanguageVersion::Preview => "preview",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::LanguageVersion::*;
    use super::*;

    /// The FS3350 message, as FCS's `chkFeatureNotLanguageSupported` words it:
    /// the `#elif` one `parser_diff_ifdef` holds to FCS, and a preview
    /// feature's, whose version FCS spells `'PREVIEW'`.
    #[test]
    fn not_available_message_matches_fcs() {
        assert_eq!(
            LanguageFeature::PreprocessorElif.not_available_message(V10_0),
            "Feature '#elif preprocessor directive' is not available in F# 10.0. \
             Please use language version 11.0 or greater.",
        );
        assert_eq!(
            LanguageFeature::FromEndSlicing.not_available_message(V4_6),
            "Feature 'from-end slicing' is not available in F# 4.6. \
             Please use language version 'PREVIEW' or greater.",
        );
    }

    /// [`LanguageFeature::ALL`] lists each feature once.
    #[test]
    fn all_features_are_listed_once() {
        let distinct: std::collections::HashSet<_> = LanguageFeature::ALL.into_iter().collect();
        assert_eq!(distinct.len(), LanguageFeature::ALL.len());
    }

    /// The full `getVersionFromString` mapping, transcribed from
    /// `LanguageFeatures.fs`. This table *is* the oracle: every recognised
    /// string, both numeric forms, every alias, and a sample of inputs FCS maps
    /// to its `0m` invalid sentinel (which we surface as `None`).
    #[test]
    fn fcs_string_mapping() {
        let cases: &[(&str, Option<LanguageVersion>)] = &[
            ("preview", Some(Preview)),
            ("default", Some(V10_0)),
            ("latest", Some(V10_0)),
            ("latestmajor", Some(V10_0)),
            ("4.6", Some(V4_6)),
            ("4.7", Some(V4_7)),
            ("5.0", Some(V5_0)),
            ("5", Some(V5_0)),
            ("6.0", Some(V6_0)),
            ("6", Some(V6_0)),
            ("7.0", Some(V7_0)),
            ("7", Some(V7_0)),
            ("8.0", Some(V8_0)),
            ("8", Some(V8_0)),
            ("9.0", Some(V9_0)),
            ("9", Some(V9_0)),
            ("10.0", Some(V10_0)),
            ("10", Some(V10_0)),
            ("11.0", Some(V11_0)),
            ("11", Some(V11_0)),
            // FCS's `0m` sentinel cases -> None for us.
            ("?", None),
            ("", None),
            ("5.00", None),
            ("4.6.0", None),
            ("12", None),
            ("12.0", None),
            ("3.0", None),
            ("latest-major", None),
            ("garbage", None),
        ];
        for (text, expected) in cases {
            assert_eq!(
                LanguageVersion::from_lang_version_text(text),
                *expected,
                "from_lang_version_text({text:?})"
            );
        }
    }

    /// FCS upper-cases via `ToUpperInvariant`; resolution must be
    /// case-insensitive for the alias keywords and the numeric forms alike.
    #[test]
    fn case_insensitive() {
        for text in ["PREVIEW", "Preview", "pReViEw", "DEFAULT", "Default"] {
            assert_eq!(
                LanguageVersion::from_lang_version_text(text),
                LanguageVersion::from_lang_version_text(&text.to_lowercase()),
                "case folding of {text:?}",
            );
        }
        assert_eq!(
            LanguageVersion::from_lang_version_text("LATESTMAJOR"),
            Some(V10_0)
        );
    }

    /// `default`/`latest`/`latestmajor` are all `defaultVersion` (10.0) in FCS,
    /// and that is our [`LanguageVersion::DEFAULT`] / supported surface.
    #[test]
    fn aliases_resolve_to_default() {
        assert_eq!(LanguageVersion::DEFAULT, V10_0);
        for alias in ["default", "latest", "latestmajor"] {
            assert_eq!(
                LanguageVersion::from_lang_version_text(alias),
                Some(LanguageVersion::DEFAULT),
                "{alias:?} should resolve to DEFAULT",
            );
        }
    }

    /// Ordering is by language surface: numbered versions ascend in declaration
    /// order and `Preview` sits above all of them (FCS's `9999`).
    #[test]
    fn ordering_is_ascending_with_preview_on_top() {
        let mut all: Vec<LanguageVersion> = LanguageVersion::NUMBERED.to_vec();
        all.push(Preview);
        let mut sorted = all.clone();
        sorted.sort();
        assert_eq!(all, sorted, "variants are declared in ascending order");

        assert!(V4_6 < V11_0);
        assert!(LanguageVersion::DEFAULT < V11_0);
        assert!(V11_0 < Preview);
        assert!(*all.iter().max().unwrap() == Preview);
    }

    /// `#elif` is gated on F# 11.0 (FCS's `PreprocessorElif`): 11.0 and
    /// `Preview` support it; 10.0 (our `DEFAULT`) and everything below do not.
    #[test]
    fn preprocessor_elif_gated_at_11() {
        assert!(!LanguageVersion::DEFAULT.supports(LanguageFeature::PreprocessorElif));
        assert!(!V10_0.supports(LanguageFeature::PreprocessorElif));
        assert!(V11_0.supports(LanguageFeature::PreprocessorElif));
        assert!(Preview.supports(LanguageFeature::PreprocessorElif));
        for v in LanguageVersion::NUMBERED {
            assert_eq!(
                v.supports(LanguageFeature::PreprocessorElif),
                v >= V11_0,
                "{v:?}"
            );
        }
    }

    /// Each version maps to the representative of its shape band: the one in
    /// [`LanguageVersion::SHAPE_REPRESENTATIVES`] that answers both
    /// shape-affecting gates the same way, and each representative to itself.
    #[test]
    fn shape_representative_shares_the_gates() {
        let shape_gates = |v: LanguageVersion| {
            (
                v.supports(LanguageFeature::RelaxWhitespace2),
                v.supports(LanguageFeature::StrictIndentation),
            )
        };
        for v in LanguageVersion::NUMBERED.into_iter().chain([Preview]) {
            let rep = v.shape_representative();
            assert!(LanguageVersion::SHAPE_REPRESENTATIVES.contains(&rep), "{v}");
            assert_eq!(shape_gates(rep), shape_gates(v), "{v}");
        }
        for rep in LanguageVersion::SHAPE_REPRESENTATIVES {
            assert_eq!(rep.shape_representative(), rep);
        }
    }

    /// Every variant's [`Display`] round-trips through
    /// [`from_lang_version_text`], including `Preview`.
    #[test]
    fn display_roundtrips() {
        let mut all: Vec<LanguageVersion> = LanguageVersion::NUMBERED.to_vec();
        all.push(Preview);
        for v in all {
            let rendered = v.to_string();
            assert_eq!(
                LanguageVersion::from_lang_version_text(&rendered),
                Some(v),
                "{v:?} renders as {rendered:?} which should parse back",
            );
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::LanguageVersion;
    use proptest::prelude::*;

    proptest! {
        /// Resolution never panics on arbitrary input — it either recognises a
        /// version or returns `None`. (The `0m` sentinel in FCS is total; so are we.)
        #[test]
        fn never_panics(text in ".*") {
            let _ = LanguageVersion::from_lang_version_text(&text);
        }

        /// Resolution is case-insensitive for *any* input, matching FCS's
        /// `ToUpperInvariant`: an ASCII-case perturbation cannot change the result.
        #[test]
        fn ascii_case_insensitive(text in "[A-Za-z0-9.?-]{0,12}") {
            let lower = LanguageVersion::from_lang_version_text(&text.to_ascii_lowercase());
            let upper = LanguageVersion::from_lang_version_text(&text.to_ascii_uppercase());
            prop_assert_eq!(lower, upper);
        }
    }
}
