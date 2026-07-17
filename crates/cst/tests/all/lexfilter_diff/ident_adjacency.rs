//! High-precedence application from adjacent `(`/`[`.

use crate::common::assert_filtered_streams_match;

/// Plain identifier-adjacent paren call: `f(x)`. FCS's
/// `rulesForBothSoftWhiteAndHardWhite` (LexFilter.fs:2655) injects
/// `HIGH_PRECEDENCE_PAREN_APP` between the IDENT and the adjacent `(`
/// via `insertHighPrecedenceApp`. Our port mirrors this with a
/// `Virtual::HighPrecedenceParenApp` delayed at the `(`'s location.
#[test]
fn diff_filtered_ident_adjacent_paren_call() {
    assert_filtered_streams_match("let y = f(x)\n");
}

/// Plain identifier-adjacent bracket indexer: `arr[0]`. FCS injects
/// `HIGH_PRECEDENCE_BRACK_APP` (LexFilter.fs:2650) so the parser binds
/// `arr[0]` as one indexer call rather than `arr` followed by a list
/// literal.
#[test]
fn diff_filtered_ident_adjacent_lbrack_indexer() {
    assert_filtered_streams_match("let y = arr[0]\n");
}

/// `f (x)` with a space between IDENT and `(`. The `isAdjacent` check
/// fails (whitespace widens the span gap), so no
/// `HIGH_PRECEDENCE_PAREN_APP` is inserted; this parses as ordinary
/// function application.
#[test]
fn diff_filtered_ident_with_space_no_paren_app() {
    assert_filtered_streams_match("let y = f (x)\n");
}

/// `arr [0]` with a space between IDENT and `[`. The adjacency check
/// fails, so no `HIGH_PRECEDENCE_BRACK_APP` is inserted; `[0]` parses
/// as a list literal that is the argument of the function `arr`.
#[test]
fn diff_filtered_ident_with_space_no_brack_app() {
    assert_filtered_streams_match("let y = arr [0]\n");
}

/// `f\n(x)` — newline between IDENT and `(` likewise breaks
/// adjacency, so no `HIGH_PRECEDENCE_PAREN_APP` is emitted.
#[test]
fn diff_filtered_ident_newline_no_paren_app() {
    assert_filtered_streams_match("let y =\n    f\n    (x)\n");
}

/// Quoted identifier `` ``foo`` `` adjacent to `[`. FCS's IDENT arm
/// covers both regular and quoted identifiers (the lexer collapses
/// both to a single IDENT token), so the bracket-app rule fires here
/// too.
#[test]
fn diff_filtered_quoted_ident_adjacent_lbrack() {
    assert_filtered_streams_match("let y = ``arr``[0]\n");
}

/// Chained indexer / call: `arr[0](x)`. The first IDENT-LBRACK
/// adjacency fires, then the next dispatch — but the next token after
/// `]` is `(`, and `]` is not an IDENT, so no further HPP is injected
/// at that boundary. Exercises that the LBRACK injection doesn't
/// leave delayed in a state that mis-fires on the trailing call.
#[test]
fn diff_filtered_ident_chained_brack_then_paren() {
    assert_filtered_streams_match("let y = arr[0](x)\n");
}
