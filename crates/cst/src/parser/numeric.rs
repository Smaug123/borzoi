//! Numeric-literal validation and classification: separator-placement checks,
//! suffix/radix splitting, and the per-width range checks that map a lexer
//! numeric token to its `*_LIT` [`SyntaxKind`]. Split out of `parser/mod.rs`;
//! every entry is a pure function over the literal's source text.

use crate::syntax::SyntaxKind;

/// `true` if every `_` in `text` is flanked on both sides by a character
/// `is_digit` calls a digit. Matches FCS's separator rule from `lex.fsl`'s
/// `integer = digit ((digit | separator)* digit)?` and the analogous
/// float/decimal/xinteger productions — separators may appear between
/// digits but not at digit-run boundaries (start, end, adjacent to `.`,
/// `e`, `E`, sign, prefix, or suffix). Our lexer regex is more permissive
/// (e.g. `[0-9][0-9_]*\.[0-9_]*` would accept `1_.5`), so the parser has
/// to enforce this rule itself.
pub(super) fn separators_well_placed(text: &str, is_digit: impl Fn(char) -> bool) -> bool {
    // FCS's `(digit | separator)*` lets `_` runs sit inside a digit run
    // (`1__2` is well-formed). So for each `_`, walk outward skipping
    // further `_`s; both sides must land on a digit. A bare run of `_`s
    // — no neighbouring digit on one side — is rejected (e.g. `_1`,
    // `1_`, `1_.5`, `1_e10`).
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != '_' {
            continue;
        }
        let mut left = i;
        while left > 0 && chars[left - 1] == '_' {
            left -= 1;
        }
        let prev_ok = left > 0 && is_digit(chars[left - 1]);
        let mut right = i;
        while right + 1 < chars.len() && chars[right + 1] == '_' {
            right += 1;
        }
        let next_ok = right + 1 < chars.len() && is_digit(chars[right + 1]);
        if !(prev_ok && next_ok) {
            return false;
        }
    }
    true
}

