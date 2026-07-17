//! `#elif` legality gate (plan Stage 1 / property P1).
//!
//! Under a sub-11.0 pin every `#elif` directive draws a feature diagnostic;
//! under 11.0 / `preview` none do. The gate is diagnostic-only — the parse tree
//! is byte-identical regardless of `lang` — mirroring FCS, which reports the
//! `PreprocessorElif` feature error and then parses the directive anyway. See
//! `docs/ast-versioning-plan.md`.

use std::collections::HashSet;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, Parse, ParseOptions, parse, parse_with_options};
use proptest::prelude::*;

fn parse_at(src: &str, lang: LanguageVersion) -> Parse {
    let symbols = HashSet::new();
    parse_with_options(
        src,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &symbols,
            lang,
        },
    )
}

/// The FS3350 `#elif` feature diagnostic specifically — *not* directive-syntax
/// errors (a bare `#elif` still produces a `MissingSeparator` error, which is
/// not the feature gate and must not be counted here).
const FEATURE_MSG: &str = "#elif preprocessor directive";

fn elif_diagnostics(p: &Parse) -> usize {
    p.errors
        .iter()
        .filter(|e| e.message.contains(FEATURE_MSG))
        .count()
}

/// Errors that are *not* the `#elif` feature gate — should be identical across
/// versions (the gate is the only version-dependent diagnostic).
fn non_elif_errors(p: &Parse) -> Vec<String> {
    p.errors
        .iter()
        .filter(|e| !e.message.contains(FEATURE_MSG))
        .map(|e| e.message.clone())
        .collect()
}

/// A `#if`/`#elif`/`#endif` chain with `k` `#elif` arms (no symbols defined, so
/// every arm is dead — the directives are still lexed and gated).
fn chain_with_elifs(k: usize) -> String {
    let mut s = String::from("#if C0\n0\n");
    for i in 1..=k {
        s.push_str(&format!("#elif C{i}\n{i}\n"));
    }
    s.push_str("#endif\n");
    s
}

const ONE_ELIF: &str = "#if FOO\n1\n#elif BAR\n2\n#endif\n";

#[test]
fn elif_rejected_under_v10() {
    let p = parse_at(ONE_ELIF, LanguageVersion::V10_0);
    assert_eq!(elif_diagnostics(&p), 1, "errors: {:?}", p.errors);
    // FCS FS3350 wording, rendered for the pinned version.
    let msg = &p.errors[0].message;
    assert!(msg.contains("not available in F# 10.0"), "{msg}");
    assert!(msg.contains("language version 11.0 or greater"), "{msg}");
    assert_eq!(p.lang, LanguageVersion::V10_0);
}

#[test]
fn elif_accepted_under_preview_and_v11() {
    for lang in [LanguageVersion::Preview, LanguageVersion::V11_0] {
        let p = parse_at(ONE_ELIF, lang);
        assert_eq!(elif_diagnostics(&p), 0, "{lang:?}: {:?}", p.errors);
        assert_eq!(p.errors.len(), 0, "{lang:?}: {:?}", p.errors);
    }
}

#[test]
fn convenience_entry_points_default_to_preview() {
    // `parse` and friends keep the parser's historical all-features-on behaviour
    // — `#elif` is accepted, no gate diagnostic.
    let p = parse(ONE_ELIF);
    assert_eq!(p.lang, LanguageVersion::Preview);
    assert_eq!(elif_diagnostics(&p), 0, "{:?}", p.errors);
}

#[test]
fn no_elif_means_no_gate_diagnostic_even_at_v10() {
    let src = "#if FOO\n1\n#else\n2\n#endif\n";
    assert_eq!(elif_diagnostics(&parse_at(src, LanguageVersion::V10_0)), 0);
}

