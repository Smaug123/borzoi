//! The parser's lexer-level diagnostic scans (`parser::lexical_diagnostics`:
//! FS1161 tabs, FS0046 reserved identifiers, `:`/`$` in operator names)
//! against their pre-Stage-7 copies in `borzoi_cst::reference::scans`
//! (`docs/token-pipeline-soa-plan.md`, Stage 7).
//!
//! The live scans test a token's kind before reading its text, and the tab
//! scan returns at once for a source with no tab byte. They must report
//! exactly what the copies do. Under the `token-reference` feature every parse
//! also asserts this (`parser::parse_inner`), so the parser suites and the
//! corpus sweeps hold the scans to it too; this group covers the shapes that
//! decide each scan's answer.

use std::collections::HashSet;

use borzoi_cst::parser::lexical_diagnostics;
use borzoi_cst::reference::scans;
use borzoi_cst::tokens::lex_full_trivia;
use proptest::prelude::*;

use crate::parser_panic_sweep::WIDE;
use crate::token_table_reference::LEXICAL;

/// The shapes each scan's answer turns on: tab runs in whitespace, beside
/// spaces, inside a string, comment or character literal, and on a directive
/// line or in an inactive region; each reserved word, bare, with a glued or
/// spaced `!` or `#`, quoted, and as part of a longer identifier; and
/// operators holding `:` or `$`, in and out of the `>`-headed rule.
const SCAN_SHAPES: &[&str] = &[
    "\t",
    "\t\t",
    " \t ",
    "x\t=\t1",
    "\"\t\"",
    "(*\t*)",
    "//\t\n",
    "'\t'",
    "#if X\n\tlet y = 1\n#endif\n",
    "\t#if X\n",
    "#if\tX\n",
    "\t#nowarn 40\n",
    "break",
    "break!",
    "break!!",
    "break!=",
    "break #",
    "break#",
    "break !",
    "``break``",
    "breaker",
    "sealed",
    "virtual",
    "tailcall",
    "<$",
    "$>",
    ">:",
    ".>:",
    "?>$",
    ".$",
    "!:",
    "+:+",
    "|:>",
    "::",
    ":=",
    ":?>",
    "$",
    "\n",
    "\r\n",
    " ",
    "x",
];

/// What the scans found over a set of sources, so the test can tell it
/// reached every answer.
#[derive(Default)]
struct Reached {
    tab_error: bool,
    reserved_warning: bool,
    operator_char_error: bool,
    tab_without_error: bool,
}

/// Assert the live scans report what the reference does on `src`, under no
/// symbols and under the symbol the `#if` shapes test.
fn check(src: &str, reached: &mut Reached) {
    for symbols in [HashSet::new(), HashSet::from(["X".to_string()])] {
        let lexed = lex_full_trivia(src, &symbols);
        let got = lexical_diagnostics(src, &lexed.tokens);
        let want = scans::lexical_diagnostics(src, &lexed.tokens);
        assert_eq!(
            got, want,
            "lexical diagnostics differ on {src:?} under {symbols:?}"
        );
        reached.tab_error |= !got[0].is_empty();
        reached.reserved_warning |= !got[1].is_empty();
        reached.operator_char_error |= !got[2].is_empty();
        reached.tab_without_error |= src.contains('\t') && got[0].is_empty();
    }
}

fn assert_reached_everything(reached: &Reached) {
    assert!(reached.tab_error, "no source drew a tab error");
    assert!(
        reached.reserved_warning,
        "no source drew a reserved-identifier warning"
    );
    assert!(
        reached.operator_char_error,
        "no source drew an operator-character error"
    );
    assert!(
        reached.tab_without_error,
        "no source held a tab that draws no error"
    );
}

/// Every one- and two-piece source over [`SCAN_SHAPES`] and the token table's
/// lexical alphabet.
#[test]
fn every_scan_and_lexical_pair_matches_the_reference_scans() {
    let alphabet: Vec<&str> = SCAN_SHAPES.iter().chain(LEXICAL).copied().collect();
    let mut reached = Reached::default();
    for a in &alphabet {
        check(a, &mut reached);
        for b in &alphabet {
            check(&format!("{a}{b}"), &mut reached);
        }
    }
    assert_reached_everything(&reached);
}

/// Every one- and two-piece source over the panic sweep's `WIDE` alphabet.
#[test]
fn every_wide_pair_matches_the_reference_scans() {
    let mut reached = Reached::default();
    for a in WIDE {
        check(a, &mut reached);
        for b in WIDE {
            check(&format!("{a}{b}"), &mut reached);
        }
    }
}

proptest! {
    /// Random walks over all three alphabets, at lengths the pairs cannot
    /// reach.
    #[test]
    fn random_sources_match_the_reference_scans(
        pieces in prop::collection::vec(
            prop::sample::select(
                SCAN_SHAPES.iter().chain(LEXICAL).chain(WIDE).copied().collect::<Vec<_>>(),
            ),
            0..16,
        )
    ) {
        check(&pieces.concat(), &mut Reached::default());
    }
}
