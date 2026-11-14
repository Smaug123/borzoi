//! The raw token table (`borzoi_cst::tokens::RawTokens`) against the
//! full-trivia driver iterator it replaces, as a reference implementation of
//! the same stream (`docs/token-pipeline-soa-plan.md`, D7).
//!
//! The table stores a kind and a start offset per token and rebuilds each item
//! from them and the source text. The preprocessor's state machine writes it
//! directly, without the iterator's queue. It must rebuild exactly what the
//! iterator yields, item for item, spans included, with the preprocessor
//! errors the table keeps beside the stream and the `#elif` spans the same as
//! the iterator's. Under the `token-reference` feature every parse also
//! asserts this (`tokens::lex_full_trivia`), so the parser suites and the
//! corpus sweeps hold the table to it too; this group covers the lexical
//! shapes those suites need not reach.
//!
//! The lex also reports what the swallow-mode driver does
//! (`directives::preprocess`): every preprocessor error in stream order and
//! the `#line` directives, which a parse carries for the LSP's diagnostics.
//! The same cases hold it to that, and a parse to passing it on unchanged.

use std::collections::{BTreeSet, HashSet};

use borzoi_cst::directives::{PreprocError, lex_with_symbols_full_trivia, preprocess};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::lexer::TokenKind;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_cst::tokens::{RawItem, RawKind, lex_full_trivia};
use proptest::prelude::*;

use crate::parser_panic_sweep::WIDE;

/// Every lexical shape whose kind or text the table rebuilds by a rule of its
/// own: each interpolated-string fragment (with and without a byte suffix, and
/// the extended forms' delimiter length), the trivia and directive lines, the
/// lexer's error tokens, both line-break spellings and a lone `\r`, a BOM, a
/// tab, non-ASCII text, and an unlexable byte; plus a sample of the
/// payload-carrying tokens.
pub(crate) const LEXICAL: &[&str] = &[
    "$\"a\"",
    "$\"a\"B",
    "$\"a{1}b\"",
    "$\"a{1}b\"B",
    "$\"a{1}b{2}c\"",
    "$\"\"\"a\"\"\"",
    "$\"\"\"a\"\"\"B",
    "$\"\"\"a{1}b\"\"\"",
    "$@\"a\"",
    "$@\"a\"B",
    "@$\"a{1}b\"",
    "$$\"\"\"a\"\"\"",
    "$$\"\"\"a{{1}}b\"\"\"",
    "$$$\"\"\"a{{{1}}}b\"\"\"",
    "$\"a{",
    "}",
    "\"s\"",
    "\"s\"B",
    "\"unterminated",
    "@\"v\"",
    "\"\"\"t\"\"\"",
    "'c'",
    "'\\n'B",
    "(* c *)",
    "(* unterminated",
    "(*)",
    "// line\n",
    "#if X\n",
    "#elif Y\n",
    "#else\n",
    "#endif\n",
    "#line 3 \"f.fs\"\n",
    "# 4\n",
    "#nowarn 40\n",
    "#warnon 3370\n",
    "#if\n",
    "#endif junk\n",
    "#light\n",
    "__SOURCE_DIRECTORY__",
    "``quoted id``",
    "0x1F",
    "1uy",
    "0b1L",
    "0x1lf",
    "0x1LF",
    "1I",
    "1.5m",
    "1.0f",
    "1e3",
    "1..",
    "( * )",
    ".[..,..]",
    "<@@",
    "@@>",
    "|]",
    ">=",
    "<",
    ">",
    ":?>",
    "x'",
    "break",
    "\r\n",
    "\r",
    "\n",
    "\u{FEFF}",
    "\t",
    " ",
    "é",
    "→",
    "\u{1}",
    "`",
];

/// The symbol sets every case runs under: none, and the symbol `LEXICAL`'s
/// `#if` tests, so each arm is both active and inactive.
fn symbol_sets() -> [HashSet<String>; 2] {
    [HashSet::new(), HashSet::from(["X".to_string()])]
}

/// What the reference produces: the iterator's stream with the preprocessor
/// errors other than lexer errors split out (the parse reports those beside
/// the tree, and their spans do not tile the source), and the `#elif` spans.
struct Reference<'a> {
    stream: Vec<RawItem<'a>>,
    errors: Vec<(PreprocError, std::ops::Range<usize>)>,
    elif_directives: Vec<std::ops::Range<usize>>,
}

