//! Differential test (`parser::parse` vs FCS): numeric, char, bool, unit, and
//! identifier *literals* at the top level. Split out of the former monolithic
//! `parser_diff.rs`.

use std::io::Write;

use crate::common::fcs_ast_batch;
use crate::common::normalised_ast::normalise_fcs_dump;
use crate::common::{assert_asts_match, assert_asts_match_allow_errors};
use serde_json::Value;
use tempfile::NamedTempFile;

/// The empty file. FCS produces a `ParsedImplFileInput` with a single
/// anonymous module whose decls list is empty; our parser produces the same
/// shape. Pins the "no input, no decls" baseline.
#[test]
fn diff_ast_empty_file() {
    assert_asts_match("");
}

/// A single integer literal at the top level. FCS produces one
/// `SynModuleDecl.Expr` wrapping a `SynExpr.Const(SynConst.Int32 42)`; our
/// parser produces the same shape via `EXPR_DECL > CONST_EXPR > INT32_LIT`.
#[test]
fn diff_ast_lone_integer() {
    assert_asts_match("42\n");
}

/// `true` literal at the top level. FCS produces `SynConst.Bool(true)`; we
/// emit `EXPR_DECL > CONST_EXPR > BOOL_LIT "true"` and project to the same
/// `NormalisedConst::Bool(true)`.
#[test]
fn diff_ast_lone_true() {
    assert_asts_match("true\n");
}

/// `false` literal at the top level — symmetric to `diff_ast_lone_true`.
#[test]
fn diff_ast_lone_false() {
    assert_asts_match("false\n");
}

/// `null` literal at the top level. FCS produces `SynExpr.Null` (a
/// distinct expression, *not* a `SynConst`); we emit
/// `EXPR_DECL > NULL_EXPR > NULL_TOK "null"` and both project to
/// `NormalisedExpr::Null`.
#[test]
fn diff_ast_lone_null() {
    assert_asts_match("null\n");
}

/// `null` on a binding RHS — `let x = null` — the motivating case. Pins
/// that `SynExpr.Null` stands wherever an atom does.
#[test]
fn diff_ast_let_null_rhs() {
    assert_asts_match("let x = null\n");
}

/// `null` in application-argument position (`f null`). Confirms `null`
/// is admitted as an `argExpr` (`atomicExpr`), so FCS's
/// `App(f, Null)` matches our projection.
#[test]
fn diff_ast_null_app_arg() {
    assert_asts_match("f null\n");
}

/// `null` as a tuple element (`null, null`). FCS attaches each `Null`
/// as a `Tuple` element; pins that the atom composes under `,`.
#[test]
fn diff_ast_null_tuple() {
    assert_asts_match("null, null\n");
}

/// `()` unit literal at the top level. FCS produces
/// `SynExpr.Const(SynConst.Unit)`; we emit
/// `EXPR_DECL > CONST_EXPR > [LPAREN_TOK, RPAREN_TOK]` and project to the
/// same `NormalisedConst::Unit`.
#[test]
fn diff_ast_lone_unit() {
    assert_asts_match("()\n");
}

/// `( )` — whitespace-between-parens is still unit (the interior is
/// trivia). Pins that internal trivia handling doesn't perturb the
/// FCS-equivalent `SynConst.Unit`.
#[test]
fn diff_ast_unit_with_internal_whitespace() {
    assert_asts_match("( )\n");
}

/// Single identifier at the top level. FCS produces
/// `SynExpr.Ident(Ident "x")`; we emit
/// `EXPR_DECL > IDENT_EXPR > IDENT_TOK "x"` and project to
/// `NormalisedExpr::Ident("x")` on both sides.
#[test]
fn diff_ast_lone_ident() {
    assert_asts_match("x\n");
}

