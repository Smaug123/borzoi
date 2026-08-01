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

use crate::common::catch_unwind_silent;

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
            // Version-*gated* lines, so the diagnostics property below has a
            // subject: `#elif` is a legality gate at F# 11 and a nullness
            // annotation one at F# 9. Both leave the tree untouched, which is
            // exactly why the shape flag cannot stand in for the diagnostics
            // one.
            Just("#if FOO"),
            Just("#elif BAR"),
            Just("#endif"),
            Just("let g (x : System.String | null) = 1"),
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

proptest! {
    /// The diagnostics counterpart of [`unset_flag_proves_version_invariance`],
    /// and the property the LSP's recovery reading trusts when a project's
    /// `LangVersion` provenance is unknowable: an unset
    /// `diagnostics_depend_on_language_version` must **prove** the reported
    /// errors and warnings are identical at every language version.
    ///
    /// This is the guard the shape flag cannot be: a legality gate reports its
    /// feature error and then parses the construct anyway, so the tree is
    /// byte-identical across the threshold while the diagnostics are not.
    /// Checking every version rather than the two extremes the flag itself
    /// compares keeps the property independent of the monotonicity argument
    /// that justifies looking at only two.
    #[test]
    fn unset_diagnostics_flag_proves_version_invariance(src in snippet()) {
        const VERSIONS: &[LanguageVersion] = &[
            LanguageVersion::V4_6,
            LanguageVersion::V5_0,
            LanguageVersion::V7_0,
            LanguageVersion::V8_0,
            LanguageVersion::V9_0,
            LanguageVersion::V10_0,
            LanguageVersion::V11_0,
            LanguageVersion::Preview,
        ];
        if !borzoi_cst::parser::diagnostics_are_version_invariant(
            &src,
            &HashSet::new(),
            FileKind::Impl,
        ) {
            return Ok(());
        }
        let baseline = parse_at(&src, LanguageVersion::DEFAULT);
        for &lang in VERSIONS {
            let p = parse_at(&src, lang);
            prop_assert_eq!(
                &p.errors,
                &baseline.errors,
                "unset flag must prove the errors are version-invariant, but {:?} differs at {}",
                src,
                lang
            );
            prop_assert_eq!(
                &p.warnings,
                &baseline.warnings,
                "unset flag must prove the warnings are version-invariant, but {:?} differs at {}",
                src,
                lang
            );
        }
    }
}

/// Punctuation soup over the characters that reach the parser's hand-written
/// invariant guards — bracket and pipe openers, dotted heads, and the `match`
/// keyword, which is where a const-payload guard lives. The
/// [`snippet`] generator builds well-shaped *lines*, so it never reaches them;
/// this one is deliberately not F#.
fn adversarial_soup() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just(">"),
        Just("("),
        Just(")"),
        Just("{"),
        Just("}"),
        Just("["),
        Just("]"),
        Just("|"),
        Just("."),
        Just(".."),
        Just("match"),
        Just("with"),
        Just("let"),
        Just("\n"),
        Just(" "),
    ];
    proptest::collection::vec(piece, 1..12).prop_map(|ps| {
        let mut s = ps.concat();
        s.push('\n');
        s
    })
}

/// A parser panic at an endpoint version must not escape the verdict.
///
/// The parser is hand-written recursive descent whose invariant guards fire on
/// some malformed input — a pre-existing hazard the LSP contains by wrapping
/// every parse in `catch_unwind`. Asking for the verdict re-parses at versions
/// the caller never requested, so a buffer that parses at the *asked-for*
/// version can still panic at an endpoint, on a path with no wrapper around it
/// (`borzoi_sema::SyntaxRecovery::of_guessed_version` calls straight in).
///
/// A panic proves nothing about version-invariance, so the conservative reading
/// is the version-dependent one: the caller retains no diagnostics and commits
/// nothing.
#[test]
fn an_endpoint_parser_panic_does_not_escape_the_verdict() {
    let src = ">(match|.\n>\nmatch}..";
    assert!(
        catch_unwind_silent(|| parse_at(src, LanguageVersion::MIN)).is_err(),
        "this buffer must still panic the bottom-of-ladder parse"
    );
    assert!(
        catch_unwind_silent(|| parse_at(src, LanguageVersion::MAX)).is_ok(),
        "...and must still be parsed by the top one, or the case proves nothing"
    );

    let verdict = catch_unwind_silent(|| {
        borzoi_cst::parser::diagnostics_are_version_invariant(src, &HashSet::new(), FileKind::Impl)
    });
    assert_eq!(
        verdict.ok(),
        Some(false),
        "a panicking endpoint must be contained, and read as version-dependent"
    );
}

proptest! {
    /// The generated form of [`an_endpoint_parser_panic_does_not_escape_the_verdict`]:
    /// no input makes the verdict itself panic.
    #[test]
    fn the_verdict_never_escapes_a_parser_panic(src in adversarial_soup()) {
        let verdict = catch_unwind_silent(|| {
            borzoi_cst::parser::diagnostics_are_version_invariant(
                &src,
                &HashSet::new(),
                FileKind::Impl,
            )
        });
        prop_assert!(
            verdict.is_ok(),
            "the verdict must contain an endpoint panic, but {:?} escaped",
            src
        );
    }

    /// Two endpoints decide the verdict, and the monotone-threshold argument
    /// covers *diagnostics* between them — it says nothing about a parser guard
    /// firing at an interior version only. So an invariant verdict must also
    /// mean no version panics; otherwise the caller trusts a reading assembled
    /// from versions it never checked.
    #[test]
    fn an_invariant_verdict_means_no_version_panics(src in adversarial_soup()) {
        let Ok(true) = catch_unwind_silent(|| {
            borzoi_cst::parser::diagnostics_are_version_invariant(
                &src,
                &HashSet::new(),
                FileKind::Impl,
            )
        }) else {
            return Ok(());
        };
        let every_version = LanguageVersion::NUMBERED
            .into_iter()
            .chain([LanguageVersion::Preview]);
        for lang in every_version {
            prop_assert!(
                catch_unwind_silent(|| parse_at(&src, lang)).is_ok(),
                "verdict says invariant, but {:?} panics at {}",
                src,
                lang
            );
        }
    }
}
