//! Logos callbacks for tokens that need more than a regex.
//!
//! These are the implementations behind `#[token("(*", lex_block_comment)]`,
//! the three `lex_*_string` token actions, and the byte-level helpers they
//! call. Keeping them out of `mod.rs` lets the Token enum stay the centrepiece
//! of that file.

use logos::Lexer;

use super::{InterpKind, LexError, Token};

pub(super) fn lex_block_comment<'a>(lex: &mut Lexer<'a, Token<'a>>) -> Result<(), LexError> {
    // `(*` is already consumed. F# block comments nest, but the `comment` rule
    // in `lex.fsl` also recognises embedded char and string literals so that
    // characters inside them don't get mistaken for comment delimiters. We
    // mirror those special cases (`(*)`, `'…'`, `"…"`, `"""…"""`, `@"…"`).
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut depth: usize = 1;
    let mut i = 0;
    while i < bytes.len() {
        // `(*)` is the parenthesised-star operator, not a nested-comment opener.
        if matches_at(bytes, i, b"(*)") {
            i += 3;
            continue;
        }
        if matches_at(bytes, i, b"(*") {
            depth += 1;
            i += 2;
            continue;
        }
        if matches_at(bytes, i, b"*)") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                lex.bump(i);
                return Ok(());
            }
            continue;
        }
        // Triple-quote string `"""…"""` (must check before `"`).
        if matches_at(bytes, i, b"\"\"\"") {
            i = skip_triple_string(bytes, i + 3);
            continue;
        }
        // Verbatim string `@"…"`.
        if matches_at(bytes, i, b"@\"") {
            i = skip_verbatim_string(bytes, i + 2);
            continue;
        }
        if bytes[i] == b'"' {
            i = skip_single_string(bytes, i + 1);
            continue;
        }
        if bytes[i] == b'\'' {
            // Could be a char literal or a stray quote (e.g. `'T` type parameter
            // in commented-out code). Try to match a complete char literal; if
            // we can't, just advance one byte.
            if let Some(end) = match_char_literal(bytes, i) {
                i = end;
            } else {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedComment)
}

#[inline]
fn matches_at(bytes: &[u8], i: usize, needle: &[u8]) -> bool {
    bytes.len() >= i + needle.len() && &bytes[i..i + needle.len()] == needle
}

