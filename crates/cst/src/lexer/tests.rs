use super::*;

fn toks(src: &str) -> Vec<Token<'_>> {
    lex(src).map(|(t, _)| t.expect("lex error")).collect()
}

#[test]
fn keywords_beat_identifiers() {
    assert_eq!(toks("let"), vec![Token::Let]);
    assert_eq!(toks("letx"), vec![Token::Ident("letx")]);
    assert_eq!(
        toks("let x = 1"),
        vec![
            Token::Let,
            Token::Whitespace,
            Token::Ident("x"),
            Token::Whitespace,
            Token::Equals,
            Token::Whitespace,
            Token::Int("1"),
        ]
    );
}

#[test]
fn bang_forms() {
    assert_eq!(toks("let!"), vec![Token::LetBang]);
    assert_eq!(toks("do!"), vec![Token::DoBang]);
    assert_eq!(toks("yield!"), vec![Token::YieldBang]);
}

#[test]
fn keyword_strings() {
    assert_eq!(
        toks("__SOURCE_DIRECTORY__"),
        vec![Token::KeywordString("__SOURCE_DIRECTORY__")]
    );
    assert_eq!(
        toks("__SOURCE_FILE__"),
        vec![Token::KeywordString("__SOURCE_FILE__")]
    );
    assert_eq!(toks("__LINE__"), vec![Token::KeywordString("__LINE__")]);
    // A trailing suffix kills the keyword-string match — Logos picks the
    // longest run, and the Ident regex covers `__LINE__X`.
    assert_eq!(toks("__LINE__X"), vec![Token::Ident("__LINE__X")]);
    // A near-miss with different underscoring is just an identifier.
    assert_eq!(toks("__LINE_"), vec![Token::Ident("__LINE_")]);
}

#[test]
fn quoted_identifiers() {
    assert_eq!(
        toks("``hello world``"),
        vec![Token::QuotedIdent("``hello world``")]
    );
}

#[test]
fn integer_suffixes() {
    assert_eq!(toks("42"), vec![Token::Int("42")]);
    assert_eq!(toks("42L"), vec![Token::IntSuffixed("42L")]);
    assert_eq!(toks("42uL"), vec![Token::IntSuffixed("42uL")]);
    assert_eq!(toks("0xCAFE"), vec![Token::XInt("0xCAFE")]);
    assert_eq!(toks("0xCAFEL"), vec![Token::XIntSuffixed("0xCAFEL")]);
    assert_eq!(toks("0b1010uy"), vec![Token::XIntSuffixed("0b1010uy")]);
    assert_eq!(toks("0o755L"), vec![Token::XIntSuffixed("0o755L")]);
}

#[test]
fn bit_pattern_floats_across_bases() {
    // `lex.fsl` defines xieee32/xieee64 in terms of `xinteger`, which
    // includes octal and binary prefixes. We should accept all three.
    assert_eq!(toks("0x40000000lf"), vec![Token::XIEEE32("0x40000000lf")]);
    assert_eq!(toks("0x40000000LF"), vec![Token::XIEEE64("0x40000000LF")]);
    assert_eq!(
        toks("0b00111111100000000000000000000000lf"),
        vec![Token::XIEEE32("0b00111111100000000000000000000000lf")]
    );
    assert_eq!(toks("0o7777LF"), vec![Token::XIEEE64("0o7777LF")]);
}

#[test]
fn funky_operator_names() {
    // Declaration forms — see `FUNKY_OPERATOR_NAME` in `lex.fsl`.
    assert_eq!(toks(".[]"), vec![Token::FunkyOpName(".[]")]);
    assert_eq!(toks(".()"), vec![Token::FunkyOpName(".()")]);
    assert_eq!(toks(".[]<-"), vec![Token::FunkyOpName(".[]<-")]);
    assert_eq!(toks(".[,,,]<-"), vec![Token::FunkyOpName(".[,,,]<-")]);
    assert_eq!(toks(".[..,..]"), vec![Token::FunkyOpName(".[..,..]")]);
}

#[test]
fn separators_in_numeric_literals() {
    // F# uses `_` as a digit-separator anywhere between digits.
    assert_eq!(toks("1_000"), vec![Token::Int("1_000")]);
    assert_eq!(toks("0xCA_FE"), vec![Token::XInt("0xCA_FE")]);
    assert_eq!(toks("1_0e1_0"), vec![Token::Float64("1_0e1_0")]);
}