fn reference<'a>(src: &'a str, symbols: &HashSet<String>) -> Reference<'a> {
    let mut driver = lex_with_symbols_full_trivia(src, symbols);
    let mut stream = Vec::new();
    let mut errors = Vec::new();
    for (res, span) in driver.by_ref() {
        match res {
            Ok(_) | Err(PreprocError::Lex(_)) => stream.push((res, span)),
            Err(e) => errors.push((e, span)),
        }
    }
    Reference {
        stream,
        errors,
        elif_directives: driver.elif_directives().to_vec(),
    }
}

/// What a lex reported beside its stream, by shape: the preprocessor errors
/// by variant, and whether a `#line` directive was recorded. A generator that
/// stops reaching one has stopped testing that the report is the same.
fn report_shapes(src: &str, symbols: &HashSet<String>, seen: &mut BTreeSet<String>) {
    let report = preprocess(src, symbols);
    for (err, _) in &report.errors {
        let shape = format!("{err:?}");
        let variant = shape.split(['(', ' ', '{']).next().unwrap_or_default();
        seen.insert(variant.to_string());
    }
    if report.line_directives != Default::default() {
        seen.insert("#line".to_string());
    }
}

/// Assert the table rebuilds the reference on `src` under every symbol set,
/// recording the kinds it saw in `seen`.
fn check(src: &str, seen: &mut BTreeSet<RawKind>) {
    for symbols in symbol_sets() {
        let want = reference(src, &symbols);
        let got = lex_full_trivia(src, &symbols);
        assert_eq!(
            got.preprocessed,
            preprocess(src, &symbols),
            "the preprocessor's report differs on {src:?}"
        );
        let rebuilt: Vec<RawItem<'_>> = got.tokens.items(..).collect();
        assert_eq!(rebuilt, want.stream, "stream differs on {src:?}");
        assert_eq!(got.preproc_errors, want.errors, "errors differ on {src:?}");
        assert_eq!(
            got.elif_directives, want.elif_directives,
            "#elif spans differ on {src:?}"
        );
        assert_eq!(got.tokens.len(), want.stream.len());
        for i in 0..got.tokens.len() {
            let span = got.tokens.span(i);
            assert_eq!(got.tokens.text(i), &src[span]);
            seen.insert(got.tokens.kind(i));
        }
    }
}

/// The kinds rebuilt by a rule of their own rather than by the generated
/// `Token::from_kind` arm every plain kind shares. A generator that stops
/// reaching one of them has stopped testing that rule.
fn special_kinds() -> Vec<RawKind> {
    let mut kinds = vec![
        RawKind::HashLine,
        RawKind::WarnDirective,
        RawKind::HashIf,
        RawKind::HashElse,
        RawKind::HashElif,
        RawKind::HashEndif,
        RawKind::InactiveCode,
        RawKind::UnterminatedString,
        RawKind::UnterminatedComment,
        RawKind::UnknownToken,
    ];
    kinds.extend(
        [
            TokenKind::Less,
            TokenKind::Greater,
            TokenKind::InterpBeginEnd,
            TokenKind::InterpBeginEndBytes,
            TokenKind::InterpBegin,
            TokenKind::InterpPart,
            TokenKind::InterpEnd,
            TokenKind::InterpEndBytes,
            TokenKind::InterpTripleBeginEnd,
            TokenKind::InterpTripleBeginEndBytes,
            TokenKind::InterpTripleBegin,
            TokenKind::InterpVerbatimBeginEnd,
            TokenKind::InterpVerbatimBeginEndBytes,
            TokenKind::InterpVerbatimBegin,
            TokenKind::InterpExtendedBeginEnd,
            TokenKind::InterpExtendedBegin,
        ]
        .map(RawKind::Lexed),
    );
    kinds
}

/// Every one- and two-piece source over [`LEXICAL`].
#[test]
fn every_lexical_pair_rebuilds_the_driver_stream() {
    let mut seen = BTreeSet::new();
    for a in LEXICAL {
        check(a, &mut seen);
        for b in LEXICAL {
            check(&format!("{a}{b}"), &mut seen);
        }
    }
    let missed: Vec<_> = special_kinds()
        .into_iter()
        .filter(|k| !seen.contains(k))
        .collect();
    assert!(
        missed.is_empty(),
        "the lexical pairs never reach {missed:?}"
    );
}

