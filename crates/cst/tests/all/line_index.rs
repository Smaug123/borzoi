//! Unit and property tests for `LineIndex` — the FCS `(line, col)` →
//! byte-offset translator used by the lexer/lexfilter diff harnesses.
//!
//! FCS reports columns in **UTF-16 code units**. The early `LineIndex`
//! implementation added `col` to the line's byte start directly; that's
//! correct on ASCII (1 byte = 1 UTF-16 unit) but silently wrong on any
//! character whose UTF-8 width differs from its UTF-16 width — every BMP
//! non-ASCII char (em-dashes, accented letters, …) and every supplementary
//! char (emoji, mathematical symbols, …). These tests pin both regimes.

use crate::common::LineIndex;
use proptest::prelude::*;

// ============================================================================
// ASCII regression — the case the old code already handled correctly.
// ============================================================================

#[test]
fn ascii_single_line() {
    let idx = LineIndex::new("hello world");
    assert_eq!(idx.offset(1, 0), 0);
    assert_eq!(idx.offset(1, 5), 5);
    assert_eq!(idx.offset(1, 11), 11);
}

#[test]
fn ascii_multi_line() {
    let idx = LineIndex::new("abc\ndef\nghi\n");
    assert_eq!(idx.offset(1, 0), 0);
    assert_eq!(idx.offset(1, 3), 3);
    assert_eq!(idx.offset(2, 0), 4);
    assert_eq!(idx.offset(2, 3), 7);
    assert_eq!(idx.offset(3, 0), 8);
    assert_eq!(idx.offset(3, 3), 11);
}

// ============================================================================
// BMP non-ASCII — `—` (U+2014, 3 UTF-8 bytes / 1 UTF-16 unit). The
// `diff_fcs_dump_program` test in tests/all/lexer_diff.rs trips on this exact
// character in a string literal inside tools/fcs-dump/Program.fs.
// ============================================================================

#[test]
fn em_dash_columns_are_utf16_not_bytes() {
    // bytes:   0 ('a')  1..4 ('—')  4 ('b')  5 ('\n')
    // utf-16:  0        1            2        (newline)
    let idx = LineIndex::new("a—b\n");
    assert_eq!(idx.offset(1, 0), 0, "start of line");
    assert_eq!(idx.offset(1, 1), 1, "after 'a'");
    assert_eq!(idx.offset(1, 2), 4, "after '—' (1 utf16 unit, 3 bytes)");
    assert_eq!(idx.offset(1, 3), 5, "after 'b'");
}

#[test]
fn multiple_em_dashes_accumulate_drift() {
    // bytes:   0..3 ('—')  3..6 ('—')  6 ('x')  7 ('\n')
    // utf-16:  0           1           2        (newline)
    let idx = LineIndex::new("——x\n");
    assert_eq!(idx.offset(1, 0), 0);
    assert_eq!(idx.offset(1, 1), 3, "after first '—'");
    assert_eq!(idx.offset(1, 2), 6, "after second '—'");
    assert_eq!(idx.offset(1, 3), 7, "after 'x'");
}

// ============================================================================
// Supplementary-plane char — `💩` (U+1F4A9, 4 UTF-8 bytes / 2 UTF-16
// units via a surrogate pair). Worth exercising because `len_utf16() == 2`
// here whereas BMP chars are always 1; a naive `chars().count()`
// implementation would get this case wrong.
// ============================================================================

#[test]
fn surrogate_pair_advances_two_utf16_units() {
    // bytes:   0..4 ('💩')  4 ('x')  5 ('\n')
    // utf-16:  0, 1 (high+low surrogate)  2  (newline)
    let idx = LineIndex::new("💩x\n");
    assert_eq!(idx.offset(1, 0), 0);
    assert_eq!(idx.offset(1, 2), 4, "after surrogate pair");
    assert_eq!(idx.offset(1, 3), 5, "after 'x'");
}

#[test]
fn col_inside_surrogate_pair_clamps_to_char_start() {
    // FCS itself only emits valid col positions, but be defensive: a col
    // that lands between the high and low surrogate of one char should
    // resolve to the byte just before the surrogate pair, not midway
    // through the UTF-8 encoding (which isn't a char boundary).
    let idx = LineIndex::new("💩x\n");
    assert_eq!(idx.offset(1, 1), 0);
}

// ============================================================================
// Mixed widths on a single line — the realistic worst case.
// ============================================================================

#[test]
fn mixed_widths_on_one_line() {
    // bytes:   0 ('a')  1..3 ('é')  3..6 ('—')  6..10 ('💩')  10 ('z')  11 ('\n')
    //          é = U+00E9 (2 utf-8 bytes, 1 utf-16 unit)
    //          — = U+2014 (3 utf-8 bytes, 1 utf-16 unit)
    //          💩 = U+1F4A9 (4 utf-8 bytes, 2 utf-16 units)
    // utf-16:  0        1            2            3, 4              5
    let idx = LineIndex::new("aé—💩z\n");
    assert_eq!(idx.offset(1, 0), 0);
    assert_eq!(idx.offset(1, 1), 1, "after 'a'");
    assert_eq!(idx.offset(1, 2), 3, "after 'é'");
    assert_eq!(idx.offset(1, 3), 6, "after '—'");
    assert_eq!(idx.offset(1, 5), 10, "after '💩'");
    assert_eq!(idx.offset(1, 6), 11, "after 'z'");
}

