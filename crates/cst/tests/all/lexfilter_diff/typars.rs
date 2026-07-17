//! Type-application `<...>` scan-ahead and its non-typar carve-outs.

use crate::common::assert_filtered_streams_match;

/// Simplest typar application: `list<int>` in a type annotation. Exercises
/// `peek_adjacent_typars` from the `list` IDENT trigger, producing
/// `HIGH_PRECEDENCE_TYAPP / LESS(true) / int / GREATER(true)`.
#[test]
fn diff_filtered_tyapp_simple() {
    assert_filtered_streams_match("let xs : list<int> = []\n");
}

/// Typar application followed by an adjacent paren application: `f<int>(x)`.
/// The closing `>` is adjacent to `(`, so `HIGH_PRECEDENCE_PAREN_APP` is
/// injected between `GREATER(true)` and `LParen`.
#[test]
fn diff_filtered_tyapp_with_call() {
    assert_filtered_streams_match("let y = f<int>(x)\n");
}

/// Nested generic with `>>` close: `dict<string, list<int>>` exercises
/// `typars_close_op_split` on `Op(">>")`, splitting it into two
/// `GREATER(true)` tokens.
#[test]
fn diff_filtered_tyapp_nested_greater_greater() {
    assert_filtered_streams_match("let d : dict<string, list<int>> = ()\n");
}

/// Underscore typar: `Foo<_>`. Underscore must be in the scan whitelist.
#[test]
fn diff_filtered_tyapp_underscore() {
    assert_filtered_streams_match("let z : Foo<_> = ()\n");
}

/// Multi-argument typar: `Map<string, int>`. Comma must be in the scan
/// whitelist.
#[test]
fn diff_filtered_tyapp_multi_arg() {
    assert_filtered_streams_match("let m : Map<string, int> = ()\n");
}

/// Comparison `a < b` with whitespace. Adjacency check fails (space between
/// `a` and `<`), scan does not fire, `<` stays as `LESS(false)` and there is
/// no `HIGH_PRECEDENCE_TYAPP`.
#[test]
fn diff_filtered_comparison_not_tyapp_with_space() {
    assert_filtered_streams_match("let b = a < b\n");
}

/// Adjacent `<` but no closing `>` before EOL: `a<b`. The scan-ahead runs
/// out of tokens without balancing, backtracks, and leaves the stream as
/// `IDENT < IDENT`.
#[test]
fn diff_filtered_comparison_not_tyapp_no_space() {
    assert_filtered_streams_match("let b = a<b\n");
}

/// Method call with typar after a dotted ident: `xs.Select<int>(f)`.
/// Triggered from the `Select` IDENT after the `.`. Also yields
/// `HIGH_PRECEDENCE_PAREN_APP`.
#[test]
fn diff_filtered_tyapp_after_method_dot() {
    assert_filtered_streams_match("let r = xs.Select<int>(f)\n");
}

/// `1<2>3` — int literal is not a typar trigger in FCS (only idents and a
/// few keywords trigger `peekAdjacentTypars`). Should parse as
/// `1 < 2 > 3`.
#[test]
fn diff_filtered_tyapp_int_literal_not() {
    assert_filtered_streams_match("let n = 1<2>3\n");
}

/// Typar with array-of-T inside, written with a space so we don't trip
/// FCS's separate `HIGH_PRECEDENCE_BRACK_APP` insertion (LexFilter.fs:2650),
/// which is its own feature outside `peekAdjacentTypars`. With the space,
/// this exercises only that the `[` and `]` are permitted inside the typar
/// scan (they increment/decrement the paren counter in lockstep).
#[test]
fn diff_filtered_tyapp_array_inside() {
    assert_filtered_streams_match("let a : Foo<int []> = ()\n");
}

/// `>=` after a typar-eligible ident must NOT be split into `> =` by the
/// typar machinery. The scan should treat `>=` as a non-typar-close
/// operator and reject (returning false), leaving the stream unchanged.
#[test]
fn diff_filtered_greater_equal_not_typar() {
    assert_filtered_streams_match("let g = a >= b\n");
}