/// Backticked ident `` ``foo bar`` `` at the top level. FCS stores
/// `Ident.idText = "foo bar"` (backticks stripped); our normaliser strips
/// them too. Pins the projection's backtick-stripping path.
#[test]
fn diff_ast_backticked_ident() {
    assert_asts_match("``foo bar``\n");
}

/// Three-segment dotted path `Foo.Bar.Baz`. FCS produces
/// `SynExpr.LongIdent(false, SynLongIdent([Foo;Bar;Baz], …), None, _)`; we
/// emit `EXPR_DECL > LONG_IDENT_EXPR > LONG_IDENT > [IDENT, DOT, …]` and
/// both sides project to `NormalisedExpr::LongIdent(["Foo","Bar","Baz"])`.
#[test]
fn diff_ast_three_segment_long_ident() {
    assert_asts_match("Foo.Bar.Baz\n");
}

/// Two-segment path `Foo.Bar` — minimum that exercises the
/// `SynExpr.LongIdent` (vs `SynExpr.Ident`) path. FCS uses the dedicated
/// `SynExpr.LongIdent` representation only for two-or-more segments.
#[test]
fn diff_ast_two_segment_long_ident() {
    assert_asts_match("Foo.Bar\n");
}

/// Backticked trailing segment — pins that `Ident.idText` is matched after
/// backtick-stripping on both sides, not just for single idents.
#[test]
fn diff_ast_long_ident_with_backticked_segment() {
    assert_asts_match("Foo.``bar baz``\n");
}

/// `127y` is the maximum positive `SynConst.SByte`. Pins the `y` suffix
/// arm of the small-suffix classifier and the typed projection on both
/// sides.
#[test]
fn diff_ast_sbyte_literal() {
    assert_asts_match("127y\n");
}

/// `255uy` is the maximum `SynConst.Byte`. Pins the longer-suffix `uy`
/// arm (`uy` must beat `y`).
#[test]
fn diff_ast_byte_literal() {
    assert_asts_match("255uy\n");
}

/// `32767s` is the maximum positive `SynConst.Int16`. Pins the `s`
/// suffix arm.
#[test]
fn diff_ast_int16_literal() {
    assert_asts_match("32767s\n");
}

/// `65535us` is the maximum `SynConst.UInt16`. Pins the `us` suffix arm
/// (must beat `s`).
#[test]
fn diff_ast_uint16_literal() {
    assert_asts_match("65535us\n");
}

/// `42l` is `SynConst.Int32 42` with the explicit `l` suffix. Pins the
/// `l` arm of the suffix classifier (vs the `Int` bare-decimal path).
#[test]
fn diff_ast_int32_suffixed_literal() {
    assert_asts_match("42l\n");
}

/// `42u` — bare `u` uint32 suffix.
#[test]
fn diff_ast_uint32_u_literal() {
    assert_asts_match("42u\n");
}

/// `42ul` — long `ul` uint32 suffix; classifier must beat `l` and `u`.
#[test]
fn diff_ast_uint32_ul_literal() {
    assert_asts_match("42ul\n");
}

/// `42uL` — uppercase `L` switches the width to 64 even with the `u`
/// unsigned marker, so FCS produces `SynConst.UInt64` (see `lex.fsl`:273).
/// Pins the case-sensitivity of the L distinction in our classifier.
#[test]
#[allow(non_snake_case)]
fn diff_ast_uint64_uL_literal() {
    assert_asts_match("42uL\n");
}

/// `9223372036854775807L` is `SynConst.Int64 i64::MAX`. Pins the boundary
/// and the `L` suffix arm.
#[test]
fn diff_ast_int64_at_max() {
    assert_asts_match("9223372036854775807L\n");
}

/// `18446744073709551615UL` is `SynConst.UInt64 u64::MAX`. Specifically
/// exercises u64-magnitude bodies; an i64-bodied classifier would error.
#[test]
fn diff_ast_uint64_at_max() {
    assert_asts_match("18446744073709551615UL\n");
}

