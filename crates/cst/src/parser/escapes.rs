//! Unicode-escape range validation for char/string literals.
//!
//! FCS decodes a `\U........` (eight-hex) escape via `unicodeGraphLong`
//! (`LexHelpers.fs`:253): let `v` be the 32-bit value of the eight hex digits.
//!   * `v <= 0xFFFF`            → a single BMP code unit (`SingleChar`);
//!   * `0x10000 <= v <= 0x10FFFF` → a surrogate pair (`SurrogatePair`);
//!   * `v > 0x10FFFF`           → `Invalid`.
//!
//! In an escape-processing *string* literal an `Invalid` escape is FS1245
//! (`lexInvalidUnicodeLiteral`, `lex.fsl`:1323-1325); in a *char* literal
//! anything that isn't a single code unit (`v > 0xFFFF`, i.e. `SurrogatePair`
//! *or* `Invalid`) is FS1159 (`lexThisUnicodeOnlyInStringLiterals`,
//! `lex.fsl`:572-575). `\u....` (four-hex) escapes always decode to a `uint16`
//! code unit — lone surrogates included — so they never trigger either error.
//!
//! These checks are purely a function of a literal token's source text, so the
//! whole module is free functions over `&str`, mirroring `parser/numeric.rs`.
//! The single current consumer is `parse_const_payload` (char/byte-char and
//! regular/byte string literals, in both expression and pattern position).
//! Interpolated single strings (`$"\U…"`) decode escapes the same way and so
//! also emit FS1245 in FCS, but they're tokenised as `INTERP_STRING_FRAGMENT`s
//! through `parse_interp_string_expr`; wiring this scanner in there (alongside
//! the existing `check_extended_braces` fragment pass) is a follow-up.

use std::ops::Range;

/// The largest value a `\U` escape may name: U+10FFFF, the top of the Unicode
/// scalar range. Above this `unicodeGraphLong` returns `Invalid`.
pub(super) const MAX_UNICODE_SCALAR: u32 = 0x10_FFFF;

/// The largest value a `\U` escape may name *and still be a single code unit*
/// (`SingleChar`). Above this it's a surrogate pair, which a char literal
/// cannot hold.
pub(super) const MAX_BMP_CODE_UNIT: u32 = 0xFFFF;

/// A `\U........` escape found inside a literal body: the value of its eight
/// hex digits and the escape's byte span *relative to the body* passed in
/// (i.e. `0` is the first byte after the opening quote).
pub(super) struct LongUnicodeEscape {
    pub value: u32,
    pub span: Range<usize>,
}

/// Walk `body` — the text *between* a single-quote string/char literal's
/// delimiters — and yield every `\U`+eight-hex escape it contains.
///
/// The walk mirrors FCS's `singleQuoteString` lexer so that a backslash which
/// belongs to *another* escape can't be mistaken for the start of a `\U`
/// escape: `\\` consumes both backslashes (so `\\U00110000` is an escaped
/// backslash followed by the literal text `U00110000`, **not** a `\U` escape),
/// and `\u`/`\x` consume their own hex bodies. A `\U` not followed by exactly
/// eight hex digits is not an escape (FCS's regex doesn't match) and is
/// skipped. Every other escape form (single-char, trigraph, line
/// continuation, or an unrecognised `\<c>`) consumes the backslash plus one
/// byte — enough to stay aligned, since none of those forms can themselves
/// contain a `\U`.
pub(super) fn long_unicode_escapes(body: &str) -> Vec<LongUnicodeEscape> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'U') => match fixed_hex(bytes, i + 2, 8) {
                Some(value) => {
                    out.push(LongUnicodeEscape {
                        value,
                        span: i..i + 10,
                    });
                    i += 10;
                }
                // `\U` without eight trailing hex digits isn't an escape.
                None => i += 1,
            },
            Some(b'u') => {
                i += if fixed_hex(bytes, i + 2, 4).is_some() {
                    6
                } else {
                    1
                }
            }
            Some(b'x') => {
                i += if fixed_hex(bytes, i + 2, 2).is_some() {
                    4
                } else {
                    1
                }
            }
            // Single-char escape, trigraph, line continuation, or an
            // unrecognised `\<c>`: consume the backslash + one byte.
            Some(_) => i += 2,
            // Trailing lone backslash (the lexer shouldn't produce one inside
            // a closed literal, but stay total).
            None => i += 1,
        }
    }
    out
}

