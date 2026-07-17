//! `Parse::shape_depends_on_language_version` — the version-shape-sensitivity
//! flag (see `FilterRun::shape_depends_on_language_version`).
//!
//! The lex-filter's strict-indentation gate (F# 8,
//! `LanguageVersion::strict_indentation_is_error`) is the one place the
//! filtered stream's *shape* depends on the language version: a version-gated
//! context push whose anchor is offside is aborted at F# 8+ but kept (with a
//! warning) below. The flag records reaching such a point, and `false` must
//! **prove** the tree is identical under every version — that is the property
//! the LSP's fold trusts when a project's `<LangVersion>` provenance is
//! unknowable.

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

/// The structural fingerprint of a parse tree: rowan's debug rendering covers
/// every node kind and span, and both parses are lossless over the same
/// source, so equal renderings ⇔ equal trees.
fn tree_shape(p: &Parse) -> String {
    format!("{:#?}", p.root)
}

/// A deterministic straddling fixture: an offside version-gated push (the
/// EOF-anchored `MatchClauses` — FCS reads EOF as column −1, offside of the
/// enclosing top-level block), so the tree genuinely differs across the F# 8
/// boundary and the flag must be set under both versions. Found by the
/// property test below (its shrunk counterexample against a stubbed flag);
/// the LSP's fold tests reuse the same shape.
const STRADDLING: &str = "match x with\n";

#[test]
fn straddling_source_sets_the_flag_and_diverges() {
    let v7 = parse_at(STRADDLING, LanguageVersion::V7_0);
    let v10 = parse_at(STRADDLING, LanguageVersion::V10_0);
    assert_ne!(
        tree_shape(&v7),
        tree_shape(&v10),
        "fixture must genuinely straddle the F# 8 boundary"
    );
    assert!(v7.shape_depends_on_language_version);
    assert!(v10.shape_depends_on_language_version);
}

/// The flag is a sound *over*-approximation: an EOF-anchored version-gated
/// push differs as a context-stack operation (aborted vs kept), but the EOF
/// force-closure cascade can reconverge to the identical tree. Consumers that
/// would pay for a false positive (the LSP's project fold, where this shape —
/// `module M =` at end of file — is a common mid-edit state) must verify
/// genuine divergence by comparing a parse from the other side of the
/// boundary before acting on `true`; `false` alone stays the proof.
#[test]
fn eof_anchored_flag_can_over_approximate() {
    let src = "module M =\n";
    let v7 = parse_at(src, LanguageVersion::V7_0);
    let v10 = parse_at(src, LanguageVersion::V10_0);
    assert!(v10.shape_depends_on_language_version);
    assert_eq!(
        tree_shape(&v7),
        tree_shape(&v10),
        "this fixture documents the over-approximation; if the filter gains \
         a precise flag, fold-side verification can be reconsidered"
    );
}

#[test]
fn plain_source_does_not_set_the_flag() {
    let src = "module M\n\nlet answer = 42\n\nlet double = answer * 2\n";
    let v7 = parse_at(src, LanguageVersion::V7_0);
    let v10 = parse_at(src, LanguageVersion::V10_0);
    assert_eq!(tree_shape(&v7), tree_shape(&v10));
    assert!(!v7.shape_depends_on_language_version);
    assert!(!v10.shape_depends_on_language_version);
}

/// Indentation-soup generator: short statement fragments under random
/// indents, exactly the terrain where offside context pushes fire.
fn snippet() -> impl Strategy<Value = String> {
    let line = (
        0usize..6,
        prop_oneof![
            Just("let x = 1"),
            Just("let f () ="),
            Just("if true then"),
            Just("else 2"),
            Just("1"),
            Just("x + 1"),
            Just("match x with"),
            Just("| _ -> 1"),
            Just("do ()"),
            Just("fun () ->"),
            Just("()"),
        ],
    )
        .prop_map(|(indent, stmt)| format!("{}{}", " ".repeat(indent), stmt));
    proptest::collection::vec(line, 1..8).prop_map(|lines| {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    })
}

proptest! {
    /// Soundness (the property the LSP fold trusts): an unset flag proves the
    /// tree is identical across the F# 8 boundary. And symmetry: the first
    /// divergence point is reached in the same state by both runs, so the two
    /// runs always agree on the flag.
    #[test]
    fn unset_flag_proves_version_invariance(src in snippet()) {
        let v7 = parse_at(&src, LanguageVersion::V7_0);
        let v10 = parse_at(&src, LanguageVersion::V10_0);
        prop_assert_eq!(
            v7.shape_depends_on_language_version,
            v10.shape_depends_on_language_version,
            "both runs reach the first version-gated divergence point identically"
        );
        if !v10.shape_depends_on_language_version {
            prop_assert_eq!(
                tree_shape(&v7),
                tree_shape(&v10),
                "an unset flag must prove the tree is version-invariant"
            );
        }
    }
}