/// Advance past a closing `"` from position `from`, honouring `\X` escapes.
/// Returns `bytes.len()` if no closer is found.
fn skip_single_string(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn skip_verbatim_string(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if bytes.get(i + 1) == Some(&b'"') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_triple_string(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i + 2 < bytes.len() {
        if bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            return i + 3;
        }
        i += 1;
    }
    bytes.len()
}

/// Length of a backslash escape recognised by FCS's `singleQuoteString`.
///
/// Interpolated-string boundary scanning may skip only these forms. An
/// unrecognised pair like `\{` is two ordinary content characters to FCS: the
/// backslash is consumed by the fallback arm, then `{` is still seen as an
/// interpolation delimiter.
pub(super) fn single_quote_escape_len(bytes: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(bytes.get(i), Some(&b'\\'));
    let next = *bytes.get(i + 1)?;
    match next {
        b'\\' | b'"' | b'\'' | b'a' | b'f' | b'v' | b'n' | b't' | b'b' | b'r' => Some(2),
        b'\n' => Some(line_continuation_len(bytes, i, 2)),
        b'\r' if bytes.get(i + 2) == Some(&b'\n') => Some(line_continuation_len(bytes, i, 3)),
        b'0'..=b'9' if ascii_digits(bytes, i + 1, 3) => Some(4),
        b'x' if ascii_hex_digits(bytes, i + 2, 2) => Some(4),
        b'u' if ascii_hex_digits(bytes, i + 2, 4) => Some(6),
        b'U' if ascii_hex_digits(bytes, i + 2, 8) => Some(10),
        _ => None,
    }
}

fn line_continuation_len(bytes: &[u8], start: usize, newline_len: usize) -> usize {
    let mut end = start + newline_len;
    while matches!(bytes.get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    end - start
}

fn ascii_digits(bytes: &[u8], start: usize, len: usize) -> bool {
    bytes
        .get(start..start + len)
        .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
}

fn ascii_hex_digits(bytes: &[u8], start: usize, len: usize) -> bool {
    bytes
        .get(start..start + len)
        .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
}

/// If the bytes at `i` form a complete char literal — strictly the `char` rule
/// from `lex.fsl` line 305: `'\'' ([^excluded] | escape_char) '\''` — return
/// the byte index just after it. Otherwise None.
///
/// We deliberately do NOT match the trigraph / hex / unicode-graph forms here:
/// inside the `comment` rule, lex.fsl only invokes plain `char`, and accepting
/// the longer forms would cause us to greedily swallow text like `'a,'b,'c>*)`
/// (treating `'a,'` as a phantom literal and missing the real `*)`).
fn match_char_literal(bytes: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(bytes[i], b'\'');
    let rest = bytes.get(i + 1..)?;
    // Escape form: '\X' where X is one of the recognised escape letters.
    if rest.first() == Some(&b'\\') {
        if rest.len() < 3 || rest[2] != b'\'' {
            return None;
        }
        if !matches!(
            rest[1],
            b'\\' | b'"' | b'\'' | b'a' | b'f' | b'v' | b'n' | b't' | b'b' | b'r'
        ) {
            return None;
        }
        return Some(i + 1 + 3);
    }
    // Plain form: '<one UTF-8 char>'.
    let first = *rest.first()?;
    let n = utf8_char_len(first)?;
    if rest.len() < n + 1 || rest[n] != b'\'' {
        return None;
    }
    // Excluded body chars per lex.fsl:305: \, \n, \r, \t, \b — *not* the
    // apostrophe, so `'''` (the apostrophe char literal) is a match.
    if n == 1 && matches!(first, b'\\' | b'\n' | b'\r' | b'\t' | 0x08) {
        return None;
    }
    Some(i + 1 + n + 1)
}

#[inline]
fn utf8_char_len(b: u8) -> Option<usize> {
    if b < 0x80 {
        Some(1)
    } else if b < 0xC0 {
        None
    } else if b < 0xE0 {
        Some(2)
    } else if b < 0xF0 {
        Some(3)
    } else if b < 0xF8 {
        Some(4)
    } else {
        None
    }
}

pub(super) fn lex_single_string<'a>(lex: &mut Lexer<'a, Token<'a>>) -> Result<(), LexError> {
    // Opening `"` already consumed. F# `"..."` may span lines — see `singleQuoteString`
    // in `lex.fsl`, which has explicit rules for `'\n'` and `'\\' newline ...`.
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                // Skip the escape character; semantic validation is a later layer.
                i += 2;
            }
            b'"' => {
                let consumed = i + 1 + byte_suffix_len(&bytes[i + 1..]);
                lex.bump(consumed);
                return Ok(());
            }
            _ => i += 1,
        }
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}

pub(super) fn lex_verbatim_string<'a>(lex: &mut Lexer<'a, Token<'a>>) -> Result<(), LexError> {
    // Opening `@"` already consumed. The only escape is `""` (a literal quote).
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if bytes.get(i + 1) == Some(&b'"') {
                i += 2;
                continue;
            }
            let consumed = i + 1 + byte_suffix_len(&bytes[i + 1..]);
            lex.bump(consumed);
            return Ok(());
        }
        i += 1;
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}

pub(super) fn lex_triple_string<'a>(lex: &mut Lexer<'a, Token<'a>>) -> Result<(), LexError> {
    // Opening `"""` already consumed.
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            let consumed = i + 3 + byte_suffix_len(&bytes[i + 3..]);
            lex.bump(consumed);
            return Ok(());
        }
        i += 1;
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}

/// F# byte-string literals are written as `"abc"B`, `@"abc"B`, or `"""abc"""B`.
/// After consuming the closing quote(s), return 1 if the next byte is `B`
/// (so the suffix is included in the string token's span), else 0.
fn byte_suffix_len(after_close: &[u8]) -> usize {
    if after_close.first() == Some(&b'B') {
        1
    } else {
        0
    }
}