/// FCS's verdict on a byte-char literal's value (`'…'B`).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ByteCharVerdict {
    /// In range (≤ 127) — no diagnostic.
    Ok,
    /// FS1157 *warning* (`lexInvalidTrigraphAsciiByteLiteral`): a decimal
    /// trigraph in 128..=255 (`'\200'B`). FCS wraps it to a byte and warns
    /// (`lex.fsl`:544-550); only the trigraph form has this warning band.
    Warning,
    /// FS1157 *error* (`lexInvalidAsciiByteLiteral`): any other out-of-byte-range
    /// value — a trigraph > 255, or any non-trigraph form > 127.
    Error,
}

/// Classify the byte-char literal `text` (`'…'B`, quotes and `B` suffix
/// included) against FCS's byte-range rules (`lex.fsl`:522-585). The threshold
/// is form-dependent: the decimal trigraph `'\NNN'B` errors only above 255 and
/// *warns* in 128..=255, while every other form — plain char, single-letter
/// escape, `\xHH`, `\uHHHH`, `\UHHHHHHHH` — errors above 127 (no warning band).
///
/// The lexer's `Char` regex (`src/lexer/mod.rs`) guarantees the body is exactly
/// one of those forms, so the dispatch is total.
pub(super) fn classify_byte_char(text: &str) -> ByteCharVerdict {
    let body = text
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('B'))
        .and_then(|t| t.strip_suffix('\''))
        .expect("byte-char token is wrapped in `'…'B`");
    let bytes = body.as_bytes();
    if bytes.first() != Some(&b'\\') {
        // Plain char: error iff its scalar exceeds 127 (no warning band).
        return ascii_byte_verdict(body.chars().next().map(|c| c as u32));
    }
    match bytes.get(1) {
        // Decimal trigraph `\NNN`: > 255 error, 128..=255 warning, else ok.
        Some(b'0'..=b'9') => match body[1..].parse::<u32>() {
            Ok(v) if v > 0xFF => ByteCharVerdict::Error,
            Ok(v) if v > 0x7F => ByteCharVerdict::Warning,
            _ => ByteCharVerdict::Ok,
        },
        // Fixed-width hex escapes (`\xHH`/`\uHHHH`/`\UHHHHHHHH`): error above 127.
        Some(b'x') => ascii_byte_verdict(fixed_hex(bytes, 2, 2)),
        Some(b'u') => ascii_byte_verdict(fixed_hex(bytes, 2, 4)),
        Some(b'U') => ascii_byte_verdict(fixed_hex(bytes, 2, 8)),
        // Single-letter escape (`\n`, `\\`, `\'`, …): always ≤ 127.
        _ => ByteCharVerdict::Ok,
    }
}

/// Verdict for the non-trigraph forms, whose only threshold is > 127 → error.
fn ascii_byte_verdict(value: Option<u32>) -> ByteCharVerdict {
    if value.is_some_and(|v| v > 0x7F) {
        ByteCharVerdict::Error
    } else {
        ByteCharVerdict::Ok
    }
}

