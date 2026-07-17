//! Differential test (`parser::parse` vs FCS): string and interpolated-string
//! literals — plain/verbatim/triple/byte strings, single & triple
//! interpolation, nesting, and the brace-digraph collapser. Split out of the
//! former monolithic `parser_diff.rs`.

use crate::common::{
    assert_asts_match, assert_asts_match_allow_errors, assert_asts_match_with_diagnostic,
};

/// Plain `"hello"` — `SynConst.String("hello", SynStringKind.Regular,
/// _)`. Exercises the no-escape path of the string decoder.
#[test]
fn diff_ast_lone_string() {
    assert_asts_match("\"hello\"\n");
}

/// `"a\nb"` — `SynConst.String "a\nb"`. Pins that the string decoder's
/// single-letter escape table agrees with FCS's `escape` helper
/// (`lex.fsl`:303).
#[test]
fn diff_ast_lone_string_with_escape() {
    assert_asts_match("\"a\\nb\"\n");
}

/// `"À"` (U+00C0) — non-ASCII Unicode codepoint inside a regular
/// string. Pins that the UTF-8 pass-through path works (no escape; the
/// source bytes are UTF-8 already).
#[test]
fn diff_ast_lone_string_unicode() {
    assert_asts_match("\"À\"\n");
}

/// `"\q"` — unknown backslash escape. FCS's `singleQuoteString` falls
/// through and stores `\q` literally (we confirmed via `dotnet`). Pins
/// the projector's pass-through behaviour for backslash + unknown char.
#[test]
fn diff_ast_lone_string_unknown_escape() {
    assert_asts_match("\"\\q\"\n");
}

/// `"\x"` — `\x` with no hex body. FCS's regex requires two hex digits
/// after `\x`, so the rule doesn't fire and the unrecognised-escape rule
/// stores `\x` literally. Pins the decoder's fall-through path for
/// incomplete fixed-width escapes.
#[test]
fn diff_ast_lone_string_incomplete_hex_escape() {
    assert_asts_match("\"\\x\"\n");
}

/// `"\1"` — single-digit body where trigraph regex needs three. Same
/// fall-through pattern as `\x` but for the decimal-trigraph arm.
#[test]
fn diff_ast_lone_string_incomplete_trigraph() {
    assert_asts_match("\"\\1\"\n");
}

/// `"\uD800 \uDBFF \uDC00 \uDFFF"` — four *lone* UTF-16 surrogate code
/// units (two high, two low), each separated by a space so none pairs
/// with a neighbour. A surrogate is a valid UTF-16 code unit but not a
/// Unicode scalar, so it cannot live in a Rust `char`/`String`. The
/// normaliser compares the raw UTF-16 units so each surrogate half stays
/// observable. (Regression: `Conformance/.../UnicodeString01.fs` used to panic
/// the normaliser here.)
#[test]
fn diff_ast_string_lone_surrogates_short() {
    assert_asts_match("\"\\uD800 \\uDBFF \\uDC00 \\uDFFF\"\n");
}

/// `"\uD900\uD901\uD902"` — three *adjacent* high surrogates; none is
/// followed by a low surrogate. Pins that adjacent high-surrogate code units
/// stay distinct instead of being collapsed through a lossy JSON string path.
/// (Regression: `UnicodeString01.fs`.)
#[test]
fn diff_ast_string_adjacent_high_surrogates() {
    assert_asts_match("\"\\uD900\\uD901\\uD902\"\n");
}

/// `"𐐷"` — a high surrogate immediately followed by its
/// matching low surrogate: a *valid* UTF-16 surrogate pair encoding
/// U+10437 (𐐷). The normaliser compares the two underlying code units rather
/// than a scalar-normalised JSON string.
#[test]
fn diff_ast_string_surrogate_pair_short() {
    assert_asts_match("\"\\uD801\\uDC37\"\n");
}

/// `"\uD801x\uDC37"` — the same two surrogate halves separated by `x`, so
/// they do *not* form an adjacent UTF-16 pair. Pins that the intervening scalar
/// remains visible between the raw units.
#[test]
fn diff_ast_string_surrogate_halves_separated() {
    assert_asts_match("\"\\uD801x\\uDC37\"\n");
}

/// `"\U0000D800 \U0000DBFF \U0000DC00 \U0000DFFF"` — the long-form (`\U`,
/// eight hex) spelling of the lone-surrogate case. Values ≤ U+FFFF decode
/// to a single UTF-16 code unit, so a surrogate-valued `\U` behaves
/// exactly like the `\u` form and is compared as that raw unit. (Regression:
/// `UnicodeString02.fs`.)
#[test]
fn diff_ast_string_lone_surrogates_long() {
    assert_asts_match("\"\\U0000D800 \\U0000DBFF \\U0000DC00 \\U0000DFFF\"\n");
}

/// `"\U00010437"` — a long-form `\U` escape naming an astral scalar
/// (U+10437, 𐐷) directly. The decoder splits it into a surrogate pair of
/// UTF-16 code units, matching FCS's string buffer. (Regression:
/// `UnicodeString02.fs`.)
#[test]
fn diff_ast_string_astral_long_escape() {
    assert_asts_match("\"\\U00010437\"\n");
}