// ============================================================================
// Clamping — over-long col / past-end line. Preserve the existing
// "line = lastLine+1, col = 0 → source.len()" contract that FCS depends on.
// ============================================================================

#[test]
fn line_past_end_clamps_to_source_len() {
    let src = "abc\ndef";
    let idx = LineIndex::new(src);
    // The source has 2 lines (no trailing newline → no third entry). FCS
    // can emit `(line = 3, col = 0)` as an end-of-file end position.
    assert_eq!(idx.offset(3, 0), src.len());
    assert_eq!(idx.offset(99, 0), src.len());
}

#[test]
fn col_past_line_end_clamps_to_line_end() {
    // For the "abc\n" line, line 1 contains 3 utf16 units + newline. A
    // col of, say, 50 should clamp to the end of the line (the newline byte).
    let idx = LineIndex::new("abc\ndef\n");
    // We don't pin the exact clamp target, but it must lie within the
    // source and be a char boundary on the requested line.
    let got = idx.offset(1, 50);
    assert!(got <= "abc\ndef\n".len());
    assert!(got >= 3, "should at least reach end of 'abc'");
}

#[test]
fn empty_source() {
    let idx = LineIndex::new("");
    assert_eq!(idx.offset(1, 0), 0);
    assert_eq!(idx.offset(2, 0), 0);
}

// ============================================================================
// Property test — round-trip.
//
// For any source string and any char-boundary byte position `p`, compute
// the FCS-style `(line, col)` (line = 1 + newlines before p, col = UTF-16
// length of the slice from line start to p), then `LineIndex::offset(line,
// col)` must return `p`.
//
// This is the invariant the diff harness actually relies on: FCS gives us
// `(line, col)` for a token boundary; we need the byte offset of that same
// boundary. Round-trip captures it cleanly.
// ============================================================================

/// Reference implementation: given source and a char-boundary byte offset
/// `p`, return the `(line, col)` an FCS-compatible position-reporter would
/// emit. `line` is 1-based; `col` counts UTF-16 code units from the start
/// of the current line.
fn position_for(source: &str, p: usize) -> (u32, u32) {
    let mut line: u32 = 1;
    let mut line_start = 0usize;
    let bytes = source.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if i >= p {
            break;
        }
        if b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let col: u32 = source[line_start..p]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    (line, col)
}

// Deterministic reproduction of the generator-domain bug: a leading BOM is
// stripped by `LineIndex` (so logical `(1, 0)` maps to byte 3), but the
// `position_for` reference is not BOM-aware, so it labels byte 0 as `(1, 0)`.
// The round-trip therefore cannot hold at the BOM bytes on a *correct*
// implementation. This mirrors what `any::<String>()` occasionally generates.
#[test]
fn repro_leading_bom_breaks_naive_round_trip() {
    let source = "\u{feff}a";
    let idx = LineIndex::new(source);
    // FCS strips the BOM: logical (line 1, col 0) is the byte *after* it.
    assert_eq!(idx.offset(1, 0), '\u{feff}'.len_utf8());
    // The naive reference would (wrongly) expect byte 0 to round-trip.
    let (line, col) = position_for(source, 0);
    assert_eq!((line, col), (1, 0));
    assert_ne!(
        idx.offset(line, col),
        0,
        "byte 0 is a BOM byte, not FCS-addressable"
    );
}

proptest! {
    #[test]
    fn round_trip_arbitrary_string(
        // A leading UTF-8 BOM is stripped by `LineIndex` (matching FCS), which
        // makes the BOM bytes non-round-trippable: FCS never emits a position
        // pointing inside the BOM, and logical `(1, 0)` resolves to the first
        // post-BOM byte. `position_for` is deliberately *not* BOM-aware (it is
        // the naive FCS-position reference), so including leading-BOM sources
        // would put the property outside its valid domain and fail
        // nondeterministically. Excluding them keeps the reference exact.
        source in any::<String>().prop_filter(
            "no leading BOM (see repro_leading_bom_breaks_naive_round_trip)",
            |s| !s.starts_with('\u{feff}'),
        ),
    ) {
        let idx = LineIndex::new(&source);
        // Test every char boundary including 0 and source.len().
        for (p, _) in source.char_indices() {
            let (line, col) = position_for(&source, p);
            prop_assert_eq!(
                idx.offset(line, col),
                p,
                "round-trip failed at byte {} of {:?}: (line={}, col={})",
                p, source, line, col,
            );
        }
        let p = source.len();
        let (line, col) = position_for(&source, p);
        prop_assert_eq!(idx.offset(line, col), p);
    }
}