/// `Ok(())` if the lexer's raw decimal-int text is a well-formed F# `Int32`
/// literal — both the shape rule (`digit ((digit|sep)* digit)?`; the lexer
/// over-accepts `[0-9][0-9_]*`) and the value rule (fits in `i32`; FCS
/// reports `lexOutsideThirtyTwoBitSigned` for out-of-range literals such as
/// `2147483649`). F# explicitly accepts repeated separators between digits,
/// so `1__2` is well-formed, but `_1`/`1_`/`1__` are not.
///
/// A folded `+`/`-` sign (see `sign_fold`) may prefix the text. The magnitude
/// is what's range-checked; a `-` additionally rescues the exact cleaned
/// string `2147483648` (`i32::MIN`), mirroring FCS's `isInt32BadMax`. The
/// rescue is spelling-sensitive — `-02147483648` still overflows — and `+`
/// never rescues.
pub(super) fn validate_decimal_int(text: &str) -> Result<(), DecimalIntError> {
    let (minus, body) = split_fold_sign(text);
    if !separators_well_placed(body, |c| c.is_ascii_digit()) {
        return Err(DecimalIntError::Malformed);
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    match cleaned.parse::<u64>() {
        // Magnitude fits the positive `i32` half — `int32` succeeds for any sign.
        Ok(v) if v <= i32::MAX as u64 => Ok(()),
        // `MaxValue + 1` is `i32::MIN`, but only when folded under `-` and
        // spelled exactly (`isInt32BadMax` compares the cleaned text).
        _ if minus && cleaned == INT32_BAD_MAX => Ok(()),
        // Out of range, or `u64::parse` overflowed for a ≥20-digit magnitude.
        _ => Err(DecimalIntError::OutOfRangeInt32),
    }
}

#[derive(Debug)]
pub(super) enum DecimalIntError {
    Malformed,
    OutOfRangeInt32,
}

/// `Ok(())` if `text` (a `Token::XInt`, e.g. `0x10`, `0o755`, `0b1010`)
/// has a body that fits in `u32`. FCS's `int32` parser (called from the
/// `xint` arm of `lex.fsl`:411) reinterprets the u32 as signed 32-bit
/// two's complement, so `0x80000000` is valid and decodes to `i32::MIN`.
/// Only literals whose magnitude exceeds `u32::MAX` (`0x1_0000_0000` and
/// larger) error. Separator placement (no leading/trailing `_`) is
/// validated against the radix's digit set — `0x_10` and `0x10_` are
/// rejected by FCS but our lexer regex over-accepts them.
pub(super) fn validate_xint_int32(text: &str) -> Result<(), ()> {
    // A folded sign may precede the `0x`/`0o`/`0b` prefix (`-0xFF`, see
    // `sign_fold`); strip it before splitting. The range rule is unchanged:
    // any `u32`-fitting bit pattern is a valid `int32`, and negating an
    // in-range `i32` stays in range (`-0xFFFFFFFF` ⇒ `1`), so no extra bound.
    let (_, body) = split_fold_sign(text);
    let (radix, body) = xint_split(body);
    if !separators_well_placed(body, |c| c.is_digit(radix)) {
        return Err(());
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    match u32::from_str_radix(&cleaned, radix) {
        Ok(_) => Ok(()),
        Err(_) => Err(()),
    }
}

/// `true` if `text` (a `Token::Float32`, e.g. `1.0f`, `42F`, `1.5e-3f`)
/// parses as `f32` after FCS's `evalFloat` prep — strip the trailing
/// `f`/`F` and remove underscores. The lexer's regex over-accepts shapes
/// the parser still rejects (e.g. `1_.5f` once `_.` is admitted as a
/// separator), so revalidate before emitting the kind. Matches FCS
/// `lex.fsl`:212-217.
pub(super) fn float32_body_parses(text: &str) -> bool {
    if text.len() < 2 {
        return false;
    }
    let body = &text[..text.len() - 1];
    if !separators_well_placed(body, |c| c.is_ascii_digit()) {
        return false;
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f32>().is_ok()
}

/// `Ok(())` if `text` (a `Token::XIEEE32`, e.g. `0x40490fdblf`, possibly
/// carrying a sign folded by `sign_fold`) has a body that fits in `u32`.
/// Mirrors FCS's `lex.fsl`:498-504 — strip `lf`, remove underscores, parse as
/// int64, range-check `0..=0xFFFFFFFF`.
pub(super) fn validate_xieee32(text: &str) -> Result<(), ()> {
    let (_, text) = split_fold_sign(text);
    let body = text
        .strip_suffix("lf")
        .expect("XIEEE32 token must end with `lf`");
    let (radix, digits) = xint_split(body);
    if !separators_well_placed(digits, |c| c.is_digit(radix)) {
        return Err(());
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    let value = u64::from_str_radix(&cleaned, radix).map_err(|_| ())?;
    if value <= u64::from(u32::MAX) {
        Ok(())
    } else {
        Err(())
    }
}

/// `Ok(())` if `text` (a `Token::XIEEE64`, e.g. `0x4024000000000000LF`,
/// possibly carrying a sign folded by `sign_fold`) has a body that fits in
/// `u64`. Mirrors FCS's `lex.fsl`:506-509 which parses the body as `int64` and
/// bit-casts via `BitConverter.Int64BitsToDouble`; any 64-bit value is a valid
/// double bit pattern, so range-check is just "magnitude ≤ u64::MAX".
pub(super) fn validate_xieee64(text: &str) -> Result<(), ()> {
    let (_, text) = split_fold_sign(text);
    let body = text
        .strip_suffix("LF")
        .expect("XIEEE64 token must end with `LF`");
    let (radix, digits) = xint_split(body);
    if !separators_well_placed(digits, |c| c.is_digit(radix)) {
        return Err(());
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    match u64::from_str_radix(&cleaned, radix) {
        Ok(_) => Ok(()),
        Err(_) => Err(()),
    }
}

/// Split a hex/oct/bin literal into `(radix, digits-without-prefix)`.
/// The lexer guarantees the leading `0x`/`0o`/`0b` (case-insensitive on
/// the letter), so panicking on a missing prefix means the parser was
/// handed an `XInt` it shouldn't have been.
fn xint_split(text: &str) -> (u32, &str) {
    let bytes = text.as_bytes();
    debug_assert!(bytes.len() >= 2 && bytes[0] == b'0');
    match bytes[1] {
        b'x' | b'X' => (16, &text[2..]),
        b'o' | b'O' => (8, &text[2..]),
        b'b' | b'B' => (2, &text[2..]),
        other => panic!("XInt token without 0x/0o/0b prefix: leading byte {other:?}"),
    }
}

/// Strip the longest-matching alpha suffix off `text` and return the
/// matching `*_LIT` syntax kind plus the digit body. Longest-first:
/// `uy` beats `y`, `us` beats `s`, `ul` beats `u`/`l`, `uL`/`UL` beat
/// `L`, `un` beats `u`/`n`. The L's case carries the width (lowercase
/// `l`/`ul` = 32-bit, uppercase `L`/`uL`/`UL` = 64-bit) — see FCS
/// `lex.fsl`:261-273.
///
/// Shared between `Token::IntSuffixed` (decimal body) and
/// `Token::XIntSuffixed` (hex/oct/bin body); each caller pairs the
/// suffix with its own body decoder + range check.
fn split_int_suffix(text: &str) -> Option<(SyntaxKind, &str)> {
    if let Some(b) = text.strip_suffix("uy") {
        Some((SyntaxKind::BYTE_LIT, b))
    } else if let Some(b) = text.strip_suffix("us") {
        Some((SyntaxKind::UINT16_LIT, b))
    } else if let Some(b) = text.strip_suffix("ul") {
        Some((SyntaxKind::UINT32_LIT, b))
    } else if let Some(b) = text.strip_suffix("uL") {
        Some((SyntaxKind::UINT64_LIT, b))
    } else if let Some(b) = text.strip_suffix("UL") {
        Some((SyntaxKind::UINT64_LIT, b))
    } else if let Some(b) = text.strip_suffix("un") {
        Some((SyntaxKind::UINTPTR_LIT, b))
    } else if let Some(b) = text.strip_suffix('y') {
        Some((SyntaxKind::SBYTE_LIT, b))
    } else if let Some(b) = text.strip_suffix('s') {
        Some((SyntaxKind::INT16_LIT, b))
    } else if let Some(b) = text.strip_suffix('l') {
        Some((SyntaxKind::INT32_LIT, b))
    } else if let Some(b) = text.strip_suffix('u') {
        Some((SyntaxKind::UINT32_LIT, b))
    } else if let Some(b) = text.strip_suffix('L') {
        Some((SyntaxKind::INT64_LIT, b))
    } else {
        text.strip_suffix('n').map(|b| (SyntaxKind::INTPTR_LIT, b))
    }
}

/// `true` if `value` fits the typed width that `kind` denotes. Signed
/// kinds widen their range to the matching *unsigned* maximum when the
/// body came from a hex/oct/bin literal (`hex_bit_pattern = true`),
/// because FCS reinterprets non-decimal bodies as two's-complement bit
/// patterns — `0xFFFFFFFFl` is valid `int32` (`= -1`) but `4294967295l`
/// would be `lexOutsideThirtyTwoBitSigned`. See FCS `lex.fsl`:373-422
/// (each `int*`/`xint*` rule chooses its own range check).
fn int_value_in_range(kind: SyntaxKind, value: u64, hex_bit_pattern: bool) -> bool {
    match kind {
        SyntaxKind::SBYTE_LIT if hex_bit_pattern => value <= u64::from(u8::MAX),
        SyntaxKind::SBYTE_LIT => value <= i8::MAX as u64,
        SyntaxKind::BYTE_LIT => value <= u64::from(u8::MAX),
        SyntaxKind::INT16_LIT if hex_bit_pattern => value <= u64::from(u16::MAX),
        SyntaxKind::INT16_LIT => value <= i16::MAX as u64,
        SyntaxKind::UINT16_LIT => value <= u64::from(u16::MAX),
        SyntaxKind::INT32_LIT if hex_bit_pattern => value <= u64::from(u32::MAX),
        SyntaxKind::INT32_LIT => value <= i32::MAX as u64,
        SyntaxKind::UINT32_LIT => value <= u64::from(u32::MAX),
        SyntaxKind::INT64_LIT if hex_bit_pattern => true,
        SyntaxKind::INT64_LIT => value <= i64::MAX as u64,
        SyntaxKind::UINT64_LIT => true,
        SyntaxKind::INTPTR_LIT if hex_bit_pattern => true,
        SyntaxKind::INTPTR_LIT => value <= i64::MAX as u64,
        SyntaxKind::UINTPTR_LIT => true,
        _ => unreachable!("int_value_in_range called with non-int kind {kind:?}"),
    }
}

/// FCS string form of `Int32.MaxValue + 1` = `string(1UL <<< 31)`.
const INT32_BAD_MAX: &str = "2147483648";
/// FCS string form of `Int64.MaxValue + 1` = `string(1UL <<< 63)`.
const INT64_BAD_MAX: &str = "9223372036854775808";

/// `true` if a `-`-folded *decimal* signed literal with cleaned magnitude
/// `cleaned` (value `value`, no underscores) is the type's `MinValue` — i.e.
/// the magnitude `|MinValue|` that FCS's lexer accepts only under a minus
/// sign (`isInt*BadMax` + the `plus && bad` clearing, `LexFilter.fs:2737`).
///
/// FCS is **spelling-sensitive in different ways per width**, which this
/// mirrors exactly:
/// * int8/int16 compare the parsed int32 *value* (`isInt8BadMax`/
///   `isInt16BadMax`, `lex.fsl:26-29`), so leading-zero spellings like
///   `-0128y` are rescued too;
/// * int32/int64/nativeint compare the cleaned *text* (`isInt32BadMax`/
///   `isInt64BadMax`, `lex.fsl:32-35` — nativeint routes through
///   `isInt64BadMax`), so only the exact spelling is rescued and
///   `-02147483648l` still overflows.
///
/// Only ever consulted for the decimal (non-hex-bit-pattern) path; the hex
/// path admits the full unsigned width already, so no rescue is needed there.
fn is_signed_min_value(kind: SyntaxKind, cleaned: &str, value: u64) -> bool {
    match kind {
        SyntaxKind::SBYTE_LIT => value == 1 << 7,
        SyntaxKind::INT16_LIT => value == 1 << 15,
        SyntaxKind::INT32_LIT => cleaned == INT32_BAD_MAX,
        SyntaxKind::INT64_LIT | SyntaxKind::INTPTR_LIT => cleaned == INT64_BAD_MAX,
        _ => false,
    }
}

/// Split an optional leading `+`/`-` (a folded sign — see `sign_fold`) off a
/// numeric literal's text, returning `(is_minus, rest)`. A bare literal with
/// no sign yields `(false, text)`.
pub(super) fn split_fold_sign(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        (false, rest)
    } else {
        (false, text)
    }
}

/// Classify a `Token::IntSuffixed` source string (e.g. `127y`, `255uy`,
/// `42u`, `9999L`, `1n`) into the matching `*_LIT` syntax kind. Returns
/// `UnsupportedSuffix` for suffix bytes that don't match the F# table;
/// returns `OutOfRange` if the decimal magnitude doesn't fit the typed
/// width.
pub(super) fn classify_int_suffixed(text: &str) -> Result<SyntaxKind, IntSuffixedError> {
    // A folded `-`/`+` sign may prefix the digits (see `sign_fold`); the
    // magnitude body excludes it, and a `-` additionally rescues the type's
    // `MinValue` magnitude (`isInt*BadMax`).
    let (minus, body) = split_fold_sign(text);
    let (kind, body) = split_int_suffix(body).ok_or(IntSuffixedError::UnsupportedSuffix)?;
    // The magnitude is non-negative; a `u64` body is enough for every typed
    // width — including `UInt64`'s `u64::MAX`. Numbers above that overflow
    // u64 parsing and surface as `OutOfRange`.
    if !separators_well_placed(body, |c| c.is_ascii_digit()) {
        return Err(IntSuffixedError::OutOfRange);
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    let value: u64 = cleaned.parse().map_err(|_| IntSuffixedError::OutOfRange)?;
    if int_value_in_range(kind, value, false)
        || (minus && is_signed_min_value(kind, &cleaned, value))
    {
        Ok(kind)
    } else {
        Err(IntSuffixedError::OutOfRange)
    }
}

/// Classify a `Token::XIntSuffixed` source string (e.g. `0xFFul`,
/// `0b1010uy`, `0o755L`). Strips the trailing alpha suffix via
/// [`split_int_suffix`], then parses the remaining `0x`/`0o`/`0b` body
/// per [`xint_split`] and range-checks the value via [`int_value_in_range`]
/// with the bit-pattern widening rules.
pub(super) fn classify_xint_suffixed(text: &str) -> Result<SyntaxKind, IntSuffixedError> {
    // A folded `-`/`+` sign may prefix the body (see `sign_fold`); strip it.
    // No `MinValue` rescue is needed here: the hex/oct/bin bit-pattern range
    // already admits the full unsigned width (`0x80000000l` = `i32::MIN`), and
    // negating an in-range value stays in range, so the sign can't widen it.
    let (_, body) = split_fold_sign(text);
    let (kind, body) = split_int_suffix(body).ok_or(IntSuffixedError::UnsupportedSuffix)?;
    let (radix, digits) = xint_split(body);
    if !separators_well_placed(digits, |c| c.is_digit(radix)) {
        return Err(IntSuffixedError::OutOfRange);
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    let value = u64::from_str_radix(&cleaned, radix).map_err(|_| IntSuffixedError::OutOfRange)?;
    if int_value_in_range(kind, value, true) {
        Ok(kind)
    } else {
        Err(IntSuffixedError::OutOfRange)
    }
}

/// Classify a suffixed integer literal (`Token::IntSuffixed` or
/// `Token::XIntSuffixed`), choosing the decimal vs hex/oct/bin decoder by
/// inspecting the body after any folded `+`/`-` sign. Folds the dispatch the
/// caller used to do inline so the `0x`/`0o`/`0b` probe sees the prefix even
/// when a sign precedes it (`-0xFFL`).
pub(super) fn classify_suffixed_int(text: &str) -> Result<SyntaxKind, IntSuffixedError> {
    let (_, body) = split_fold_sign(text);
    let is_xint = body.starts_with("0x")
        || body.starts_with("0X")
        || body.starts_with("0o")
        || body.starts_with("0O")
        || body.starts_with("0b")
        || body.starts_with("0B");
    if is_xint {
        classify_xint_suffixed(text)
    } else {
        classify_int_suffixed(text)
    }
}

#[derive(Debug)]
pub(super) enum IntSuffixedError {
    /// The suffix is well-formed (matches the lexer regex) but Phase 2 hasn't
    /// landed the matching syntax kind / typed normaliser yet. Per the plan
    /// commits 6/7 add the remaining widths.
    UnsupportedSuffix,
    /// The decimal magnitude doesn't fit the suffix's type — same shape as
    /// FCS reporting an overflow at parse time.
    OutOfRange,
}