/// `1n` is `SynConst.IntPtr 1` — signed native-int. Pins the `n` arm
/// of the suffix classifier and the FCS `IntPtr` case-tag projection.
#[test]
fn diff_ast_intptr_literal() {
    assert_asts_match("1n\n");
}

/// `1un` is `SynConst.UIntPtr 1` — unsigned native-int. Pins the `un`
/// arm (which must beat both `u` and `n` in the longest-first walk).
#[test]
fn diff_ast_uintptr_literal() {
    assert_asts_match("1un\n");
}

/// `0x10` is `SynConst.Int32 16` — bare hex (`Token::XInt`). Pins that
/// hex routes to INT32_LIT and our base-16 decode matches FCS's.
#[test]
fn diff_ast_hex_int_literal() {
    assert_asts_match("0x10\n");
}

/// `0o17` is `SynConst.Int32 15` — bare octal. Same routing as hex,
/// different base.
#[test]
fn diff_ast_oct_int_literal() {
    assert_asts_match("0o17\n");
}

/// `0b101` is `SynConst.Int32 5` — bare binary. Same routing as hex,
/// different base.
#[test]
fn diff_ast_bin_int_literal() {
    assert_asts_match("0b101\n");
}

/// `0x80000000` fits `u32` but not `i32`; FCS reinterprets the bit
/// pattern, producing `SynConst.Int32 -2147483648`. Pins the
/// two's-complement boundary on both sides.
#[test]
fn diff_ast_hex_int_at_i32_min() {
    assert_asts_match("0x80000000\n");
}

/// `0xFFFFFFFF` is the largest unsuffixed hex; bit-reinterprets to
/// `SynConst.Int32 -1`. Pins the upper edge of the `u32`-bodied window.
#[test]
fn diff_ast_hex_int_at_u32_max() {
    assert_asts_match("0xFFFFFFFF\n");
}

/// `0xFFuy` is `SynConst.Byte 255` — suffixed hex byte. Pins that
/// `XIntSuffixed` shares the suffix table with `IntSuffixed` but parses
/// the body in base 16.
#[test]
fn diff_ast_hex_suffixed_byte() {
    assert_asts_match("0xFFuy\n");
}

/// `0o17l` is `SynConst.Int32 15` — suffixed octal. Pins that the
/// `0o…` prefix's `o` isn't mistaken for a suffix during digit scan.
#[test]
fn diff_ast_oct_suffixed_int32() {
    assert_asts_match("0o17l\n");
}

/// `0b1010UL` is `SynConst.UInt64 10` — binary body with the `UL`
/// suffix. Pins multi-char suffix dispatch on a non-decimal base.
#[test]
fn diff_ast_bin_suffixed_uint64() {
    assert_asts_match("0b1010UL\n");
}

/// `0x80000000l` — explicitly-suffixed int32 form of the
/// two's-complement-boundary case. Pins that the `xint32` rule mirrors
/// `xint` for bit-reinterpretation.
#[test]
fn diff_ast_hex_suffixed_int32_at_min() {
    assert_asts_match("0x80000000l\n");
}

/// `0x80y` — top-bit-set hex sbyte literal. FCS narrows via
/// two's-complement to `SynConst.SByte(-128y)`, serialised as `-128` in
/// JSON. The diff harness must read the JSON value as a signed integer;
/// reading as `u64` would panic on the negative.
#[test]
fn diff_ast_hex_sbyte_at_min() {
    assert_asts_match("0x80y\n");
}

/// `0xFFFFFFFFFFFFFFFFL` — top-bit-set hex int64 literal. FCS narrows
/// to `SynConst.Int64(-1L)`; exercises the signed-i64 path of the diff
/// harness.
#[test]
fn diff_ast_hex_int64_negative() {
    assert_asts_match("0xFFFFFFFFFFFFFFFFL\n");
}