/// FCS FS1140 (`lexByteArrayCannotEncode`): the number of UTF-16 code units in
/// this byte-string `content` (delimiters already stripped) whose value exceeds
/// 255, i.e. that don't encode as a single byte. `errorsInByteStringBuffer`
/// (LexHelpers.fs:197) counts these at finish; the error fires when the count
/// is non-zero and its message echoes the count.
///
/// A literal source char with scalar > 0xFF contributes one wide unit per
/// UTF-16 unit ([`char::len_utf16`]: 1 for a BMP char ≥ U+0100, 2 for an astral
/// char's surrogate pair). Escape *bodies* are pure ASCII, so this literal-char
/// pass also serves the non-escape kinds (`escapes == false`). For the regular
/// `"…"B` form, a `\u`/`\U` escape additionally contributes: a `\u` above 0xFF
/// adds one unit; a `\U` adds two for a surrogate pair (U+10000..=U+10FFFF), one
/// for a wide `SingleChar` (U+0100..=U+FFFF), and none for an `Invalid` escape
/// (above U+10FFFF — that's FS1245, which emits no unit). `\xHH` and decimal
/// trigraphs are byte-valued, so they never contribute.
pub(super) fn byte_string_wide_unit_count(content: &str, escapes: bool) -> usize {
    let mut count: usize = content
        .chars()
        .filter(|&c| c as u32 > 0xFF)
        .map(char::len_utf16)
        .sum();
    if escapes {
        let bytes = content.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'\\' {
                i += 1;
                continue;
            }
            match bytes.get(i + 1) {
                Some(b'u') => match fixed_hex(bytes, i + 2, 4) {
                    Some(v) => {
                        count += usize::from(v > 0xFF);
                        i += 6;
                    }
                    None => i += 1,
                },
                Some(b'U') => match fixed_hex(bytes, i + 2, 8) {
                    Some(v) => {
                        count += match v {
                            0x1_0000..=0x10_FFFF => 2,
                            0x100..=0xFFFF => 1,
                            // ≤ 0xFF (one byte), or > U+10FFFF (Invalid → FS1245).
                            _ => 0,
                        };
                        i += 10;
                    }
                    None => i += 1,
                },
                // `\xHH` is byte-valued; everything else (letter escape,
                // trigraph, line continuation, unrecognised `\<c>`) either
                // can't be wide or has its char counted by the literal pass.
                Some(b'x') => {
                    i += if fixed_hex(bytes, i + 2, 2).is_some() {
                        4
                    } else {
                        1
                    }
                }
                Some(_) => i += 2,
                None => i += 1,
            }
        }
    }
    count
}