#[test]
fn nested_inactive_elif_is_gated() {
    // `#elif` inside a `#if` nested under an inactive outer branch. FCS
    // feature-checks it anyway (lex.fsl `ifdefSkip`, nested `n > 0` arm), even
    // though the whole inner region collapses to one inactive trivia token — so
    // a marker-only scan would miss it. The gate reads recognised directives,
    // not emitted markers, so it is caught.
    let src = "#if UNDEF\n#if UNDEF2\n1\n#elif UNDEF3\n2\n#endif\n#endif\n";
    assert_eq!(elif_diagnostics(&parse_at(src, LanguageVersion::V10_0)), 1);
    assert_eq!(
        elif_diagnostics(&parse_at(src, LanguageVersion::Preview)),
        0
    );
}

#[test]
fn bare_elif_is_not_gated() {
    // A bare `#elif` (no separating whitespace + body) does not match FCS's
    // feature-checked rule — FCS treats it as whitespace and the parser reports
    // the malformed directive. So it draws a `MissingSeparator` error but *no*
    // feature diagnostic, at any version.
    let src = "#if FOO\n1\n#elif\n2\n#endif\n";
    let p = parse_at(src, LanguageVersion::V10_0);
    assert_eq!(
        elif_diagnostics(&p),
        0,
        "bare #elif must not be feature-gated"
    );
    // ...but the directive is still malformed, independent of the gate.
    assert!(
        !non_elif_errors(&p).is_empty(),
        "expected a directive-syntax error for the bare #elif",
    );
}

#[test]
fn malformed_elif_body_is_still_gated() {
    // `#elif !` has a separator, so FCS feature-checks it *before* evaluating
    // the (invalid) expression — gate fires, plus a separate expression error.
    let src = "#if FOO\n1\n#elif !\n2\n#endif\n";
    assert_eq!(elif_diagnostics(&parse_at(src, LanguageVersion::V10_0)), 1);
}

#[test]
fn gate_is_diagnostic_only_tree_is_invariant() {
    // The green tree (structure *and* text) must not depend on `lang`, and the
    // only error difference is the `#elif` gate.
    let v10 = parse_at(ONE_ELIF, LanguageVersion::V10_0);
    let preview = parse_at(ONE_ELIF, LanguageVersion::Preview);
    assert_eq!(
        format!("{:#?}", v10.root),
        format!("{:#?}", preview.root),
        "tree structure differs by version",
    );
    assert_eq!(
        v10.root.text().to_string(),
        preview.root.text().to_string(),
        "tree text differs by version",
    );
    assert_eq!(
        non_elif_errors(&v10),
        non_elif_errors(&preview),
        "non-gate errors differ by version",
    );
}

proptest! {
    /// P1: a chain with `k` `#elif` arms yields exactly `k` gate diagnostics
    /// under a sub-11.0 pin and none under `preview`; the tree is invariant.
    #[test]
    fn elif_count_matches_under_v10(k in 0usize..12) {
        let src = chain_with_elifs(k);
        let v10 = parse_at(&src, LanguageVersion::V10_0);
        let preview = parse_at(&src, LanguageVersion::Preview);
        prop_assert_eq!(elif_diagnostics(&v10), k);
        prop_assert_eq!(elif_diagnostics(&preview), 0);
        prop_assert_eq!(format!("{:#?}", v10.root), format!("{:#?}", preview.root));
    }

    /// The same `k`-arm chain nested under an inactive outer `#if` (its whole
    /// body dead) is still gated `k` times — FCS feature-checks `#elif` in
    /// skipped and nested branches alike.
    #[test]
    fn nested_inactive_elif_count_matches(k in 0usize..12) {
        let src = format!("#if UNDEF_OUTER\n{}#endif\n", chain_with_elifs(k));
        prop_assert_eq!(elif_diagnostics(&parse_at(&src, LanguageVersion::V10_0)), k);
        prop_assert_eq!(elif_diagnostics(&parse_at(&src, LanguageVersion::Preview)), 0);
    }
}
