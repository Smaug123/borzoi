//! Read the declared `<TargetFramework>` or `<TargetFrameworks>` from a
//! parsed fsproj.
//!
//! MSBuild's multi-targeting model treats `<TargetFrameworks>` (plural,
//! semicolon-separated) and `<TargetFramework>` (singular) as siblings:
//! if both are non-empty, the plural value drives an *outer build* that
//! dispatches per-TFM *inner builds* with `TargetFramework=X` set. For
//! a pure-parse view ("which TFMs does this project declare?") the
//! preference is therefore plural-when-non-empty, singular-as-fallback.
//!
//! This entry point is enumeration-only. Policy — whether to auto-pick
//! when there's exactly one, or error when the caller hasn't
//! disambiguated — belongs to the consumer (e.g. the LSP layer that
//! decides what TFM to ask the C# sidecar for). Keeping the parser
//! policy-free matches the [`find_global_json`](crate::find_global_json)
//! / [`parse_global_json`](crate::parse_global_json) split: discovery
//! is one concern, selection another.
//!
//! See [`docs/completed/fsproj-parser-plan.md`](../../../docs/completed/fsproj-parser-plan.md)
//! D9 for the rationale.

use crate::ParsedProject;

/// Extract the target framework(s) the project declares, after the
/// usual `$(...)` substitution / condition evaluation has run.
///
/// Preference order matches MSBuild:
/// 1. If `<TargetFrameworks>` is declared and resolves to at least one
///    non-empty entry (after splitting on `;` and trimming), those
///    entries are returned in document order.
/// 2. Otherwise, if `<TargetFramework>` is declared and non-empty after
///    trimming, it is returned as a single-element vec.
/// 3. Otherwise, an empty vec.
///
/// The function is total: a [`ParsedProject`] always has a properties
/// map, and reading a property is infallible. Callers that need to
/// distinguish "no TFM declared" from "valid empty result" should
/// branch on `Vec::is_empty`.
pub fn target_frameworks(project: &ParsedProject) -> Vec<String> {
    // The list is computed by the evaluator, where the values are still escaped:
    // MSBuild splits a property into a list on the semicolons of the *escaped*
    // text, so `net8.0%3bnet9.0` is one framework named `net8.0;net9.0` rather
    // than two. `project.properties` is unescaped for its own consumers, so
    // splitting it here would be splitting on the wrong side of the boundary.
    if !project.target_frameworks.is_empty() {
        return project.target_frameworks.clone();
    }
    if let Some(singular) = lookup_ci(project, "TargetFramework") {
        let trimmed = singular.trim();
        if !trimmed.is_empty() {
            return vec![trimmed.to_string()];
        }
    }
    Vec::new()
}

