use super::super::*;
use super::*;

/// `$"abc"B` — bare single-quoted interpolated string with a byte
/// suffix. FCS fires FS3377 ("a byte string may not be
/// interpolated") and downgrades the token to `BYTEARRAY`, recovering
/// `SynConst.Bytes([0x61; 0x62; 0x63], SynByteStringKind.Regular, _)`.
/// We mirror that: a `BYTE_STRING_LIT` node plus a parse error.
#[test]
fn lone_byte_interp_string_literal() {
    let source = "$\"abc\"B\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("byte string may not be interpolated"),
        "unexpected message: {:?}",
        parse.errors[0],
    );
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      CONST_EXPR@0..7
        BYTE_STRING_LIT@0..7 \"$\\\"abc\\\"B\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `$"""abc"""B` — triple-quoted byte-interp. Same FS3377 +
/// `SynByteStringKind.Regular` recovery as the single-quoted form;
/// the parser emits `TRIPLE_BYTE_STRING_LIT`.
#[test]
fn lone_triple_byte_interp_string_literal() {
    let source = "$\"\"\"abc\"\"\"B\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("byte string may not be interpolated"),
        "unexpected message: {:?}",
        parse.errors[0],
    );
    let expected = "\
IMPL_FILE@0..12
  MODULE_OR_NAMESPACE@0..12
    EXPR_DECL@0..11
      CONST_EXPR@0..11
        TRIPLE_BYTE_STRING_LIT@0..11 \"$\\\"\\\"\\\"abc\\\"\\\"\\\"B\"
    NEWLINE@11..12 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `$"a={x}"B` — fill-bearing byte-interp. FCS has no clean recovery
/// here (it emits `SynExpr.ArbitraryAfterError` + FS3377 + FS0010), so
/// we don't try to match its AST. We keep the ordinary
/// `INTERP_STRING_EXPR` shape and surface the FS3377-style diagnostic
/// at the byte-tagged `End` closer.
#[test]
fn byte_interp_with_fill_emits_error() {
    let source = "$\"a={x}\"B\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string may not be interpolated")),
        "expected FS3377-style error; got {:?}",
        parse.errors,
    );
    let has_interp = parse
        .root
        .descendants()
        .any(|n| n.kind() == SyntaxKind::INTERP_STRING_EXPR);
    assert!(has_interp, "expected an INTERP_STRING_EXPR node");
    assert_lossless(source, &parse);
}

/// `let $"abc"B = 1` — bare byte-interp in *pattern* position. FCS
/// downgrades the token to `BYTEARRAY` at the lexer and parses it
/// through the shared `constant` production, yielding
/// `SynPat.Const(SynConst.Bytes(_, Regular, _))` + FS3377 — exactly
/// like the plain `"abc"B` byte string already does. We mirror that:
/// a `CONST_PAT` head wrapping `BYTE_STRING_LIT`, plus the diagnostic.
#[test]
fn byte_interp_pattern_head_recovers_const() {
    let source = "let $\"abc\"B = 1\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string may not be interpolated")),
        "expected FS3377-style error; got {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Const(const_pat) = pat else {
        panic!("expected CONST_PAT head, got {pat:?}");
    };
    let lit = const_pat.literal().expect("CONST_PAT must own its literal");
    assert_eq!(lit.kind(), SyntaxKind::BYTE_STRING_LIT);
    assert_lossless(source, &parse);
}

/// `$"a={x}b={y}"` — two fills with literal text on both sides and
/// between. FCS produces `InterpolatedString([String "a="; FillExpr x;
/// String "b="; FillExpr y; String ""], Regular, _)`; we mirror that as
/// a five-part chain: `Begin` fragment, fill, `Part` fragment, fill,
/// `End` fragment.
#[test]
fn interp_string_two_fills() {
    let source = "$\"a={x}b={y}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$\"a={", "<fill>", "}b={", "<fill>", "}\""],
    );
    assert_lossless(source, &parse);
}

/// `$"{x}{y}"` — two adjacent fills, no surrounding text. The middle
/// `Part` fragment's body is empty (`}{`), as are the leading/trailing
/// fragments.
#[test]
fn interp_string_two_fills_no_text() {
    let source = "$\"{x}{y}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$\"{", "<fill>", "}{", "<fill>", "}\""],
    );
    assert_lossless(source, &parse);
}

/// `$"{a}{b}{c}"` — three fills. Pins that the fill-loop handles N>2:
/// four fragments (`Begin`, two `Part`s, `End`) interleaved with three
/// fills.
#[test]
fn interp_string_three_fills() {
    let source = "$\"{a}{b}{c}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$\"{", "<fill>", "}{", "<fill>", "}{", "<fill>", "}\""],
    );
    assert_lossless(source, &parse);
}

/// `$"""a={x}b={y}"""` — triple-quoted multi-fill. Same five-part shape
/// as the single-quoted form, with triple-quote delimiters on the
/// bracketing fragments.
#[test]
fn triple_interp_string_two_fills() {
    let source = "$\"\"\"a={x}b={y}\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$\"\"\"a={", "<fill>", "}b={", "<fill>", "}\"\"\""],
    );
    assert_lossless(source, &parse);
}

/// `$"{a:N2}b={c}"` — the first fill carries a `: ident` format
/// qualifier, the second does not. The qualifier tokens are bumped at
/// the `INTERP_STRING_EXPR` level (so `parts()` ignores them); the
/// chain still parses cleanly as two fills.
#[test]
fn interp_string_fill_with_qualifier_then_fill() {
    let source = "$\"{a:N2}b={c}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$\"{", "<fill>", "}b={", "<fill>", "}\""],
    );
    assert_lossless(source, &parse);
}

/// `$"{a:N2}b={c}"` — `parts()` attaches the `: ident` qualifier to the
/// fill it trails (first fill gets `N2`, second gets none), rather than
/// dropping it. Pins that the qualifier `IDENT_TOK`, bumped at the
/// `INTERP_STRING_EXPR` level, is associated with the correct fill.
#[test]
fn interp_string_fill_qualifier_association() {
    let source = "$\"{a:N2}b={c}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_fill_qualifiers(&parse),
        vec![Some("N2".to_string()), None],
    );
    assert_lossless(source, &parse);
}

/// `$"{a} {b:N2} {c}"` — only the *middle* fill carries a qualifier.
/// Pins that a bare fill before a qualified one stays bare, i.e. the
/// qualifier isn't smeared onto the wrong fill.
#[test]
fn interp_string_fill_qualifier_middle_only() {
    let source = "$\"{a} {b:N2} {c}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_fill_qualifiers(&parse),
        vec![None, Some("N2".to_string()), None],
    );
    assert_lossless(source, &parse);
}

/// `$"x={ $"y" }"` — a single-quoted interp nested inside a
/// single-quoted interp's fill. FCS fires FS3373
/// (`lexSingleQuoteInSingleQuote`) at the inner opener and still
/// recovers the nested `InterpolatedString` tree. We mirror the
/// diagnostic; the nested tree was already built via recursion.
#[test]
fn nested_interp_single_in_single_emits_fs3373() {
    let source = "$\"x={ $\"y\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected FS3373-style message; got {:?}",
        parse.errors[0],
    );
    // Pinned at the inner `$"` delimiter (bytes 6..8), not the whole
    // `$"y"` opener fragment.
    assert_eq!(parse.errors[0].span, 6..8);
    assert_eq!(
        interp_node_count(&parse),
        2,
        "expected outer + inner interp"
    );
    assert_lossless(source, &parse);
}

/// `$"x={ $"y={1}" }"` — single-in-single where the inner interp itself
/// has a fill. Same FS3373 at the inner opener; the inner fill parses
/// normally.
#[test]
fn nested_interp_single_in_single_with_fill() {
    let source = "$\"x={ $\"y={1}\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected FS3373-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$"x={ $"""y""" }"` — a *triple*-quoted interp nested inside a
/// single-quoted interp. FCS fires FS3374
/// (`lexTripleQuoteInTripleQuote`) — triple inner is always an error
/// regardless of the enclosing style.
#[test]
fn nested_interp_triple_inner_emits_fs3374() {
    let source = "$\"x={ $\"\"\"y\"\"\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("Triple quote string literals"),
        "expected FS3374-style message; got {:?}",
        parse.errors[0],
    );
    // Pinned at the inner `$"""` delimiter (bytes 6..10), not the whole
    // `$"""y"""` opener fragment.
    assert_eq!(parse.errors[0].span, 6..10);
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$"""a={ $"""b""" }"""` — triple inner inside a triple outer. Still
/// FS3374: triple-quote inner strings may never appear in an
/// interpolated expression, even inside a triple-quoted outer.
#[test]
fn nested_interp_triple_in_triple_emits_fs3374() {
    let source = "$\"\"\"a={ $\"\"\"b\"\"\" }\"\"\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("Triple quote string literals"),
        "expected FS3374-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$"""a={ $"b" }"""` — a single-quoted interp nested inside a
/// *triple*-quoted interp. This is the one legal nesting (FCS's own
/// recommended workaround): no diagnostic. The nested tree is still
/// built.
#[test]
fn nested_interp_single_in_triple_is_clean() {
    let source = "$\"\"\"a={ $\"b\" }\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$"""a={ $"b={ $"c" }" }"""` — three levels. The middle `$"b…"` is
/// single-in-triple (legal); the innermost `$"c"` is single-in-single
/// (FS3373). Exactly one error, validating the per-level enclosing-style
/// stack rather than a single global flag.
#[test]
fn nested_interp_deep_single_in_single_in_triple() {
    let source = "$\"\"\"a={ $\"b={ $\"c\" }\" }\"\"\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected exactly the innermost FS3373; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 3);
    assert_lossless(source, &parse);
}

/// `$"x={ $"y"B }"` — a byte-suffixed bare interp nested inside a
/// single-quoted interp. FCS fires *both* FS3377 (byte string may not
/// be interpolated) and FS3373 (single-in-single), so the nesting check
/// must fire independently of the byte-recovery branch.
#[test]
fn nested_interp_byte_inner_emits_both() {
    let source = "$\"x={ $\"y\"B }\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string")),
        "expected FS3377-style byte error; got {:?}",
        parse.errors,
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("Single quote or verbatim")),
        "expected FS3373-style nesting error; got {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `$"x={ "y" }"` — an *ordinary* single-quoted string inside a
/// single-quoted interp fill. FCS's FS3373 rule covers plain string
/// literals, not only nested interp openers, so this fires the same
/// diagnostic. The inner is a plain `STRING_LIT`, not an interp, so only
/// the outer `INTERP_STRING_EXPR` node exists.
#[test]
fn nested_plain_string_in_single_emits_fs3373() {
    let source = "$\"x={ \"y\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected FS3373-style message; got {:?}",
        parse.errors[0],
    );
    // The whole `"y"` literal span (bytes 6..9) — nothing foreign bleeds
    // in, unlike an interp opener fragment.
    assert_eq!(parse.errors[0].span, 6..9);
    assert_eq!(interp_node_count(&parse), 1, "inner is a plain string");
    assert_lossless(source, &parse);
}

/// `$"x={ @"y" }"` — a verbatim string inside a single-quoted fill. FCS
/// folds verbatim into the single/verbatim FS3373 case.
#[test]
fn nested_verbatim_string_in_single_emits_fs3373() {
    let source = "$\"x={ @\"y\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected FS3373-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"x={ "y"B }"` — a byte string inside a single-quoted fill. FCS fires
/// FS3373 (the byte suffix doesn't change the single/verbatim verdict),
/// and *only* FS3373 — unlike the byte *interp* opener which adds FS3377.
#[test]
fn nested_byte_string_in_single_emits_fs3373() {
    let source = "$\"x={ \"y\"B }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected FS3373-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"x={ """y""" }"` — a triple string inside a single-quoted fill. A
/// triple inner is always FS3374, regardless of enclosing style.
#[test]
fn nested_triple_string_in_single_emits_fs3374() {
    let source = "$\"x={ \"\"\"y\"\"\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("Triple quote string literals"),
        "expected FS3374-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"""x={ """y""" }"""` — triple string inside a triple fill. Still
/// FS3374.
#[test]
fn nested_triple_string_in_triple_emits_fs3374() {
    let source = "$\"\"\"x={ \"\"\"y\"\"\" }\"\"\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("Triple quote string literals"),
        "expected FS3374-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"""x={ "y" }"""` — a single-quoted string inside a *triple* fill.
/// Legal (the single-in-triple workaround), so no diagnostic.
#[test]
fn nested_plain_string_in_triple_is_clean() {
    let source = "$\"\"\"x={ \"y\" }\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"x={ 'y' }"` — a char literal inside a single-quoted fill. Char
/// literals are exempt from the nested-string rule (FCS-legal).
#[test]
fn nested_char_in_single_is_clean() {
    let source = "$\"x={ 'y' }\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"x={ fun "y" -> 1 }"` — an ordinary string used as a *pattern*
/// (the lambda parameter) inside a single-quoted fill. The nesting check
/// fires from the shared `parse_const_payload` path regardless of
/// expr-vs-pattern position, so this is FS3373 like the expression form.
#[test]
fn nested_plain_string_pattern_in_single_emits_fs3373() {
    let source = "$\"x={ fun \"y\" -> 1 }\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("Single quote or verbatim")),
        "expected FS3373-style nesting error; got {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `$"x={ fun $"y"B -> 1 }"` — a byte-suffixed bare interp used as a
/// *pattern* inside a single-quoted fill. FCS fires *both* FS3377 (byte)
/// and FS3373 (single-in-single). The byte-interp const-pattern arm must
/// run the nesting check too, not just the expression path.
#[test]
fn nested_byte_interp_pattern_in_single_emits_both() {
    let source = "$\"x={ fun $\"y\"B -> 1 }\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string")),
        "expected FS3377-style byte error; got {:?}",
        parse.errors,
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("Single quote or verbatim")),
        "expected FS3373-style nesting error; got {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `$"""x={ fun $"y"B -> 1 }"""` — the same byte-interp pattern inside a
/// *triple* fill. The inner `$"y"B` is single-in-triple, which is legal,
/// so FCS emits *only* FS3377 (byte) and no nesting diagnostic.
#[test]
fn nested_byte_interp_pattern_in_triple_emits_byte_only() {
    let source = "$\"\"\"x={ fun $\"y\"B -> 1 }\"\"\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string")),
        "expected FS3377-style byte error; got {:?}",
        parse.errors,
    );
    assert!(
        !parse
            .errors
            .iter()
            .any(|e| e.message.contains("Single quote or verbatim")),
        "single-in-triple is legal; expected no FS3373, got {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `$@"hello"` — bare verbatim interpolated string (no fills). FCS lexes
/// the `$@"` opener (`lex.fsl:687`) to `LexerStringStyle.Verbatim` and
/// produces `InterpolatedString([String "hello"], Verbatim, _)` with no
/// errors. One `INTERP_STRING_EXPR` node whose single fragment is the
/// whole literal.
#[test]
fn verbatim_interp_bare_dollar_at() {
    let source = "$@\"hello\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_eq!(interp_part_shapes(&parse), vec!["$@\"hello\""]);
    assert_lossless(source, &parse);
}

/// `@$"hello"` — the other spelling of the bare verbatim opener. FCS
/// treats `$@` and `@$` interchangeably; same Verbatim recovery.
#[test]
fn verbatim_interp_bare_at_dollar() {
    let source = "@$\"hello\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_eq!(interp_part_shapes(&parse), vec!["@$\"hello\""]);
    assert_lossless(source, &parse);
}

/// `$@"a={x}b={y}"` — verbatim multi-fill. Same five-part chain as the
/// single-quoted form, with the `$@"` opener / `"` closer delimiters on
/// the bracketing fragments.
#[test]
fn verbatim_interp_two_fills() {
    let source = "$@\"a={x}b={y}\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$@\"a={", "<fill>", "}b={", "<fill>", "}\""],
    );
    assert_lossless(source, &parse);
}

/// `$@"abc"B` — bare verbatim byte-interp. FCS fires FS3377 ("a byte
/// string may not be interpolated") and recovers `SynConst.Bytes(_,
/// SynByteStringKind.Verbatim, _)`. We route that to
/// `VERBATIM_BYTE_STRING_LIT` (the normaliser projects it as Verbatim
/// bytes).
#[test]
fn verbatim_byte_interp_string_literal() {
    let source = "$@\"abc\"B\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0]
            .message
            .contains("byte string may not be interpolated"),
        "unexpected message: {:?}",
        parse.errors[0],
    );
    let expected = "\
IMPL_FILE@0..9
  MODULE_OR_NAMESPACE@0..9
    EXPR_DECL@0..8
      CONST_EXPR@0..8
        VERBATIM_BYTE_STRING_LIT@0..8 \"$@\\\"abc\\\"B\"
    NEWLINE@8..9 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `$"a={ $@"y" }"` — a verbatim interp nested inside a single-quoted
/// fill. A verbatim opener inside a single fill is the
/// single/verbatim-in-single case, so FCS fires FS3373 and still
/// recovers the nested tree (outer + inner interp).
#[test]
fn nested_verbatim_interp_in_single_emits_fs3373() {
    let source = "$\"a={ $@\"y\" }\"\n";
    let parse = parse(source);
    assert_eq!(parse.errors.len(), 1, "errors: {:?}", parse.errors);
    assert!(
        parse.errors[0].message.contains("Single quote or verbatim"),
        "expected FS3373-style message; got {:?}",
        parse.errors[0],
    );
    assert_eq!(
        interp_node_count(&parse),
        2,
        "expected outer + inner interp"
    );
    assert_lossless(source, &parse);
}

/// `$"""a={ $@"y" }"""` — a verbatim interp nested inside a *triple*
/// fill. Verbatim-in-triple is legal (the single/verbatim-in-triple
/// workaround), so no diagnostic; the nested tree is still built.
#[test]
fn nested_verbatim_interp_in_triple_is_clean() {
    let source = "$\"\"\"a={ $@\"y\" }\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$"x={ fun $@"y"B -> 1 }"` — a verbatim byte-interp used as a
/// *pattern* inside a single-quoted fill. FCS fires *both* FS3377 (byte)
/// and FS3373 (verbatim-in-single). The byte-interp const-pattern arm
/// must run the nesting check too, like the single-quoted byte form.
#[test]
fn nested_verbatim_byte_interp_pattern_in_single_emits_both() {
    let source = "$\"x={ fun $@\"y\"B -> 1 }\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string")),
        "expected FS3377-style byte error; got {:?}",
        parse.errors,
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("Single quote or verbatim")),
        "expected FS3373-style nesting error; got {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `$$"""hello"""` — bare extended interp (N=2). One `INTERP_STRING_EXPR`,
/// no fills, no errors; the single fragment retains the `$$"""…"""`
/// delimiters.
#[test]
fn extended_interp_bare() {
    let source = "$$\"\"\"hello\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_eq!(interp_part_shapes(&parse), vec!["$$\"\"\"hello\"\"\""]);
    assert_lossless(source, &parse);
}

/// `$$"""{ }"""` — single braces (run 1 < N=2) are literal content, not a
/// fill. Stays a bare one-fragment interp with no errors.
#[test]
fn extended_interp_literal_braces_are_content() {
    let source = "$$\"\"\"{ }\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_eq!(interp_part_shapes(&parse), vec!["$$\"\"\"{ }\"\"\""]);
    assert_lossless(source, &parse);
}

/// `$$"""a={{1}}b={{2}}c"""` — extended multi-fill (N=2). A `{{`-run opens a
/// fill, a `}}`-run closes it; five-part chain like the other styles, with
/// the `{{`/`}}` two-brace delimiters on the bracketing fragments.
#[test]
fn extended_interp_two_fills() {
    let source = "$$\"\"\"a={{1}}b={{2}}c\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$$\"\"\"a={{", "<fill>", "}}b={{", "<fill>", "}}c\"\"\""],
    );
    assert_lossless(source, &parse);
}

/// `$$$"""a={{{1}}}b"""` — N=3: the fill delimiter is three braces. A `{{{`
/// opens, `}}}` closes; runs of fewer than three are content.
#[test]
fn extended_interp_n3_fill() {
    let source = "$$$\"\"\"a={{{1}}}b\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        interp_part_shapes(&parse),
        vec!["$$$\"\"\"a={{{", "<fill>", "}}}b\"\"\""],
    );
    assert_lossless(source, &parse);
}

/// `$$"""a{{{{1}}}}b"""` — a fill-opening `{`-run of 4 (≥ 2N=4) is FS1248:
/// FCS still opens the fill, but warns there aren't enough `$` to take the
/// surplus braces as content. The trailing `}}}}` likewise leaves a
/// content `}}`-run (≥ N) → FS1249. The interp node still recovers.
#[test]
fn extended_interp_fs1248_too_many_lbraces() {
    let source = "$$\"\"\"a{{{{1}}}}b\"\"\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("consecutive opening braces")),
        "expected FS1248-style message; got {:?}",
        parse.errors,
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$$$"""a}}}b"""` — a content `}`-run of 3 (= N) is unmatched: FS1249.
/// The run is dropped from the decoded text; the node still recovers.
#[test]
fn extended_interp_fs1249_unmatched_rbraces() {
    let source = "$$$\"\"\"a}}}b\"\"\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("unmatched closing braces")),
        "expected FS1249-style message; got {:?}",
        parse.errors,
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$$"""a%%%%d{{x}}b"""` — d=2, so a `%`-run of 4 (= 2d) is over-long:
/// FS1250. FCS drops the whole run from the decoded text but still recovers
/// the interp node.
#[test]
fn extended_interp_fs1250_too_many_percents() {
    let source = "$$\"\"\"a%%%%d{{x}}b\"\"\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("consecutive '%' characters")),
        "expected FS1250-style message; got {:?}",
        parse.errors,
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$$$"""a%%%%%%{{{x}}}"""` — d=3, so a `%`-run of 6 (= 2d) is over-long:
/// FS1250. Exercises the `2d` boundary for d=3.
#[test]
fn extended_interp_fs1250_n3_too_many_percents() {
    let source = "$$$\"\"\"a%%%%%%{{{x}}}\"\"\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("consecutive '%' characters")),
        "expected FS1250-style message; got {:?}",
        parse.errors,
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$$"""a%%%d{{x}}b"""` — d=2, a `%`-run of 3 (∈ [d, 2d-1]) is legal (one
/// format `%` + a doubled surplus). No FS1250.
#[test]
fn extended_interp_percent_run_in_range_is_clean() {
    let source = "$$\"\"\"a%%%d{{x}}b\"\"\"\n";
    let parse = parse(source);
    assert!(
        !parse
            .errors
            .iter()
            .any(|e| e.message.contains("consecutive '%' characters")),
        "expected no FS1250; got {:?}",
        parse.errors,
    );
    assert_eq!(interp_node_count(&parse), 1);
    assert_lossless(source, &parse);
}

/// `$"a={ $$"""y""" }"` — an extended interp nested inside a single-quoted
/// fill. Extended is triple-like, so FCS fires FS3374 (triple-quote in
/// interp) and still recovers the nested tree (outer + inner interp).
#[test]
fn nested_extended_interp_in_single_emits_fs3374() {
    let source = "$\"a={ $$\"\"\"y\"\"\" }\"\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("Triple quote string literals")),
        "expected FS3374-style message; got {:?}",
        parse.errors,
    );
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$$"""a={{ $"y" }}b"""` — a single-quoted interp nested inside an
/// extended fill. Single-in-extended is legal (like single-in-triple): no
/// diagnostic. The nested tree is still built.
#[test]
fn nested_single_interp_in_extended_is_clean() {
    let source = "$$\"\"\"a={{ $\"y\" }}b\"\"\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 2);
    assert_lossless(source, &parse);
}

/// `$$"""x"""B` — the extended closer (`lex.fsl:1641`) has no byte arm, so
/// the `B` is a *separate* identifier: FCS yields `App(interp, ident "B")`
/// with no FS3377. We mirror that — one bare interp node, no errors, and
/// the `B` parses as a following expression rather than a byte suffix.
#[test]
fn extended_interp_trailing_b_is_not_byte() {
    let source = "$$\"\"\"x\"\"\"B\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(interp_node_count(&parse), 1);
    assert_eq!(interp_part_shapes(&parse), vec!["$$\"\"\"x\"\"\""]);
    assert_lossless(source, &parse);
}

// ---- FS1245 in interpolated strings --------------------------------------
//
// A `\U........` escape > U+10FFFF is FS1245 in an *escape-processing*
// interpolated string — single-quoted only (`$"…"`, `$"…{`, and their
// `}…{` / `}…"` continuations). FCS emits one error per offending escape
// across every literal fragment; verbatim/triple/extended interp don't
// honour backslash escapes and so never flag. Confirmed against FCS.

/// Count FS1245 errors (by message) and assert losslessness.
fn fs1245_count(source: &str) -> usize {
    let parse = parse(source);
    assert_lossless(source, &parse);
    parse
        .errors
        .iter()
        .filter(|e| {
            e.message
                .contains("not a valid Unicode character escape sequence")
        })
        .count()
}

/// A bare single-interp escape is FS1245; mid-text placement is the same.
#[test]
fn interp_single_long_unicode_overflow_is_rejected() {
    assert_eq!(fs1245_count("let s = $\"\\U00110000\"\n"), 1);
    assert_eq!(fs1245_count("let s = $\"a\\U00110000b\"\n"), 1);
}

/// The escape is flagged in the opener fragment (before the first fill) and
/// in the `End` fragment (after the last fill).
#[test]
fn interp_long_unicode_in_opener_and_end_fragments() {
    assert_eq!(fs1245_count("let s = $\"\\U00110000{1}\"\n"), 1);
    assert_eq!(fs1245_count("let s = $\"{1}\\U00110000\"\n"), 1);
}

/// One FS1245 per offending escape, across opener + `Part` + `End`.
#[test]
fn interp_long_unicode_each_fragment_reported() {
    assert_eq!(fs1245_count("let s = $\"\\U00110000{1}\\U00110000\"\n"), 2);
    assert_eq!(
        fs1245_count("let s = $\"\\U00110000{1}\\U00110000{2}\\UFFFFFFFF\"\n"),
        3,
    );
}

/// Verbatim / triple / extended interp don't process backslash escapes, so
/// an out-of-range `\U` is literal text — no FS1245.
#[test]
fn interp_non_escape_kinds_do_not_flag_long_unicode() {
    assert_eq!(fs1245_count("let s = $@\"\\U00110000\"\n"), 0);
    assert_eq!(fs1245_count("let s = $\"\"\"\\U00110000\"\"\"\n"), 0);
    assert_eq!(fs1245_count("let s = $$\"\"\"\\U00110000\"\"\"\n"), 0);
}

/// Surrogate `\u`/BMP `\U` escapes, brace digraphs, and an escaped
/// backslash are all inert — same scanner guarantees as plain strings.
#[test]
fn interp_accepts_surrogates_digraphs_and_escaped_backslash() {
    assert_eq!(fs1245_count("let s = $\"\\uD800\"\n"), 0);
    assert_eq!(fs1245_count("let s = $\"a{{b}}\\U00110000\"\n"), 1);
    assert_eq!(fs1245_count("let s = $\"\\\\U00110000\"\n"), 0);
}

/// A byte single-interp (`$"…"B`) is itself FS3377, but FCS *also* emits
/// FS1245 for an out-of-range escape in it. We surface both.
#[test]
fn interp_byte_single_reports_both_fs1245_and_fs3377() {
    let source = "let s = $\"\\U00110000\"B\n";
    let parse = parse(source);
    assert!(
        parse.errors.iter().any(|e| e
            .message
            .contains("not a valid Unicode character escape sequence")),
        "expected FS1245; errors: {:?}",
        parse.errors,
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string may not be interpolated")),
        "expected FS3377; errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// The FS1245 span points at the 10-char `\U........` escape itself, as FCS
/// reports it. `let s = $"\U00110000"` puts `\U` at byte offset 10.
#[test]
fn interp_long_unicode_error_span_is_the_escape() {
    let source = "let s = $\"\\U00110000\"\n";
    let parse = parse(source);
    let e = parse
        .errors
        .iter()
        .find(|e| {
            e.message
                .contains("not a valid Unicode character escape sequence")
        })
        .expect("an FS1245 error");
    assert_eq!(e.span, 10..20, "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
}

/// A bare byte single-interp in *pattern* position routes through
/// `parse_const_payload`'s byte-recovery branch, not
/// `parse_interp_string_expr` — but FCS still reports FS1245 (then FS3377)
/// while lexing the token, so the const/pattern path must scan too.
#[test]
fn interp_byte_single_pattern_reports_both_fs1245_and_fs3377() {
    let source = "let f x = match x with | $\"\\U00110000\"B -> 1 | _ -> 2\n";
    let parse = parse(source);
    assert!(
        parse.errors.iter().any(|e| e
            .message
            .contains("not a valid Unicode character escape sequence")),
        "expected FS1245; errors: {:?}",
        parse.errors,
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("byte string may not be interpolated")),
        "expected FS3377; errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Verbatim/triple byte-interp patterns don't honour backslash escapes, so an
/// out-of-range `\U` is literal text — FS3377 only, no FS1245.
#[test]
fn interp_byte_verbatim_triple_pattern_no_fs1245() {
    for source in [
        "let f x = match x with | $@\"\\U00110000\"B -> 1 | _ -> 2\n",
        "let f x = match x with | $\"\"\"\\U00110000\"\"\"B -> 1 | _ -> 2\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.iter().any(|e| e
                .message
                .contains("not a valid Unicode character escape sequence")),
            "unexpected FS1245 for {source:?}: {:?}",
            parse.errors,
        );
        assert_lossless(source, &parse);
    }
}