/// `"x\U00110000y"` — a `\U` escape *above* U+10FFFF. FCS reports FS1245
/// and recovers by dropping the escape entirely, so the value is `"xy"`;
/// the decoder must skip the escape rather than emit a replacement char.
/// Both sides flag the bad escape, so this is an `allow_errors` case.
#[test]
fn diff_ast_string_out_of_range_long_escape() {
    assert_asts_match_allow_errors("\"x\\U00110000y\"\n");
}

/// `@"path"` — verbatim string `SynConst.String("path",
/// SynStringKind.Verbatim, _)`. Pins the `Verbatim` discriminant agrees
/// across our normaliser and FCS's `SynStringKind.Verbatim` case.
#[test]
fn diff_ast_lone_verbatim_string() {
    assert_asts_match("@\"path\"\n");
}

/// `@"a""b"` — verbatim string with the `""` escape collapsing to a
/// single `"` in the value. The only escape verbatim strings have.
#[test]
fn diff_ast_verbatim_string_escaped_quote() {
    assert_asts_match("@\"a\"\"b\"\n");
}

/// `"""abc"""` — triple-quoted string `SynConst.String("abc",
/// SynStringKind.TripleQuote, _)`. No escapes apply; the only thing
/// the normaliser does is strip the outer triple-quotes.
#[test]
fn diff_ast_lone_triple_string() {
    assert_asts_match("\"\"\"abc\"\"\"\n");
}

/// `"abc"B` — regular byte string, `SynConst.Bytes([0x61; 0x62; 0x63],
/// SynByteStringKind.Regular, _)`. Pins that the trailing-`B` lexer
/// regex routes through `BYTE_STRING_LIT`, the source-text decoder
/// agrees with FCS's `addByteChar`, and the FCS-side base64 decoder
/// recovers the same byte sequence.
#[test]
fn diff_ast_lone_byte_string() {
    assert_asts_match("\"abc\"B\n");
}

/// `"a\nb"B` — regular byte string exercising the single-letter escape
/// table (`\n` → 0x0a). Same escape decoder as `STRING_LIT`; pins the
/// per-char-to-byte truncation step.
#[test]
fn diff_ast_byte_string_with_escape() {
    assert_asts_match("\"a\\nb\"B\n");
}

/// `@"abc"B` — verbatim byte string `SynConst.Bytes(_, Verbatim, _)`.
/// Pins that the verbatim `""` escape works through the byte-string
/// path and the `Verbatim` kind survives.
#[test]
fn diff_ast_lone_verbatim_byte_string() {
    assert_asts_match("@\"abc\"B\n");
}

/// `"""abc"""B` — triple-quoted byte string. FCS classifies these as
/// `SynByteStringKind.Regular` (no triple-quote case exists; see
/// `lex.fsl:135-136`). Pins that the normaliser stamps `Regular` on
/// the `TRIPLE_BYTE_STRING_LIT` side even though the source form is
/// triple-quoted.
#[test]
fn diff_ast_lone_triple_byte_string() {
    assert_asts_match("\"\"\"abc\"\"\"B\n");
}

/// `"\U0001F600"B` — `\U` escape decoding to a single astral codepoint.
/// FCS's `\U` decoder splits the surrogate pair and pushes two UTF-16
/// code units (`lex.fsl:1330-1332`), then `stringBufferAsBytes` takes
/// the low byte of each → `[0x3d, 0x00]`. Pins that the byte-string
/// projection decodes at code-unit granularity (the astral scalar's
/// surrogate pair contributes two bytes), so the astral path agrees with
/// FCS. Both sides also raise the byte-array overflow error (FCS FS1140;
/// ours via `byte_string_wide_unit_count`) since the surrogate units
/// don't fit a byte — so this uses the `allow_errors` variant, which still
/// compares the `SynConst.Bytes` AST both sides emit.
#[test]
fn diff_ast_byte_string_with_astral_escape() {
    assert_asts_match_allow_errors("\"\\U0001F600\"B\n");
}

/// `"\uD800"B` — a *lone surrogate* in a byte string. FCS records the low
/// byte of the raw UTF-16 unit (`0xD800` → `0x00`). The byte projection
/// therefore keeps raw code units before truncating to bytes. `> 255`, so both
/// sides raise the byte-array overflow error (FCS FS1140) — the `allow_errors`
/// variant still compares the recovered `SynConst.Bytes`.
#[test]
fn diff_ast_byte_string_with_lone_surrogate() {
    assert_asts_match_allow_errors("\"\\uD800\"B\n");
}

/// `$"\uD800"B` — the *interpolated* byte-string spelling of the same
/// lone-surrogate case. FCS downgrades the bare interp string to a byte
/// array and records the raw unit's low byte (`0x00`), so the projection
/// must decode the interp fragment to raw units too. `> 255`, so both sides
/// flag FS1140 — hence the `allow_errors` variant.
#[test]
fn diff_ast_byte_interp_string_with_lone_surrogate() {
    assert_asts_match_allow_errors("$\"\\uD800\"B\n");
}

