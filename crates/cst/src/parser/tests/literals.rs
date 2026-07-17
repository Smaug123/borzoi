use super::super::*;
use super::*;

#[test]
fn lone_integer_literal() {
    let source = "42\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..3
  MODULE_OR_NAMESPACE@0..3
    EXPR_DECL@0..2
      CONST_EXPR@0..2
        INT32_LIT@0..2 \"42\"
    NEWLINE@2..3 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `true` and `false` are `SynConst.Bool` literals — Phase 2's smallest
/// leaf-only extension over Phase 1's `INT32_LIT`. Pins the shape:
/// `EXPR_DECL > CONST_EXPR > BOOL_LIT` with the keyword text preserved
/// on the token.
#[test]
fn lone_true_literal() {
    let source = "true\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        BOOL_LIT@0..4 \"true\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

#[test]
fn lone_false_literal() {
    let source = "false\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      CONST_EXPR@0..5
        BOOL_LIT@0..5 \"false\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `()` is the unit literal `SynConst.Unit`. Shape:
/// `EXPR_DECL > CONST_EXPR > [LPAREN_TOK, RPAREN_TOK]`. Multi-token
/// const form — `ConstExpr::literal` returns the `LPAREN_TOK` rather
/// than a single literal token.
#[test]
fn lone_unit_literal() {
    let source = "()\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..3
  MODULE_OR_NAMESPACE@0..3
    EXPR_DECL@0..2
      CONST_EXPR@0..2
        LPAREN_TOK@0..1 \"(\"
        RPAREN_TOK@1..2 \")\"
    NEWLINE@2..3 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `( )` (whitespace between the parens) is still `SynConst.Unit` —
/// the whitespace is trivia and lands under `CONST_EXPR` (it sits
/// between the parens semantically, not before or after the decl).
/// Paren *expressions* (`( e )` with a non-trivia interior) are
/// Phase 3 and would not match `peek_is_expr_start`.
#[test]
fn unit_literal_with_internal_whitespace() {
    let source = "( )\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      CONST_EXPR@0..3
        LPAREN_TOK@0..1 \"(\"
        WHITESPACE@1..2 \" \"
        RPAREN_TOK@2..3 \")\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `127y` is `SynConst.SByte 127`. Shape:
/// `EXPR_DECL > CONST_EXPR > SBYTE_LIT`. Pins the small-suffix
/// classifier's `y` arm.
#[test]
fn lone_sbyte_literal() {
    let source = "127y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        SBYTE_LIT@0..4 \"127y\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `255uy` is `SynConst.Byte 255` — the longest-suffix `uy` must win
/// over `y` (the classifier strips suffixes longest-first).
#[test]
fn lone_byte_literal() {
    let source = "255uy\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      CONST_EXPR@0..5
        BYTE_LIT@0..5 \"255uy\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `32767s` is `SynConst.Int16`. Boundary value (`i16::MAX`).
#[test]
fn lone_int16_literal() {
    let source = "32767s\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..7
  MODULE_OR_NAMESPACE@0..7
    EXPR_DECL@0..6
      CONST_EXPR@0..6
        INT16_LIT@0..6 \"32767s\"
    NEWLINE@6..7 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `65535us` is `SynConst.UInt16`. Boundary value (`u16::MAX`).
/// `us` must win over `s` — same longest-suffix rule as `uy`/`y`.
#[test]
fn lone_uint16_literal() {
    let source = "65535us\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      CONST_EXPR@0..7
        UINT16_LIT@0..7 \"65535us\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `128y` overflows `i8` (the max positive SByte is `127`). The lexer
/// accepts the shape but the classifier rejects the value with the
/// same shape FCS uses for `lexOutsideEightBitSigned`.
#[test]
fn sbyte_overflow_is_error() {
    let source = "128y\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("outside its type's range")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `256uy` overflows `u8` (max is `255`). Same range-check shape as
/// `sbyte_overflow_is_error`.
#[test]
fn byte_overflow_is_error() {
    let source = "256uy\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("outside its type's range")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `1n` is `SynConst.IntPtr 1` — the signed native-int suffix.
/// Shape: `EXPR_DECL > CONST_EXPR > INTPTR_LIT "1n"`.
#[test]
fn lone_intptr_literal() {
    let source = "1n\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_intptr = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::INTPTR_LIT));
    assert!(any_intptr, "expected an INTPTR_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `1un` is `SynConst.UIntPtr 1` — the unsigned native-int suffix
/// (`un` beats `u` then `n` in the classifier's longest-first walk).
#[test]
fn lone_uintptr_literal() {
    let source = "1un\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_uintptr = parse.root.descendants_with_tokens().any(
        |el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::UINTPTR_LIT),
    );
    assert!(any_uintptr, "expected a UINTPTR_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `0x10` is `SynConst.Int32 0x10` — bare hex routes to INT32_LIT
/// (no separate XINT kind: FCS treats hex/oct/bin literals as int32
/// with two's-complement reinterpretation).
#[test]
fn lone_hex_int_literal() {
    let source = "0x10\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        INT32_LIT@0..4 \"0x10\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `0o17`/`0b101` are `SynConst.Int32` via the same INT32_LIT routing
/// as hex; pins the prefix discrimination in [`xint_split`].
#[test]
fn lone_oct_and_bin_int_literals() {
    for source in ["0o17\n", "0b101\n"] {
        let parse = parse(source);
        assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
        assert_lossless(source, &parse);
        let any_int32 = parse.root.descendants_with_tokens().any(
            |el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::INT32_LIT),
        );
        assert!(any_int32, "expected INT32_LIT for {source:?}");
    }
}

/// `0x80000000` fits `u32` but not `i32`. FCS treats this as
/// `SynConst.Int32 -2147483648` via 32-bit two's complement reinterp;
/// our parser emits INT32_LIT without an error.
#[test]
fn xint_at_u32_boundary_is_valid_int32() {
    let source = "0x80000000\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
}

/// `0x100000000` overflows `u32` (33 bits). FCS reports the same
/// "outside 32-bit signed" diagnostic decimal overflow gets.
#[test]
fn xint_overflowing_u32_is_error() {
    let source = "0x100000000\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("outside 32-bit signed range")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `0xFFuy` is `SynConst.Byte 255` — suffixed hex; the classifier
/// for `XIntSuffixed` reuses the same suffix table as `IntSuffixed`,
/// but parses the body in base 16.
#[test]
fn lone_hex_suffixed_byte_literal() {
    let source = "0xFFuy\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_byte = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::BYTE_LIT));
    assert!(any_byte, "expected a BYTE_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `0b1010UL` is `SynConst.UInt64 10` — binary body with `UL`
/// suffix; pins multi-char suffix dispatch on a non-decimal base.
#[test]
fn lone_bin_suffixed_uint64_literal() {
    let source = "0b1010UL\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_uint64 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::UINT64_LIT));
    assert!(any_uint64, "expected a UINT64_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `42l` is `SynConst.Int32 42` — the explicitly-suffixed int32 form
/// (a synonym for bare `42`). Shape:
/// `EXPR_DECL > CONST_EXPR > INT32_LIT "42l"`.
#[test]
fn lone_int32_suffixed_literal() {
    let source = "42l\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      CONST_EXPR@0..3
        INT32_LIT@0..3 \"42l\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `42u` is `SynConst.UInt32 42` — bare `u` suffix.
#[test]
fn lone_uint32_u_literal() {
    let source = "42u\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      CONST_EXPR@0..3
        UINT32_LIT@0..3 \"42u\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `42ul` is `SynConst.UInt32 42` — the longer `ul` suffix must beat
/// `l` and `u`.
#[test]
fn lone_uint32_ul_literal() {
    let source = "42ul\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    // Just check the kind via tree text — same shape, different
    // suffix; deep-string-compare on the tree would be repetitive.
    let any_uint32 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::UINT32_LIT));
    assert!(any_uint32, "expected a UINT32_LIT in the tree");
}

/// `42uL` — uppercase `L` switches the width to 64 even with the
/// `u` unsigned marker, so this is `SynConst.UInt64` per FCS
/// `lex.fsl`:273. Pins the case-sensitivity of the L distinction.
#[test]
#[allow(non_snake_case)]
fn lone_uint64_uL_literal() {
    let source = "42uL\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_uint64 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::UINT64_LIT));
    assert!(any_uint64, "expected a UINT64_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `9223372036854775807L` is the maximum `SynConst.Int64` (`i64::MAX`).
/// Pins both the `L` suffix arm and the upper bound; `9223372036854775808L`
/// would error.
#[test]
fn lone_int64_literal_at_max() {
    let source = "9223372036854775807L\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_int64 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::INT64_LIT));
    assert!(any_int64, "expected an INT64_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `18446744073709551615UL` is the maximum `SynConst.UInt64`
/// (`u64::MAX`). Specifically exercises the body-parses-as-u64 path:
/// this value overflows i64, so an i64-bodied classifier would error.
#[test]
fn lone_uint64_literal_at_max() {
    let source = "18446744073709551615UL\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_uint64 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::UINT64_LIT));
    assert!(any_uint64, "expected a UINT64_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `2147483648l` (`i32::MAX + 1` with `l` suffix) overflows the int32
/// width — same shape FCS uses for `lexOutsideThirtyTwoBitSigned`.
#[test]
fn int32_suffixed_overflow_is_error() {
    let source = "2147483648l\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("outside its type's range")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `1.0` is `SynConst.Double 1.0`. Shape:
/// `EXPR_DECL > CONST_EXPR > IEEE64_LIT "1.0"`. Pins the decimal-form
/// (`Token::Float64`) path; the body's actual value is checked via
/// the differential test.
#[test]
fn lone_float64_literal() {
    let source = "1.0\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      CONST_EXPR@0..3
        IEEE64_LIT@0..3 \"1.0\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1e10` exercises the exponent-only Float64 lexer arm (no dot,
/// mandatory exponent). Same syntax kind as `1.0`.
#[test]
fn lone_float64_exponent_literal() {
    let source = "1e10\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_ieee64 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IEEE64_LIT));
    assert!(any_ieee64, "expected an IEEE64_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `0x4024000000000000LF` is the hex bit-pattern form of the double
/// `10.0`. FCS lex.fsl:506-509 parses the body as int64 and bit-casts
/// via `BitConverter.Int64BitsToDouble`; our parser emits IEEE64_LIT
/// and the normaliser does the bit-cast.
#[test]
fn lone_xieee64_literal() {
    let source = "0x4024000000000000LF\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_ieee64 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IEEE64_LIT));
    assert!(any_ieee64, "expected an IEEE64_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `'a'` is `SynConst.Char 'a'`. Shape:
/// `EXPR_DECL > CONST_EXPR > CHAR_LIT`. Pins the plain-char path
/// (no `B` suffix).
#[test]
fn lone_char_literal() {
    let source = "'a'\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      CONST_EXPR@0..3
        CHAR_LIT@0..3 \"'a'\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `'\n'` exercises the escape-form char lexer arm. Same syntax kind
/// as `'a'`; the normaliser does the escape-decoding.
#[test]
fn lone_char_escape_literal() {
    let source = "'\\n'\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_char = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::CHAR_LIT));
    assert!(any_char, "expected a CHAR_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `'a'B` is `SynConst.Byte 97uy` — byte-char form. Routes to
/// `BYTE_LIT` (same kind as `42uy`) since FCS emits `SynConst.Byte`
/// for both forms. The text retains the trailing `B`.
#[test]
fn lone_byte_char_literal() {
    let source = "'a'B\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        BYTE_LIT@0..4 \"'a'B\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

// ---- Byte-char range diagnostics (FS1157) --------------------------------
//
// A byte-char `'…'B` (→ `SynConst.Byte`) whose value leaves the byte range is
// FS1157 (`lexInvalidAsciiByteLiteral`, "This is not a valid byte character
// literal. The value must be less than or equal to '\127'B."). FCS's threshold
// depends on the form (`lex.fsl`:522-585): the decimal trigraph `'\NNN'B`
// errors only when the value exceeds 255 (128..=255 is a *warning*, which we
// don't model — `ParseError` has no severity), while every other form (plain
// char, `\xHH`, `\uHHHH`, `\UHHHHHHHH`) errors when the value exceeds 127.
// Letter escapes (`\n`) and ASCII ≤127 never error. Confirmed against FCS.

/// `'\xFF'B`: a `\x` byte-char value > 127 → FS1157 (`lex.fsl`:565-570). The
/// `BYTE_LIT` node still lands (lossless); we now surface the diagnostic.
#[test]
fn byte_char_hex_above_ascii_is_rejected() {
    assert_lit_errors("'\\xFF'B\n", 1, "valid byte character literal");
}

/// `\u`/`\U` byte-char forms error above 127 too — including a supplementary
/// `\U` (`unicodeGraphLong` → `SurrogatePair`, which the byte arm rejects via
/// `lex.fsl`:577-585).
#[test]
fn byte_char_unicode_escape_above_ascii_is_rejected() {
    assert_lit_errors("'\\u00FF'B\n", 1, "valid byte character literal");
    assert_lit_errors("'\\U000000FF'B\n", 1, "valid byte character literal");
    assert_lit_errors("'\\U0001F600'B\n", 1, "valid byte character literal");
}

/// A plain non-ASCII char (`'é'B`, U+00E9 = 233) is > 127 → FS1157
/// (`lex.fsl`:522-528).
#[test]
fn byte_char_non_ascii_plain_is_rejected() {
    assert_lit_errors("'é'B\n", 1, "valid byte character literal");
}

/// The decimal trigraph errors only above 255 (`lex.fsl`:539-543).
#[test]
fn byte_char_trigraph_above_255_is_rejected() {
    assert_lit_errors("'\\256'B\n", 1, "valid byte character literal");
}

/// ASCII-range byte chars — plain, boundary `\x7F`, and letter escapes — are
/// all accepted.
#[test]
fn byte_char_within_ascii_is_accepted() {
    assert_lit_ok("'a'B\n");
    assert_lit_ok("'\\x7F'B\n");
    assert_lit_ok("'\\n'B\n");
}

/// `'\U0001F600'`: non-BMP code point. FCS rejects a `\U` escape ≥ U+10000
/// in a char literal — `unicodeGraphLong` returns `SurrogatePair`, and the
/// char arm (`lex.fsl`:572-575) `fail`s with FS1159
/// (`lexThisUnicodeOnlyInStringLiterals`). The `CHAR_LIT` node still lands
/// (lossless), but we now surface the matching diagnostic.
#[test]
fn non_bmp_char_literal_is_rejected() {
    let source = "'\\U0001F600'\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("only valid in string literals"),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `'\999'`: decimal trigraph > 255. FCS errors via `lex.fsl`:530-537
/// (`lexInvalidCharLiteral`). Phase 2 only asserts tree shape.
#[test]
fn trigraph_above_255_parses_as_char_lit() {
    let source = "'\\999'\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
}

/// `"\U00110000"`: codepoint outside U+10FFFF. FCS's `unicodeGraphLong`
/// (LexHelpers.fs:253) returns `Invalid`, and `singleQuoteString`
/// (`lex.fsl`:1323-1325) `fail`s with FS1245 (`lexInvalidUnicodeLiteral`).
/// The `STRING_LIT` node still lands (lossless); we now surface the
/// diagnostic.
#[test]
fn string_unicode_long_overflow_is_rejected() {
    let source = "\"\\U00110000\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("not a valid Unicode character escape sequence"),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Byte-char decimal trigraph in 128..=255 is a *warning* in FCS
/// (`lex.fsl`:544-550 emits FS1157 `lexInvalidTrigraphAsciiByteLiteral` with
/// severity Warning), not an error — `SynConst.Byte` is still produced and
/// `hadErrors` stays false. We surface it on `Parse::warnings` (not `errors`),
/// so the error-parity holds while the diagnostic is still reported.
#[test]
fn byte_char_trigraph_above_ascii_warns() {
    assert_lit_warns("'\\200'B\n", 1, "valid byte character literal");
    assert_lit_warns("'\\255'B\n", 1, "valid byte character literal");
}

// ---- Unicode-escape range diagnostics (FS1245 / FS1159) ------------------
//
// FCS decodes `\U........` (eight-hex) escapes via `unicodeGraphLong`
// (`LexHelpers.fs`:253): the 32-bit value `v` of the eight hex digits is a
// single BMP code unit when `v ≤ 0xFFFF`, a surrogate pair when
// `0x10000 ≤ v ≤ 0x10FFFF`, and `Invalid` when `v > 0x10FFFF`. In an
// escape-processing *string* literal an `Invalid` escape is FS1245; in a
// *char* literal anything that isn't a single code unit (`v > 0xFFFF`) is
// FS1159. `\u....` (four-hex) escapes always decode to a `uint16` code unit
// — lone surrogates included — so they never error. The boundary values and
// per-kind behaviour below were all confirmed against FCS.

/// Helper: assert `source` parses to exactly `n` errors whose messages each
/// contain `needle`, and that the tree is lossless.
fn assert_lit_errors(source: &str, n: usize, needle: &str) {
    let parse = parse(source);
    assert_eq!(parse.errors.len(), n, "errors: {:?}", parse.errors);
    for e in &parse.errors {
        assert!(
            e.message.contains(needle),
            "error message {:?} missing {needle:?}",
            e.message,
        );
    }
    assert_lossless(source, &parse);
}

/// Helper: assert `source` parses cleanly — no errors *and* no warnings — and
/// is lossless. FCS accepts these inputs with `hadErrors` false and no
/// `warning(...)`.
fn assert_lit_ok(source: &str) {
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(parse.warnings.is_empty(), "warnings: {:?}", parse.warnings);
    assert_lossless(source, &parse);
}

/// Helper: assert `source` parses to no errors but exactly `n` warnings whose
/// messages each contain `needle`, and is lossless — FCS's `warning(...)`
/// cases that leave `hadErrors` false.
fn assert_lit_warns(source: &str, n: usize, needle: &str) {
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(parse.warnings.len(), n, "warnings: {:?}", parse.warnings);
    for w in &parse.warnings {
        assert!(
            w.message.contains(needle),
            "warning message {:?} missing {needle:?}",
            w.message,
        );
    }
    assert_lossless(source, &parse);
}

/// `\u` always yields a `uint16` code unit, lone surrogates included, so a
/// surrogate escape is accepted by FCS in both string and char position.
#[test]
fn surrogate_short_unicode_escape_is_accepted() {
    assert_lit_ok("let s = \"\\uD800\"\n");
    assert_lit_ok("let s = \"\\uDFFF\"\n");
    assert_lit_ok("let c = '\\uD800'\n");
}

/// A `\U` escape whose value is ≤ U+FFFF decodes to a single code unit
/// (`SingleChar`), so it's accepted in both string and char position —
/// even when that code unit is a lone surrogate (`\U0000D800`).
#[test]
fn long_unicode_escape_within_bmp_is_accepted() {
    assert_lit_ok("let s = \"\\U0000D800\"\n");
    assert_lit_ok("let s = \"\\U0000FFFF\"\n");
    assert_lit_ok("let c = '\\U0000D800'\n");
    assert_lit_ok("let c = '\\U0000FFFF'\n");
}

/// A `\U` escape in `0x10000..=0x10FFFF` is a valid supplementary code point
/// in a *string* (`SurrogatePair` → two code units), so no FS1245.
#[test]
fn long_unicode_escape_supplementary_string_is_accepted() {
    assert_lit_ok("let s = \"\\U00010000\"\n");
    assert_lit_ok("let s = \"\\U0001F600\"\n");
    assert_lit_ok("let s = \"\\U0010FFFF\"\n");
}

/// The same supplementary `\U` escapes are FS1159 in a *char* literal:
/// `unicodeGraphLong` returns `SurrogatePair`, which the char arm rejects.
#[test]
fn long_unicode_escape_supplementary_char_is_rejected() {
    assert_lit_errors(
        "let c = '\\U00010000'\n",
        1,
        "only valid in string literals",
    );
    assert_lit_errors(
        "let c = '\\U0010FFFF'\n",
        1,
        "only valid in string literals",
    );
}

/// A `\U` escape > U+10FFFF is `Invalid`: FS1245 in a string, FS1159 in a
/// char (the char arm folds `Invalid` into the same diagnostic as
/// `SurrogatePair`). The far-out `\UFFFFFFFF` exercises the top of the range.
#[test]
fn long_unicode_escape_out_of_range_is_rejected() {
    assert_lit_errors(
        "let s = \"\\UFFFFFFFF\"\n",
        1,
        "not a valid Unicode character escape sequence",
    );
    assert_lit_errors(
        "let c = '\\U00110000'\n",
        1,
        "only valid in string literals",
    );
    assert_lit_errors(
        "let c = '\\UFFFFFFFF'\n",
        1,
        "only valid in string literals",
    );
}

/// Byte strings (`"..."B`) process `\U` escapes through the same
/// `singleQuoteString` path, so an out-of-range escape is still FS1245.
/// Crucially, an `Invalid` `\U` adds no code unit, so it does *not* also
/// trigger FS1140 — exactly one error.
#[test]
fn byte_string_unicode_long_overflow_is_rejected() {
    assert_lit_errors(
        "let s = \"\\U00110000\"B\n",
        1,
        "not a valid Unicode character escape sequence",
    );
}

// ---- Byte-string wide-unit diagnostics (FS1140) --------------------------
//
// A byte string (`"…"B`, `@"…"B`, `"""…"""B`) whose decoded content has any
// UTF-16 code unit > 255 is FS1140 (`lexByteArrayCannotEncode`, "This byte
// array literal contains N characters that do not encode as a single byte"),
// one error per literal, count `N` = number of such units (a surrogate pair
// counts as two). A literal char with scalar > 0xFF qualifies; in the regular
// form a `\u`/`\U` escape in 0x100..=0x10FFFF does too (`\U` > U+10FFFF stays
// FS1245-only — it emits no unit). `\xHH` (≤255), trigraphs (byte-valued), and
// U+00FF=255 itself are *warnings* (FS1253), which we don't model. The interp
// byte form (`$"…"B`) is FS3377-only — no FS1140. Confirmed against FCS.

/// Regular byte strings: a literal wide char or a wide `\u`/`\U` escape →
/// FS1140; a valid supplementary `\U` (surrogate pair) and a lone-surrogate
/// `\u` qualify too.
#[test]
fn byte_string_wide_unit_is_rejected() {
    assert_lit_errors("let s = \"Ā\"B\n", 1, "do not encode as a single byte");
    assert_lit_errors(
        "let s = \"\\u0100\"B\n",
        1,
        "do not encode as a single byte",
    );
    assert_lit_errors(
        "let s = \"\\U0001F600\"B\n",
        1,
        "do not encode as a single byte",
    );
    assert_lit_errors(
        "let s = \"\\uD800\"B\n",
        1,
        "do not encode as a single byte",
    );
}

/// Verbatim and triple byte strings don't process escapes, but a literal wide
/// char still overflows the byte buffer → FS1140.
#[test]
fn verbatim_and_triple_byte_string_wide_unit_is_rejected() {
    assert_lit_errors("let s = @\"Ā\"B\n", 1, "do not encode as a single byte");
    assert_lit_errors(
        "let s = \"\"\"Ā\"\"\"B\n",
        1,
        "do not encode as a single byte",
    );
}

/// In-range byte content is accepted: ASCII, a `\xFF`/`ÿ` (=255, a
/// warning we don't model), and the verbatim/triple ASCII forms.
#[test]
fn byte_string_within_byte_range_is_accepted() {
    assert_lit_ok("let s = \"abc\"B\n");
    assert_lit_ok("let s = \"\\xFF\"B\n");
    assert_lit_ok("let s = \"ÿ\"B\n");
    assert_lit_ok("let s = @\"abc\"B\n");
    assert_lit_ok("let s = \"\"\"abc\"\"\"B\n");
}

/// A literal `Ā` in a verbatim byte string is literal text (no escape
/// decoding), all ASCII — so no FS1140.
#[test]
fn verbatim_byte_string_does_not_decode_escapes() {
    assert_lit_ok("let s = @\"\\u0100\"B\n");
}

/// The FS1140 message echoes the count: one BMP wide char → 1, two → 2, and a
/// supplementary `\U` (surrogate pair) → 2.
#[test]
fn byte_string_wide_unit_count_in_message() {
    let count_msg = |source: &str| -> String {
        let parse = parse(source);
        parse
            .errors
            .iter()
            .find(|e| e.message.contains("do not encode as a single byte"))
            .unwrap_or_else(|| panic!("expected FS1140 for {source:?}: {:?}", parse.errors))
            .message
            .clone()
    };
    assert!(count_msg("let s = \"Ā\"B\n").contains("contains 1 characters"));
    assert!(count_msg("let s = \"AĀBĀ\"B\n").contains("contains 2 characters"));
    assert!(count_msg("let s = \"\\U0001F600\"B\n").contains("contains 2 characters"));
}

/// FCS emits one FS1245 per offending escape, so two bad escapes → two
/// errors.
#[test]
fn multiple_out_of_range_escapes_each_reported() {
    assert_lit_errors(
        "let s = \"\\U00110000\\U00110000\"\n",
        2,
        "not a valid Unicode character escape sequence",
    );
}

/// Verbatim (`@"..."`) and triple-quoted (`"""..."""`) strings don't honour
/// backslash escapes at all, so `\U00110000` is literal text — no FS1245.
#[test]
fn non_escape_string_kinds_do_not_flag_long_unicode() {
    assert_lit_ok("let s = @\"\\U00110000\"\n");
    assert_lit_ok("let s = \"\"\"\\U00110000\"\"\"\n");
}

/// An escaped backslash (`\\`) consumes both backslashes, so the following
/// `U00110000` is literal text — not a `\U` escape. We must not false-fire.
#[test]
fn escaped_backslash_before_u_is_not_an_escape() {
    assert_lit_ok("let s = \"\\\\U00110000\"\n");
}

/// The diagnostics fire in pattern position too — char and string literals
/// reach the same shared `parse_const_payload`.
#[test]
fn unicode_escape_diagnostics_fire_in_patterns() {
    assert_lit_errors(
        "let f x = match x with | '\\U00110000' -> 1 | _ -> 2\n",
        1,
        "only valid in string literals",
    );
    assert_lit_errors(
        "let f x = match x with | \"\\U00110000\" -> 1 | _ -> 2\n",
        1,
        "not a valid Unicode character escape sequence",
    );
}

/// `"\U0000000Z"` — the 8-byte body after `\U` isn't all hex, so
/// FCS's `unicodeGraphLong` regex doesn't match. The escape falls
/// through to the unrecognised-`\<char>` rule and the bytes are
/// stored literally (`\U0000000Z`). We must not flag the overflow
/// check on a body that isn't hex.
#[test]
fn string_unicode_long_non_hex_body_is_accepted() {
    assert_lit_ok("let s = \"\\U0000000Z\"\n");
}

/// Regular `"..."` string: `SynConst.String("hello",
/// SynStringKind.Regular, _)`. Shape:
/// `EXPR_DECL > CONST_EXPR > STRING_LIT`. The lexer's token text
/// keeps the surrounding double quotes (no decoding at this layer).
#[test]
fn lone_string_literal() {
    let source = "\"hello\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      CONST_EXPR@0..7
        STRING_LIT@0..7 \"\\\"hello\\\"\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `"a\nb"` exercises an escape inside a regular string. Same syntax
/// kind as the no-escape form; the normaliser does the
/// escape-decoding when comparing to FCS.
#[test]
fn lone_string_with_escape_literal() {
    let source = "\"a\\nb\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_string = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::STRING_LIT));
    assert!(any_string, "expected a STRING_LIT in the tree");
    assert_lossless(source, &parse);
}

/// Regular byte string `"abc"B` — `SynConst.Bytes([0x61; 0x62; 0x63],
/// SynByteStringKind.Regular, _)`. The lexer's `Token::String` regex
/// consumes the trailing `B`; the parser dispatches on it to pick
/// `BYTE_STRING_LIT` over `STRING_LIT`.
#[test]
fn lone_byte_string_literal() {
    let source = "\"abc\"B\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..7
  MODULE_OR_NAMESPACE@0..7
    EXPR_DECL@0..6
      CONST_EXPR@0..6
        BYTE_STRING_LIT@0..6 \"\\\"abc\\\"B\"
    NEWLINE@6..7 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Verbatim byte string `@"abc"B` — `SynConst.Bytes(_,
/// SynByteStringKind.Verbatim, _)`. Same dispatch but on the
/// `VerbatimString` token.
#[test]
fn lone_verbatim_byte_string_literal() {
    let source = "@\"abc\"B\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      CONST_EXPR@0..7
        VERBATIM_BYTE_STRING_LIT@0..7 \"@\\\"abc\\\"B\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Triple-quoted byte string `"""abc"""B`. FCS classifies these as
/// `SynByteStringKind.Regular` (no triple variant exists in
/// `SyntaxTree.fs:132-135`); the parser still uses a distinct
/// `TRIPLE_BYTE_STRING_LIT` token kind so the normaliser knows which
/// decoder to run on the source text.
#[test]
fn lone_triple_byte_string_literal() {
    let source = "\"\"\"abc\"\"\"B\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..11
  MODULE_OR_NAMESPACE@0..11
    EXPR_DECL@0..10
      CONST_EXPR@0..10
        TRIPLE_BYTE_STRING_LIT@0..10 \"\\\"\\\"\\\"abc\\\"\\\"\\\"B\"
    NEWLINE@10..11 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `@"path"` is a verbatim string — `SynConst.String("path",
/// SynStringKind.Verbatim, _)`. Shape:
/// `EXPR_DECL > CONST_EXPR > VERBATIM_STRING_LIT`.
#[test]
fn lone_verbatim_string_literal() {
    let source = "@\"path\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      CONST_EXPR@0..7
        VERBATIM_STRING_LIT@0..7 \"@\\\"path\\\"\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `@"a""b"` exercises the only in-string escape `""` → `"`. Same
/// kind as the no-escape form; the normaliser collapses the double
/// quotes.
#[test]
fn verbatim_string_with_escaped_quote() {
    let source = "@\"a\"\"b\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_string = parse.root.descendants_with_tokens().any(|el| {
            matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::VERBATIM_STRING_LIT)
        });
    assert!(any_string, "expected a VERBATIM_STRING_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `"""abc"""` is a triple-quoted string — `SynConst.String("abc",
/// SynStringKind.TripleQuote, _)`. Shape:
/// `EXPR_DECL > CONST_EXPR > TRIPLE_STRING_LIT`.
#[test]
fn lone_triple_string_literal() {
    let source = "\"\"\"abc\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..10
  MODULE_OR_NAMESPACE@0..10
    EXPR_DECL@0..9
      CONST_EXPR@0..9
        TRIPLE_STRING_LIT@0..9 \"\\\"\\\"\\\"abc\\\"\\\"\\\"\"
    NEWLINE@9..10 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1.0m` is `SynConst.Decimal 1.0M`. Shape:
/// `EXPR_DECL > CONST_EXPR > DECIMAL_LIT "1.0m"`.
#[test]
fn lone_decimal_literal() {
    let source = "1.0m\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        DECIMAL_LIT@0..4 \"1.0m\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1m` exercises the integer-only mantissa path (no `.`, no exponent).
#[test]
fn lone_decimal_integer_literal() {
    let source = "1m\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_decimal = parse.root.descendants_with_tokens().any(
        |el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::DECIMAL_LIT),
    );
    assert!(any_decimal, "expected a DECIMAL_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `1e10m` exercises the exponent-only mantissa path (no `.`).
#[test]
fn lone_decimal_exponent_literal() {
    let source = "1e10m\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_decimal = parse.root.descendants_with_tokens().any(
        |el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::DECIMAL_LIT),
    );
    assert!(any_decimal, "expected a DECIMAL_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `1e29m` is `10^29`, just above `System.Decimal.MaxValue`
/// (`2^96 - 1 ≈ 7.92e28`). FCS errors via FS1154 (`lexOutsideDecimal`).
/// Phase 2 only asserts tree shape — `DECIMAL_LIT` still lands.
#[test]
fn decimal_value_overflow_parses_as_decimal_lit() {
    let source = "1e29m\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
}

/// 29-digit unsigned integer above `2^96 - 1`. FCS errors via FS1154.
/// Phase 2 only asserts tree shape.
#[test]
fn decimal_coefficient_overflow_parses_as_decimal_lit() {
    let source = "79228162514264337593543950336m\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
}

/// 29 fractional digits is one beyond System.Decimal's nominal
/// scale limit of 28, but `System.Decimal.Parse` *rounds* over-precision
/// fractions rather than rejecting them — FCS therefore accepts this
/// (`SynConst.Decimal 0.1234567890123456789012345679`, rounded). We
/// match the runtime: truncate to 28 frac digits and accept.
#[test]
fn decimal_over_precision_fractional_is_accepted() {
    let source = "0.12345678901234567890123456789m\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
}

/// Just under the System.Decimal max — must NOT error. Pins that
/// the bound is inclusive of `2^96 - 1`.
#[test]
fn decimal_at_max_coefficient_is_valid() {
    let source = "79228162514264337593543950335m\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
}

/// `123I` is `SynConst.UserNum("123", "I")`. Shape:
/// `EXPR_DECL > CONST_EXPR > USER_NUM_LIT "123I"`.
#[test]
fn lone_user_num_literal() {
    let source = "123I\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        USER_NUM_LIT@0..4 \"123I\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1_000G` exercises underscore-stripping in the value field.
#[test]
fn lone_user_num_with_underscore_literal() {
    let source = "1_000G\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_user_num = parse.root.descendants_with_tokens().any(
        |el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::USER_NUM_LIT),
    );
    assert!(any_user_num, "expected a USER_NUM_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `__SOURCE_DIRECTORY__` lexes as `Token::KeywordString` and FCS
/// surfaces it through the `sourceIdentifier` → `constant` chain
/// (`pars.fsy:3475-3477`) as `SynConst.SourceIdentifier`. The token
/// must route through `parse_const_expr` so the RHS of
/// `let dir = __SOURCE_DIRECTORY__` parses. Shape:
/// `BINDING > … > CONST_EXPR > SOURCE_IDENTIFIER_LIT`.
#[test]
fn keyword_string_in_expr_position() {
    let source = "let dir = __SOURCE_DIRECTORY__\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_source_ident = parse.root.descendants_with_tokens().any(|el| {
            matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::SOURCE_IDENTIFIER_LIT)
        });
    assert!(
        any_source_ident,
        "expected a SOURCE_IDENTIFIER_LIT in the tree"
    );
    assert_lossless(source, &parse);
}

/// Bare `__LINE__` as the only expression in the file — pins the
/// short shape `EXPR_DECL > CONST_EXPR > SOURCE_IDENTIFIER_LIT` and
/// guards against the leading-token gate (`raw_starts_atomic_expr`)
/// regressing for the other keyword-string spellings.
#[test]
fn lone_keyword_string_literal() {
    let source = "__LINE__\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..9
  MODULE_OR_NAMESPACE@0..9
    EXPR_DECL@0..8
      CONST_EXPR@0..8
        SOURCE_IDENTIFIER_LIT@0..8 \"__LINE__\"
    NEWLINE@8..9 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1.0f` is `SynConst.Single 1.0f32`. Shape:
/// `EXPR_DECL > CONST_EXPR > IEEE32_LIT "1.0f"`. Pins the
/// decimal-form (`Token::Float32`) path.
#[test]
fn lone_float32_literal() {
    let source = "1.0f\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        IEEE32_LIT@0..4 \"1.0f\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `42f` is dotless `SynConst.Single 42f32` — exercises the
/// `ieee32_dotless_no_exponent` regex arm (no dot, no exponent).
#[test]
fn lone_float32_dotless_literal() {
    let source = "42f\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_ieee32 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IEEE32_LIT));
    assert!(any_ieee32, "expected an IEEE32_LIT in the tree");
    assert_lossless(source, &parse);
}

/// `0x40490fdblf` is the hex bit-pattern form of the single
/// `3.1415927f32` (pi-ish). FCS lex.fsl:498-504 parses the body as
/// int64 in `0..=0xFFFFFFFF` and bit-casts via `ToSingle`.
#[test]
fn lone_xieee32_literal() {
    let source = "0x40490fdblf\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let any_ieee32 = parse
        .root
        .descendants_with_tokens()
        .any(|el| matches!(el, rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IEEE32_LIT));
    assert!(any_ieee32, "expected an IEEE32_LIT in the tree");
    assert_lossless(source, &parse);
}

/// XIEEE32 body must fit `u32`. `0x100000000lf` is 33 bits — FCS
/// errors with `lexOutsideThirtyTwoBitFloat`; we match with the
/// `body doesn't fit 32 bits` ParseError.
#[test]
fn xieee32_overflowing_u32_is_error() {
    let source = "0x100000000lf\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("doesn't fit 32 bits")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Two top-level integer literals separated by only whitespace are
/// expression application in F# (`42 43` is `App(42, 43)`, one decl).
/// Phase 3.3 produces the application shape; the result is well-formed
/// even though applying an `Int32` to an `Int32` is a *type* error
/// (which is post-parser).
#[test]
fn two_int_literals_on_same_line_is_app() {
    let source = "42 43\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      APP_EXPR@0..5
        CONST_EXPR@0..2
          INT32_LIT@0..2 \"42\"
        CONST_EXPR@2..5
          WHITESPACE@2..3 \" \"
          INT32_LIT@3..5 \"43\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The lexer over-accepts `1_` (trailing underscore) as `Token::Int`,
/// but FCS rejects it at parse time. We surface a parse error to match.
#[test]
fn trailing_underscore_int_is_error() {
    let parse = parse("1_\n");
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.starts_with("malformed integer literal")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless("1_\n", &parse);
}

/// Well-formed digit separators (`1_000`) are still accepted.
#[test]
fn underscore_between_digits_is_accepted() {
    let parse = parse("1_000\n");
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless("1_000\n", &parse);
}

/// An integer literal whose underscore-stripped value doesn't fit in
/// `i32` is malformed: FCS reports `lexOutsideThirtyTwoBitSigned`. The
/// shape check alone would accept `2147483649`, so the value check is
/// what matters here.
#[test]
fn int32_overflow_is_error() {
    let parse = parse("2147483649\n");
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("outside 32-bit signed range")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless("2147483649\n", &parse);
}

/// Boundary case for the overflow check: `i32::MAX` (`2147483647`) is
/// in range and should parse cleanly.
#[test]
fn int32_max_is_accepted() {
    let parse = parse("2147483647\n");
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless("2147483647\n", &parse);
}

/// Literals so wide they overflow `i64` during validation still surface
/// as out-of-i32-range — the `i64::parse` failure path must not panic or
/// silently accept.
#[test]
fn int32_huge_overflow_is_error() {
    // 25 digits — far past i64::MAX (~9.2 × 10^18).
    let parse = parse("1234567890123456789012345\n");
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("outside 32-bit signed range")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless("1234567890123456789012345\n", &parse);
}

/// F# (and FCS) explicitly accept repeated digit separators: `1__2` is a
/// valid integer literal. Pin that we don't over-reject it.
#[test]
fn repeated_underscore_int_is_accepted() {
    let parse = parse("1__2\n");
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless("1__2\n", &parse);
}

/// `1"x"` — symmetric to [`adjacent_numeric_then_paren_is_app`].
/// A string literal is not an ident_char, so FCS treats this as a
/// valid App, not a malformed numeric. Regression guard.
#[test]
fn adjacent_numeric_then_string_is_app() {
    let source = "1\"x\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "`1\"x\"` should parse as App(1, \"x\")",
    );
    assert_lossless(source, &parse);
}

/// `1π` — Greek small letter pi (U+03C0) matches `\p{L}` and is
/// therefore an `ident_char` under FCS's rules. The lexer splits
/// at the digit/letter boundary (`Int("1")` then `Ident("π")`),
/// so without a Unicode-aware adjacency check, my round-4 guard
/// silently accepts the parse. The check must use `char`-level
/// Unicode inspection, not `is_ascii_alphanumeric()` on the
/// first byte.
#[test]
fn adjacent_numeric_then_unicode_ident_not_app() {
    let source = "1\u{03C0}\n";
    let parse = parse(source);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "must not build App(1, π) for malformed numeric `1π`",
    );
    assert!(
        !parse.errors.is_empty(),
        "expected parse error for malformed numeric `1π`",
    );
    assert_lossless(source, &parse);
}
