//! The filtered stream against the frozen reference pipeline
//! (`borzoi_cst::reference`, `docs/token-pipeline-soa-plan.md` D7).
//!
//! Under the `token-reference` feature every parse asserts that its filtered
//! stream, offside diagnostics and language-version flag equal the reference
//! pipeline's, so the parser suites, the panic sweep's exhaustive `WIDE` pairs
//! and `CORE` triples, and the corpus sweeps all hold the live pipeline to it.
//! (The triples matter: dropping `)` from the live filter's atomic-expression
//! ends changes how `)-1` folds, which no pair spells and the triples catch.)
//! This group adds the
//! lexical shapes those need not reach (every string, comment, directive and
//! line-break spelling of `token_table_reference::LEXICAL`), at each
//! language-version band the filter's shape depends on, with the `#if` arms
//! both active and inactive; and checks that the assertion is live.

use std::collections::HashSet;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use proptest::prelude::*;

use crate::parser_panic_sweep::WIDE;
use crate::token_table_reference::LEXICAL;

/// Parse `src` under every language-version band and `#if` symbol set; each
/// parse runs the reference comparison.
fn check(src: &str) {
    for symbols in [HashSet::new(), HashSet::from(["X".to_string()])] {
        for lang in LanguageVersion::SHAPE_REPRESENTATIVES {
            parse_with_options(
                src,
                ParseOptions {
                    file_kind: FileKind::Impl,
                    symbols: &symbols,
                    lang,
                },
            );
        }
    }
}

/// Every one- and two-piece source over the lexical alphabet.
#[test]
fn every_lexical_pair_matches_the_reference_filter() {
    for a in LEXICAL {
        check(a);
        for b in LEXICAL {
            check(&format!("{a}{b}"));
        }
    }
}

proptest! {
    /// Random walks over both alphabets, at lengths the pairs cannot reach.
    #[test]
    fn random_sources_match_the_reference_filter(
        pieces in prop::collection::vec(
            prop::sample::select(LEXICAL.iter().chain(WIDE).copied().collect::<Vec<_>>()),
            0..16,
        )
    ) {
        check(&pieces.concat());
    }
}

/// The comparison runs inside every parse: the reference module is compiled
/// into this binary, and a parse of a source the filter rewrites goes through
/// it without failing.
#[test]
fn the_reference_comparison_is_compiled_in() {
    let run = borzoi_cst::reference::filtered_stream(
        "let x =\n  1\n",
        &HashSet::new(),
        LanguageVersion::Preview,
    );
    assert!(
        run.tokens.len() > 3,
        "the reference filter inserts layout tokens: {:?}",
        run.tokens
    );
    check("let x =\n  1\n");
}