/// `$"hello"` — bare interpolated string with no fills. FCS produces
/// `SynExpr.InterpolatedString([String("hello", _)],
/// SynStringKind.Regular, _)`; the rust side emits a one-fragment
/// `INTERP_STRING_EXPR`.
#[test]
fn diff_ast_lone_interp_string_bare() {
    assert_asts_match("$\"hello\"\n");
}

/// `$""` — empty bare interpolated string. Same shape as the
/// `$"hello"` case but with an empty `String` part value, exercising
/// the boundary where the fragment body is exactly the empty string.
#[test]
fn diff_ast_lone_interp_string_empty() {
    assert_asts_match("$\"\"\n");
}

/// `$"\uD800"` — an interpolated-string literal fragment can also carry a lone
/// surrogate. Pins the `SynInterpolatedStringPart.String` payload against the
/// same raw UTF-16 oracle as `SynConst.String`.
#[test]
fn diff_ast_interp_string_lone_surrogate_fragment() {
    assert_asts_match("$\"\\uD800\"\n");
}

/// `$"x={ 1 }"` — single-fill interpolated string. FCS emits
/// `InterpolatedString([String("x="); FillExpr(Const(Int32 1));
/// String("")], Regular, _)`; the rust side emits a three-child
/// `INTERP_STRING_EXPR` (Begin fragment, inner expr, End fragment).
#[test]
fn diff_ast_lone_interp_string_single_fill() {
    assert_asts_match("$\"x={1}\"\n");
}

/// `$"{1}"` — single-fill with no leading text. The Begin fragment's
/// body is empty, the inner expression is `Const(Int32 1)`, and the
/// End fragment's body is empty.
#[test]
fn diff_ast_lone_interp_string_fill_only() {
    assert_asts_match("$\"{1}\"\n");
}

/// `$"\{1}"` — `\{` is not an escape in FCS's `singleQuoteString`; the
/// backslash is literal content and the `{` opens a fill. The same rule applies
/// to continuation fragments after a fill closes.
#[test]
fn diff_ast_single_interp_backslash_open_brace_starts_fill() {
    assert_asts_match("$\"\\{1}\"\n");
    assert_asts_match("$\"{1}\\{2}\"\n");
}

/// `$"a={1+2}b"` — single-fill with an arithmetic expression inside
/// the fill. Pins that the inner `parse_expr` correctly handles full
/// expressions, not just constants.
#[test]
fn diff_ast_lone_interp_string_fill_arith() {
    assert_asts_match("$\"a={1+2}b\"\n");
}

/// `$"{1:N2}"` — single-fill with a format qualifier. FCS's grammar
/// `declExpr COLON ident %prec interpolation_fill` consumes the
/// trailing `: ident` as the qualifier on
/// `SynInterpolatedStringPart.FillExpr (expr, Some ident)`; the
/// normaliser now models it (`Some("N2")`), so this also pins that our
/// parser recovers the qualifier ident and attaches it to the fill.
#[test]
fn diff_ast_lone_interp_string_fill_qualifier() {
    assert_asts_match("$\"{1:N2}\"\n");
}

/// `$"{a} {b:N2} {c}"` — three fills, qualifier on the middle one only.
/// Pins qualifier *association*: the normaliser now distinguishes which
/// fill owns the `N2`, so a parser that smeared it onto the wrong fill
/// would diverge from FCS here.
#[test]
fn diff_ast_interp_string_fill_qualifier_middle_only() {
    assert_asts_match("let a = 1\nlet b = 2\nlet c = 3\n$\"{a} {b:N2} {c}\"\n");
}

/// `$"{\n  1\n}"` — single fill whose body spans newlines. FCS's
/// `lex.fsl` transitions the inner lexer to the normal token rule on
/// `{`, and `LexFilter.fs:2281-2288` pushes a `CtxtParen(INTERP_…_PART)`
/// followed by a `CtxtSeqBlock(NotFirstInSeqBlock, …, NoAddBlockEnd)`. The
/// inner block still recovers the fill expression (so the fragment tree
/// matches), but the body `1` (col 2) is offside of the interpolation opener's
/// column, so FCS reports FS0058 both there and at the `$"{` opener. Since the
/// §A offside emission landed we report the matching FS0058 at both spans while
/// recovering the identical tree.
#[test]
fn diff_ast_lone_interp_string_fill_multiline() {
    assert_asts_match_with_diagnostic("$\"{\n  1\n}\"\n", 58);
}

/// Multi-line fill with surrounding text fragments. Same offside FS0058
/// (opener + body) as `diff_ast_lone_interp_string_fill_multiline`, but the
/// fragment normaliser also has to handle a `Part` whose preceding
/// fragment span ends with `{` after a newline.
#[test]
fn diff_ast_lone_interp_string_fill_multiline_with_text() {
    assert_asts_match_with_diagnostic("$\"a={\n  1\n}b\"\n", 58);
}