/// `1.0` is `SynConst.Double 1.0` (`Token::Float64` decimal form). Pins
/// that the projector compares double bit patterns via `f64::to_bits` on
/// both sides — `1.0_f64.to_bits() = 0x3FF0000000000000`.
#[test]
fn diff_ast_lone_float64_decimal() {
    assert_asts_match("1.0\n");
}

/// `1e10` exercises the exponent-only `Token::Float64` lexer arm (no
/// dot, mandatory exponent). FCS still produces `SynConst.Double`; pins
/// that the decimal-form arm handles both shapes uniformly.
#[test]
fn diff_ast_lone_float64_exponent() {
    assert_asts_match("1e10\n");
}

/// `1.5430806348152437` (≈ `cosh 1.0`, lifted from `OperatorsModule1.fs`) is a
/// double whose shortest round-trippable decimal `serde_json` decodes one ULP
/// low. Our parser rounds it correctly (`bits …472`), as does FCS's `float`;
/// the spurious divergence came only from the harness reading FCS's value back
/// through `serde_json`'s float parser. Pins that `fcs-dump` now emits the exact
/// IEEE bits (so the comparison no longer depends on `serde_json`'s rounding).
#[test]
fn diff_ast_lone_float64_serde_misround() {
    assert_asts_match("1.5430806348152437\n");
}

/// `0x4024000000000000LF` is the hex bit-pattern form of the double
/// `10.0` — FCS lex.fsl:506-509 parses the body as int64 and bit-casts
/// via `BitConverter.Int64BitsToDouble`. Pins that the normaliser's
/// bit-pattern decode path agrees with FCS's serialised JSON number.
#[test]
#[allow(non_snake_case)]
fn diff_ast_lone_xieee64_LF() {
    assert_asts_match("0x4024000000000000LF\n");
}

/// `1.0f` is `SynConst.Single 1.0f32` (`Token::Float32` decimal form).
/// Bit-pattern equality via `f32::to_bits` — `1.0f32` is exactly
/// representable, so both sides yield `0x3F800000`.
#[test]
fn diff_ast_lone_float32_decimal() {
    assert_asts_match("1.0f\n");
}

/// `42f` exercises the `ieee32_dotless_no_exponent` lexer arm — dotless
/// f32 literal (LanguageFeature.DotlessFloat32Literal). FCS still
/// produces `SynConst.Single`.
#[test]
fn diff_ast_lone_float32_dotless() {
    assert_asts_match("42f\n");
}

/// `0x40490fdblf` is the hex bit-pattern form of the single
/// `3.1415927f32` (≈ π). FCS lex.fsl:498-504 parses the body as int64
/// in `0..=0xFFFFFFFF` and bit-casts via `ToSingle`. Pins the XIEEE32
/// decode path against FCS.
#[test]
fn diff_ast_lone_xieee32_lf() {
    assert_asts_match("0x40490fdblf\n");
}

/// Plain ASCII char literal — `SynConst.Char 'a'`. Exercises the
/// `unescaped-char` lexer arm and the normaliser's no-escape path.
#[test]
fn diff_ast_lone_char_ascii() {
    assert_asts_match("'a'\n");
}

/// Char with single-letter escape — `SynConst.Char '\n'`. Pins the
/// lex.fsl:303-313 `escape` table against the normaliser's mapping for
/// `\n` → U+000A.
#[test]
fn diff_ast_lone_char_newline_escape() {
    assert_asts_match("'\\n'\n");
}

/// Unescaped apostrophe char literal — `'''` is `SynConst.Char '\''`. FCS's
/// `lex.fsl:305` char body excludes `\ \n \r \t \b` but not the apostrophe,
/// so the middle `'` is an ordinary body char (as seen in real source, e.g.
/// `isTypeParameter '''`).
#[test]
fn diff_ast_lone_char_apostrophe() {
    assert_asts_match("let x = '''\n");
}

