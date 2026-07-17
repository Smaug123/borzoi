//! Conditional-compilation-aware parsing (`docs/completed/parser-ifdef-plan.md`,
//! stage C2): the parser consumes the directive driver, so the tree reflects
//! only the active `#if` branch while staying lossless.
//!
//! The load-bearing oracle is the **lossless** property
//! `text(tree) == source` over arbitrary directive-bearing sources — the
//! invariant the parser maintained only structurally before this stage.

use std::collections::HashSet;
use std::ops::Range;

use borzoi_cst::directives::{PreprocError, lex_with_symbols};
use borzoi_cst::parser::{Parse, parse, parse_with_symbols};
use proptest::prelude::*;

use crate::common::normalised_ast::{NormalisedRoot, NormalisedWarnDirectiveKind, normalise_parse};

/// `text(tree)` — the full source the green tree covers.
fn tree_text(p: &Parse) -> String {
    p.root.text().to_string()
}

// ---- generator: directive-bearing sources ----------------------------------

#[derive(Clone, Debug)]
enum Block {
    /// A content line that lexes (and usually parses). Rendered with a
    /// trailing newline.
    Content(String),
    /// `#nowarn "40"`.
    NoWarn,
    /// `#line 7 "f.fs"`.
    Line,
    /// `#if SYM … (#else …)? #endif`.
    If {
        sym: String,
        then_: Vec<Block>,
        else_: Option<Vec<Block>>,
    },
}

fn arb_content() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("let x = 1".to_string()),
        Just("let y = 2".to_string()),
        Just("a + b".to_string()),
        Just("( )".to_string()),
        Just("module Foo".to_string()),
        Just(String::new()),
    ]
}

fn arb_sym() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("FOO".to_string()),
        Just("BAR".to_string()),
        Just("BAZ".to_string()),
    ]
}

fn arb_block() -> impl Strategy<Value = Block> {
    let leaf = prop_oneof![
        arb_content().prop_map(Block::Content),
        Just(Block::NoWarn),
        Just(Block::Line),
    ];
    leaf.prop_recursive(3, 24, 3, |inner| {
        prop_oneof![
            arb_content().prop_map(Block::Content),
            Just(Block::NoWarn),
            Just(Block::Line),
            (
                arb_sym(),
                prop::collection::vec(inner.clone(), 0..3),
                prop::option::of(prop::collection::vec(inner, 0..3)),
            )
                .prop_map(|(sym, then_, else_)| Block::If { sym, then_, else_ }),
        ]
    })
}