/// `$"""hello"""` — bare triple-quoted interpolated string. FCS emits
/// `SynExpr.InterpolatedString([String("hello", _)],
/// SynStringKind.TripleQuote, _)`; the rust side emits a one-fragment
/// `INTERP_STRING_EXPR` with the triple-quoted opener variant.
#[test]
fn diff_ast_lone_triple_interp_string_bare() {
    assert_asts_match("$\"\"\"hello\"\"\"\n");
}

/// `$""""""` — empty bare triple-quoted interp (6 quotes after `$`):
/// opener `$"""` then immediately closer `"""`. Pins the byte-walker's
/// detection that a leading `"""` terminates the body with empty
/// content rather than falling through to a literal `"`.
#[test]
fn diff_ast_lone_triple_interp_string_empty() {
    assert_asts_match("$\"\"\"\"\"\"\n");
}

/// `$"""{1}"""` — single fill with no surrounding text. Same fill
/// semantics as single-quoted interp (one `{`, one `}`).
#[test]
fn diff_ast_lone_triple_interp_string_fill_only() {
    assert_asts_match("$\"\"\"{1}\"\"\"\n");
}

/// `$"""a={1+2}b"""` — single fill with arithmetic, surrounded by text.
#[test]
fn diff_ast_lone_triple_interp_string_fill_arith() {
    assert_asts_match("$\"\"\"a={1+2}b\"\"\"\n");
}

/// `$"""{` newline body `}"""` — the fill body spans newlines, same as
/// the single-quoted case (LexFilter pushes a NoAddBlockEnd SeqBlock). FCS
/// recovers the same AST and reports the offside FS0058 (opener + body); we
/// match both spans.
#[test]
fn diff_ast_lone_triple_interp_string_fill_multiline() {
    assert_asts_match_with_diagnostic("$\"\"\"{\n  1\n}\"\"\"\n", 58);
}

/// Body of a triple-quoted interp may itself contain literal newlines
/// (unlike single-quoted, where a newline closes the string). The
/// byte-walker must pass newlines through transparently.
#[test]
fn diff_ast_lone_triple_interp_string_body_multiline() {
    assert_asts_match("$\"\"\"\n  hello\n\"\"\"\n");
}

/// Body containing one and two consecutive `"` characters — both are
/// content; only a run of `"""` (3 or more) terminates. Pins that
/// `scan_cont_triple`'s greedy match doesn't fire early.
#[test]
fn diff_ast_lone_triple_interp_string_inner_quotes() {
    assert_asts_match("$\"\"\"a \"b\" \"\"c\"\"\"\n");
}

/// `$"""{1:N2}"""` — format qualifier on a triple-quoted fill. The
/// `: ident` after the inner expression is unchanged from the single
/// case.
#[test]
fn diff_ast_lone_triple_interp_string_fill_qualifier() {
    assert_asts_match("$\"\"\"{1:N2}\"\"\"\n");
}

/// `$"""\n"""` — backslash-n in the body. Triple-quoted strings do NOT
/// honour backslash escapes (lex.fsl `tripleQuoteString` has no `\\X`
/// arm); the resulting `SynConst.String` carries the two literal
/// characters `\` and `n`. Pins that our fragment decoder doesn't
/// process backslash escapes for the triple case.
#[test]
fn diff_ast_lone_triple_interp_string_literal_backslash() {
    assert_asts_match("$\"\"\"\\n\"\"\"\n");
}

/// `$"abc"B` — bare byte-interp. FCS fires FS3377 ("a byte string may
/// not be interpolated") and downgrades the token to `BYTEARRAY`,
/// recovering `SynConst.Bytes("YWJj", SynByteStringKind.Regular, _)`.
/// Both sides carry a parse error, so this uses the allow-errors
/// variant. Pins that our byte-recovery produces the same bytes + kind.
#[test]
fn diff_ast_byte_interp_single_bare() {
    assert_asts_match_allow_errors("$\"abc\"B\n");
}

/// `$"""abc"""B` — bare triple-quoted byte-interp. Same `SynConst.Bytes`
/// target (`SynByteStringKind.Regular`, no triple variant) and FS3377 as
/// the single-quoted form.
#[test]
fn diff_ast_byte_interp_triple_bare() {
    assert_asts_match_allow_errors("$\"\"\"abc\"\"\"B\n");
}

/// `$"{{"B` — bare byte-interp whose body uses the interp brace-escape
/// `{{`. FCS collapses `{{` → `{` while lexing the interpolated string,
/// then downgrades to `BYTEARRAY`, so the recovered `SynConst.Bytes` is
/// the single byte `{` (`ew==`), not two braces. Pins that the
/// differential normaliser runs byte-interp literals through the
/// interp-aware (brace-collapsing) decoder rather than the plain
/// string decoder.
#[test]
fn diff_ast_byte_interp_brace_escape() {
    assert_asts_match_allow_errors("$\"{{\"B\n");
}