#[test]
fn radix_suffix_respects_base() {
    // `0b102uy` and `0o9L` are not valid binary/octal literals — the digit
    // sets must match `xinteger` in `lex.fsl`. We must NOT emit them as a
    // single `XIntSuffixed`. Concretely the leading `0b10`/`0o` lexes as
    // a plain `XInt` and the rest falls out as separate tokens.
    let ts = toks("0b102uy");
    assert!(
        !matches!(ts.first(), Some(Token::XIntSuffixed(_))),
        "0b102uy must not lex as a single suffixed-binary literal: {ts:?}"
    );
    let ts = toks("0o9L");
    assert!(
        !matches!(ts.first(), Some(Token::XIntSuffixed(_))),
        "0o9L must not lex as a single suffixed-octal literal: {ts:?}"
    );
}

#[test]
fn float_and_decimal() {
    assert_eq!(toks("1.0"), vec![Token::Float64("1.0")]);
    assert_eq!(toks("1e10"), vec![Token::Float64("1e10")]);
    assert_eq!(toks("1.5f"), vec![Token::Float32("1.5f")]);
    assert_eq!(toks("1.5m"), vec![Token::Decimal("1.5m")]);
    assert_eq!(toks("1m"), vec![Token::Decimal("1m")]);
    assert_eq!(toks("42I"), vec![Token::BigNum("42I")]);
    assert_eq!(toks("0x40000000lf"), vec![Token::XIEEE32("0x40000000lf")]);
}

#[test]
fn int_range_does_not_consume_dot_as_float() {
    // `1..10` must not lex as Float64("1."), Dot, Int("10"). F# mirrors this
    // with INT32_DOT_DOT; here we emit a single IntDotDot token covering
    // the int and the range operator.
    assert_eq!(
        toks("1..10"),
        vec![Token::IntDotDot("1.."), Token::Int("10")]
    );
    assert_eq!(
        toks("[1..10]"),
        vec![
            Token::LBrack,
            Token::IntDotDot("1.."),
            Token::Int("10"),
            Token::RBrack,
        ]
    );
    // Open-ended slice `arr.[1..]`.
    assert_eq!(
        toks("arr.[1..]"),
        vec![
            Token::Ident("arr"),
            Token::Dot,
            Token::LBrack,
            Token::IntDotDot("1.."),
            Token::RBrack,
        ]
    );
    // Floats on both sides of `..` still tokenise correctly: `1.0..2.0`.
    assert_eq!(
        toks("1.0..2.0"),
        vec![Token::Float64("1.0"), Token::DotDot, Token::Float64("2.0"),]
    );
}