fn render(blocks: &[Block], out: &mut String) {
    for b in blocks {
        match b {
            Block::Content(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Block::NoWarn => out.push_str("#nowarn \"40\"\n"),
            Block::Line => out.push_str("#line 7 \"f.fs\"\n"),
            Block::If { sym, then_, else_ } => {
                out.push_str("#if ");
                out.push_str(sym);
                out.push('\n');
                render(then_, out);
                if let Some(eb) = else_ {
                    out.push_str("#else\n");
                    render(eb, out);
                }
                out.push_str("#endif\n");
            }
        }
    }
}

fn arb_program() -> impl Strategy<Value = String> {
    prop::collection::vec(arb_block(), 0..5).prop_map(|blocks| {
        let mut s = String::new();
        render(&blocks, &mut s);
        s
    })
}

fn arb_symbols() -> impl Strategy<Value = HashSet<String>> {
    prop::collection::hash_set(arb_sym(), 0..3)
}

proptest! {
    /// The parse tree is lossless: its text is exactly the source, for any
    /// directive structure and symbol set. This holds even when active code
    /// fails to parse (ERROR nodes still carry their text) and when dead
    /// branches contain bytes the lexer never sees (INACTIVECODE).
    #[test]
    fn parse_with_symbols_is_lossless(s in arb_program(), symbols in arb_symbols()) {
        let parsed = parse_with_symbols(&s, &symbols);
        let text = tree_text(&parsed);
        prop_assert_eq!(text, s);
    }

    /// `parse` (no symbols) is lossless too.
    #[test]
    fn parse_is_lossless(s in arb_program()) {
        let text = tree_text(&parse(&s));
        prop_assert_eq!(text, s);
    }

    /// Parsing never panics on directive-bearing input.
    #[test]
    fn parse_with_symbols_is_total(s in arb_program(), symbols in arb_symbols()) {
        let _ = parse_with_symbols(&s, &symbols);
    }

    /// Every non-`Lex` preprocessor error the driver reports (malformed
    /// directive line, orphan closer, unclosed `#if`, …) is surfaced in
    /// `Parse::errors` with the same reporting span and message. This pins the
    /// wiring: the parser drops these from its raw token stream (so they don't
    /// stall the productions) but must re-attach every one as a parser error,
    /// matching FCS's treatment of a malformed directive as a compile error.
    #[test]
    fn directive_errors_are_surfaced_in_parse_errors(
        s in arb_maybe_malformed_program(),
        symbols in arb_symbols(),
    ) {
        let parsed = parse_with_symbols(&s, &symbols);
        let got: Vec<(Range<usize>, String)> = parsed
            .errors
            .iter()
            .map(|e| (e.span.clone(), e.message.clone()))
            .collect();
        for expected in driver_directive_errors(&s, &symbols) {
            prop_assert!(
                got.contains(&expected),
                "directive error {expected:?} missing from parse errors {got:?} for {s:?}",
            );
        }
    }
}

// ---- example tests: active-branch selection --------------------------------

fn syms(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// A malformed `(*` in a dead branch is INACTIVECODE — never lexed — so it
/// produces no parse error, and the active `#else` branch parses cleanly.
#[test]
fn dead_branch_malformed_produces_no_parse_errors() {
    let src = "#if FOO\n(* unterminated\n#else\nlet y = 1\n#endif\n";
    let parsed = parse(src); // FOO undefined → `#else` active
    assert!(
        parsed.errors.is_empty(),
        "dead-branch malformed bytes produced parse errors: {:?}",
        parsed.errors
    );
    assert_eq!(tree_text(&parsed), src);
}

/// The tree's *structure* is that of the selected branch alone: with the
/// symbol undefined the `#else` branch is parsed; with it defined the `then`
/// branch is. The directive lines and dead branch are trivia, so the
/// projected AST equals that of the active branch parsed on its own.
#[test]
fn active_branch_is_else_when_symbol_undefined() {
    let full = "#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n";
    let got = normalise_parse(&parse(full)); // FOO undefined
    let want = normalise_parse(&parse("let y = 2\n"));
    assert_eq!(got, want);
}

#[test]
fn active_branch_is_then_when_symbol_defined() {
    let full = "#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n";
    let got = normalise_parse(&parse_with_symbols(full, &syms(&["FOO"])));
    let want = normalise_parse(&parse("let x = 1\n"));
    assert_eq!(got, want);
}

/// A malformed/orphan directive (`#endif` with no `#if`) is a structural
/// preprocessor error. It is surfaced in `Parse::errors` (matching FCS, which
/// also rejects it) but must not derail parsing of the active code that
/// follows: the orphan is trivia (`HASH_ENDIF`), so `module M` parses exactly
/// as it would on its own. (Regression: structural directive errors must not
/// act as phantom stoppers in the productions' raw lookahead — they are
/// filtered out of the raw token stream, then re-attached to `Parse::errors`.)
#[test]
fn orphan_directive_does_not_derail_following_code() {
    let src = "#endif\nmodule M\n";
    let parsed = parse(src);
    assert_eq!(tree_text(&parsed), src);
    // The orphan `#endif` is reported, over its own line (bytes 0..6).
    assert_eq!(parsed.errors.len(), 1, "errors: {:?}", parsed.errors);
    assert_eq!(parsed.errors[0].span, 0..6);
    // …but the following `module M` parses as it would standalone (the tree,
    // which `normalise_parse` reads, carries no error from the orphan).
    assert_eq!(
        normalise_parse(&parsed),
        normalise_parse(&parse("module M\n"))
    );
}

/// A malformed `#if` condition (`*COMPILED*` is not a valid preprocessor
/// expression) is surfaced as a parser error, matching FCS's FS3182. The bad
/// condition is treated as false, so `exit 0` is dead and `exit 1` is the
/// active code — the tree matches the bare active branch.
#[test]
fn malformed_if_condition_is_a_parse_error() {
    let src = "#if *COMPILED*\nexit 0\n#endif\nexit 1\n";
    let parsed = parse(src);
    assert_eq!(tree_text(&parsed), src);
    assert_eq!(parsed.errors.len(), 1, "errors: {:?}", parsed.errors);
    // The squiggle covers the whole `#if *COMPILED*` directive line (0..14).
    assert_eq!(parsed.errors[0].span, 0..14);
    assert_eq!(
        normalise_parse(&parsed),
        normalise_parse(&parse("exit 1\n"))
    );
}

/// An `#if` left unclosed at EOF is reported (matching FCS), squiggled on the
/// opening `#if` line rather than at the zero-width EOF point.
#[test]
fn unclosed_if_at_eof_is_a_parse_error() {
    // `FOO` undefined → the body is dead; the only diagnostic is the unclosed
    // `#if` itself.
    let src = "#if FOO\nlet x = 1\n";
    let parsed = parse(src);
    assert_eq!(tree_text(&parsed), src);
    assert_eq!(parsed.errors.len(), 1, "errors: {:?}", parsed.errors);
    assert_eq!(parsed.errors[0].span, 0..7);
}

/// Oracle for the property below: the non-`Lex` preprocessor errors the driver
/// reports for `src`, each as the `(reporting_span, message)` the parser is
/// expected to surface.
fn driver_directive_errors(src: &str, symbols: &HashSet<String>) -> Vec<(Range<usize>, String)> {
    lex_with_symbols(src, symbols)
        .filter_map(|(res, span)| match res {
            Err(e) if !matches!(e, PreprocError::Lex(_)) => {
                Some((e.reporting_span(span), e.diagnostic_message()))
            }
            _ => None,
        })
        .collect()
}

/// A palette of lines, some of which are malformed directives, so the
/// generated programs exercise the directive-error path (orphan closers, bad
/// `#if` bodies, unclosed `#if`s) as well as well-formed code.
fn arb_maybe_malformed_line() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("let x = 1"),
        Just("#if FOO"),
        Just("#endif"),
        Just("#else"),
        Just("#elif BAR"),
        Just("#if *BAD*"),   // invalid char in condition
        Just("#if !"),       // dangling `!`
        Just("#if FOO BAR"), // trailing tokens
        Just("#nowarn \"40\""),
    ]
}

fn arb_maybe_malformed_program() -> impl Strategy<Value = String> {
    prop::collection::vec(arb_maybe_malformed_line(), 0..8).prop_map(|lines| {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    })
}

/// Directive lines in active code are trivia, not parse errors: a buffer of
/// only directives + an active `let` parses with no errors and the same module
/// structure as the bare `let`. Warning directives are modelled explicitly.
#[test]
fn directive_lines_are_trivia_not_errors() {
    let src = "#nowarn \"40\"\n#line 1 \"x.fs\"\nlet x = 1\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "directive lines produced parse errors: {:?}",
        parsed.errors
    );
    assert_eq!(tree_text(&parsed), src);
    let mut expected = normalise_parse(&parse("let x = 1\n"));
    let NormalisedRoot::Impl(expected_file) = &mut expected else {
        panic!("bare impl file normalised as a signature file");
    };
    expected_file
        .warn_directives
        .push(NormalisedWarnDirectiveKind::Nowarn);
    assert_eq!(normalise_parse(&parsed), expected);
}