/// `$"""}}"""B` — triple-quoted byte-interp exercising the `}}` escape.
/// Same collapse-then-downgrade story as the single-quoted form; the
/// recovered bytes are the single byte `}`.
#[test]
fn diff_ast_byte_interp_triple_brace_escape() {
    assert_asts_match_allow_errors("$\"\"\"}}\"\"\"B\n");
}

/// `$"a={1}b={2}"` — two fills with literal text on both sides and
/// between. FCS produces `InterpolatedString([String "a="; FillExpr 1;
/// String "b="; FillExpr 2; String ""], Regular, _)`; the rust side emits
/// a five-part `INTERP_STRING_EXPR`. Multi-fill is FCS-clean (no parse
/// error), so this uses the no-errors variant.
#[test]
fn diff_ast_interp_string_two_fills() {
    assert_asts_match("$\"a={1}b={2}\"\n");
}

/// `$"{1}{2}"` — two adjacent fills, no surrounding text. The middle
/// `Part` fragment's body is empty (`}{`).
#[test]
fn diff_ast_interp_string_fills_adjacent() {
    assert_asts_match("$\"{1}{2}\"\n");
}

/// `$"{1}{2}{3}"` — three fills. Pins that the fill-loop handles N>2 and
/// that the normaliser projects the resulting four-fragment / three-fill
/// chain to the same part list FCS produces.
#[test]
fn diff_ast_interp_string_three_fills() {
    assert_asts_match("$\"{1}{2}{3}\"\n");
}

/// `$"{1+2}x{3*4}"` — arithmetic expressions in two fills with text
/// between. Pins that the inner `parse_expr` handles full expressions
/// across successive fills, not just constants.
#[test]
fn diff_ast_interp_string_fill_arith_multi() {
    assert_asts_match("$\"{1+2}x{3*4}\"\n");
}

/// `$"{1:N2}-{2:D}"` — a format qualifier on each of two fills. FCS's
/// `declExpr COLON ident %prec interpolation_fill` consumes the trailing
/// `: ident` per fill; the normaliser drops the qualifier on both sides,
/// so only the inner expressions and fragment shape need to match.
#[test]
fn diff_ast_interp_string_fill_qualifier_multi() {
    assert_asts_match("$\"{1:N2}-{2:D}\"\n");
}

/// `$"""a={1}b={2}"""` — triple-quoted multi-fill. Same five-part target
/// as the single-quoted form, stamped `SynStringKind.TripleQuote`.
#[test]
fn diff_ast_triple_interp_string_two_fills() {
    assert_asts_match("$\"\"\"a={1}b={2}\"\"\"\n");
}

/// `$"""{1}{2}"""` — triple-quoted adjacent fills, empty middle fragment.
#[test]
fn diff_ast_triple_interp_string_fills_adjacent() {
    assert_asts_match("$\"\"\"{1}{2}\"\"\"\n");
}

/// `$"x={ $"y" }"` — single-quoted interp nested inside a single-quoted
/// fill. FCS fires FS3373 (`lexSingleQuoteInSingleQuote`) at the inner
/// `$"` but still recovers the nested `InterpolatedString` tree (outer
/// interp whose fill is the inner interp); our parser builds the same
/// tree and emits its own FS3373-equivalent diagnostic. allow_errors:
/// both sides error, recovered ASTs must match.
#[test]
fn diff_ast_nested_interp_single_in_single() {
    assert_asts_match_allow_errors("$\"x={ $\"y\" }\"\n");
}

/// `$"x={ $"y={1}" }"` — same single-in-single nesting as above but the
/// inner interp itself carries a fill, so the recovered tree is two
/// fill-bearing interps. Still FS3373 on the inner opener.
#[test]
fn diff_ast_nested_interp_single_in_single_fill() {
    assert_asts_match_allow_errors("$\"x={ $\"y={1}\" }\"\n");
}

/// `$"x={ $"""y""" }"` — triple-quoted interp nested inside a
/// single-quoted fill. FCS fires FS3374 (`lexTripleQuoteInTripleQuote`)
/// — a triple inner is always an error regardless of enclosing style.
#[test]
fn diff_ast_nested_interp_triple_inner() {
    assert_asts_match_allow_errors("$\"x={ $\"\"\"y\"\"\" }\"\n");
}

/// `$"""a={ $"""b""" }"""` — triple inner inside a triple enclosing.
/// Still FS3374 (triple inner is always an error).
#[test]
fn diff_ast_nested_interp_triple_in_triple() {
    assert_asts_match_allow_errors("$\"\"\"a={ $\"\"\"b\"\"\" }\"\"\"\n");
}

/// `$"""a={ $"b" }"""` — single-quoted interp nested inside a
/// triple-quoted fill. This is FCS's recommended workaround and is
/// **legal** (no diagnostic), so this uses the no-errors variant: the
/// nested tree must match FCS with both error lists empty.
#[test]
fn diff_ast_nested_interp_single_in_triple_clean() {
    assert_asts_match("$\"\"\"a={ $\"b\" }\"\"\"\n");
}

