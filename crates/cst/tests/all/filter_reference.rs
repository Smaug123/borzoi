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
//!
//! The parse holds the filtered table ([`FilteredTokens`]) to the reference
//! after the sign fold. [`check`] also holds the table to the reference filter
//! before it, so a fault in the table's encoding is not masked by the fold,
//! and records which [`FilteredKind`]s a generator reached: one that stops
//! reaching a kind rebuilt by a rule of its own has stopped testing that rule.

use std::collections::{BTreeSet, HashSet};

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::lexer::{Token, TokenKind};
use borzoi_cst::lexfilter::{FilteredToken, Virtual, filter_collect};
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_cst::tokens::{FilteredKind, FilteredTokens, RawTokens, lex_full_trivia};
use proptest::prelude::*;

use crate::line_table_reference::NEWLINE_SHAPES;
use crate::parser_panic_sweep::{CORE, WIDE};
use crate::token_table_reference::LEXICAL;

/// Shapes the filter rewrites by a rule of its own: type applications (the
/// promoted `<`/`>`, `HIGH_PRECEDENCE_*`, the fused closers it splits), the
/// splits of `1..`, `..^` and the quotation closers, the sign fold, glued
/// and spaced on each side, and the keywords whose offside contexts close with
/// virtual tokens.
const FILTER_SHAPES: &[&str] = &[
    "let x = 1\n",
    "fun x -> x",
    "match x with\n| _ -> 1\n",
    "f<int>",
    "f<int>(x)",
    "f<int list>]",
    "f<'a>>",
    "f<int>>=",
    "f<int>|]",
    "[<A<int>>]",
    "f(x)",
    "f[x]",
    "x.[0]",
    "1..2",
    "a.[..^1]",
    "<@ 1 @>.",
    "{| <@ 1 @>|}",
    "-1",
    "x-1",
    "(x)-1",
    "x -1",
    "- 1",
    "-1uy",
    "+1.5",
    "-0x1l",
];

/// Hold the live filter's table to the reference filter on `src` under `lang`
/// and `symbols`, before the sign fold, recording the kinds it saw.
///
/// Also hold the filter over the active stream alone
/// ([`RawTokens::from_active`], what [`borzoi_cst::lexfilter::filter`] and the
/// FCS differential read) to the filter over the full-trivia table the parser
/// reads: the gaps where the preprocessor removed code must filter as the
/// directive and inactive-code entries they replace.
fn check_table(
    src: &str,
    symbols: &HashSet<String>,
    lang: LanguageVersion,
    seen: &mut BTreeSet<FilteredKind>,
) {
    let lexed = lex_full_trivia(src, symbols);
    let live = filter_collect(&lexed.tokens, lang);
    let reference =
        borzoi_cst::reference::lexfilter::filter_collect(src, lang, lexed.tokens.active());
    let live_items: Vec<String> = live.tokens.items().map(|t| format!("{t:?}")).collect();
    let want: Vec<String> = reference.tokens.iter().map(|t| format!("{t:?}")).collect();
    assert_eq!(
        live_items, want,
        "filtered table differs at {lang} on {src:?}"
    );
    let from_active = filter_collect(&RawTokens::from_active(src, lexed.tokens.active()), lang);
    assert_eq!(
        from_active
            .tokens
            .items()
            .map(|t| format!("{t:?}"))
            .collect::<Vec<_>>(),
        live_items,
        "the active stream filters differently from the table at {lang} on {src:?}"
    );
    assert_eq!(
        (
            from_active.diagnostics,
            from_active.shape_depends_on_language_version
        ),
        (
            live.diagnostics.clone(),
            live.shape_depends_on_language_version
        ),
        "the active stream's side outputs differ at {lang} on {src:?}"
    );
    for i in 0..live.tokens.len() {
        let kind = live.tokens.kind(i);
        assert_eq!(
            Some(kind),
            live.tokens.get(i).map(|(res, _)| FilteredKind::of(&res)),
            "kind {i} on {src:?}"
        );
        seen.insert(kind);
    }
}

/// Parse `src` under every language-version band and `#if` symbol set; each
/// parse runs the reference comparison. Also hold the table to the reference
/// filter directly ([`check_table`]).
///
/// Each source is checked again followed by an aligned sibling line, so the
/// filter inserts an `OBLOCKSEP` after whatever token the source ends with.
/// That token's span decides where the `OBLOCKSEP` starts, and the filter
/// rewrites some tokens it ends with (a promoted `>`, the pieces of a split),
/// so a fault in what it records about the token it last emitted shows only
/// there.
///
/// Each of those is checked again with a multi-byte character before every
/// line break: a virtual token the filter places after a line's last token
/// takes a span from that token's end, which then starts inside the
/// character, so the table must never slice a virtual token's span.
fn check(src: &str, seen: &mut BTreeSet<FilteredKind>) {
    let sibling = format!("{src}\nx\n");
    let perturbed = src.replace('\n', "😃\n");
    let perturbed_sibling = sibling.replace('\n', "😃\n");
    for src in [src, &sibling, &perturbed, &perturbed_sibling] {
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
                check_table(src, &symbols, lang, seen);
            }
        }
    }
}