#[test]
fn strings() {
    assert_eq!(toks(r#""hello""#), vec![Token::String]);
    assert_eq!(toks(r#""he\"llo""#), vec![Token::String]);
    assert_eq!(toks(r#"@"hello""#), vec![Token::VerbatimString]);
    assert_eq!(toks(r#"@"a""b""#), vec![Token::VerbatimString]);
    assert_eq!(toks(r#""""triple""""#), vec![Token::TripleString]);
}

#[test]
fn byte_string_suffix_consumed() {
    // `"abc"B`, `@"abc"B`, `"""abc"""B` are byte-string literals — the trailing
    // `B` is part of the string token, not a separate `Ident("B")`.
    // See `lex.fsl`'s `singleQuoteString` / `verbatimString` / `tripleQuoteString` rules.
    let src = r#""abc"B"#;
    let toks: Vec<_> = lex(src).collect();
    assert_eq!(toks.len(), 1);
    assert!(matches!(toks[0].0, Ok(Token::String)));
    assert_eq!(toks[0].1, 0..src.len());

    let src = r#"@"abc"B"#;
    let toks: Vec<_> = lex(src).collect();
    assert_eq!(toks.len(), 1);
    assert!(matches!(toks[0].0, Ok(Token::VerbatimString)));
    assert_eq!(toks[0].1, 0..src.len());

    let src = r#""""abc"""B"#;
    let toks: Vec<_> = lex(src).collect();
    assert_eq!(toks.len(), 1);
    assert!(matches!(toks[0].0, Ok(Token::TripleString)));
    assert_eq!(toks[0].1, 0..src.len());

    // No `B` → no consumption: identifier `B` follows the string.
    let toks: Vec<_> = lex(r#""abc" B"#).collect();
    assert!(matches!(toks[0].0, Ok(Token::String)));
    assert!(matches!(toks.last().unwrap().0, Ok(Token::Ident("B"))));
}

#[test]
fn unterminated_string_is_error() {
    // Only EOF terminates a single-quoted string with an error — newlines are allowed.
    let results: Vec<_> = lex("\"oh no").collect();
    assert!(matches!(results[0].0, Err(LexError::UnterminatedString)));
}

#[test]
fn single_quoted_string_spans_lines() {
    // F# permits raw newlines inside `"..."`. See `lex.fsl`'s `singleQuoteString` rule.
    assert_eq!(toks("\"a\nb\""), vec![Token::String]);
}

#[test]
fn char_literal() {
    assert_eq!(toks("'a'"), vec![Token::Char("'a'")]);
    assert_eq!(toks(r"'\n'"), vec![Token::Char(r"'\n'")]);
    assert_eq!(toks(r"'ÿ'"), vec![Token::Char(r"'ÿ'")]);
    assert_eq!(toks("'a'B"), vec![Token::Char("'a'B")]);
}

#[test]
fn apostrophe_char_literal() {
    // `'''` is the char literal for the apostrophe — FCS's `lex.fsl:305` char
    // body excludes `\ \n \r \t \b` but *not* the apostrophe, so the middle
    // `'` is an ordinary body char. (The escaped form `'\''` also works.)
    assert_eq!(toks("'''"), vec![Token::Char("'''")]);
    assert_eq!(toks("'''B"), vec![Token::Char("'''B")]);
    assert_eq!(toks(r"'\''"), vec![Token::Char(r"'\''")]);
}

#[test]
fn line_comment_consumes_to_eol() {
    assert_eq!(
        toks("// hello\nlet"),
        vec![Token::LineComment, Token::Newline, Token::Let]
    );
    assert_eq!(toks("// hello"), vec![Token::LineComment]);
}

#[test]
fn shebang_is_line_comment() {
    // F# `.fsx` scripts allow `#!/usr/bin/env fsi` on line 1. See
    // `lex.fsl`'s `"#!" op_char*` rule, which routes to `singleLineComment`.
    assert_eq!(
        toks("#!/usr/bin/env fsi\nlet x = 1"),
        vec![
            Token::LineComment,
            Token::Newline,
            Token::Let,
            Token::Whitespace,
            Token::Ident("x"),
            Token::Whitespace,
            Token::Equals,
            Token::Whitespace,
            Token::Int("1"),
        ]
    );
}

#[test]
fn block_comment_nests() {
    assert_eq!(toks("(* a (* b *) c *)"), vec![Token::BlockComment]);
    // Unterminated.
    let results: Vec<_> = lex("(* never closes").collect();
    assert!(matches!(results[0].0, Err(LexError::UnterminatedComment)));
}

// FCS's `comment` rule (lex.fsl:1794-1857) recognises only `"`, `"""` and
// `@"` as string openers inside a block comment — there is **no**
// interpolation arm. So the two verbatim-interp spellings split differently
// inside a comment, and we must mirror that (verified against the oracle):
//
//   * `$@"…"` — the `$` is an ordinary comment char, then `@"` matches the
//     verbatim arm, so the body uses verbatim rules (`\` is literal). Thus
//     `(* $@"\" *)` closes: `\` is content, the next `"` ends the verbatim
//     string, and `*)` then closes the comment.
//   * `@$"…"` — `@` is ordinary (the `@"` arm needs `"` next, but sees `$`),
//     `$` is ordinary, then a bare `"` opens a *single-quote* string-in-
//     comment with backslash escapes. Thus `(* @$"\" *)` does **not** close:
//     `\"` is an escaped quote, the string runs to EOF, comment unterminated.
//
// (Codex once suggested treating `@$"` as verbatim here; that would diverge
// from FCS, which leaves `(* @$"\" *)` unterminated.)
#[test]
fn block_comment_verbatim_interp_spellings_match_fcs() {
    // `$@"` → verbatim body inside the comment: `\` is literal, the close
    // quote terminates the string, then `*)` closes the comment.
    assert_eq!(
        toks("(* $@\"\\\" *)\nlet y = 2\n"),
        vec![
            Token::BlockComment,
            Token::Newline,
            Token::Let,
            Token::Whitespace,
            Token::Ident("y"),
            Token::Whitespace,
            Token::Equals,
            Token::Whitespace,
            Token::Int("2"),
            Token::Newline,
        ]
    );
    // `@$"` → single-quote body: `\"` is an escaped quote, so the string
    // never closes and the comment runs to EOF (unterminated).
    let results: Vec<_> = lex("(* @$\"\\\" *)\nlet x = 1\n").collect();
    assert!(matches!(results[0].0, Err(LexError::UnterminatedComment)));
}

#[test]
fn paren_star_paren_is_not_block_comment() {
    assert_eq!(toks("(*)"), vec![Token::LParenStarRParen]);
}

#[test]
fn operators() {
    assert_eq!(toks("->"), vec![Token::RArrow]);
    assert_eq!(toks("<-"), vec![Token::LArrow]);
    assert_eq!(toks(":?>"), vec![Token::ColonQMarkGreater]);
    assert_eq!(toks(".."), vec![Token::DotDot]);
    assert_eq!(toks("..^"), vec![Token::DotDotHat]);
    assert_eq!(toks("=>"), vec![Token::Op("=>")]);
    assert_eq!(toks("|>"), vec![Token::Op("|>")]);
    assert_eq!(toks("<="), vec![Token::Op("<=")]);
}

#[test]
fn bare_less_greater_are_dedicated_tokens() {
    // Bare `<`/`>` lex as Less(false)/Greater(false). The bool is the lex-time
    // default; lexfilter promotes to true when the `<` opens a typar.
    assert_eq!(toks("<"), vec![Token::Less(false)]);
    assert_eq!(toks(">"), vec![Token::Greater(false)]);

    // Longer compounds still win by maximal-munch — these must NOT split into
    // Less + something.
    assert_eq!(toks("<-"), vec![Token::LArrow]);
    assert_eq!(toks("->"), vec![Token::RArrow]);
    assert_eq!(toks("<="), vec![Token::Op("<=")]);
    assert_eq!(toks(">="), vec![Token::Op(">=")]);
    assert_eq!(toks("<>"), vec![Token::Op("<>")]);
    assert_eq!(toks(">>"), vec![Token::Op(">>")]);
    assert_eq!(toks("<@"), vec![Token::LQuote]);
    assert_eq!(toks("<@@"), vec![Token::LQuoteRaw]);
    assert_eq!(toks("[<"), vec![Token::LBrackLess]);
    assert_eq!(toks(">]"), vec![Token::GreaterRBrack]);

    // Adjacent bare `<`/`>` separated by other tokens.
    assert_eq!(
        toks("a<b"),
        vec![Token::Ident("a"), Token::Less(false), Token::Ident("b")]
    );
    assert_eq!(
        toks("a>b"),
        vec![Token::Ident("a"), Token::Greater(false), Token::Ident("b")]
    );
}

#[test]
fn spans_cover_entire_input() {
    let src = "let x = 42 + foo // hi\nlet y = \"hello\"";
    let spans: Vec<_> = lex(src).map(|(_, s)| s).collect();
    // No gaps and no overlaps: each next.start == prev.end, last.end == src.len().
    let mut cursor = 0usize;
    for span in &spans {
        assert_eq!(span.start, cursor, "gap or overlap before span {span:?}");
        cursor = span.end;
    }
    assert_eq!(cursor, src.len());
}

#[test]
fn small_program() {
    let src = "let add x y = x + y";
    let ts = toks(src);
    assert_eq!(
        ts,
        vec![
            Token::Let,
            Token::Whitespace,
            Token::Ident("add"),
            Token::Whitespace,
            Token::Ident("x"),
            Token::Whitespace,
            Token::Ident("y"),
            Token::Whitespace,
            Token::Equals,
            Token::Whitespace,
            Token::Ident("x"),
            Token::Whitespace,
            Token::Op("+"),
            Token::Whitespace,
            Token::Ident("y"),
        ]
    );
}

#[test]
fn code_quotation_tokens() {
    // Openers: `<@` and `<@@` beat the generic Op regex (priority 2 > 1).
    assert_eq!(toks("<@"), vec![Token::LQuote]);
    assert_eq!(toks("<@@"), vec![Token::LQuoteRaw]);
    // Closers: `@>` and `@@>` also beat Op (priority 2 > 1).
    assert_eq!(toks("@>"), vec![Token::RQuote]);
    assert_eq!(toks("@@>"), vec![Token::RQuoteRaw]);
    // Compound forms: the raw lexer matches the full string to beat `Op`.
    // The lexfilter splits them into (R)Quote + Dot / BarRBrace before emitting.
    assert_eq!(toks("@>."), vec![Token::RQuoteDot]);
    assert_eq!(toks("@@>."), vec![Token::RQuoteRawDot]);
    assert_eq!(toks("@>|}"), vec![Token::RQuoteBarRBrace]);
    assert_eq!(toks("@@>|}"), vec![Token::RQuoteRawBarRBrace]);
}

#[test]
fn triple_interp_opener_beats_single() {
    // `$"""..."""` must take the triple-quoted opener arm rather than
    // falling through to `$"` + an empty single-quoted body. Logos
    // resolves the ambiguity by longest-literal-wins (4 bytes > 2).
    assert_eq!(
        toks("$\"\"\"hello\"\"\""),
        vec![Token::InterpString(InterpKind::TripleBeginEnd {
            is_byte: false
        })]
    );
    // The bare triple-quoted *interp* opener also beats the plain
    // triple-quoted *string* arm (`"""..."""`): the leading `$` is part
    // of the longer literal token.
    assert_eq!(
        toks("$\"\"\"\"\"\""),
        vec![Token::InterpString(InterpKind::TripleBeginEnd {
            is_byte: false
        })]
    );
    // Single-quoted opener still wins when there's no third quote.
    assert_eq!(
        toks("$\"hi\""),
        vec![Token::InterpString(InterpKind::BeginEnd { is_byte: false })]
    );
}

#[test]
fn triple_interp_opener_with_fill() {
    // Opener span includes the trailing `{`. The driver synthesises
    // the matching `Part`/`End` after the parent expression's `}`.
    let ts = toks("$\"\"\"a={1}b\"\"\"");
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::TripleBegin),
            Token::Int("1"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

#[test]
fn triple_interp_body_allows_embedded_quotes_and_newlines() {
    // 1- and 2-quote runs in a triple-quoted body are content; only
    // `"""` closes. Newlines in the body are also content.
    let ts = toks("$\"\"\"a \"b\" \"\"c\n\"\"\"");
    assert_eq!(
        ts,
        vec![Token::InterpString(InterpKind::TripleBeginEnd {
            is_byte: false
        })]
    );
}

#[test]
fn triple_interp_no_backslash_escape() {
    // `\"` in a triple body does NOT escape the quote — it's two
    // literal bytes `\` and `"`. The string then continues until
    // `"""`. (Pins the missing `\\X` arm in `lex_interp_triple_opener`.)
    let ts = toks("$\"\"\"\\n\"\"\"");
    assert_eq!(
        ts,
        vec![Token::InterpString(InterpKind::TripleBeginEnd {
            is_byte: false
        })]
    );
}

#[test]
fn single_interp_backslash_open_brace_starts_fill() {
    assert_eq!(
        toks("$\"\\{1}\""),
        vec![
            Token::InterpString(InterpKind::Begin),
            Token::Int("1"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

#[test]
fn single_interp_backslash_open_brace_starts_fill_in_continuation() {
    assert_eq!(
        toks("$\"{1}\\{2}\""),
        vec![
            Token::InterpString(InterpKind::Begin),
            Token::Int("1"),
            Token::InterpString(InterpKind::Part),
            Token::Int("2"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// `$"abc"B` — bare single-quoted interpolated string with a byte
/// suffix. FCS lexes this with `IsByteString = true` on the `Finish`
/// call (lex.fsl:1341) and downgrades the token to `BYTEARRAY` after
/// firing FS3377; we surface the byte fact on the closer token and
/// leave the diagnostic + recovery shape to the parser.
#[test]
fn interp_byte_suffix_single_bare() {
    let ts: Vec<_> = lex("$\"abc\"B").collect();
    assert_eq!(ts.len(), 1, "tokens: {ts:?}");
    let (tok, span) = &ts[0];
    assert_eq!(
        tok,
        &Ok(Token::InterpString(InterpKind::BeginEnd { is_byte: true })),
    );
    assert_eq!(span.clone(), 0..7);
}

/// `$"""abc"""B` — triple-quoted byte-interp. Same shape as the single
/// case, on `TripleBeginEnd`. The `B` is consumed greedily by the
/// `"""` arm of `lex_interp_triple_opener`.
#[test]
fn interp_byte_suffix_triple_bare() {
    let ts: Vec<_> = lex("$\"\"\"abc\"\"\"B").collect();
    assert_eq!(ts.len(), 1, "tokens: {ts:?}");
    let (tok, span) = &ts[0];
    assert_eq!(
        tok,
        &Ok(Token::InterpString(InterpKind::TripleBeginEnd {
            is_byte: true
        })),
    );
    assert_eq!(span.clone(), 0..11);
}

/// `$"a={x}"B` — fill-bearing single-quoted byte-interp. The opener
/// `$"a={` is a non-byte `Begin`; only the closing `}"B` carries the
/// byte fact, on the synthesised `End { is_byte: true }`.
#[test]
fn interp_byte_suffix_single_fill() {
    let ts: Vec<_> = lex("$\"a={x}\"B")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::Begin),
            Token::Ident("x"),
            Token::InterpString(InterpKind::End { is_byte: true }),
        ]
    );
}

/// `$"""a={x}"""B` — fill-bearing triple-quoted byte-interp. Same
/// closer-side byte fact, on the triple-style `End`.
#[test]
fn interp_byte_suffix_triple_fill() {
    let ts: Vec<_> = lex("$\"\"\"a={x}\"\"\"B")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::TripleBegin),
            Token::Ident("x"),
            Token::InterpString(InterpKind::End { is_byte: true }),
        ]
    );
}

/// `$@"hello"` / `@$"hello"` — both spellings of the bare verbatim interp
/// opener (`lex.fsl:687`, interchangeable). Each must lex as a single
/// `VerbatimBeginEnd` token, not split into `$`/`@`-prefixed pieces (the
/// 3-char `$@"` / `@$"` literals beat the 1-char `$` and the `@`/`@$` Op
/// regex run).
#[test]
fn verbatim_interp_opener_both_spellings() {
    assert_eq!(
        toks("$@\"hello\""),
        vec![Token::InterpString(InterpKind::VerbatimBeginEnd {
            is_byte: false
        })]
    );
    assert_eq!(
        toks("@$\"hello\""),
        vec![Token::InterpString(InterpKind::VerbatimBeginEnd {
            is_byte: false
        })]
    );
}

/// `$@"a={1}b"` / `@$"a={1}b"` — fill-bearing verbatim interp. The opener
/// span includes the trailing `{`; the driver synthesises the closing
/// `End` after the fill's `}`. Both spellings produce the same stream.
#[test]
fn verbatim_interp_opener_with_fill() {
    for src in ["$@\"a={1}b\"", "@$\"a={1}b\""] {
        assert_eq!(
            toks(src),
            vec![
                Token::InterpString(InterpKind::VerbatimBegin),
                Token::Int("1"),
                Token::InterpString(InterpKind::End { is_byte: false }),
            ],
            "src: {src}"
        );
    }
}

/// `$@"a""b"` — a doubled quote `""` inside a verbatim body is a literal
/// quote and does NOT terminate the string; the single `"` after `b`
/// closes it. So this is one bare `VerbatimBeginEnd`, not two tokens.
#[test]
fn verbatim_interp_doubled_quote_is_content() {
    assert_eq!(
        toks("$@\"a\"\"b\""),
        vec![Token::InterpString(InterpKind::VerbatimBeginEnd {
            is_byte: false
        })]
    );
}

/// `$@"\n"` — a backslash in a verbatim body is a literal byte, not an
/// escape (cf. the triple form). The body is the two characters `\` and
/// `n`; the closer is the single `"`.
#[test]
fn verbatim_interp_no_backslash_escape() {
    assert_eq!(
        toks("$@\"\\n\""),
        vec![Token::InterpString(InterpKind::VerbatimBeginEnd {
            is_byte: false
        })]
    );
}

/// `$@"{{"` — the interp brace digraph `{{` is a literal `{` in a verbatim
/// body (does not open a fill), so this is one bare `VerbatimBeginEnd`.
#[test]
fn verbatim_interp_brace_digraph_is_content() {
    assert_eq!(
        toks("$@\"{{\""),
        vec![Token::InterpString(InterpKind::VerbatimBeginEnd {
            is_byte: false
        })]
    );
}

/// `$@"abc"B` — bare verbatim byte-interp. The byte fact rides on the
/// `VerbatimBeginEnd` closer (span includes the trailing `B`); FCS fires
/// FS3377 and the parser recovers `SynConst.Bytes(Verbatim)`.
#[test]
fn verbatim_interp_byte_suffix_bare() {
    let ts: Vec<_> = lex("$@\"abc\"B").collect();
    assert_eq!(ts.len(), 1, "tokens: {ts:?}");
    let (tok, span) = &ts[0];
    assert_eq!(
        tok,
        &Ok(Token::InterpString(InterpKind::VerbatimBeginEnd {
            is_byte: true
        })),
    );
    assert_eq!(span.clone(), 0..8);
}

/// `$@"a={x}"B` — fill-bearing verbatim byte-interp. The opener `$@"a={`
/// is a non-byte `VerbatimBegin`; only the closing `}"B` carries the byte
/// fact, on the synthesised `End { is_byte: true }`.
#[test]
fn verbatim_interp_byte_suffix_fill() {
    let ts: Vec<_> = lex("$@\"a={x}\"B")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::VerbatimBegin),
            Token::Ident("x"),
            Token::InterpString(InterpKind::End { is_byte: true }),
        ]
    );
}

/// `$@"a={1}b={2}c"` — verbatim multi-fill. The driver splits consecutive
/// fills with `Part` (`}…{`) and closes with `End` (`}…"`); a doubled `""`
/// mid-body would be content, but here we pin the plain multi-fill chain.
#[test]
fn verbatim_interp_multi_fill_driver() {
    let ts: Vec<_> = lex("$@\"a={1}b={2}c\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::VerbatimBegin),
            Token::Int("1"),
            Token::InterpString(InterpKind::Part),
            Token::Int("2"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// `$@"a={1}b""c"` — a doubled quote `""` in the *continuation* fragment
/// after a fill is a literal quote, not a closer; the single `"` after the
/// final `c` closes the string. Pins the driver's `scan_cont` verbatim
/// branch (not just the opener callback).
#[test]
fn verbatim_interp_doubled_quote_in_continuation() {
    let ts: Vec<_> = lex("$@\"a={1}b\"\"c\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::VerbatimBegin),
            Token::Int("1"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// `$$"""hello"""` / `$$$"""hello"""` — bare extended (bracket-count) interp
/// openers. FCS's `('$'+) '"' '"' '"'` rule (lex.fsl:620) fires for N≥2
/// leading `$`; the interpolation delimiter length N equals the `$` count.
/// With no `{`-run ≥ N the whole string is bare → one `ExtendedBeginEnd`
/// carrying N.
#[test]
fn extended_interp_opener_bare() {
    assert_eq!(
        toks("$$\"\"\"hello\"\"\""),
        vec![Token::InterpString(InterpKind::ExtendedBeginEnd { n: 2 })]
    );
    assert_eq!(
        toks("$$$\"\"\"hello\"\"\""),
        vec![Token::InterpString(InterpKind::ExtendedBeginEnd { n: 3 })]
    );
}

/// The extended opener regex is anchored on `"""`, so a `$$` run not
/// followed by a triple quote keeps its ordinary `Op`/identifier lexing.
/// Regression guard that we don't swallow `$$x` / `$$ `.
#[test]
fn extended_interp_opener_regression() {
    assert_eq!(toks("$$x"), vec![Token::Op("$$"), Token::Ident("x")]);
    assert_eq!(toks("$$ "), vec![Token::Op("$$"), Token::Whitespace]);
}

/// `$$"""a={{1}}b={{2}}c"""` — extended multi-fill, N=2. A fill opens on a
/// `{`-run ≥ N (`{{`) and closes on a `}`-run of N (`}}`). The driver
/// splits consecutive fills with `Part` and closes with `End`.
#[test]
fn extended_interp_multi_fill_driver() {
    let ts: Vec<_> = lex("$$\"\"\"a={{1}}b={{2}}c\"\"\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::ExtendedBegin { n: 2 }),
            Token::Int("1"),
            Token::InterpString(InterpKind::Part),
            Token::Int("2"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// `$$$"""a={{{1}}}b"""` — extended multi-fill, N=3. The fill opens on
/// `{{{` (run = N = 3) and closes on `}}}`.
#[test]
fn extended_interp_multi_fill_n3() {
    let ts: Vec<_> = lex("$$$\"\"\"a={{{1}}}b\"\"\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::ExtendedBegin { n: 3 }),
            Token::Int("1"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// `$$"""{ }"""` / `$$"""a{b}c"""` — single braces (run < N) are literal
/// content in an N=2 extended body, so neither opens a fill: each is one
/// bare `ExtendedBeginEnd`.
#[test]
fn extended_interp_subdelim_braces_are_content() {
    assert_eq!(
        toks("$$\"\"\"{ }\"\"\""),
        vec![Token::InterpString(InterpKind::ExtendedBeginEnd { n: 2 })]
    );
    assert_eq!(
        toks("$$\"\"\"a{b}c\"\"\""),
        vec![Token::InterpString(InterpKind::ExtendedBeginEnd { n: 2 })]
    );
}

/// In an N=2 extended continuation fragment a single `}` is literal content
/// (a `}`-run must reach N to close), so `$$"""a={{1}}b}c"""` is `Begin`,
/// fill `1`, `End` — the lone `}` after `b` does not split the fragment.
/// Pins the driver's `scan_cont` extended branch.
#[test]
fn extended_interp_single_rbrace_in_content_is_literal() {
    let ts: Vec<_> = lex("$$\"\"\"a={{1}}b}c\"\"\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::ExtendedBegin { n: 2 }),
            Token::Int("1"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// A `}`-run *inside* a fill that is shorter than N must NOT close the fill —
/// FCS keeps the interpolation expression open until it sees a full N-`}` run
/// at brace-counter top level (`$$"""a={{x}y}}z"""`, N=2: the lone `}` between
/// `x` and `y` is an ordinary `RBrace` token in the fill, and only the later
/// `}}` closes). Pins the driver's depth-0 close guard.
#[test]
fn extended_interp_short_rbrace_run_does_not_close_fill() {
    let ts: Vec<_> = lex("$$\"\"\"a={{x}y}}z\"\"\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::ExtendedBegin { n: 2 }),
            Token::Ident("x"),
            Token::RBrace,
            Token::Ident("y"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// A long `%`-run is plain content to the boundary scanner — `%` never opens
/// or closes a fragment (only `{`/`}` runs ≥ N do). `$$"""%%%%{{x}}"""` (N=2)
/// still tokenises to `ExtendedBegin`, fill `x`, `End`; the `%%%%` rides in the
/// opener fragment. Pins that the `%`-transform (a parser/normaliser concern)
/// needs no driver change.
#[test]
fn extended_interp_percent_run_does_not_perturb_boundaries() {
    let ts: Vec<_> = lex("$$\"\"\"%%%%{{x}}\"\"\"")
        .map(|(t, _)| t.expect("lex error"))
        .collect();
    assert_eq!(
        ts,
        vec![
            Token::InterpString(InterpKind::ExtendedBegin { n: 2 }),
            Token::Ident("x"),
            Token::InterpString(InterpKind::End { is_byte: false }),
        ]
    );
}

/// `$$"""x"""B` — unlike regular single/triple interp, FCS does NOT treat a
/// trailing `B` as a byte suffix on an extended string (the
/// `extendedInterpolatedString` closer at lex.fsl:1641 has no byte arm). The
/// `"""` closes the string and `B` lexes as a separate identifier.
#[test]
fn extended_interp_trailing_b_is_separate_ident() {
    assert_eq!(
        toks("$$\"\"\"x\"\"\"B"),
        vec![
            Token::InterpString(InterpKind::ExtendedBeginEnd { n: 2 }),
            Token::Ident("B"),
        ]
    );
}
