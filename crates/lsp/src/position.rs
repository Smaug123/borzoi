//! Byte offset → LSP `Position` translation shared by every diagnostic
//! producer in the server. Lives in its own module so the lexer
//! diagnostics ([`crate::diagnostics`]) and the fsproj diagnostics
//! ([`crate::fsproj_diagnostics`]) agree on UTF-16 column counting and
//! newline handling — divergence would mean two diagnostic sources
//! pointing at different lines for the same byte offset.

use lsp_types::Position;

/// Byte offset → LSP `Position` (line, UTF-16 column).
///
/// All three forms LSP accepts as line breaks — `\n`, `\r`, and `\r\n` —
/// bump the line counter (and a `\r\n` pair counts as one break, not
/// two). This must agree with the F# lexer's own newline regex
/// (`\r\n|\n|\r`), otherwise diagnostics in CR-terminated files would
/// point at the wrong line. The same definition applies to the fsproj
/// parser's spans, since they index into the same source string.
///
/// If `offset` falls inside a multi-byte UTF-8 sequence, it's rounded
/// *up* to the next char boundary. Spans from the F# lexer and from
/// roxmltree are produced over `&str` and should already be
/// char-aligned, but we don't rely on it. The same snap-up applies if
/// `offset` falls between the `\r` and `\n` of a `\r\n` pair — we treat
/// the pair as one atomic line break, so a mid-pair offset is "inside"
/// a multi-byte structure in the same sense.
///
/// `offset` is clamped to `text.len()` if it exceeds it; callers that
/// would otherwise panic on an out-of-range span get the end-of-file
/// position instead.
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let mut offset = offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset += 1;
    }
    // Treat `\r\n` as one atomic line break: an offset that splits the pair
    // is also "inside" a multi-byte structure, just like mid-UTF-8. Snap up,
    // mirroring the boundary loop above. Keeps `position_to_offset` an exact
    // inverse for all char-boundary offsets.
    if offset > 0
        && offset < text.len()
        && text.as_bytes()[offset - 1] == b'\r'
        && text.as_bytes()[offset] == b'\n'
    {
        offset += 1;
    }
    let mut line: u32 = 0;
    let mut character: u32 = 0;
    let mut chars = text[..offset].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                line += 1;
                character = 0;
                // `\r\n` is one break, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' => {
                line += 1;
                character = 0;
            }
            _ => character += c.len_utf16() as u32,
        }
    }
    Position { line, character }
}

