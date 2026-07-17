//! Position-tracking tests for the lex-filter's line/column cursor.
//!
//! The cursor must advance a line for every `\n` *inside* a token, not only for
//! the standalone `Newline` token. FCS does this via `incrLine` on its
//! `newline = '\n' | '\r' '\n'` pattern (`lex.fsl:315`, called from the block
//! comment / triple-quote / verbatim / line-continuation scanners), so a lone
//! `\r` is **not** a line break and the new line begins one byte past the `\n`
//! (`prim-lexing.fs:225`, `Position.NextLine`).
//!
//! These drive the private `Filter::pull_raw` directly and assert the computed
//! [`Pos`] against an independent reference oracle.

use super::*;
use crate::language_version::LanguageVersion;
use crate::lexer::{Token, lex};

/// Independent oracle: the FCS-faithful position of a byte `offset`. Line is
/// 1-based and counts `\n` bytes (so `\r\n` is one break, a lone `\r` none);
/// column is the byte offset from the start of the current line (one past the
/// last `\n`). Works on raw bytes, so it never panics on a char boundary.
fn ref_pos(source: &str, offset: usize) -> Pos {
    let bytes = &source.as_bytes()[..offset];
    let nls = bytes.iter().filter(|&&b| b == b'\n').count() as u32;
    let line_start = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    Pos {
        line: 1 + nls,
        col: (offset - line_start) as u32,
    }
}

/// Pull every token through the raw layer, in stream order, including the
/// synthetic EOF.
fn pulled_positions(source: &str) -> Vec<TokenTup<'_>> {
    let mut f = Filter::new(source, LanguageVersion::DEFAULT, lex(source));
    let mut out = Vec::new();
    while let Some(tt) = f.pull_raw() {
        let is_eof = matches!(tt.token, TokenContent::Eof);
        out.push(tt);
        if is_eof {
            break;
        }
    }
    out
}

/// Find the first `Token::Let` and return its `(start, end)`.
fn let_pos(source: &str) -> (Pos, Pos) {
    let mut f = Filter::new(source, LanguageVersion::DEFAULT, lex(source));
    while let Some(tt) = f.pull_raw() {
        match tt.token {
            TokenContent::Real(Token::Let) => return (tt.start, tt.end),
            TokenContent::Eof => break,
            _ => {}
        }
    }
    panic!("no `let` token in {source:?}");
}

#[test]
fn leading_bom_does_not_shift_line_1_column() {
    // FCS strips a file-start UTF-8 BOM (`U+FEFF`, 3 bytes) for column purposes,
    // so the first real token is column 0 — not 3, the BOM's byte width. We keep
    // the BOM as leading trivia (losslessness) but must not offside-shift line 1;
    // otherwise a later same-column top-level token is misread as a continuation.
    let (start, end) = let_pos("\u{FEFF}let x = 1\n");
    assert_eq!(start, Pos { line: 1, col: 0 });
    assert_eq!(end, Pos { line: 1, col: 3 });
}

#[test]
fn leading_bom_with_no_real_tokens_eof_does_not_underflow() {
    // A BOM-prefixed source whose only content is trivia — a directive with no
    // trailing newline, a BOM-only file, a comment — leaves `last_byte` at 0
    // while the line-1 baseline is past the BOM. The EOF column must saturate,
    // not underflow-panic. Reaching the EOF token at all proves that.
    for src in ["\u{FEFF}#nowarn \"40\"", "\u{FEFF}", "\u{FEFF}// c"] {
        let positions = pulled_positions(src);
        assert!(
            matches!(positions.last().map(|t| &t.token), Some(TokenContent::Eof)),
            "{src:?} should pull an EOF without panicking",
        );
    }
}

#[test]
fn block_comment_with_newline_then_next_line() {
    // Comment spans physical lines 1..=2; `let` is on line 3, col 0.
    let (start, end) = let_pos("(* line one\nline two *)\nlet x = 1\n");
    assert_eq!(start, Pos { line: 3, col: 0 });
    assert_eq!(end, Pos { line: 3, col: 3 });
}

#[test]
fn block_comment_then_same_line_token() {
    // `let` follows `*)` on the same physical line: line 2, col 12
    // ("line two *) " is 12 bytes).
    let (start, _) = let_pos("(* line one\nline two *) let x = 1\n");
    assert_eq!(start, Pos { line: 2, col: 12 });
}