/// MSBuild property names are case-insensitive, but
/// [`ParsedProject::properties`] preserves the project's spelling (see the
/// evaluator's `into_parsed` comment). Look up the value with an ASCII
/// case-insensitive comparison so `<targetframeworks>` and `<TargetFrameworks>`
/// alike resolve.
fn lookup_ci<'a>(project: &'a ParsedProject, name: &str) -> Option<&'a str> {
    project
        .properties
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use crate::parse_fsproj;

    use super::target_frameworks;

    fn parse(source: &str) -> crate::ParsedProject {
        parse_fsproj(
            source,
            Path::new("/repo/proj/Demo.fsproj"),
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("well-formed XML parses")
    }

    #[test]
    fn singular_target_framework() {
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(target_frameworks(&p), vec!["net10.0".to_string()]);
    }

    #[test]
    fn plural_single_value() {
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net10.0</TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(target_frameworks(&p), vec!["net10.0".to_string()]);
    }

    #[test]
    fn plural_multiple_values() {
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net8.0;net10.0;net472</TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec![
                "net8.0".to_string(),
                "net10.0".to_string(),
                "net472".to_string()
            ],
        );
    }

    #[test]
    fn plural_wins_when_both_declared() {
        // `<TargetFrameworks>` is the outer-build selector; `<TargetFramework>`
        // would only be the inner-build pick. Our enumeration view follows
        // MSBuild's preference and reports the plural list.
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec!["net8.0".to_string(), "net10.0".to_string()],
        );
    }

    #[test]
    fn neither_declared_yields_empty() {
        let p = parse(r#"<Project><PropertyGroup/></Project>"#);
        assert!(target_frameworks(&p).is_empty());
    }

    #[test]
    fn empty_plural_falls_through_to_singular() {
        // An empty `<TargetFrameworks>` after substitution is effectively
        // "not declared" — MSBuild's own SDK targets explicitly guard on
        // `'$(TargetFrameworks)' == ''` to mean "treat as undeclared and
        // use TargetFramework instead". Match that semantic so a
        // conditional rewrite that evaluates to nothing doesn't shadow a
        // valid singular value.
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks></TargetFrameworks>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(target_frameworks(&p), vec!["net10.0".to_string()]);
    }

    #[test]
    fn whitespace_around_semicolons_is_trimmed() {
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks> net8.0 ; net10.0 </TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec!["net8.0".to_string(), "net10.0".to_string()],
        );
    }

    #[test]
    fn empty_entries_in_plural_are_dropped() {
        // Trailing / doubled semicolons are common when callers concatenate
        // lists conditionally (the FSC pattern below). Drop the empties
        // rather than carry an empty TFM string downstream.
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net8.0;;net10.0;</TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec!["net8.0".to_string(), "net10.0".to_string()],
        );
    }

    #[test]
    fn substitution_inside_value() {
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <MyTfm>net8.0</MyTfm>
    <TargetFrameworks>$(MyTfm);net10.0</TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec!["net8.0".to_string(), "net10.0".to_string()],
        );
    }

    #[test]
    fn conditional_declaration_evaluates() {
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <Configuration>Release</Configuration>
    <TargetFrameworks Condition="'$(Configuration)' == 'Release'">net10.0</TargetFrameworks>
    <TargetFrameworks Condition="'$(Configuration)' == 'Debug'">net8.0</TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(target_frameworks(&p), vec!["net10.0".to_string()]);
    }

    use proptest::prelude::*;

    /// A TFM-shaped identifier: starts with a letter, then letters/digits/dots.
    /// Deliberately *excludes* `;` (would split into multiple TFMs) and
    /// whitespace (would be trimmed). Covers the realistic moniker shape
    /// without needing a complete monikers RFC.
    fn tfm_strategy() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9.]{0,12}"
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// For any list of TFM-shaped strings, joining with `;` and round-
        /// tripping through the parser must yield the original list. This
        /// is the order-preserving, lossless invariant the LSP consumer
        /// will rely on when it eventually maps "which TFM to ask the
        /// sidecar for?" onto an MSBuild project's declarations.
        #[test]
        fn join_then_parse_round_trips(
            tfms in proptest::collection::vec(tfm_strategy(), 1..=6),
        ) {
            let joined = tfms.join(";");
            let source = format!(
                "<Project><PropertyGroup><TargetFrameworks>{joined}</TargetFrameworks></PropertyGroup></Project>"
            );
            let p = parse(&source);
            prop_assert_eq!(target_frameworks(&p), tfms);
        }

        /// Adding arbitrary amounts of whitespace around each semicolon
        /// must not change the parsed output. MSBuild's own outer-build
        /// machinery trims; we have to mirror that or a project authored
        /// with stylistic spacing would parse to different TFMs than
        /// MSBuild sees.
        #[test]
        fn whitespace_around_semicolons_is_invariant(
            tfms in proptest::collection::vec(tfm_strategy(), 1..=6),
            pad_left in proptest::collection::vec(" *", 1..=6),
            pad_right in proptest::collection::vec(" *", 1..=6),
        ) {
            // Build a joined string with random padding around each separator.
            let mut joined = String::new();
            for (i, tfm) in tfms.iter().enumerate() {
                if i > 0 {
                    let pl = pad_left.get(i).map(String::as_str).unwrap_or("");
                    let pr = pad_right.get(i).map(String::as_str).unwrap_or("");
                    joined.push_str(pl);
                    joined.push(';');
                    joined.push_str(pr);
                }
                joined.push_str(tfm);
            }
            let source = format!(
                "<Project><PropertyGroup><TargetFrameworks>{joined}</TargetFrameworks></PropertyGroup></Project>"
            );
            let p = parse(&source);
            prop_assert_eq!(target_frameworks(&p), tfms);
        }

        /// Empty segments — leading, trailing, or interspersed — must
        /// drop out. This is the canonical "doubled semicolon"
        /// idiom MSBuild itself uses when conditionally appending to a
        /// TFM list (`$(MyOptionalTfm);$(TargetFrameworks)` when
        /// `MyOptionalTfm` is empty).
        #[test]
        fn empty_segments_are_filtered(
            tfms in proptest::collection::vec(tfm_strategy(), 1..=4),
            empties_before in 0usize..=3,
            empties_between in 0usize..=3,
            empties_after in 0usize..=3,
        ) {
            let mut joined = String::new();
            for _ in 0..empties_before { joined.push(';'); }
            for (i, tfm) in tfms.iter().enumerate() {
                if i > 0 {
                    joined.push(';');
                    for _ in 0..empties_between { joined.push(';'); }
                }
                joined.push_str(tfm);
            }
            for _ in 0..empties_after { joined.push(';'); }
            let source = format!(
                "<Project><PropertyGroup><TargetFrameworks>{joined}</TargetFrameworks></PropertyGroup></Project>"
            );
            let p = parse(&source);
            prop_assert_eq!(target_frameworks(&p), tfms);
        }
    }

    #[test]
    fn lowercase_property_name_is_recognised() {
        // MSBuild property names are case-insensitive, and our evaluator's
        // [`PropertyMap`] resolves substitution case-insensitively too. The
        // exported `ParsedProject::properties` preserves the project's
        // spelling, though — so a `<targetframeworks>` element lands in the
        // map under the lowercase key. A consumer doing exact `.get("TargetFrameworks")`
        // would miss it; mirror MSBuild's case-insensitivity at this layer
        // so the enumeration view matches what MSBuild would see.
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <targetframeworks>net8.0;net10.0</targetframeworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec!["net8.0".to_string(), "net10.0".to_string()],
        );
    }

    #[test]
    fn mixed_case_singular_is_recognised() {
        // Same rationale as the lowercase-plural case above, applied to the
        // singular fallback. `TARGETFRAMEWORK` and `TargetFramework` are the
        // same property to MSBuild.
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TARGETFRAMEWORK>net10.0</TARGETFRAMEWORK>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(target_frameworks(&p), vec!["net10.0".to_string()]);
    }

    #[test]
    fn self_reference_rewrites_in_place() {
        // The shape `FSharp.Compiler.Service.fsproj` uses: redefine
        // `<TargetFrameworks>` in terms of its existing value plus a new
        // entry. Our evaluator should treat the right-hand `$(TargetFrameworks)`
        // as the value at the point of evaluation, producing a concatenation.
        let p = parse(
            r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net8.0</TargetFrameworks>
    <TargetFrameworks>net10.0;$(TargetFrameworks)</TargetFrameworks>
  </PropertyGroup>
</Project>"#,
        );
        assert_eq!(
            target_frameworks(&p),
            vec!["net10.0".to_string(), "net8.0".to_string()],
        );
    }
}