/// Opener for an interpolated string. The `$"` two bytes are already
/// consumed; this callback walks the body until it finds either an
/// unescaped `{` (start of the first fill) or the matching closing `"`.
///
/// The callback discriminates between [`InterpKind::Begin`] (a `{` was
/// reached first — the matched span includes the opening `$"`, the body
/// up to that point, *and* the `{`) and [`InterpKind::BeginEnd`] (a `"`
/// was reached first — the whole string was bare, matching span covers
/// `$"..."`). A trailing `B` (`$"..."B`) is folded into the `BeginEnd`
/// span and recorded as `is_byte`; FCS rejects interpolated byte strings
/// (FS3377) but the diagnostic and recovery belong to the parser.
///
/// Escape rules (matching `singleQuoteString` in `lex.fsl:1255-1383`):
/// * recognised backslash escapes (`\"`, `\\`, line continuation, numeric /
///   Unicode escapes, etc.) are content and skipped as one string item.
/// * unrecognised backslash pairs are not escapes. In particular, `\{` leaves
///   the `{` visible as a fill opener; use `{{` for a literal brace.
/// * `{{` is a literal `{` — skip both bytes without opening a fill.
/// * `}}` is a literal `}` — skip both bytes; it doesn't close a fill (we
///   aren't in one yet in the opener).
///
/// Triple-quoted interp strings (`$"""..."""`) are handled by
/// [`lex_interp_triple_opener`]; extended bracket-count forms (`$$"""..."""`,
/// ≥2 `$`) by [`lex_interp_extended_opener`].
pub(super) fn lex_interp_opener<'a>(
    lex: &mut Lexer<'a, Token<'a>>,
) -> Result<InterpKind, LexError> {
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if let Some(len) = single_quote_escape_len(bytes, i) {
                    i += len;
                } else {
                    i += 1;
                }
            }
            b'{' => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2;
                    continue;
                }
                lex.bump(i + 1);
                return Ok(InterpKind::Begin);
            }
            b'}' => {
                if bytes.get(i + 1) == Some(&b'}') {
                    i += 2;
                    continue;
                }
                // Stray `}` outside a fill — FCS treats it as part of the
                // string body and reports a separate diagnostic (FS1102).
                // We let the byte through and keep scanning; surfacing the
                // diagnostic belongs to a later phase.
                i += 1;
            }
            b'"' => {
                let is_byte = bytes.get(i + 1) == Some(&b'B');
                lex.bump(i + 1 + usize::from(is_byte));
                return Ok(InterpKind::BeginEnd { is_byte });
            }
            _ => i += 1,
        }
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}

/// Opener for a triple-quoted interpolated string. The `$"""` four bytes
/// are already consumed; this callback walks the body until it finds
/// either an unescaped `{` (start of the first fill) or the matching
/// closing `"""`.
///
/// Returns [`InterpKind::TripleBegin`] when a `{` was reached first
/// (span includes the opening `$"""`, the body up to that point, and
/// the trailing `{`) or [`InterpKind::TripleBeginEnd`] when `"""` was
/// reached first (whole bare string). A trailing `B` (`$"""..."""B`) is
/// folded into the `TripleBeginEnd` span and recorded as `is_byte`; see
/// [`lex_interp_opener`].
///
/// Escape rules differ from the single-quoted form (`singleQuoteString`
/// in `lex.fsl:1255-1383` vs `tripleQuoteString` in
/// `lex.fsl:1540-1638`):
/// * No backslash escape — `\` is a literal content byte. The body of
///   `$"""\n"""` is the two characters `\` and `n`, not a newline.
/// * `{{` is a literal `{`; `}}` is a literal `}` — same as the
///   single-quoted form.
/// * Newlines in the body are content (single-quoted strings can't
///   span lines at all).
/// * The closer is `"""` (greedy first run of three or more `"`). A
///   single or double `"` in the body is content.
pub(super) fn lex_interp_triple_opener<'a>(
    lex: &mut Lexer<'a, Token<'a>>,
) -> Result<InterpKind, LexError> {
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2;
                    continue;
                }
                lex.bump(i + 1);
                return Ok(InterpKind::TripleBegin);
            }
            b'}' => {
                if bytes.get(i + 1) == Some(&b'}') {
                    i += 2;
                    continue;
                }
                // Stray `}` outside a fill — same handling as the
                // single-quoted opener: let it through, defer the
                // diagnostic.
                i += 1;
            }
            b'"' if bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'"') => {
                let is_byte = bytes.get(i + 3) == Some(&b'B');
                lex.bump(i + 3 + usize::from(is_byte));
                return Ok(InterpKind::TripleBeginEnd { is_byte });
            }
            _ => i += 1,
        }
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}