/// `#if` chains no pair of [`LEXICAL`] spells: a second `#else`, and an
/// `#elif` after an `#else`, in an active and in a dead chain, with an error
/// token and a `#line` inside.
const CHAINS: &[&str] = &[
    "#if X\n#else\n#else\n#endif\n",
    "#if X\n#else\n#elif Y\n#endif\n",
    "#if X\n#if Y\n#else\n#else\n#endif\n#endif\n",
    "#if X\n\"a\n#else\n#line 7 \"g.fs\"\n`\n#elif Y\n#endif\n",
];

/// A parse carries the lex's report unchanged, whatever the language version
/// or file kind: every one-piece source over [`LEXICAL`] and every pair, and
/// [`CHAINS`], at both ends of the version ladder. Together they reach every
/// preprocessor error variant and a `#line` directive.
#[test]
fn a_parse_reports_what_the_preprocessor_does() {
    let mut shapes = BTreeSet::new();
    let mut sources: Vec<String> = LEXICAL
        .iter()
        .chain(CHAINS)
        .map(|a| a.to_string())
        .collect();
    sources.extend(
        LEXICAL
            .iter()
            .flat_map(|a| LEXICAL.iter().map(move |b| format!("{a}{b}"))),
    );
    for src in &sources {
        for symbols in symbol_sets() {
            report_shapes(src, &symbols, &mut shapes);
            let want = preprocess(src, &symbols);
            for (file_kind, lang) in [
                (FileKind::Impl, LanguageVersion::MIN),
                (FileKind::Sig, LanguageVersion::MAX),
            ] {
                let opts = ParseOptions {
                    file_kind,
                    symbols: &symbols,
                    lang,
                };
                assert_eq!(
                    parse_with_options(src, opts).preprocessor,
                    want,
                    "the parse's report differs on {src:?}"
                );
            }
        }
    }
    let missed: Vec<_> = [
        "Lex",
        "Directive",
        "UnmatchedEndIf",
        "OrphanElse",
        "OrphanElif",
        "DoubleElse",
        "ElifAfterElse",
        "UnclosedIfAtEof",
        "#line",
    ]
    .into_iter()
    .filter(|shape| !shapes.contains(*shape))
    .collect();
    assert!(missed.is_empty(), "the sources never reach {missed:?}");
}

/// Every one- and two-piece source over the panic sweep's `WIDE` alphabet.
#[test]
fn every_wide_pair_rebuilds_the_driver_stream() {
    let mut seen = BTreeSet::new();
    for a in WIDE {
        check(a, &mut seen);
        for b in WIDE {
            check(&format!("{a}{b}"), &mut seen);
        }
    }
}

proptest! {
    /// Random walks over both alphabets, at lengths the pairs cannot reach.
    #[test]
    fn random_sources_rebuild_the_driver_stream(
        pieces in prop::collection::vec(
            prop::sample::select(LEXICAL.iter().chain(WIDE).copied().collect::<Vec<_>>()),
            0..16,
        )
    ) {
        check(&pieces.concat(), &mut BTreeSet::new());
    }
}

/// The table's span searches and `items` ranges agree with the same
/// operations on the rebuilt stream, at every offset and every index range.
#[test]
fn span_searches_and_ranges_match_the_rebuilt_stream() {
    let src = "let x = $\"a{1}b\" (* c *)\n#if X\nlet y = 2\n#endif\n// tail\n";
    for symbols in symbol_sets() {
        let lexed = lex_full_trivia(src, &symbols);
        let stream: Vec<RawItem<'_>> = lexed.tokens.items(..).collect();
        for byte in 0..=src.len() + 1 {
            assert_eq!(
                lexed.tokens.count_starting_before(byte),
                stream.partition_point(|(_, s)| s.start < byte),
            );
            assert_eq!(
                lexed.tokens.count_ending_by(byte),
                stream.partition_point(|(_, s)| s.end <= byte),
            );
        }
        for lo in 0..=stream.len() {
            for hi in lo..=stream.len() {
                let got: Vec<_> = lexed.tokens.items(lo..hi).collect();
                assert_eq!(got, stream[lo..hi]);
                let back: Vec<_> = lexed.tokens.items(lo..hi).rev().collect();
                let want: Vec<_> = stream[lo..hi].iter().rev().cloned().collect();
                assert_eq!(back, want);
            }
            assert_eq!(lexed.tokens.items(lo..).count(), stream.len() - lo);
            assert_eq!(lexed.tokens.items(..lo).count(), lo);
            assert_eq!(lexed.tokens.get(lo), stream.get(lo).cloned());
        }
    }
}