/// `$"x={ "y" }"` — an *ordinary* single-quoted string inside a
/// single-quoted fill. FCS's FS3373 rule covers plain string literals,
/// not only nested interp openers; FCS still recovers the fill's inner
/// `SynConst.String` expression. allow_errors: both sides error.
#[test]
fn diff_ast_nested_plain_string_in_single() {
    assert_asts_match_allow_errors("$\"x={ \"y\" }\"\n");
}

/// `$"x={ @"y" }"` — a verbatim string inside a single-quoted fill.
/// Folds into the same FS3373 case as a plain string.
#[test]
fn diff_ast_nested_verbatim_string_in_single() {
    assert_asts_match_allow_errors("$\"x={ @\"y\" }\"\n");
}

/// `$"x={ """y""" }"` — a triple string inside a single-quoted fill.
/// Triple inner is always FS3374.
#[test]
fn diff_ast_nested_triple_string_in_single() {
    assert_asts_match_allow_errors("$\"x={ \"\"\"y\"\"\" }\"\n");
}

/// `$"""x={ "y" }"""` — a single-quoted string inside a *triple* fill.
/// Legal (the single-in-triple workaround), so the no-errors variant:
/// the recovered tree must match FCS with both error lists empty.
#[test]
fn diff_ast_nested_plain_string_in_triple_clean() {
    assert_asts_match("$\"\"\"x={ \"y\" }\"\"\"\n");
}

/// `$@"a={1}b"` — verbatim interpolated string with a fill. FCS lexes the
/// `$@"` opener (`lex.fsl:687`) to `LexerStringStyle.Verbatim` and produces
/// `SynExpr.InterpolatedString([String "a="; FillExpr 1; String "b"],
/// Verbatim, _)` with no errors. Pins that our verbatim opener +
/// `SynStringKind.Verbatim` projection matches FCS.
#[test]
fn diff_ast_verbatim_interp_dollar_at_fill() {
    assert_asts_match("$@\"a={1}b\"\n");
}

/// `@$"a={1}b"` — the other spelling of the verbatim opener (`@$`). FCS
/// treats `$@` and `@$` interchangeably; both yield Verbatim. Pins that
/// our second Logos arm produces the same AST.
#[test]
fn diff_ast_verbatim_interp_at_dollar_fill() {
    assert_asts_match("@$\"a={1}b\"\n");
}

/// `$@"hello"` — bare verbatim interp, no fills. FCS:
/// `InterpolatedString([String "hello"], Verbatim, _)`.
#[test]
fn diff_ast_verbatim_interp_bare() {
    assert_asts_match("$@\"hello\"\n");
}

/// `$@"a""b={1}c"` — verbatim escape rule inside an interp. The `""` is a
/// literal quote in the body (does *not* terminate), `{1}` is a fill. FCS
/// decodes the leading fragment to `a"b`. Pins that our verbatim
/// continuation walker treats `""` as a literal quote and that the
/// normaliser collapses `""` → `"` in the fragment body.
#[test]
fn diff_ast_verbatim_interp_doubled_quote() {
    assert_asts_match("$@\"a\"\"b={1}c\"\n");
}

/// `$"""a={ $@"y" }"""` — a verbatim interp nested inside a triple-quoted
/// fill. Verbatim-in-triple is **legal** (the single/verbatim-in-triple
/// workaround), so this uses the no-errors variant: the recovered tree
/// (outer triple interp whose fill is the inner verbatim interp) must
/// match FCS with both error lists empty.
#[test]
fn diff_ast_nested_verbatim_interp_in_triple_clean() {
    assert_asts_match("$\"\"\"a={ $@\"y\" }\"\"\"\n");
}

/// `$"a={ $@"y" }"` — a verbatim interp nested inside a single-quoted
/// fill. FCS fires FS3373 (`lexSingleQuoteInSingleQuote`) — a verbatim
/// opener inside a single fill is the single/verbatim-in-single case — but
/// still recovers the nested tree. allow_errors: both sides error.
#[test]
fn diff_ast_nested_verbatim_interp_in_single() {
    assert_asts_match_allow_errors("$\"a={ $@\"y\" }\"\n");
}

/// `$@"{\n  1\n}"` — a verbatim fill whose body spans newlines. Mirrors
/// `diff_ast_lone_interp_string_fill_multiline` for the verbatim opener:
/// the LexFilter pushes the same `CtxtParen(InterpFill)` + `NoAddBlockEnd`
/// `CtxtSeqBlock` on `VerbatimBegin` as for single/triple opens (so the
/// fragment tree recovers), and the offside fill body draws the matching
/// FS0058 (opener + body) we now report alongside FCS.
#[test]
fn diff_ast_verbatim_interp_fill_multiline() {
    assert_asts_match_with_diagnostic("$@\"{\n  1\n}\"\n", 58);
}

/// `$$"""hello"""` — bare extended interp (N=2). FCS lexes the `$$"""`
/// opener (`lex.fsl:620`) and produces `InterpolatedString([String
/// "hello"], TripleQuote, _)` — the AST does *not* distinguish extended
/// from regular triple. Pins our `ExtendedBeginEnd` → `TripleQuote`
/// projection.
#[test]
fn diff_ast_extended_interp_bare() {
    assert_asts_match("$$\"\"\"hello\"\"\"\n");
}

