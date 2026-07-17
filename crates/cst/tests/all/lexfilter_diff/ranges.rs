//! `INT32_DOT_DOT` splitting. The lexer fuses an integer literal glued to a
//! following `..` into one `IntDotDot` token (so the float regex doesn't eat
//! `1.`); the lex-filter splits it back into `INT32` + `DOT_DOT`, mirroring
//! FCS's LexFilter (LexFilter.fs:2680-2684). These pin our post-filter stream
//! against FCS's for the range-specification forms.

use crate::common::assert_filtered_streams_match;

/// Bare range `1..10` — `IntDotDot("1..")` + `Int("10")` must filter to
/// `INT32 / DOT_DOT / INT32`, matching FCS's split.
#[test]
fn diff_filtered_range_bare() {
    assert_filtered_streams_match("let r = 1..10\n");
}

/// Spaced `1 .. 10` already lexes as `Int / DotDot / Int` (no fused token);
/// pin it so the split doesn't perturb the already-correct path.
#[test]
fn diff_filtered_range_spaced() {
    assert_filtered_streams_match("let r = 1 .. 10\n");
}

/// Open upper range `2..` inside a slice — `IntDotDot("2..")` glued to the
/// closing `]`. Splits to `INT32 / DOT_DOT / RBRACK`.
#[test]
fn diff_filtered_range_open_upper() {
    assert_filtered_streams_match("let s = argv.[2..]\n");
}

/// Open lower range `..3` already lexes as a clean `DotDot` (no fusion).
#[test]
fn diff_filtered_range_open_lower() {
    assert_filtered_streams_match("let s = argv.[..3]\n");
}

/// List range `[1..10]` — the fused token sits adjacent to the `[` opener.
#[test]
fn diff_filtered_range_in_list() {
    assert_filtered_streams_match("let xs = [1..10]\n");
}

/// `for` range `for i in 1..10 do ()` — the enumerable is a fused range.
#[test]
fn diff_filtered_range_for_loop() {
    assert_filtered_streams_match("for i in 1..10 do ()\n");
}