/// `>|]` closing a generic application inside an array literal. Our lexer
/// fuses `>|` into `Op(">|")` (maximal munch) and then sees `RBrack`. FCS
/// instead sees `Greater + BarRightBracket`. The typar smash must detect
/// the adjacent `RBrack` after `Op(">|")` and re-fuse to `BarRBrack`,
/// otherwise downstream sees a stray `Op("|")` followed by `]`.
#[test]
fn diff_filtered_tyapp_greater_bar_rbrack() {
    assert_filtered_streams_match("let x = [| typeof<int>|]\n");
}

/// `>|}` closing a generic application inside an anonymous-record literal.
/// Same mechanism as `tyapp_greater_bar_rbrack`, but the brace variant:
/// `Op(">|")` + `RBrace` must fuse to `BarRBrace`.
#[test]
fn diff_filtered_tyapp_greater_bar_rbrace() {
    assert_filtered_streams_match("let x = {| X = typeof<int>|}\n");
}

/// `+` between typar arguments is NOT permitted by FCS's depth-1
/// whitelist (LexFilter.fs:1158-1180 — `MINUS` is in, `PLUS_MINUS_OP` is
/// not). The scan must backtrack and emit a plain comparison.
#[test]
fn diff_filtered_plus_not_typar() {
    assert_filtered_streams_match("let y = a<b+c>d\n");
}

/// `**` between typar arguments is NOT in the FCS whitelist either —
/// the scan must backtrack and emit a plain comparison.
#[test]
fn diff_filtered_star_star_not_typar() {
    assert_filtered_streams_match("let y = a<b**c>d\n");
}

/// `>:` inside a *successful* nested typar scan must be smashed into
/// `Greater(res) + Colon` (FCS LexFilter.fs:1204-1207). Outer scan:
/// `Foo<Bar<int>:Baz>>x` succeeds because the outer `>>` closes via
/// the standard `TyparsCloseOp` split; the inner `>:` is reached via
/// the OtherToken path at depth 2 and must be re-split on smash.
#[test]
fn diff_filtered_tyapp_inner_greater_colon_split() {
    assert_filtered_streams_match("let z = Foo<Bar<int>:Baz>>x\n");
}

/// Nested generic immediately followed by `|}` closing an enclosing
/// anonymous-record type. FCS lexes `>|}` as the atomic
/// `GREATER_BAR_RBRACE` (one paren-decrement); our lexer splits it into
/// `Op(">|")` + `RBrace`. The outer typar scan must treat that pair as
/// the single close of the nested generic — otherwise the bare `RBrace`
/// is `OtherToken` at depth 1 and the outer scan backtracks, dropping
/// `HighPrecedenceTypeApp` and emitting `Less false` instead of
/// `Less true` for the outer `<`.
#[test]
fn diff_filtered_tyapp_nested_greater_bar_rbrace() {
    assert_filtered_streams_match("let x : Foo<{| X : Bar<int>|}> = ()\n");
}

/// `>|]` variant of [`diff_filtered_tyapp_nested_greater_bar_rbrace`]:
/// nested generic closes immediately before `|]` of an enclosing
/// `[| ... |]` array literal embedded in a type-argument position.
#[test]
fn diff_filtered_tyapp_nested_greater_bar_rbrack() {
    assert_filtered_streams_match("let x : Foo<list<int>> = [| typeof<list<int>> |]\n");
}

/// Multi-line generic application: `Foo<\n Bar\n>()`. FCS still emits
/// the `HighPrecedenceTypeApp` / `HighPrecedenceParenthesisApp` pair
/// and balances the offside separator across the inner line break.
/// Verifies the typar scan and outer offside tracking interact
/// correctly when the `<…>` spans multiple lines.
#[test]
fn diff_filtered_tyapp_multiline() {
    assert_filtered_streams_match("let x =\n    Foo<\n        Bar\n    >()\n");
}