/// A virtual token whose span starts inside a multi-byte character (the
/// `OBLOCKSEP` after `1😃`) is rebuilt without slicing it.
#[test]
fn a_virtual_span_inside_a_character_is_not_sliced() {
    check("let x = 1😃\nlet y = 2\n", &mut BTreeSet::new());
}

/// The filtered kinds rebuilt by a rule of their own, or that only a
/// rewriting arm of the filter produces.
fn special_kinds() -> Vec<FilteredKind> {
    let mut kinds = vec![
        FilteredKind::TyparLess,
        FilteredKind::TyparGreater,
        FilteredKind::Token(TokenKind::Less),
        FilteredKind::Token(TokenKind::Greater),
        FilteredKind::UnterminatedString,
        FilteredKind::UnterminatedComment,
        FilteredKind::UnknownToken,
        FilteredKind::Token(TokenKind::InterpBegin),
        FilteredKind::Token(TokenKind::InterpPart),
        FilteredKind::Token(TokenKind::InterpEnd),
        FilteredKind::Token(TokenKind::InterpExtendedBegin),
        FilteredKind::Token(TokenKind::RQuote),
        FilteredKind::Token(TokenKind::BarRBrace),
        FilteredKind::Token(TokenKind::DotDot),
    ];
    kinds.extend(
        [
            Virtual::Let,
            Virtual::BlockBegin,
            Virtual::BlockEnd,
            Virtual::DeclEnd,
            Virtual::BlockSep,
            Virtual::Fun,
            Virtual::End,
            Virtual::With,
            Virtual::RightBlockEnd,
            Virtual::HighPrecedenceTyApp,
            Virtual::HighPrecedenceParenApp,
            Virtual::HighPrecedenceBrackApp,
        ]
        .map(FilteredKind::Virtual),
    );
    kinds
}

fn assert_reached(seen: &BTreeSet<FilteredKind>, what: &str) {
    let missed: Vec<_> = special_kinds()
        .into_iter()
        .filter(|k| !seen.contains(k))
        .collect();
    assert!(missed.is_empty(), "{what} never reach {missed:?}");
}

/// The pieces the pairs and the random walk draw on: the lexical alphabet,
/// the filter's rewriting shapes, and line breaks inside tokens and inactive
/// code, which move the line of every token after them.
fn pieces() -> impl Iterator<Item = &'static &'static str> + Clone {
    LEXICAL.iter().chain(FILTER_SHAPES).chain(NEWLINE_SHAPES)
}

/// Every one- and two-piece source over [`pieces`].
#[test]
fn every_lexical_pair_matches_the_reference_filter() {
    let mut seen = BTreeSet::new();
    for a in pieces() {
        check(a, &mut seen);
        for b in pieces() {
            check(&format!("{a}{b}"), &mut seen);
        }
    }
    assert_reached(&seen, "the lexical pairs");
}

/// Every three-piece source over the panic sweep's `CORE` alphabet, the
/// shortest length at which `)-1` is spelled (a filter change that moves how it
/// folds passes every pair). The parse half also runs in the panic sweep; the
/// table half is this group's.
#[test]
fn every_core_triple_matches_the_reference_filter() {
    let mut seen = BTreeSet::new();
    for a in CORE {
        for b in CORE {
            for c in CORE {
                let src = format!("{a}{b}{c}\n");
                let symbols = HashSet::new();
                for lang in LanguageVersion::SHAPE_REPRESENTATIVES {
                    check_table(&src, &symbols, lang, &mut seen);
                }
            }
        }
    }
}

/// Every filtered kind survives the table: the kind of the entry a kind
/// rebuilds is that kind, whatever its text.
#[test]
fn every_filtered_kind_rebuilds_to_itself() {
    let mut kinds: Vec<FilteredKind> = TokenKind::ALL
        .iter()
        .copied()
        .filter(|&k| {
            !matches!(
                k,
                TokenKind::Whitespace
                    | TokenKind::Newline
                    | TokenKind::LineComment
                    | TokenKind::BlockComment
            )
        })
        .map(FilteredKind::Token)
        .collect();
    kinds.extend(special_kinds());
    for kind in kinds {
        for text in ["x", "$$\"\"\"", "-1"] {
            assert_eq!(FilteredKind::of(&kind.rebuild(text)), kind, "{text:?}");
        }
    }
}

/// The table holds no trivia: the filter never emits it, and a kind could not
/// say which trivia it was.
#[test]
#[should_panic(expected = "the filtered stream holds no trivia")]
fn the_table_refuses_trivia() {
    FilteredTokens::from_items(" ", [(Ok(FilteredToken::Raw(Token::Whitespace)), 0..1)]);
}

/// The table rebuilds a token's payload from its span, so a token whose
/// payload is not its source text is refused.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "is not rebuilt from its kind and text")]
fn the_table_refuses_a_payload_that_is_not_its_text() {
    FilteredTokens::from_items("ab", [(Ok(FilteredToken::Raw(Token::Ident("a"))), 1..2)]);
}

proptest! {
    /// Random walks over both alphabets, at lengths the pairs cannot reach.
    #[test]
    fn random_sources_match_the_reference_filter(
        pieces in prop::collection::vec(
            prop::sample::select(
                pieces().chain(WIDE).copied().collect::<Vec<_>>(),
            ),
            0..16,
        )
    ) {
        check(&pieces.concat(), &mut BTreeSet::new());
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
    check("let x =\n  1\n", &mut BTreeSet::new());
}