/// Char with `\xHH` hex escape — `SynConst.Char '\xFF'` decodes to
/// U+00FF. Pins the hex-escape decode path.
#[test]
fn diff_ast_lone_char_hex_escape() {
    assert_asts_match("'\\xFF'\n");
}

/// Non-ASCII Unicode char literal — `SynConst.Char 'À'` (U+00C0). The
/// lexer's `unescaped-char` arm accepts any non-control UTF-8 codepoint;
/// the normaliser's `inner.chars().next()` recovers the full Unicode
/// scalar value.
#[test]
fn diff_ast_lone_char_unicode() {
    assert_asts_match("'À'\n");
}

/// Lone high-surrogate char literal — `'\uD800'`. A char literal can name a
/// BMP code unit, and U+D800 is a valid UTF-16 unit but not a Unicode scalar.
/// The normaliser compares raw `u16` code units so this stays distinct from
/// U+FFFD instead of collapsing through JSON/Rust string replacement.
#[test]
fn diff_ast_lone_char_high_surrogate() {
    assert_asts_match("'\\uD800'\n");
}

/// Lone low-surrogate char literal — `'\uDC00'`. Same raw-code-unit path as the
/// high-surrogate fixture, but pins the other half of the surrogate range.
#[test]
fn diff_ast_lone_char_low_surrogate() {
    assert_asts_match("'\\uDC00'\n");
}

/// Astral `\U` char escape — `'\U0001F600'`. A char literal can't hold a
/// non-BMP scalar; FCS reports FS1159 ("only valid in string literals")
/// and recovers with `CHAR (char 0)`, so the value is NUL, *not* the
/// astral scalar. Both sides flag it, so this is an `allow_errors` case.
#[test]
fn diff_ast_char_astral_escape() {
    assert_asts_match_allow_errors("'\\U0001F600'\n");
}

/// Out-of-range `\U` char escape — `'\U00110000'` (above U+10FFFF). FCS
/// reports FS1245 and, like the astral case, recovers with `CHAR (char
/// 0)` → NUL. Pins that the `> U+FFFF` arm returns `'\0'` rather than a
/// replacement char.
#[test]
fn diff_ast_char_out_of_range_escape() {
    assert_asts_match_allow_errors("'\\U00110000'\n");
}

/// Byte-char literal — `'a'B` is `SynConst.Byte 97uy` not
/// `SynConst.Char`. Exercises the lexer's `Char` token text ending in
/// `B` routed to `BYTE_LIT`, and the normaliser's `decode_char_literal`
/// then range-check into `u8`.
#[test]
fn diff_ast_lone_byte_char() {
    assert_asts_match("'a'B\n");
}

/// `"1_0"` value: `1_0` literally — strings don't strip digit
/// separators. Trivially covered, but pin alongside the `1_0.0` float64
/// case to catch normaliser drift.
#[test]
fn diff_ast_lone_float64_with_separator() {
    assert_asts_match("1_0.0\n");
}

/// `1.0m` — `SynConst.Decimal 1.0M`. Decimal value-equality ignores
/// scale (`1.0 == 1.00`), so the projector compares the canonical text
/// form `decimal.ToString(InvariantCulture)` instead — trailing zeros
/// survive.
#[test]
fn diff_ast_lone_decimal() {
    assert_asts_match("1.0m\n");
}

/// `1m` — integer-only decimal mantissa, scale 0.
#[test]
fn diff_ast_lone_decimal_integer() {
    assert_asts_match("1m\n");
}

/// `1e10m` — exponent-only decimal mantissa. `decimal.Parse("1e10")`
/// gives the value `10000000000` with scale 0; our canonicaliser must
/// shift the exponent into the digit string the same way.
#[test]
fn diff_ast_lone_decimal_exponent() {
    assert_asts_match("1e10m\n");
}

/// `123I` — `SynConst.UserNum("123", "I")`. The bigint suffix.
#[test]
fn diff_ast_lone_user_num_bigint() {
    assert_asts_match("123I\n");
}