#[test]
fn triple_quoted_string_spanning_lines() {
    // `let` on the line after a 3-line triple-quoted string.
    let src = "let s = \"\"\"a\nb\nc\"\"\"\nlet y = 1\n";
    let mut f = Filter::new(src, LanguageVersion::DEFAULT, lex(src));
    let mut lets = Vec::new();
    while let Some(tt) = f.pull_raw() {
        match tt.token {
            TokenContent::Real(Token::Let) => lets.push(tt.start),
            TokenContent::Eof => break,
            _ => {}
        }
    }
    assert_eq!(lets[0], Pos { line: 1, col: 0 });
    // The string opened on line 1 and closed on line 3, so the second `let`
    // sits on line 4.
    assert_eq!(lets[1], Pos { line: 4, col: 0 });
}

#[test]
fn verbatim_string_spanning_lines() {
    let src = "let s = @\"a\nb\"\nlet y = 1\n";
    let positions = pulled_positions(src);
    let second_let = positions
        .iter()
        .filter(|tt| matches!(tt.token, TokenContent::Real(Token::Let)))
        .nth(1)
        .expect("two lets");
    assert_eq!(second_let.start, Pos { line: 3, col: 0 });
}

#[test]
fn string_line_continuation_spanning_lines() {
    // A regular string with a `\`-newline continuation is multi-line.
    let src = "let s = \"a\\\nb\"\nlet y = 1\n";
    let positions = pulled_positions(src);
    let second_let = positions
        .iter()
        .filter(|tt| matches!(tt.token, TokenContent::Real(Token::Let)))
        .nth(1)
        .expect("two lets");
    assert_eq!(second_let.start, Pos { line: 3, col: 0 });
}

#[test]
fn crlf_line_endings() {
    // `\r\n` is a single break; the next line starts one byte past the `\n`.
    let positions = pulled_positions("let x = 1\r\nlet y = 2\r\n");
    let lets: Vec<_> = positions
        .iter()
        .filter(|tt| matches!(tt.token, TokenContent::Real(Token::Let)))
        .collect();
    assert_eq!(lets[0].start, Pos { line: 1, col: 0 });
    assert_eq!(lets[1].start, Pos { line: 2, col: 0 });
}

#[test]
fn multiple_embedded_newlines_counted() {
    // Comment with three embedded newlines -> `let` on line 4.
    let (start, _) = let_pos("(* a\nb\nc\n*) let x = 1\n");
    assert_eq!(start.line, 4);
    // "*) " is 3 bytes from the line start.
    assert_eq!(start.col, 3);
}

#[test]
fn lone_cr_is_not_a_line_break() {
    // FCS's `newline` is `\n | \r\n`; a lone `\r` does not advance the line.
    let src = "let x = 1\rlet y = 2\n";
    let positions = pulled_positions(src);
    let lets: Vec<_> = positions
        .iter()
        .filter(|tt| matches!(tt.token, TokenContent::Real(Token::Let)))
        .collect();
    assert_eq!(lets[0].start.line, 1);
    // Second `let` is still on line 1 (lone `\r` is not a break).
    assert_eq!(lets[1].start.line, 1);
}

proptest::proptest! {
    /// For every token (including EOF), the cursor's computed `start`/`end`
    /// equal the independent reference oracle. This catches any stranding on
    /// multi-line tokens, regardless of token kind or lex success.
    #[test]
    fn positions_match_reference_oracle(source in arb_source()) {
        let positions = pulled_positions(&source);
        for tt in &positions {
            proptest::prop_assert_eq!(
                tt.start,
                ref_pos(&source, tt.span.start),
                "start mismatch for {:?} at {:?} in {:?}",
                tt.token, tt.span, source
            );
            proptest::prop_assert_eq!(
                tt.end,
                ref_pos(&source, tt.span.end),
                "end mismatch for {:?} at {:?} in {:?}",
                tt.token, tt.span, source
            );
        }
    }
}

/// Source strings built from lexable fragments, several of which embed
/// newlines (block comments, triple/verbatim/continuation strings, CRLF). The
/// goal is broad coverage of multi-line tokens, not valid F# — the oracle holds
/// regardless of whether the parse would succeed.
fn arb_source() -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::prelude::*;
    let fragment = prop_oneof![
        Just("let x = 1".to_string()),
        Just("foo".to_string()),
        Just("(* a\nb *)".to_string()),
        Just("(* x\r\ny\r\nz *)".to_string()),
        Just("\"\"\"multi\nline\nstr\"\"\"".to_string()),
        Just("@\"verb\natim\"".to_string()),
        Just("\"cont\\\ninued\"".to_string()),
        Just("\n".to_string()),
        Just("\r\n".to_string()),
        Just("  ".to_string()),
        Just("\t".to_string()),
    ];
    // Join fragments with a separating space so adjacent string/comment openers
    // don't fuse into a single unintended token.
    proptest::collection::vec(fragment, 0..12).prop_map(|frags| frags.join(" "))
}
