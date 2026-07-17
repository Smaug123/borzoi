//! Nullness legality gate (plan Stage 3 — the first *typed-node* feature gate).
//!
//! Under a sub-9.0 pin every `T | null` nullness type draws an FS3350 feature
//! diagnostic; at 9.0 / `preview` none do. Unlike the `#elif` gate (a trivia
//! feature checked via a span side-channel — see `langversion_gate.rs`), nullness
//! is a *node* in the tree (`WITH_NULL_TYPE`), so the gate is driven straight off
//! the green tree via the shared interval table: "the tree holds a node out of
//! surface at `lang`" *is* "the `vN` projection is not total here" (plan P2).
//!
//! The gate is diagnostic-only — the parse tree is byte-identical regardless of
//! `lang` (the tree is always the maximal/preview parse; the version is a lens +
//! diagnostic layer, not a reshape — `docs/completed/ast-versioning-nullness-proof.md`
//! D-proof-1). FCS instead gates nullness as a *parse divergence* (so it errors
//! differently on pre-9.0 `string | null`); we deliberately produce the maximal
//! tree + a diagnostic, sound under D7 ("incomplete, never wrong").

use std::collections::HashSet;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, Parse, ParseOptions, parse_with_options};
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

/// The FS3350 nullness feature diagnostic specifically. FCS's feature name is
/// `featureNullnessChecking` ("nullness checking", `FSComp.txt`).
const FEATURE_MSG: &str = "nullness checking";

fn nullness_diagnostics(p: &Parse) -> usize {
    p.errors
        .iter()
        .filter(|e| e.message.contains(FEATURE_MSG))
        .count()
}

/// Errors that are *not* the nullness feature gate — should be identical across
/// versions (the gate is the only version-dependent diagnostic for this source).
fn non_nullness_errors(p: &Parse) -> Vec<String> {
    p.errors
        .iter()
        .filter(|e| !e.message.contains(FEATURE_MSG))
        .map(|e| e.message.clone())
        .collect()
}

/// A program with `k` distinct `T | null` bindings (each a single nullness type).
fn program_with_nulls(k: usize) -> String {
    let mut s = String::new();
    for i in 0..k {
        s.push_str(&format!("let v{i} : string | null = failwith \"\"\n"));
    }
    s
}

const ONE_NULL: &str = "let x : string | null = failwith \"\"\n";

#[test]
fn nullness_gated_below_9_only() {
    // Below 9.0 the nullness type draws exactly one feature diagnostic; at 9.0
    // (its introduction), 10.0, 11.0, and preview it draws none.
    for lang in [
        LanguageVersion::V4_6,
        LanguageVersion::V7_0,
        LanguageVersion::V8_0,
    ] {
        assert_eq!(nullness_diagnostics(&parse_at(ONE_NULL, lang)), 1, "{lang}");
    }
    for lang in [
        LanguageVersion::V9_0,
        LanguageVersion::V10_0,
        LanguageVersion::V11_0,
        LanguageVersion::Preview,
    ] {
        assert_eq!(nullness_diagnostics(&parse_at(ONE_NULL, lang)), 0, "{lang}");
    }
}

#[test]
fn nullness_message_matches_fs3350_template() {
    // Mirrors FCS FS3350: feature name + the "Please use language version N.0 or
    // greater" tail the LSP routes past its overlap dedup.
    let p = parse_at(ONE_NULL, LanguageVersion::V8_0);
    let msg = p
        .errors
        .iter()
        .map(|e| e.message.as_str())
        .find(|m| m.contains(FEATURE_MSG))
        .expect("a nullness diagnostic under 8.0");
    assert!(msg.contains("not available in F# 8.0"), "{msg:?}");
    assert!(
        msg.contains("Please use language version 9.0 or greater"),
        "{msg:?}",
    );
}

#[test]
fn nullness_diagnostic_spans_the_nullness_type() {
    // The diagnostic points at the `string | null` type, not the whole binding.
    let p = parse_at(ONE_NULL, LanguageVersion::V8_0);
    let e = p
        .errors
        .iter()
        .find(|e| e.message.contains(FEATURE_MSG))
        .expect("a nullness diagnostic under 8.0");
    assert_eq!(&ONE_NULL[e.span.clone()], "string | null");
}

#[test]
fn gate_never_reshapes_the_tree() {
    // The tree is the maximal parse at every version — only the diagnostics differ.
    let v8 = parse_at(ONE_NULL, LanguageVersion::V8_0);
    let preview = parse_at(ONE_NULL, LanguageVersion::Preview);
    assert_eq!(
        format!("{:#?}", v8.root),
        format!("{:#?}", preview.root),
        "tree must not depend on lang",
    );
    assert_eq!(
        non_nullness_errors(&v8),
        non_nullness_errors(&preview),
        "non-gate errors differ by version",
    );
}

proptest! {
    /// P1: a program with `k` nullness bindings yields exactly `k` gate
    /// diagnostics under a sub-9.0 pin and none under `preview`; the tree is
    /// invariant.
    #[test]
    fn nullness_count_matches_under_v8(k in 0usize..10) {
        let src = program_with_nulls(k);
        let v8 = parse_at(&src, LanguageVersion::V8_0);
        let preview = parse_at(&src, LanguageVersion::Preview);
        prop_assert_eq!(nullness_diagnostics(&v8), k);
        prop_assert_eq!(nullness_diagnostics(&preview), 0);
        prop_assert_eq!(format!("{:#?}", v8.root), format!("{:#?}", preview.root));
    }
}