/// `42N` — `SynConst.UserNum("42", "N")`. The bignat suffix.
#[test]
fn diff_ast_lone_user_num_bignat() {
    assert_asts_match("42N\n");
}

/// `1_000G` — exercises underscore-stripping in the value field. FCS
/// strips `_` from the value before stashing it in `SynConst.UserNum`,
/// so we should too.
#[test]
fn diff_ast_lone_user_num_with_underscore() {
    assert_asts_match("1_000G\n");
}

/// Byte-char trigraph with value 128..=255. FCS emits FS1157 as a
/// *warning* and still produces `SynConst.Byte 255` — `hadErrors`
/// stays false. Pins that our validator doesn't promote this to an
/// error.
#[test]
fn diff_ast_byte_char_trigraph_above_ascii() {
    assert_asts_match("'\\255'B\n");
}

/// 29-digit over-precision fractional decimal. `System.Decimal.Parse`
/// rounds the excess away rather than rejecting it, so FCS accepts and
/// emits `SynConst.Decimal 0.1234567890123456789012345679` (rounded).
#[test]
fn diff_ast_decimal_over_precision_fractional() {
    assert_asts_match("0.12345678901234567890123456789m\n");
}

// ---- source-identifier constants ---------------------------------------
//
// `__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` / `__LINE__` lex as
// `Token::KeywordString` and FCS surfaces them through the
// `sourceIdentifier` → `constant` chain (`pars.fsy:3475-3477`) as
// `SynConst.SourceIdentifier(spelling, expanded, range)`. The normaliser
// compares the expanded source-identifier value after canonicalising physical
// file/directory paths, so these pin that we route the token through the
// *constant* productions FCS does, in every position a constant can appear.

/// `__SOURCE_DIRECTORY__` on a binding RHS — the motivating case. FCS:
/// `SynExpr.Const(SynConst.SourceIdentifier("__SOURCE_DIRECTORY__", …))`;
/// ours: `CONST_EXPR > SOURCE_IDENTIFIER_LIT`.
#[test]
fn diff_ast_source_directory_binding_rhs() {
    assert_asts_match("let dir = __SOURCE_DIRECTORY__\n");
}

/// `#line` remaps diagnostics, but source identifiers use the physical source
/// file that FCS parsed. This keeps the normaliser from accidentally deriving
/// file/directory values from virtual `#line` coordinates.
#[test]
fn diff_ast_source_identifiers_ignore_line_directive_virtual_file() {
    assert_asts_match(
        "#line 42 \"virtual/Generated.fs\"\nlet line = __LINE__\nlet file = __SOURCE_FILE__\nlet dir = __SOURCE_DIRECTORY__\n",
    );
}

/// `__SOURCE_FILE__` as a lone top-level expression.
#[test]
fn diff_ast_lone_source_file() {
    assert_asts_match("__SOURCE_FILE__\n");
}

/// FCS carries the expanded physical file name for `__SOURCE_FILE__`. The
/// normaliser must validate that payload before canonicalising it, rather than
/// silently comparing only the source spelling.
#[test]
#[should_panic(expected = "__SOURCE_FILE__")]
fn fcs_source_file_value_is_consumed() {
    let json = fcs_dump_with_source_identifier_value(
        "let file = __SOURCE_FILE__\n",
        "__SOURCE_FILE__",
        "not-the-source-file.fs",
    );
    let _ = normalise_fcs_dump(&json);
}

/// Same as `fcs_source_file_value_is_consumed`, but for the physical source
/// directory expansion.
#[test]
#[should_panic(expected = "__SOURCE_DIRECTORY__")]
fn fcs_source_directory_value_is_consumed() {
    let json = fcs_dump_with_source_identifier_value(
        "let dir = __SOURCE_DIRECTORY__\n",
        "__SOURCE_DIRECTORY__",
        "/not/the/source/directory",
    );
    let _ = normalise_fcs_dump(&json);
}