/// LSP [`Position`] (line, UTF-16 column) → byte offset.
///
/// The exact inverse of [`offset_to_position`] for any in-range position: same
/// `\n` / `\r` / `\r\n` line-break convention (a `\r\n` pair counts as one
/// break, not two), same UTF-16 surrogate handling for characters above the
/// BMP.
///
/// Clamping rules — both required by the LSP spec, where clients are free to
/// send positions past EOL/EOF and the server must not panic:
///
/// - `line` beyond the last line clamps to the end of the input.
/// - `character` beyond the line's UTF-16 length clamps to the line's EOL
///   byte (the position of the line break, or the end of the input on the
///   last line). It does NOT wrap to the next line.
///
/// If a character lands mid-surrogate (a `character` value that bisects a
/// supplementary-plane character's surrogate pair), the offset rounds *up*
/// to the next char boundary so the returned offset is always char-aligned.
pub fn position_to_offset(text: &str, pos: Position) -> usize {
    let mut line: u32 = 0;
    let mut byte: usize = 0;
    let bytes = text.as_bytes();

    // Walk to the start of the target line, treating `\n`, `\r`, and `\r\n`
    // as line terminators (mirroring `offset_to_position`).
    while line < pos.line && byte < bytes.len() {
        match bytes[byte] {
            b'\r' => {
                byte += 1;
                if byte < bytes.len() && bytes[byte] == b'\n' {
                    byte += 1;
                }
                line += 1;
            }
            b'\n' => {
                byte += 1;
                line += 1;
            }
            _ => byte += 1,
        }
    }
    // `line` past the last line: clamp to EOF.
    if line < pos.line {
        return bytes.len();
    }

    // Consume `pos.character` UTF-16 code units, stopping at the line's EOL
    // (the `\r` or `\n` that terminates it) — never wrapping onto the next
    // line, per LSP convention.
    let mut remaining = pos.character;
    while remaining > 0 && byte < bytes.len() {
        let b = bytes[byte];
        if b == b'\r' || b == b'\n' {
            break;
        }
        // Decode one UTF-8 code point starting at `byte`. `text[byte..]` is
        // guaranteed char-aligned because we only ever advance by whole code
        // points or whole line breaks.
        let ch = text[byte..]
            .chars()
            .next()
            .expect("non-empty slice yields a char");
        let units = ch.len_utf16() as u32;
        if remaining < units {
            // Mid-surrogate: round up to the next char boundary.
            byte += ch.len_utf8();
            return byte;
        }
        remaining -= units;
        byte += ch.len_utf8();
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_code_units() {
        // `À` is 1 char / 2 UTF-8 bytes / 1 UTF-16 unit.
        // `🦀` is 1 char / 4 UTF-8 bytes / 2 UTF-16 units (surrogate pair).
        // Byte 6 ('X') sits after `À🦀`, i.e. at UTF-16 column 3.
        let src = "À🦀X";
        assert_eq!(
            offset_to_position(src, 6),
            Position {
                line: 0,
                character: 3,
            }
        );
    }

    #[test]
    fn newline_counting() {
        // `\r\n` is one newline for line-counting purposes too.
        assert_eq!(
            offset_to_position("a\nb\nc", 4),
            Position {
                line: 2,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position("a\r\nb", 3),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position("a\nbc", 4),
            Position {
                line: 1,
                character: 2
            }
        );
    }

    #[test]
    fn lone_carriage_return_breaks_line() {
        // Lex.fsl accepts `\r` alone as a line terminator; LSP positions
        // must agree, or diagnostics in CR-terminated files land on the
        // previous line with an inflated UTF-16 column.
        assert_eq!(
            offset_to_position("a\rb", 2),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position("a\rb", 3),
            Position {
                line: 1,
                character: 1
            }
        );
        // Two lone CRs, then a body.
        assert_eq!(
            offset_to_position("a\r\rxy", 4),
            Position {
                line: 2,
                character: 1
            }
        );
    }

    #[test]
    fn clamps_past_eof() {
        let p = offset_to_position("abc", 100);
        assert_eq!(
            p,
            Position {
                line: 0,
                character: 3
            }
        );
    }

    #[test]
    fn rounds_inside_crlf() {
        // A byte offset between `\r` and `\n` is treated as the same atomic
        // position as the end of the line break — it snaps up to byte 2 so
        // `position_to_offset` is a true inverse.
        assert_eq!(
            offset_to_position("\r\n", 1),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position("a\r\nbc", 2),
            Position {
                line: 1,
                character: 0
            }
        );
    }

    #[test]
    fn rounds_inside_multibyte() {
        // 'À' = bytes [0xC3, 0x80]; offset 1 is mid-sequence and should snap up to 2.
        let src = "Àb";
        assert_eq!(
            offset_to_position(src, 1),
            Position {
                line: 0,
                character: 1
            }
        );
    }

    // ---- position_to_offset ----

    #[test]
    fn position_to_offset_basics() {
        assert_eq!(
            position_to_offset(
                "abc",
                Position {
                    line: 0,
                    character: 2
                }
            ),
            2
        );
        assert_eq!(
            position_to_offset(
                "a\nbc",
                Position {
                    line: 1,
                    character: 2
                }
            ),
            4
        );
        // `\r\n` is one break.
        assert_eq!(
            position_to_offset(
                "a\r\nbc",
                Position {
                    line: 1,
                    character: 1
                }
            ),
            4
        );
        // Lone `\r` is a break too (mirrors `offset_to_position`).
        assert_eq!(
            position_to_offset(
                "a\rbc",
                Position {
                    line: 1,
                    character: 1
                }
            ),
            3
        );
    }

    #[test]
    fn position_to_offset_clamps_line_past_eof() {
        assert_eq!(
            position_to_offset(
                "abc",
                Position {
                    line: 5,
                    character: 0
                }
            ),
            3
        );
    }

    #[test]
    fn position_to_offset_clamps_character_to_eol_no_wrap() {
        // Past end of line 0, but does NOT wrap onto line 1: clamps at the
        // `\n` byte (offset 3), not at `b` (offset 4).
        assert_eq!(
            position_to_offset(
                "abc\nde",
                Position {
                    line: 0,
                    character: 50
                }
            ),
            3
        );
        // Last line has no trailing break: clamp to EOF.
        assert_eq!(
            position_to_offset(
                "abc",
                Position {
                    line: 0,
                    character: 50
                }
            ),
            3
        );
    }

    #[test]
    fn position_to_offset_utf16_surrogate_pair() {
        // `🦀` is 1 char / 4 UTF-8 bytes / 2 UTF-16 units. A `character` of 2
        // (the position *after* the crab) sits at byte 4.
        let src = "🦀X";
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 2
                }
            ),
            4
        );
        // `character: 1` lands mid-surrogate; round up to the next char
        // boundary (the start of `X`).
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 1
                }
            ),
            4
        );
    }

    proptest::proptest! {
        /// Round-trip: for any in-range byte offset, going to a Position and
        /// back lands on the same offset. The interesting case (covered by
        /// the generator's mix of newline styles and multibyte chars) is that
        /// the offset and Position encodings agree under `\r\n`, lone `\r`,
        /// and `\n` line breaks plus UTF-16 surrogate pairs.
        #[test]
        fn offset_round_trips_through_position(
            (text, offset) in text_and_offset(),
        ) {
            let pos = offset_to_position(&text, offset);
            let back = position_to_offset(&text, pos);
            proptest::prop_assert_eq!(back, offset);
        }

        /// The other direction: a `Position` obtained from a valid offset
        /// round-trips to that offset. We don't claim this for arbitrary
        /// `Position`s (out-of-range ones get clamped), only for ones we
        /// know correspond to a real byte boundary.
        #[test]
        fn position_round_trips_through_offset(
            (text, offset) in text_and_offset(),
        ) {
            let pos = offset_to_position(&text, offset);
            let off2 = position_to_offset(&text, pos);
            proptest::prop_assert_eq!(offset_to_position(&text, off2), pos);
        }
    }

    /// A generator pairing a small string (mixing line breaks and multi-byte
    /// chars) with a char-boundary byte offset into it.
    fn text_and_offset() -> impl proptest::strategy::Strategy<Value = (String, usize)> {
        use proptest::prelude::*;
        // Pick from a varied alphabet: ASCII, lone-CR, LF, CRLF as a unit,
        // 2-byte (`À`) and 4-byte (`🦀`) characters. The CRLF is generated
        // as one token so the proptest doesn't have to find it by chance.
        let token = prop_oneof![
            Just("a".to_string()),
            Just("À".to_string()),
            Just("🦀".to_string()),
            Just("\n".to_string()),
            Just("\r".to_string()),
            Just("\r\n".to_string()),
        ];
        proptest::collection::vec(token, 0..16).prop_flat_map(|tokens| {
            let text: String = tokens.concat();
            // Compute char-boundary offsets, also skipping offsets that bisect
            // a `\r\n` pair (we treat the pair as one atomic unit, like a
            // multi-byte UTF-8 sequence, so it has no addressable interior).
            let bytes = text.as_bytes().to_vec();
            let boundaries: Vec<usize> = (0..=text.len())
                .filter(|&i| text.is_char_boundary(i))
                .filter(|&i| {
                    !(i > 0 && i < bytes.len() && bytes[i - 1] == b'\r' && bytes[i] == b'\n')
                })
                .collect();
            let bidx = 0usize..boundaries.len();
            (Just(text), bidx.prop_map(move |i| boundaries[i]))
        })
    }
}