/// `$$"""{ }"""` — single braces (run 1 < N=2) are literal content, not a
/// fill. FCS keeps them in the decoded `String "{ }"`; pins that our
/// identity body-decode keeps sub-N brace runs verbatim (no digraph
/// collapse).
#[test]
fn diff_ast_extended_interp_literal_braces() {
    assert_asts_match("$$\"\"\"{ }\"\"\"\n");
}

/// `$$"""a{b}c"""` — single braces around `b` are content (run 1 < N=2),
/// so the whole thing is one `String "a{b}c"`, no fill.
#[test]
fn diff_ast_extended_interp_subdelim_content() {
    assert_asts_match("$$\"\"\"a{b}c\"\"\"\n");
}

/// `$$"""a={{1}}b"""` — extended single fill (N=2): `{{` opens, `}}`
/// closes. FCS: `InterpolatedString([String "a="; FillExpr 1; String
/// "b"], TripleQuote, _)`.
#[test]
fn diff_ast_extended_interp_fill() {
    assert_asts_match("$$\"\"\"a={{1}}b\"\"\"\n");
}

/// `$$$"""a={{{1}}}b"""` — N=3: the fill delimiter is three braces.
/// Exercises threading `n` through the opener, driver, and decode.
#[test]
fn diff_ast_extended_interp_n3_fill() {
    assert_asts_match("$$$\"\"\"a={{{1}}}b\"\"\"\n");
}

/// `$$"""{{\n  1\n}}"""` — an extended fill whose body spans newlines.
/// Mirrors `diff_ast_verbatim_interp_fill_multiline`: the LexFilter pushes the
/// `CtxtParen(InterpFill)` + `NoAddBlockEnd` `CtxtSeqBlock` on `ExtendedBegin`
/// (fragment tree recovers), and the offside fill body draws the matching
/// FS0058 (opener + body) we now report alongside FCS.
#[test]
fn diff_ast_extended_interp_fill_multiline() {
    assert_asts_match_with_diagnostic("$$\"\"\"{{\n  1\n}}\"\"\"\n", 58);
}

/// `$$"""a{{{{1}}}}b"""` — a fill-opening `{`-run of 4 (≥ 2N=4) is FS1248
/// ("not enough '$'…"), and the trailing `}}}}` leaves a content `}}`-run
/// (≥ N) → FS1249. FCS still opens the fill and recovers `[String "a";
/// FillExpr 1; String "b"]`. allow_errors: both sides error.
#[test]
fn diff_ast_extended_interp_fs1248() {
    assert_asts_match_allow_errors("$$\"\"\"a{{{{1}}}}b\"\"\"\n");
}

/// `$$$"""a}}}b"""` — a content `}`-run of 3 (= N) is unmatched: FS1249.
/// FCS drops the whole run, recovering `String "ab"`. allow_errors.
#[test]
fn diff_ast_extended_interp_fs1249() {
    assert_asts_match_allow_errors("$$$\"\"\"a}}}b\"\"\"\n");
}

/// `$$"""a%%d{{x}}b"""` — d=2, a `%`-run of 2 (= d) is one format `%`:
/// FCS stores the part as `a%d`. Exercises the `%`-run transform's `r=d`
/// (format-percent) boundary.
#[test]
fn diff_ast_extended_interp_percent_format() {
    assert_asts_match("$$\"\"\"a%%d{{x}}b\"\"\"\n");
}

/// `$$"""a%{{x}}b"""` — d=2, a `%`-run of 1 (< d) is a literal percent and
/// FCS doubles it: part stored as `a%%`.
#[test]
fn diff_ast_extended_interp_percent_literal() {
    assert_asts_match("$$\"\"\"a%{{x}}b\"\"\"\n");
}

/// `$$"""a%%%d{{x}}b"""` — d=2, a `%`-run of 3 (∈ [d, 2d-1]) is one format
/// `%` plus a doubled surplus: part stored as `a%%%d`.
#[test]
fn diff_ast_extended_interp_percent_run_in_range() {
    assert_asts_match("$$\"\"\"a%%%d{{x}}b\"\"\"\n");
}

/// `$$$"""a%%%d{{{x}}}b"""` — d=3, a `%`-run of 3 (= d) is one format `%`:
/// part stored as `a%d`. Exercises the `%` transform for N=3.
#[test]
fn diff_ast_extended_interp_percent_n3_format() {
    assert_asts_match("$$$\"\"\"a%%%d{{{x}}}b\"\"\"\n");
}

/// `$$"""100%%done"""` — bare extended (no fill), d=2: the `%`-run of 2 is
/// one format `%`, stored as `100%done`. Confirms the transform runs on the
/// bare `ExtendedBeginEnd` fragment too.
#[test]
fn diff_ast_extended_interp_percent_bare() {
    assert_asts_match("$$\"\"\"100%%done\"\"\"\n");
}