/// `Some(value)` if `bytes[start..start+len]` exists and is all ASCII hex
/// digits; `None` if it runs off the end or contains a non-hex byte (an FCS
/// regex non-match). `value` is the hex digits parsed as a `u32`.
fn fixed_hex(bytes: &[u8], start: usize, len: usize) -> Option<u32> {
    let end = start.checked_add(len)?;
    let slice = bytes.get(start..end)?;
    if !slice.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    // `len` is 2/4/8, so the value always fits in u32.
    u32::from_str_radix(
        std::str::from_utf8(slice).expect("hex digits are ASCII"),
        16,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(body: &str) -> Vec<(u32, Range<usize>)> {
        long_unicode_escapes(body)
            .into_iter()
            .map(|e| (e.value, e.span))
            .collect()
    }

    #[test]
    fn finds_long_escape_with_value_and_span() {
        assert_eq!(vals("\\U00110000"), vec![(0x0011_0000, 0..10)]);
        assert_eq!(vals("\\U0010FFFF"), vec![(0x0010_FFFF, 0..10)]);
        assert_eq!(vals("ab\\U0001F600cd"), vec![(0x0001_F600, 2..12)]);
    }

    #[test]
    fn ignores_short_and_hex_escapes() {
        assert!(vals("\\uD800").is_empty());
        assert!(vals("\\xFF").is_empty());
        assert!(vals("plain text, no escapes").is_empty());
    }

    #[test]
    fn escaped_backslash_is_not_a_long_escape() {
        // `\\` consumes both backslashes; `U00110000` is then literal.
        assert!(vals("\\\\U00110000").is_empty());
        // A `\u` escape directly before a real `\U` escape: only the `\U` is
        // yielded, and its span starts after the six-byte `ꯍ`.
        assert_eq!(vals("\\uABCD\\U00110000"), vec![(0x0011_0000, 6..16)]);
    }

    #[test]
    fn incomplete_long_escape_is_not_an_escape() {
        // Seven hex digits then a non-hex byte: regex non-match.
        assert!(vals("\\U0000000Z").is_empty());
        // `\U` at the very end without a full body.
        assert!(vals("\\U123").is_empty());
    }

    #[test]
    fn multiple_escapes_each_found() {
        assert_eq!(
            vals("\\U00110000\\U00110000"),
            vec![(0x0011_0000, 0..10), (0x0011_0000, 10..20)],
        );
    }

    #[test]
    fn byte_char_classify_thresholds() {
        use ByteCharVerdict::{Error, Ok, Warning};
        // Non-trigraph forms error above 127 (no warning band).
        assert_eq!(classify_byte_char("'\\xFF'B"), Error);
        assert_eq!(classify_byte_char("'\\x7F'B"), Ok);
        assert_eq!(classify_byte_char("'\\u00FF'B"), Error);
        assert_eq!(classify_byte_char("'\\U000000FF'B"), Error);
        assert_eq!(classify_byte_char("'\\U0001F600'B"), Error);
        assert_eq!(classify_byte_char("'é'B"), Error);
        // Plain ASCII and letter escapes are fine.
        assert_eq!(classify_byte_char("'a'B"), Ok);
        assert_eq!(classify_byte_char("'\\n'B"), Ok);
        // Trigraph: ≤127 ok, 128..=255 warning, > 255 error.
        assert_eq!(classify_byte_char("'\\127'B"), Ok);
        assert_eq!(classify_byte_char("'\\200'B"), Warning);
        assert_eq!(classify_byte_char("'\\255'B"), Warning);
        assert_eq!(classify_byte_char("'\\256'B"), Error);
    }

    #[test]
    fn byte_string_wide_unit_count_regular() {
        // Literal wide chars: BMP → 1 each, astral → 2 (surrogate pair).
        assert_eq!(byte_string_wide_unit_count("Ā", true), 1);
        assert_eq!(byte_string_wide_unit_count("AĀBĀ", true), 2);
        assert_eq!(byte_string_wide_unit_count("😀", true), 2);
        // U+00FF = 255 fits a byte (it's a warning, not counted here).
        assert_eq!(byte_string_wide_unit_count("ÿ", true), 0);
        assert_eq!(byte_string_wide_unit_count("abc", true), 0);
        // Escapes: \u/\U wide values count; \x and trigraphs don't.
        assert_eq!(byte_string_wide_unit_count("\\u0100", true), 1);
        assert_eq!(byte_string_wide_unit_count("\\u00FF", true), 0);
        assert_eq!(byte_string_wide_unit_count("\\uD800", true), 1);
        assert_eq!(byte_string_wide_unit_count("\\U00000100", true), 1);
        assert_eq!(byte_string_wide_unit_count("\\U0001F600", true), 2);
        // Invalid `\U` (> U+10FFFF) emits no unit — FS1245 territory, not here.
        assert_eq!(byte_string_wide_unit_count("\\U00110000", true), 0);
        assert_eq!(byte_string_wide_unit_count("\\xFF", true), 0);
        assert_eq!(byte_string_wide_unit_count("\\999", true), 0);
        // An escaped backslash before `u0100` is literal text, not an escape.
        assert_eq!(byte_string_wide_unit_count("\\\\u0100", true), 0);
    }

    #[test]
    fn byte_string_wide_unit_count_non_escape() {
        // Verbatim/triple: only literal chars count; escape-looking text is
        // literal (all ASCII → 0).
        assert_eq!(byte_string_wide_unit_count("Ā", false), 1);
        assert_eq!(byte_string_wide_unit_count("\\u0100", false), 0);
        assert_eq!(byte_string_wide_unit_count("abc", false), 0);
    }
}
