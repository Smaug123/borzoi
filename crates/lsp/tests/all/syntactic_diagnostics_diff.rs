//! The syntactic diagnostics read from one parse
//! (`borzoi::diagnostics::syntactic_diagnostics`) against the producer they
//! replace, which lexed the buffer three times beside the parse
//! (`common::diagnostics_reference`): for every generated source, symbol
//! set, language version and grammar, the same groups of the same
//! diagnostics in the same order.
//!
//! The sources are random walks over pieces that reach every producer and
//! every rule between them: lexer errors, each preprocessor error, `#line`
//! directives with and without a file, parser errors the overlap rule drops
//! and keeps, the language-version feature errors exempt from it, warnings,
//! tabs, non-ASCII text and both line breaks. `parser_corpus_sweep` holds the
//! two to each other over the corpus.

use std::cell::RefCell;
use std::collections::HashSet;

use borzoi::diagnostics::{FileDiagnostics, SourceKind, grouped_diagnostics};
use borzoi_cst::language_version::LanguageVersion;
use lsp_types::DiagnosticSeverity;
use proptest::prelude::*;

use crate::common::diagnostics_reference;

const PIECES: &[&str] = &[
    "let x = 1\n",
    "let y = (\n",
    "let s = \"open\n",
    "(* open comment\n",
    "let c = `\n",
    "let u = \u{1}\n",
    "\tlet t = 2\n",
    "let é = \"→\"\r\n",
    "match x with\n",
    "| _ ->\n",
    "  ()\n",
    "type T =\n",
    "  abstract M : int -> int\n",
    "val f : int\n",
    "let atomic = 1\n",
    "let z : string | null = null\n",
    "module M =\n",
    "#if X\n",
    "#if Y\n",
    "#if\n",
    "#if (\n",
    "#elif Y\n",
    "#elif !\n",
    "#else\n",
    "#endif\n",
    "#endif junk\n",
    "#line 10 \"other.fs\"\n",
    "#line 3\n",
    "# 20 \"third.fs\"\n",
    "#nowarn 40\n",
    "\n",
    "  ",
];

const LANGS: [LanguageVersion; 5] = [
    LanguageVersion::MIN,
    LanguageVersion::V8_0,
    LanguageVersion::V10_0,
    LanguageVersion::DEFAULT,
    LanguageVersion::MAX,
];

fn symbol_set(bits: u8) -> HashSet<String> {
    ["X", "Y"]
        .into_iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .map(|(_, s)| s.to_string())
        .collect()
}

/// What the generated cases reached, by the reference's output.
#[derive(Debug, Default)]
struct Reached {
    lexer_errors: u64,
    directive_errors: u64,
    parser_errors: u64,
    warnings: u64,
    feature_errors: u64,
    cross_file_groups: u64,
    /// Cases whose parser reported something the overlap rule dropped.
    dropped_by_overlap: u64,
}

fn census(
    src: &str,
    symbols: &HashSet<String>,
    kind: SourceKind,
    lang: LanguageVersion,
    groups: &[FileDiagnostics],
    reached: &mut Reached,
) {
    let all: Vec<_> = groups.iter().flat_map(|g| &g.diagnostics).collect();
    let lexical = borzoi::diagnostics::diagnostics_for(src, symbols);
    for d in &all {
        let m = d.message.as_str();
        if m.contains("unterminated") || m.contains("unrecognised token") {
            reached.lexer_errors += 1;
        }
        if m.contains("#if") || m.contains("#endif") || m.contains("#else") || m.contains("#elif") {
            reached.directive_errors += 1;
        }
        if m.contains("Please use language version") {
            reached.feature_errors += 1;
        }
        if d.severity == Some(DiagnosticSeverity::WARNING) {
            reached.warnings += 1;
        }
    }
    reached.cross_file_groups += (groups.len() - 1) as u64;
    let parser = borzoi::diagnostics::parse_diagnostics(src, symbols, kind, lang);
    reached.parser_errors += parser.len() as u64;
    let symbols_owned = symbols.clone();
    let opts = borzoi_cst::parser::ParseOptions {
        file_kind: kind.file_kind(),
        symbols: &symbols_owned,
        lang,
    };
    let raw = borzoi_cst::parser::parse_with_options(src, opts);
    if raw.errors.len() + raw.warnings.len() > parser.len() && !lexical.is_empty() {
        reached.dropped_by_overlap += 1;
    }
}

fn check(
    pieces: &[&str],
    bits: u8,
    lang: usize,
    signature: bool,
    reached: &mut Reached,
) -> Result<(), TestCaseError> {
    let src = pieces.concat();
    let symbols = symbol_set(bits);
    let kind = if signature {
        SourceKind::Signature
    } else {
        SourceKind::Implementation
    };
    let lang = LANGS[lang];
    let got = grouped_diagnostics(&src, &symbols, kind, lang);
    let want = diagnostics_reference::grouped_diagnostics(&src, &symbols, kind, lang);
    prop_assert_eq!(
        &got,
        &want,
        "{:?} under {:?} at {} as {:?}",
        src,
        symbols,
        lang,
        kind
    );
    census(&src, &symbols, kind, lang, &want, reached);
    Ok(())
}

fn case() -> impl Strategy<Value = (Vec<&'static str>, u8, usize, bool)> {
    (
        prop::collection::vec(prop::sample::select(PIECES), 0..14),
        0u8..4,
        0..LANGS.len(),
        prop::bool::weighted(0.25),
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn one_parse_reports_what_three_lexes_and_a_parse_did(
        (pieces, bits, lang, signature) in case(),
    ) {
        check(&pieces, bits, lang, signature, &mut Reached::default())?;
    }
}

/// Every one- and two-piece source, under every symbol set, at the default
/// language version, as an implementation.
#[test]
fn every_pair_of_pieces_agrees() {
    let mut reached = Reached::default();
    for a in PIECES {
        for b in PIECES {
            for bits in 0..4 {
                check(&[a, b], bits, 3, false, &mut reached).unwrap();
            }
        }
    }
}

/// The generated cases reach every rule the two producers apply, over a
/// fixed seed.
#[test]
fn the_generator_reaches_every_producer_and_rule() {
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 256,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[5; 32]),
    );
    let reached = RefCell::new(Reached::default());
    runner
        .run(&case(), |(pieces, bits, lang, signature)| {
            check(&pieces, bits, lang, signature, &mut reached.borrow_mut())
        })
        .unwrap();
    let r = reached.into_inner();
    eprintln!("{r:?}");
    assert!(
        r.lexer_errors > 0
            && r.directive_errors > 0
            && r.parser_errors > 0
            && r.warnings > 0
            && r.feature_errors > 0
            && r.cross_file_groups > 0
            && r.dropped_by_overlap > 0,
        "{r:?}"
    );
}