/// Opener for a verbatim interpolated string. The `$@"` / `@$"` three
/// bytes are already consumed (the two spellings are interchangeable in
/// FCS, `lex.fsl:687`); this callback walks the body until it finds either
/// an unescaped `{` (start of the first fill) or the matching closing `"`.
///
/// Returns [`InterpKind::VerbatimBegin`] when a `{` was reached first
/// (span includes the opening `$@"` / `@$"`, the body up to that point,
/// and the trailing `{`) or [`InterpKind::VerbatimBeginEnd`] when the
/// closing `"` was reached first (whole bare string). A trailing `B`
/// (`$@"..."B`) is folded into the `VerbatimBeginEnd` span and recorded as
/// `is_byte`; see [`lex_interp_opener`].
///
/// Escape rules are the verbatim ones (cf. [`lex_verbatim_string`],
/// `lex.fsl` `verbatimString`):
/// * `""` is a literal quote — skip both bytes; it does *not* terminate.
/// * No backslash escape — `\` is a literal content byte.
/// * `{{` is a literal `{`; `}}` is a literal `}` — the interp brace
///   digraphs still apply, same as the other openers.
/// * Newlines in the body are content.
pub(super) fn lex_interp_verbatim_opener<'a>(
    lex: &mut Lexer<'a, Token<'a>>,
) -> Result<InterpKind, LexError> {
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2;
                    continue;
                }
                lex.bump(i + 1);
                return Ok(InterpKind::VerbatimBegin);
            }
            b'}' => {
                if bytes.get(i + 1) == Some(&b'}') {
                    i += 2;
                    continue;
                }
                // Stray `}` outside a fill — same handling as the other
                // openers: let it through, defer the diagnostic.
                i += 1;
            }
            b'"' => {
                if bytes.get(i + 1) == Some(&b'"') {
                    // `""` is a literal quote in a verbatim body.
                    i += 2;
                    continue;
                }
                let is_byte = bytes.get(i + 1) == Some(&b'B');
                lex.bump(i + 1 + usize::from(is_byte));
                return Ok(InterpKind::VerbatimBeginEnd { is_byte });
            }
            _ => i += 1,
        }
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}

/// Opener for an extended (bracket-count) interpolated string. The
/// `$$"""` / `$$$"""` / … prefix (≥2 `$` then `"""`) is already consumed;
/// `n` is the leading `$` count = the *interpolation delimiter length*. This
/// callback walks the body until either a `{`-run of length ≥ `n` (the first
/// fill opens) or the closing `"""`.
///
/// Returns [`InterpKind::ExtendedBegin`] when a fill-opening `{`-run was
/// reached first — the matched span includes the opening `$$"""`, the body up
/// to that point, *and* the whole `{`-run (the driver/normaliser split the run
/// into content + delimiter) — or [`InterpKind::ExtendedBeginEnd`] when `"""`
/// was reached first (whole bare string).
///
/// Content rules are triple-like (`extendedInterpolatedString`,
/// `lex.fsl:1640`) but with **no** brace digraph:
/// * No backslash escape — `\` is a literal content byte.
/// * Newlines in the body are content.
/// * The closer is `"""` (greedy first run of three or more `"`); a 1- or
///   2-`"` run is content. Unlike single/triple/verbatim interp, a trailing
///   `B` is **not** consumed as a byte suffix (`lex.fsl:1641`).
/// * A `{`-run shorter than `n` is literal content; a run ≥ `n` opens the
///   first fill (the whole run is consumed into the opener span). A `}`-run
///   is content here — outside a fill it can't close anything; the parser
///   diagnoses an over-long `}`-run (FS1249) by re-scanning the fragment.
pub(super) fn lex_interp_extended_opener<'a>(
    lex: &mut Lexer<'a, Token<'a>>,
) -> Result<InterpKind, LexError> {
    let n = lex.slice().bytes().take_while(|&b| b == b'$').count();
    let remainder = lex.remainder();
    let bytes = remainder.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                let run = bytes[i..].iter().take_while(|&&b| b == b'{').count();
                if run >= n {
                    // Fill opens — consume the whole `{`-run into the opener.
                    lex.bump(i + run);
                    return Ok(InterpKind::ExtendedBegin { n });
                }
                // Run shorter than `n` — literal content.
                i += run;
            }
            b'"' if bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'"') => {
                lex.bump(i + 3);
                return Ok(InterpKind::ExtendedBeginEnd { n });
            }
            _ => i += 1,
        }
    }
    lex.bump(bytes.len());
    Err(LexError::UnterminatedString)
}