/// `$$"""a%%%%d{{x}}b"""` — d=2, a `%`-run of 4 (= 2d) is over-long: FS1250.
/// FCS drops the whole run, recovering part `ad`. allow_errors.
#[test]
fn diff_ast_extended_interp_percent_fs1250() {
    assert_asts_match_allow_errors("$$\"\"\"a%%%%d{{x}}b\"\"\"\n");
}

/// `$$"""%%a%%%%b{{x}}"""` — d=2, two `%`-runs in one fragment: the first
/// (r=2) is transformed to one `%`, the second (r=4=2d) errors (FS1250) and
/// is dropped. Confirms per-run independence: part `%ab`. allow_errors.
#[test]
fn diff_ast_extended_interp_percent_sibling_runs() {
    assert_asts_match_allow_errors("$$\"\"\"%%a%%%%b{{x}}\"\"\"\n");
}

/// `$"a={ $$"""y""" }"` — an extended interp nested inside a single-quoted
/// fill. Extended is triple-like, so FCS fires FS3374 and still recovers
/// the nested tree (outer single interp whose fill is the inner extended
/// interp). allow_errors: both sides error, recovered ASTs must match.
#[test]
fn diff_ast_nested_extended_interp_in_single() {
    assert_asts_match_allow_errors("$\"a={ $$\"\"\"y\"\"\" }\"\n");
}

/// The triple-quoted opener
/// (`crates/cst/src/lexer/callbacks.rs:lex_interp_triple_opener`) lets a
/// stray `}` through to the fragment body for downstream recovery rather
/// than treating it as a fill terminator. The brace-digraph collapser
/// in the normaliser must make progress on such input rather than
/// spinning at the stray byte. Asserted on the helper directly because
/// a parse-and-normalise round-trip on the regressing version hangs.
#[test]
fn collapse_triple_interp_brace_digraphs_handles_stray_close_brace() {
    use crate::common::normalised_ast::collapse_triple_interp_brace_digraphs;
    assert_eq!(collapse_triple_interp_brace_digraphs("}hello"), "}hello");
    assert_eq!(collapse_triple_interp_brace_digraphs("a}b}c"), "a}b}c");
    assert_eq!(collapse_triple_interp_brace_digraphs("}}x"), "}x");
    assert_eq!(collapse_triple_interp_brace_digraphs("x{{y"), "x{y");
}

/// Same regression shape as the triple-quoted collapser, but for the
/// single-quoted interpolation path. The fallback scan used to restart at
/// the current byte, so a stray brace or trailing backslash made no progress.
/// It also copied `\X` escapes by byte, corrupting non-ASCII `X`.
#[test]
fn collapse_single_interp_brace_digraphs_handles_strays_and_utf8() {
    use crate::common::normalised_ast::collapse_interp_brace_digraphs;
    assert_eq!(collapse_interp_brace_digraphs("{hello"), "{hello");
    assert_eq!(collapse_interp_brace_digraphs("a}b}c"), "a}b}c");
    assert_eq!(collapse_interp_brace_digraphs("tail\\"), "tail\\");
    assert_eq!(collapse_interp_brace_digraphs("x{{y"), "x{y");
    assert_eq!(collapse_interp_brace_digraphs("\\é{{"), "\\é{");
    assert_eq!(collapse_interp_brace_digraphs("\\{{"), "\\{");
    assert_eq!(collapse_interp_brace_digraphs("\\}}"), "\\}");
}

/// `"\U0000000Z"` — 8-byte body after `\U` isn't hex, so FCS falls
/// through to the unrecognised-escape rule and stores the bytes
/// literally. We must not flag the overflow check on a non-hex body.
#[test]
fn diff_ast_string_unicode_long_non_hex_body() {
    assert_asts_match("\"\\U0000000Z\"\n");
}

/// Phase 6.1 — `let "x" = 1`: string-literal head exercises the shared
/// const-payload helper for a non-numeric `SynConst` variant.
#[test]
fn diff_ast_let_string_lit_value_head() {
    assert_asts_match("let \"x\" = 1\n");
}

/// Phase 5.3 — `fun "s" -> 0`: a string-literal parameter. Same lowering
/// path as the integer-const case, confirming the clause pattern carries
/// the full `SynConst.String` payload (value + `SynStringKind`) through
/// both projectors.
#[test]
fn diff_ast_fun_lambda_string_const_arg() {
    assert_asts_match("fun \"s\" -> 0\n");
}

/// Phase 7.11 — `(x : string | null)`: the minimal nullable type.
/// FCS's `appTypeCanBeNullable: appTypeWithoutNull
/// BAR_JUST_BEFORE_NULL NULL` (`pars.fsy:6357`) projects to
/// `SynType.WithNull(LongIdent string, false, _, { BarRange })`. The
/// `ambivalent` flag (always `false` at parse) and the bar/overall
/// ranges are elided by the normaliser, so this pins the inner-type
/// projection against FCS.
#[test]
fn diff_ast_typed_ident_with_with_null_string() {
    assert_asts_match("(x : string | null)\n");
}
