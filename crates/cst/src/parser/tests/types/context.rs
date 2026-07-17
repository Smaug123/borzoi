//! Type annotations in non-type positions: `open type` decls, typed let-binding patterns, and the typed-expression paren/colon recovery hook.
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 8.1 — `open type System.Math`. The `type` keyword is swallowed by
/// LexFilter and recovered from the raw stream as `TYPE_TOK`; the trailing
/// type parses via `parse_type` to a `LONG_IDENT_TYPE`. Shape:
/// `OPEN_DECL > [OPEN_TOK, TYPE_TOK, LONG_IDENT_TYPE > LONG_IDENT]`.
#[test]
fn open_type_decl() {
    let source = "open type System.Math\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..22
  MODULE_OR_NAMESPACE@0..22
    OPEN_DECL@0..21
      OPEN_TOK@0..4 \"open\"
      WHITESPACE@4..5 \" \"
      TYPE_TOK@5..9 \"type\"
      WHITESPACE@9..10 \" \"
      LONG_IDENT_TYPE@10..21
        LONG_IDENT@10..21
          IDENT_TOK@10..16 \"System\"
          DOT_TOK@16..17 \".\"
          IDENT_TOK@17..21 \"Math\"
    NEWLINE@21..22 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 8.1 recovery — a bare `open` on its own line, immediately
/// followed by a new declaration starting with the (swallowed) `type`
/// keyword, must NOT absorb that `type` into the `OPEN_DECL`. The layout
/// gate stops at the `OBLOCKSEP` boundary: `OPEN_DECL` holds only
/// `[OPEN_TOK, LONG_IDENT(empty)]` and an "expected identifier" error is
/// recorded; the following `type Foo = int` is left for the outer loop
/// (it lands in ERROR handling — `type` defs arrive in phase 9). Without
/// the gate the raw-stream `type` lookahead would cross the newline and
/// claim line 2's `type` as a `TYPE_TOK` child of `OPEN_DECL`.
#[test]
fn open_alone_does_not_absorb_following_type_decl() {
    let source = "open\ntype Foo = int\n";
    let parse = parse(source);
    let open = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::OPEN_DECL)
        .expect("an OPEN_DECL node");
    assert!(
        !open
            .descendants_with_tokens()
            .any(|el| el.kind() == SyntaxKind::TYPE_TOK),
        "OPEN_DECL absorbed a TYPE_TOK across the layout boundary:\n{}",
        debug_tree(&open),
    );
    assert!(
        !open.text().to_string().contains("type"),
        "OPEN_DECL text crossed into the `type` decl: {:?}",
        open.text().to_string(),
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected identifier after `open`")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Phase 8.1 — a `global`-rooted `open type` target
/// (`open type global.System.Math`) now parses cleanly: `global` is admitted as
/// a type-path root (phase 7 gained the `global`-rooted type path), so the
/// `OPEN_DECL` carries a `LONG_IDENT_TYPE` for `global.System.Math` with no
/// error. Byte-for-byte agreement with FCS is pinned by the diff test
/// `diff_ast_open_type_global_qualified` in `parser_diff_module_structure.rs`;
/// this unit test guards the built shape and losslessness.
#[test]
fn open_type_global_qualified_is_clean() {
    use crate::syntax::AstNode;
    let source = "open type global.System.Math\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let open_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::OPEN_DECL)
        .expect("an OPEN_DECL node");
    let open = crate::syntax::OpenDecl::cast(open_node).expect("OpenDecl");
    assert!(open.is_type(), "the `type` keyword is still recognised");
    let ty = open
        .ty()
        .expect("a `global`-rooted type target should now be built");
    assert!(
        matches!(ty, crate::syntax::Type::LongIdent(_)),
        "expected a LONG_IDENT_TYPE target, got {ty:?}",
    );
    assert_lossless(source, &parse);
}

/// Phase 6.2 — `let (x : int) = 1`: the canonical typed-pattern
/// shape. FCS only reaches `SynPat.Typed` through `parenPattern COLON
/// typeWithTypeConstraints` (`pars.fsy:3929`), so the `TYPED_PAT`
/// must always sit *inside* a `PAREN_PAT`. The inner pattern is
/// `NAMED_PAT` for `x`; the type is a `LONG_IDENT_TYPE` for `int`.
#[test]
fn let_binding_typed_named_value_head() {
    let source = "let (x : int) = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    let crate::syntax::Pat::Typed(typed) = inner else {
        panic!("expected TYPED_PAT inside PAREN_PAT, got {inner:?}");
    };
    let annotated = typed.pat().expect("TYPED_PAT must have an inner pat");
    assert!(
        matches!(annotated, crate::syntax::Pat::Named(_)),
        "expected NAMED_PAT inside TYPED_PAT for `x`, got {annotated:?}",
    );
    let ty = typed.ty().expect("TYPED_PAT must have a type annotation");
    assert!(
        matches!(ty, crate::syntax::Type::LongIdent(_)),
        "expected LONG_IDENT_TYPE for `int`, got {ty:?}",
    );
}

/// Phase 6.2 — `let (_ : int) = 1`: the wildcard variant of the
/// typed-pat shape. Same structural assertion as the named-inner
/// case, but the annotated pattern is `WILDCARD_PAT`.
#[test]
fn let_binding_typed_wildcard_value_head() {
    let source = "let (_ : int) = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    let crate::syntax::Pat::Typed(typed) = inner else {
        panic!("expected TYPED_PAT inside PAREN_PAT, got {inner:?}");
    };
    let annotated = typed.pat().expect("TYPED_PAT must have an inner pat");
    assert!(
        matches!(annotated, crate::syntax::Pat::Wildcard(_)),
        "expected WILDCARD_PAT inside TYPED_PAT for `_`, got {annotated:?}",
    );
    let ty = typed.ty().expect("TYPED_PAT must have a type annotation");
    assert!(
        matches!(ty, crate::syntax::Type::LongIdent(_)),
        "expected LONG_IDENT_TYPE for `int`, got {ty:?}",
    );
}