/// `__LINE__` as a lone top-level expression. Its expanded value (`"1"`) is
/// deterministic and compared.
#[test]
fn diff_ast_lone_line() {
    assert_asts_match("__LINE__\n");
}

/// `__LINE__` on line 2. This catches accidental spelling-only comparison.
#[test]
fn diff_ast_line_value_on_second_line() {
    assert_asts_match("let line =\n    __LINE__\n");
}

/// `__LINE__` in application-argument position (`f __LINE__`). Confirms the
/// keyword-string is admitted as an `argExpr` (`atomicExpr`), matching FCS's
/// `App(f, Const(SourceIdentifier))`.
#[test]
fn diff_ast_source_identifier_app_arg() {
    assert_asts_match("f __LINE__\n");
}

/// Source identifiers as list elements (`[__LINE__; __SOURCE_FILE__]`).
/// Pins that the atom composes inside a list, like other constants.
#[test]
fn diff_ast_source_identifier_list() {
    assert_asts_match("[__LINE__; __SOURCE_FILE__]\n");
}

/// Source identifiers as a tuple (`__LINE__, __SOURCE_FILE__`). Pins
/// composition under `,`.
#[test]
fn diff_ast_source_identifier_tuple() {
    assert_asts_match("__LINE__, __SOURCE_FILE__\n");
}

fn fcs_dump_with_source_identifier_value(
    source: &str,
    constant: &str,
    replacement: &str,
) -> String {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");

    let json = fcs_ast_batch(tmp.path());
    let mut value: Value = serde_json::from_str(&json).expect("fcs-dump JSON shape");
    assert!(
        replace_first_source_identifier_value(&mut value, constant, replacement),
        "FCS dump for {source:?} did not contain {constant}",
    );
    serde_json::to_string(&value).expect("serialise tampered FCS dump")
}

fn replace_first_source_identifier_value(
    value: &mut Value,
    constant: &str,
    replacement: &str,
) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("Case").and_then(Value::as_str) == Some("SourceIdentifier")
                && let Some(Value::Array(fields)) = map.get_mut("Fields")
                && fields.first().and_then(Value::as_str) == Some(constant)
            {
                assert!(
                    fields.len() >= 3,
                    "SourceIdentifier carries constant, value, range",
                );
                fields[1] = Value::String(replacement.to_string());
                return true;
            }
            map.values_mut()
                .any(|child| replace_first_source_identifier_value(child, constant, replacement))
        }
        Value::Array(items) => items
            .iter_mut()
            .any(|child| replace_first_source_identifier_value(child, constant, replacement)),
        _ => false,
    }
}

/// `__SOURCE_FILE__` in *pattern* position — a constant pattern. FCS routes
/// `constant` through `SynPat.Const`, so this exercises our `parse_const_pat`
/// path (distinct from the expression path the other cases hit).
#[test]
fn diff_ast_source_identifier_const_pattern() {
    assert_asts_match("match x with\n| __SOURCE_FILE__ -> 1\n| _ -> 2\n");
}

/// Source identifier as a *constructor-argument* sub-pattern
/// (`Some __SOURCE_FILE__`). Pins that the keyword-string is admitted as an
/// atomic pattern (`raw_starts_atomic_pat`), not just at a clause head.
#[test]
fn diff_ast_source_identifier_ctor_arg_pattern() {
    assert_asts_match("match x with\n| Some __SOURCE_FILE__ -> 1\n| _ -> 2\n");
}

/// Source identifier as a *tuple* pattern element (`__LINE__, _`). Pins
/// composition under `,` on the pattern side.
#[test]
fn diff_ast_source_identifier_tuple_pattern() {
    assert_asts_match("match x with\n| __LINE__, _ -> 1\n| _ -> 2\n");
}