/// Phase 6.2 — `let f (x : int) = x`: typed-pat as a function-form
/// argument. The arg slot of the `LONG_IDENT_PAT` head is itself a
/// `PAREN_PAT > TYPED_PAT`, exactly like the value-head case.
#[test]
fn let_binding_function_form_typed_arg() {
    let source = "let f (x : int) = x\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(args.len(), 1, "expected exactly one arg, got {args:?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("expected PAREN_PAT arg, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    let crate::syntax::Pat::Typed(typed) = inner else {
        panic!("expected TYPED_PAT inside PAREN_PAT arg, got {inner:?}");
    };
    let annotated = typed.pat().expect("TYPED_PAT must have an inner pat");
    assert!(
        matches!(annotated, crate::syntax::Pat::Named(_)),
        "expected NAMED_PAT inside TYPED_PAT for `x`, got {annotated:?}",
    );
    let ty = typed.ty().expect("TYPED_PAT must have a type annotation");
    assert!(
        matches!(ty, crate::syntax::Type::LongIdent(_)),
        "expected LONG_IDENT_TYPE for `int`, got {ty:?}",
    );
}

/// Phase 7.1 regression — `(x) : int` is the outer-typed form, which
/// phase 7.1 explicitly defers. The hook must NOT fire here: LexFilter
/// swallows the `)`, so `self.peek()` on the filtered stream returns
/// `Colon`, but the `)` still sits in the raw stream between the inner
/// expression and that colon. Naïvely triggering the hook (codex P2)
/// drains the real `)` as `ERROR` inside `TYPED_EXPR` and then can't
/// find a closing paren — wrong tree, two spurious errors.
///
/// The fix gates the hook on the next non-trivia *raw* token being
/// `Token::Colon`. With `)` in the way, the gate fails and the inner
/// paren expression closes cleanly; the dangling `: int` at the top
/// level is recovered as parse errors per the existing module-decl
/// recovery path (`needs_sep`-style).
#[test]
fn paren_then_outer_colon_does_not_fire_typed_hook() {
    let source = "(x) : int\n";
    let parse = parse(source);
    // The inner `(x)` is a clean PAREN_EXPR with no TYPED_EXPR inside,
    // and no ERROR child for the closing `)`.
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for the inner `(x)`");
    assert!(
        !paren
            .descendants()
            .any(|n| n.kind() == SyntaxKind::TYPED_EXPR),
        "the hook must not fire when the `:` is past a swallowed `)`; \
             got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The closing `)` must land as a real RPAREN_TOK child of
    // PAREN_EXPR, not as an ERROR token absorbed by a mis-wrapped
    // TYPED_EXPR.
    let has_rparen_child = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        has_rparen_child,
        "PAREN_EXPR must contain its closing RPAREN_TOK; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.1 regression — `(x : ) y` is an incomplete in-paren
/// annotation. The hook correctly fires on the `:`, but
/// `parse_type`'s initial trivia drain would (codex P3) consume the
/// swallowed `)` as `ERROR` and then parse `y` (outside the parens!)
/// as the type. The fix checks the raw stream for `)` before
/// committing to draining/accepting a type: if the next non-trivia
/// raw isn't a type-starter, bail with an "expected type" error
/// without consuming anything past the cursor.
#[test]
fn in_paren_missing_type_does_not_eat_outer_rparen() {
    let source = "(x : ) y\n";
    let parse = parse(source);
    // The PAREN_EXPR must contain its closing RPAREN_TOK as a direct
    // child — not an ERROR-classified `)` swallowed under the inner
    // TYPED_EXPR.
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for the inner `(x : )`");
    let has_rparen_child = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        has_rparen_child,
        "PAREN_EXPR must keep its closing `)` as RPAREN_TOK; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The outer `y` must NOT be parsed as a type inside the paren.
    // i.e. no LONG_IDENT_TYPE descendant containing the text `y`.
    let stole_y_as_type = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::LONG_IDENT_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y_as_type,
        "outer `y` must not be parsed as the in-paren type; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The parse must record at least one error (missing type).
    assert!(
        !parse.errors.is_empty(),
        "expected an 'expected type' error for `(x : )`",
    );
    assert_lossless(source, &parse);
}

/// Regression — the attributed-signature-parameter lookahead
/// ([`Parser::peek_at_type_attribute`]) must not cross a LexFilter-swallowed
/// `)`. In `(member M : ) [<A>] int` the constrained-member type is missing and
/// the inner `)` is swallowed, so the *filtered* token after it is `[<`; a
/// filtered-only lookahead would steal the outer `[<A>] int` into the member
/// signature as an attributed `SignatureParameter`. The raw-aligned predicate
/// (the next non-trivia *raw* token there is the swallowed `RParen`) blocks it,
/// so no `ATTRIBUTE_LIST` / `SIGNATURE_PARAMETER_TYPE` is produced and the parse
/// stays lossless.
#[test]
fn attributed_sig_param_lookahead_does_not_cross_swallowed_rparen() {
    let source = "type C< ^T when ^T : (member M : ) [<A>] int > = class end\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    let stole_attr = parse
        .root
        .descendants()
        .any(|n| n.kind() == SyntaxKind::ATTRIBUTE_LIST);
    assert!(
        !stole_attr,
        "the `[<A>]` past the swallowed `)` must not be stolen as a member-sig \
         attribute; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let stole_sig_param = parse
        .root
        .descendants()
        .any(|n| n.kind() == SyntaxKind::SIGNATURE_PARAMETER_TYPE);
    assert!(
        !stole_sig_param,
        "no SIGNATURE_PARAMETER_TYPE should be built across the swallowed `)`; \
         got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Regression — the *attribute-run continuation* must not cross a swallowed `)`
/// either. In `(member M : [<A>] ) [<B>] int` the member-sig type is the
/// attributed `[<A>]`, the inner `)` is swallowed, and the outer `[<B>] int`
/// follows; the generic `parse_attribute_lists` would (via its filtered-only
/// same-scope continuation) fold `[<B>]` into the member sig past the closer.
/// The per-list raw-gated loop in `parse_signature_parameter` stops after the
/// first list — so exactly **one** `ATTRIBUTE_LIST` (the inner `[<A>]`) is
/// produced, not two, and the parse stays lossless.
#[test]
fn attributed_sig_param_run_does_not_cross_swallowed_rparen() {
    let source = "type C< ^T when ^T : (member M : [<A>] ) [<B>] int > = class end\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    let attr_lists = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ATTRIBUTE_LIST)
        .count();
    assert_eq!(
        attr_lists,
        1,
        "only the inner `[<A>]` should be parsed as an attribute list; the outer \
         `[<B>]` past the swallowed `)` must not be folded in; got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 7.1 — `(x : )` alone (no follow-on token). Independent of
/// the codex P3 bug fix path, this case already produced the right
/// tree (no token to steal as a type), but pinning it ensures the
/// "expected type" error is still recorded and the `)` is still kept
/// as a real RPAREN_TOK.
#[test]
fn in_paren_missing_type_at_eof_recovers() {
    let source = "(x : )\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : )`");
    let has_rparen_child = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        has_rparen_child,
        "PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected an 'expected type' error for `(x : )`",
    );
    assert_lossless(source, &parse);
}

/// Phase 7.1 regression — `(x : int).ToString()`. The LexFilter
/// swallows the `)` of the typed-expression parens, so the filtered
/// stream after the type-ident `int` is `.ToString`. The dotted
/// long-ident-type continuation must NOT cross that `)` (codex
/// round-2 P2a): doing so wraps `int.ToString` as `LONG_IDENT_TYPE`,
/// drains the real `)` as `ERROR`, and reports a missing close
/// paren. The fix gates the dot loop on the next non-trivia raw
/// token being `Dot`, so an intervening swallowed `)` ends the path.
///
/// Phase 7.1 doesn't parse the trailing `.ToString()` member access;
/// that's later-phase business. The contract here is just: the
/// `int` is the entire type, and the outer `)` lands as RPAREN_TOK
/// directly on PAREN_EXPR.
#[test]
fn long_ident_type_does_not_cross_swallowed_rparen() {
    let source = "(x : int).ToString()\n";
    let parse = parse(source);
    // The PAREN_EXPR must still own its closing `)` as RPAREN_TOK.
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : int)`");
    let has_rparen_child = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        has_rparen_child,
        "PAREN_EXPR must keep its closing `)` as RPAREN_TOK; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The LONG_IDENT_TYPE must contain exactly the one segment
    // `int` — not `int.ToString`. Probe: no `ToString` token under
    // any LONG_IDENT_TYPE.
    let stole_member = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::LONG_IDENT_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "ToString")
            })
    });
    assert!(
        !stole_member,
        "the type's long-ident must not absorb `.ToString`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.1 — `(x :\n  int)` — a typed-annotation split across a
/// newline. Inside parens LexFilter's offside engine is in a `Paren`
/// context, so it does NOT emit a `Virtual::BlockSep` between the
/// `:` and the next-line `int`; the filtered cursor lands directly
/// on `Ident("int")` (probe: `lex_filter_one`). Codex round-3 P1
/// hypothesised this would crash via an unhandled `Virtual` in
/// `parse_atomic_type` — it does not, because the suppression is the
/// whole point of the paren context. This test pins both the
/// non-crash and the parse: `int` lands as the type, `)` lands as
/// RPAREN_TOK on the surrounding PAREN_EXPR.
#[test]
fn typed_paren_expr_across_newline_parses_cleanly() {
    let source = "(x :\n  int)\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : int)` split across a newline");
    let has_rparen_child = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        has_rparen_child,
        "PAREN_EXPR must keep its closing `)` as RPAREN_TOK; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let typed = paren
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPED_EXPR)
        .expect("TYPED_EXPR for the inner `x : int`");
    let int_under_type = typed.descendants().any(|n| {
        n.kind() == SyntaxKind::LONG_IDENT_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "int")
            })
    });
    assert!(
        int_under_type,
        "TYPED_EXPR must carry `int` as its LONG_IDENT_TYPE body; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.1 regression — `(x : ()) y`. The inner `()` is an empty
/// (i.e., currently invalid) paren-type; both its `)` and the outer
/// `)` are swallowed by LexFilter. Without a raw-stream check inside
/// `parse_atomic_type`'s PAREN_TYPE arm (codex round-2 P2b), the
/// initial trivia drain consumes both `)` as `ERROR` and the filtered
/// peek lands on `y`, which the type-starter check would then accept
/// as the paren-type body.
///
/// The fix is the same shape as `parse_type`: do the raw-stream
/// type-starter check *before* draining trivia in the LParen arm.
#[test]
fn empty_paren_type_does_not_steal_outer_token() {
    let source = "(x : ()) y\n";
    let parse = parse(source);
    // Outer PAREN_EXPR must keep its closing `)`.
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : ())`");
    let outer_has_rparen = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        outer_has_rparen,
        "outer PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The outer `y` must NOT be parsed as a type inside the inner
    // PAREN_TYPE: no LONG_IDENT_TYPE descendant contains `y`.
    let stole_y_as_type = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::LONG_IDENT_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y_as_type,
        "outer `y` must not be parsed as the empty paren-type's body; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected an 'expected type inside parentheses' error for `(x : ())`",
    );
    assert_lossless(source, &parse);
}
