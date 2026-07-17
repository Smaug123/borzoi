use super::super::*;
use super::*;

/// Phase 10.1 — a typed code quotation `<@ 1 @>`. Pins the green-tree
/// shape (`QUOTE_EXPR > [LQUOTE_TOK, CONST_EXPR, RQUOTE_TOK]`), that
/// the inner expression and both delimiters land as children, and that
/// the `is_raw` facade reads `false` from the `<@` opener text.
#[test]
fn quote_typed_shape() {
    let source = "<@ 1 @>\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      QUOTE_EXPR@0..7
        LQUOTE_TOK@0..2 \"<@\"
        WHITESPACE@2..3 \" \"
        CONST_EXPR@3..4
          INT32_LIT@3..4 \"1\"
        WHITESPACE@4..5 \" \"
        RQUOTE_TOK@5..7 \"@>\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let expr = crate::syntax::ExprDecl::cast(
        parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::EXPR_DECL)
            .expect("EXPR_DECL"),
    )
    .and_then(|d| d.expr())
    .expect("expr");
    let crate::syntax::Expr::Quote(q) = expr else {
        panic!("expected QUOTE_EXPR, got {expr:?}");
    };
    assert!(!q.is_raw(), "<@ … @> is typed (is_raw=false)");
    assert!(matches!(q.inner(), Some(crate::syntax::Expr::Const(_))));
}

/// Raw quotation `<@@ 1 @@>` — the `is_raw=true` form, recovered from
/// the `<@@` opener text.
#[test]
fn quote_raw_is_raw() {
    let source = "<@@ 1 @@>\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let q_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::QUOTE_EXPR)
        .expect("QUOTE_EXPR");
    let q = crate::syntax::QuoteExpr::cast(q_node).expect("cast QuoteExpr");
    assert!(q.is_raw(), "<@@ … @@> is raw (is_raw=true)");
    assert_lossless(source, &parse);
}

/// Delimiter mismatch `<@ 1 @@>` still produces a `QUOTE_EXPR` (with
/// the opener-derived `is_raw=false`) plus a parse error — mirroring
/// FCS's `parsMismatchedQuote` recovery.
#[test]
fn quote_mismatch_recovers_with_error() {
    let source = "<@ 1 @@>\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "mismatched quote should emit a parse error",
    );
    assert!(tree_contains_kind(&parse.root, SyntaxKind::QUOTE_EXPR));
    use crate::syntax::AstNode;
    let q = crate::syntax::QuoteExpr::cast(
        parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::QUOTE_EXPR)
            .expect("QUOTE_EXPR"),
    )
    .expect("cast QuoteExpr");
    assert!(!q.is_raw(), "opener `<@` keeps is_raw=false on mismatch");
    assert_lossless(source, &parse);
}

/// Phase 10.2 — a bare computation expression `{ 1 }`. Pins the
/// green-tree shape, including that the swallowed `}` is recovered from
/// the raw stream as a `RBRACE_TOK` child of `COMPUTATION_EXPR`.
#[test]
fn computation_expr_bare_shape() {
    let source = "{ 1 }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      COMPUTATION_EXPR@0..5
        LBRACE_TOK@0..1 \"{\"
        WHITESPACE@1..2 \" \"
        CONST_EXPR@2..3
          INT32_LIT@2..3 \"1\"
        WHITESPACE@3..4 \" \"
        RBRACE_TOK@4..5 \"}\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `seq { 1 }` is the application of the builder ident `seq` to the
/// brace atom, i.e. `APP_EXPR > [IDENT_EXPR, COMPUTATION_EXPR]`. Pins
/// the structure via the facade rather than the exact trivia layout.
#[test]
fn computation_expr_builder_application() {
    let source = "seq { 1 }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let expr = crate::syntax::ExprDecl::cast(
        parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::EXPR_DECL)
            .expect("EXPR_DECL"),
    )
    .and_then(|d| d.expr())
    .expect("expr");
    let crate::syntax::Expr::App(app) = expr else {
        panic!("expected APP_EXPR for `seq {{ 1 }}`, got {expr:?}");
    };
    assert!(!app.is_infix(), "builder application is non-infix");
    assert!(
        matches!(app.func(), Some(crate::syntax::Expr::Ident(_))),
        "func is the builder ident",
    );
    assert!(
        matches!(app.arg(), Some(crate::syntax::Expr::Computation(_))),
        "arg is the COMPUTATION_EXPR",
    );
}

/// Record expression `{ F = 1 }` — full green shape. The leading longident `F`
/// followed by `=` selects `RECORD_EXPR > [LBRACE_TOK, RECORD_FIELD >
/// [LONG_IDENT, EQUALS_TOK, <value>], RBRACE_TOK]` over a computation
/// expression; the `}` is the swallowed closer.
#[test]
fn record_single_field_shape() {
    let source = "{ F = 1 }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..10
  MODULE_OR_NAMESPACE@0..10
    EXPR_DECL@0..9
      RECORD_EXPR@0..9
        LBRACE_TOK@0..1 \"{\"
        WHITESPACE@1..2 \" \"
        RECORD_FIELD@2..7
          LONG_IDENT@2..3
            IDENT_TOK@2..3 \"F\"
          WHITESPACE@3..4 \" \"
          EQUALS_TOK@4..5 \"=\"
          CONST_EXPR@5..7
            WHITESPACE@5..6 \" \"
            INT32_LIT@6..7 \"1\"
        WHITESPACE@7..8 \" \"
        RBRACE_TOK@8..9 \"}\"
    NEWLINE@9..10 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Multi-field record `{ F = 1; G = 2 }` via the facade — `fields()` yields two
/// `RecordField`s, each with a `field_name()` longident and a `value()`; no
/// `copy_source()`.
#[test]
fn record_multi_field_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "{ F = 1; G = 2 }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Record(r) = expr_decl.expr().expect("expr") else {
        panic!("expected RecordExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        r.copy_source().is_none(),
        "field-list record has no copy source"
    );
    let fields: Vec<_> = r.fields().collect();
    assert_eq!(fields.len(), 2, "two fields");
    assert_eq!(
        fields[0]
            .field_name()
            .expect("field 0 name")
            .idents()
            .map(|t| t.text().to_string())
            .collect::<Vec<_>>(),
        vec!["F"],
    );
    assert!(
        matches!(fields[1].value(), Some(Expr::Const(_))),
        "field 1 value is the const `2`, got {:?}",
        fields[1].value(),
    );
}

/// Copy-and-update `{ x with F = 1 }` via the facade — `copy_source()` is the
/// `Ident x` and there is one field.
#[test]
fn record_copy_update_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "{ x with F = 1 }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Record(r) = expr_decl.expr().expect("expr") else {
        panic!("expected RecordExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(r.copy_source(), Some(Expr::Ident(_))),
        "copy source is `x`, got {:?}",
        r.copy_source(),
    );
    assert_eq!(r.fields().count(), 1, "one updated field");
}

/// A *repeated* field separator inside a record expression is invalid. FCS's
/// `seps_block` is a single separator group, so `{ F = 1; ; G = 2 }` and the
/// trailing `{ F = 1;; }` are parse errors (`ParseHadErrors: true`, verified
/// against `fcs-dump ast`). The parser consumes exactly one group per gap, so
/// the stray second `;` trips `parse_record_field`'s recovery — pinning that we
/// do *not* silently accept the malformed run. A *single* trailing `;`
/// (`{ F = 1; }`) and a blank line between fields stay valid (covered by the
/// diff tests).
#[test]
fn record_repeated_separator_is_flagged() {
    for source in [
        "let r = { F = 1; ; G = 2 }\n",
        "let r = { F = 1;; G = 2 }\n",
        "let r = { F = 1;; }\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "repeated record separator in {source:?} should be flagged, got no errors",
        );
    }
}

/// Phase 10.3 — the `yield` / `return!` facade flags. `yield` is a
/// non-`from` yield; `return!` is a `from` non-yield. Drills through the
/// `seq { … }` / `async { … }` application + brace to the keyword node.
#[test]
fn yield_or_return_facade_flags() {
    use crate::syntax::AstNode;
    for (source, want_yield, want_from) in [
        ("seq { yield 1 }\n", true, false),
        ("async { return! x }\n", false, true),
    ] {
        let parse = parse(source);
        assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
        let node = parse
            .root
            .descendants()
            .find(|n| crate::syntax::YieldExpr::can_cast(n.kind()))
            .expect("a yield/return node");
        let y = crate::syntax::YieldExpr::cast(node).expect("cast YieldExpr");
        assert_eq!(y.is_yield(), want_yield, "is_yield for {source:?}");
        assert_eq!(y.is_from(), want_from, "is_from for {source:?}");
        assert!(y.inner().is_some(), "inner expr present for {source:?}");
        assert_lossless(source, &parse);
    }
}

/// Phase 10.4 (do! slice) — `async { do! x }` parses to a `DO_BANG_EXPR`
/// whose inner is the bound expression, with the offside scaffolding held
/// as zero-width ERRORs (so the tree stays lossless). Structural check via
/// the facade rather than an exact tree (the zero-width placeholders make
/// a literal-tree assertion brittle).
#[test]
fn do_bang_facade() {
    use crate::syntax::AstNode;
    let source = "async { do! x }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::DO_BANG_EXPR)
        .expect("DO_BANG_EXPR");
    // The `do!` keyword text is recovered from the rewritten virtual.
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::DO_BANG_TOK && t.text() == "do!"),
        "DO_BANG_TOK present with text `do!`",
    );
    let d = crate::syntax::DoBangExpr::cast(node).expect("cast DoBangExpr");
    assert!(
        matches!(d.inner(), Some(crate::syntax::Expr::Ident(_))),
        "do! inner is the bound expr `x`",
    );
}

/// Phase 10.4b.1 — `let!`/`use!` parse to a `LET_OR_USE_EXPR` with one
/// `BINDING` and a body, the offside scaffolding held as zero-width ERRORs
/// (so the tree stays lossless). Structural check via the facade.
#[test]
fn let_bang_facade() {
    use crate::syntax::AstNode;
    for (source, kw, want_use) in [
        ("async {\n    let! x = e\n    return x\n}\n", "let!", false),
        ("async {\n    use! x = e\n    return x\n}\n", "use!", true),
    ] {
        let parse = parse(source);
        assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
        assert_lossless(source, &parse);

        let node = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::LET_OR_USE_EXPR)
            .expect("LET_OR_USE_EXPR");
        assert!(
            node.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == SyntaxKind::BINDER_TOK && t.text() == kw),
            "BINDER_TOK present with text `{kw}`",
        );
        let l = crate::syntax::LetOrUseExpr::cast(node).expect("cast LetOrUseExpr");
        assert_eq!(l.is_use(), want_use, "is_use for `{kw}`");
        assert_eq!(
            l.keyword().map(|t| t.text().to_string()).as_deref(),
            Some(kw),
            "keyword() returns the head `{kw}` token",
        );
        assert_eq!(l.bindings().count(), 1, "single binding for `{kw}`");
        assert!(
            matches!(l.body(), Some(crate::syntax::Expr::Yield(_))),
            "body is the `return x` yield-or-return",
        );
    }
}

/// A typed bang binder (`let! x : int = e`, FCS's `AllowTypedLetUseAndBang`)
/// parses the `: T` into a `BINDING_RETURN_INFO` like a regular binding — with
/// no error. The differential half (the bang form's `returnInfo` is *not*
/// `Typed`-wrapped, unlike a regular `let`) is pinned against FCS by
/// `diff_ast_compexpr_typed_letbang` &co.; here we pin the structural side: the
/// annotation belongs to the bang binding itself, `return_type()` exposes it,
/// and `expr()` remains the bare RHS rather than a synthetic `TYPED_EXPR`.
#[test]
fn typed_let_bang_emits_return_info() {
    use crate::syntax::AstNode;
    for (source, want_use, expected_type) in [
        (
            "async {\n    let! x : int = e\n    return x\n}\n",
            false,
            "int",
        ),
        (
            "async {\n    use! x : System.IDisposable = e\n    return x\n}\n",
            true,
            "System.IDisposable",
        ),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "typed bang binder should parse cleanly; got: {:?}",
            parse.errors,
        );
        assert_lossless(source, &parse);

        let node = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::LET_OR_USE_EXPR)
            .expect("LET_OR_USE_EXPR");
        let l = crate::syntax::LetOrUseExpr::cast(node).expect("cast LetOrUseExpr");
        assert_eq!(l.is_use(), want_use, "is_use for {source:?}");
        let bindings: Vec<_> = l.bindings().collect();
        assert_eq!(bindings.len(), 1, "one bang binding for {source:?}");
        assert_bang_binding_annotation(&bindings[0], expected_type, "e");
    }
}

fn assert_bang_binding_annotation(
    binding: &crate::syntax::Binding,
    expected_type: &str,
    expected_rhs: &str,
) {
    use crate::syntax::{AstNode, Expr, Type};

    assert_eq!(
        binding
            .syntax()
            .children()
            .filter(|n| n.kind() == SyntaxKind::BINDING_RETURN_INFO)
            .count(),
        1,
        "annotation must be a direct child of this BINDING:\n{}",
        debug_tree(binding.syntax())
    );
    let return_type = binding
        .return_type()
        .expect("typed bang binding should expose its return_type()");
    assert!(
        matches!(&return_type, Type::LongIdent(_)),
        "typed bang binding should expose its return_type():\n{}",
        debug_tree(binding.syntax())
    );
    assert_eq!(
        return_type.syntax().text().to_string(),
        expected_type,
        "return_type() should belong to this binding"
    );
    let expr = binding
        .expr()
        .expect("typed bang binding still has a bare RHS expression");
    assert!(
        !matches!(&expr, Expr::Typed(_)),
        "bang binder RHS must not be structurally wrapped in TYPED_EXPR:\n{}",
        debug_tree(binding.syntax())
    );
    assert!(
        binding
            .syntax()
            .children()
            .all(|n| n.kind() != SyntaxKind::TYPED_EXPR),
        "no direct TYPED_EXPR child should be synthesised for a bang binder:\n{}",
        debug_tree(binding.syntax())
    );
    assert_eq!(
        expr.syntax().text().to_string(),
        expected_rhs,
        "expr() should return the unwrapped RHS"
    );
}

/// Phase 10.4b.1 — the explicit-`in` binder claims the raw `in` as an
/// `IN_TOK` child (LexFilter does not surface it), keeping the tree lossless
/// and out of the unsupported-`in` recovery path.
#[test]
fn let_bang_explicit_in_claims_in_keyword() {
    use crate::syntax::AstNode;
    let source = "async {\n    let! x = e in return x\n}\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LET_OR_USE_EXPR)
        .expect("LET_OR_USE_EXPR");
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::IN_TOK && t.text() == "in"),
        "explicit-`in` form has an IN_TOK child",
    );
    let l = crate::syntax::LetOrUseExpr::cast(node).expect("cast LetOrUseExpr");
    assert!(
        matches!(l.body(), Some(crate::syntax::Expr::Yield(_))),
        "body is `return x` even in the explicit-`in` form",
    );
}

/// Phase 10.4b.2 — `let! … and! …` is one `LET_OR_USE_EXPR` with several
/// `BINDING` children (head + `and!` followers), not a nested chain.
#[test]
fn and_bang_grouping_facade() {
    use crate::syntax::AstNode;
    let source = "async {\n    let! x = a\n    and! y = b\n    and! z = c\n    return x\n}\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    // Exactly one LetOrUse node (the `and!`s do not nest).
    let nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LET_OR_USE_EXPR)
        .collect();
    assert_eq!(
        nodes.len(),
        1,
        "one LET_OR_USE_EXPR, `and!`s grouped not nested"
    );

    let l = crate::syntax::LetOrUseExpr::cast(nodes[0].clone()).expect("cast LetOrUseExpr");
    assert_eq!(l.bindings().count(), 3, "head + two `and!` bindings");
    assert!(!l.is_use(), "head keyword is `let!`");
    let and_bangs = nodes[0]
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::AND_BANG_TOK && t.text() == "and!")
        .count();
    assert_eq!(and_bangs, 2, "two `and!` keyword tokens");
    assert!(
        matches!(l.body(), Some(crate::syntax::Expr::Yield(_))),
        "body is the `return x`",
    );
}

/// Typed bang binders keep their `BINDING_RETURN_INFO` independently on the
/// head `let!` and every `and!` follower; annotations must not bleed between
/// bindings or wrap any RHS as `TYPED_EXPR`.
#[test]
fn typed_and_bang_annotations_are_per_binding() {
    use crate::syntax::AstNode;
    let source = "\
async {
    let! x : int = a
    and! y : string = b
    and! z : bool = c
    return x
}
";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LET_OR_USE_EXPR)
        .expect("LET_OR_USE_EXPR");
    let l = crate::syntax::LetOrUseExpr::cast(node).expect("cast LetOrUseExpr");
    let bindings: Vec<_> = l.bindings().collect();
    assert_eq!(bindings.len(), 3, "head + two typed `and!` bindings");
    for (binding, (ty, rhs)) in bindings
        .iter()
        .zip([("int", "a"), ("string", "b"), ("bool", "c")])
    {
        assert_bang_binding_annotation(binding, ty, rhs);
    }
}

/// Phase 10.4b — CE binders take no `inline`/`mutable` modifier (FCS's
/// `OBINDER …` production omits them). `let! mutable x = e` is "Invalid
/// declaration syntax" with `isMutable = false`, so our parser must record
/// an error and *not* flag the binding mutable.
#[test]
fn let_bang_rejects_modifier() {
    use crate::syntax::AstNode;
    for source in [
        "async {\n    let! mutable x = e\n    return x\n}\n",
        "async {\n    let! x = a\n    and! inline y = b\n    return x\n}\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "a modifier on a CE binder must be rejected: {source:?}",
        );
        for node in parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::BINDING)
        {
            let b = crate::syntax::Binding::cast(node).expect("cast Binding");
            assert!(!b.is_mutable(), "CE binder must not be mutable: {source:?}");
            assert!(!b.is_inline(), "CE binder must not be inline: {source:?}");
        }
    }
}

/// Phase 10.4b — the *non-block* / parenthesised binder forms are not yet
/// parsed: a `let!`/`use!` whose `CtxtLetDecl` is not block-let surfaces as a
/// raw `Token::LetBang` (not `Virtual::Binder`), so the offside CE-body
/// production doesn't fire. FCS accepts these (a raw `BINDER … IN …`
/// production); we don't yet. The contract for now is **clean rejection, never
/// a panic** — recovery-grade parsing of these forms is a follow-up slice (it
/// needs the raw-binder production). This guards that invariant.
#[test]
fn non_block_bang_binders_reject_without_panicking() {
    for source in [
        // parenthesised explicit-`in` binder (CE body and bare let-RHS)
        "async { (let! x = m in return x) }\n",
        "let r = (let! y = m in y)\n",
        // binder in a non-SeqBlock context (match guard, infix RHS)
        "match m with\n| _ when let! x = m in x -> 1\n",
        "let r = m + let! y = m in y\n",
    ] {
        let parse = parse(source);
        // We don't parse these yet, so an error is expected — but the parser
        // must recover, not panic, and the tree stays lossless.
        assert!(
            !parse.errors.is_empty(),
            "non-block binder form is expected to error (not yet parsed): {source:?}",
        );
        assert_lossless(source, &parse);
    }
}

/// A computation-expression bang binder carries no attributes (FCS's `OBINDER
/// headBindingPattern EQUALS …` has no `attributes`), so `let! [<A>] x = …` is
/// invalid F#. The binding-attribute run is parsed only for the plain-`let`
/// contexts (`allow_modifiers`), so the `[<` reaches the bang binder's pattern
/// parse and is rejected — FCS errors too. Contract: clean rejection, no panic,
/// tree lossless.
#[test]
fn attributed_bang_binder_rejects_without_panicking() {
    for source in [
        "async { let! [<A>] x = m in return x }\n",
        "seq { use! [<A>] r = m in () }\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "attributed bang binder is invalid F# and must error: {source:?}",
        );
        assert_lossless(source, &parse);
    }
}

/// `do e` as a bare *infix operand* (`1 + do f`, `id <| do f`) — FCS accepts
/// these (a `declExpr` RHS at `%prec expr_let`), but since `do` always has unit
/// type they are never meaningful code, and the infix-RHS lookahead
/// (`is_expr_start_at`) deliberately does not admit `Virtual::Do`. The contract
/// is **clean rejection, never a panic**, with the tree lossless — the same
/// documented limitation as the non-block bang binders above. (The realistic
/// forms — `(do f)`, `f (do g)`, a sequence/tuple body — all parse; only a bare
/// `do` operand directly under an infix operator declines.)
#[test]
fn do_as_infix_operand_rejects_without_panicking() {
    for source in ["let x = 1 + do f\n", "let x = id <| do f\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "bare `do` infix operand is expected to error (lenient deferral): {source:?}",
        );
        assert_lossless(source, &parse);
    }
}

/// `do e` as a bare *prefix operand* (`- do f`, `& do f`). FCS rejects these
/// (`do` is a `declExpr`, not a `minusExpr` operand), but `do` reaches the
/// dispatch as `Virtual::Do`, so — exactly like `- fun y -> y` / `& fun …`
/// already do — the prefix-operand path accepts it leniently rather than
/// emitting the `maybe_warn_keyword_after_prefix` diagnostic (which only fires
/// for *raw* keyword tokens like `if`/`while`). `do` always has unit type, so
/// these are never meaningful code; the contract is the established one for the
/// virtual-expr-starter class — **lossless, never a panic**. Diagnosing the
/// virtual prefix operands generally is the deferred `expr_op.rs` follow-up.
#[test]
fn do_as_prefix_operand_is_lenient_lossless() {
    for source in ["let x = - do f\n", "let x = & do f\n"] {
        let parse = parse(source);
        // Lenient (no diagnostic), exactly like the existing `- fun …` handling;
        // the invariant pinned here is losslessness + no panic.
        assert_lossless(source, &parse);
    }
}

/// Phase 7.3 — `(f : int -> string)`: the FUN_TYPE node must not
/// swallow the leading trivia between `:` and the LHS atomic. Pins
/// the invariant that `Type::syntax().text_range()` starts at the
/// first token of the type (here `i` of `int`), matching what the
/// other type arms (`LongIdent`, `Var`, `Paren`) already do — so
/// LSP range/ancestor consumers can't tell `Fun` apart from the
/// atomic shapes by trivia inclusion.
#[test]
fn fun_type_range_excludes_leading_trivia() {
    let source = "(f : int -> string)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let fun_type = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FUN_TYPE)
        .expect("FUN_TYPE present");
    let fun_text = fun_type.text().to_string();
    assert_eq!(
        fun_text,
        "int -> string",
        "FUN_TYPE text range must start at the LHS atomic (no leading trivia); \
             got {fun_text:?} from tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 7.3 — `(f : int -> int -> int)`: arrow is right-recursive
/// (mirroring FCS's `tupleType RARROW typ`). The outer FUN_TYPE's
/// return type is itself a FUN_TYPE.
#[test]
fn fun_type_is_right_associative() {
    let source = "(f : int -> int -> int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let outer = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FUN_TYPE)
        .expect("outer FUN_TYPE present");
    use crate::syntax::AstNode;
    let ret = crate::syntax::FunType::cast(outer)
        .expect("FUN_TYPE casts to facade")
        .ret()
        .expect("outer FUN_TYPE has a return-type child");
    assert!(
        matches!(ret, crate::syntax::Type::Fun(_)),
        "right-recursive shape: outer FUN_TYPE's return is itself a Fun, got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// `- if true then 1 else 2` — `if` is in `raw_starts_minus_expr`
/// (so a prefix `-` accepts it as having an operand) but the only
/// `if`-dispatch must apply on every entry to the minus-level
/// recursion, not just at the Pratt entry. Without that, the
/// recursive `parse_minus_expr` call from the `-` prefix path
/// would fall through to `parse_atomic_expr`/`parse_const_expr`
/// on the `Token::If`, which has no const-expr arm and panics.
///
/// FCS rejects `-` over an `if` at the grammar level (minusExpr's
/// operand is `minusExpr`, not `declExpr`), so this is malformed
/// input. The contract is: produce an error-recovery tree *with
/// a diagnostic*, not a silent accept and not a panic.
#[test]
fn minus_prefix_over_if_records_diagnostic() {
    let source = "- if true then 1 else 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("`if`") && e.message.contains("prefix")),
        "expected a diagnostic about `if` after a prefix operator, got: {:?}",
        parse.errors,
    );
}

/// `& if true then 1 else 2` — same shape as
/// [`minus_prefix_over_if_records_diagnostic`] but via the
/// `AMP`/`ADDRESS_OF_EXPR` path. `parse_address_of` recurses into
/// `parse_minus_expr`, so the same `if`-dispatch must intercept
/// or the recursive minus-expr falls through to the atomic-level
/// const-expr panic. FCS rejects this at the grammar level so
/// the recovery path must surface a diagnostic.
#[test]
fn address_of_over_if_records_diagnostic() {
    let source = "& if true then 1 else 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("`if`") && e.message.contains("prefix")),
        "expected a diagnostic about `if` after a prefix operator, got: {:?}",
        parse.errors,
    );
}

/// `- match x with A -> 1` — the `match` analogue of
/// [`minus_prefix_over_if_records_diagnostic`]. `Token::Match` is in
/// `raw_starts_minus_expr` so the prefix `-`'s recursive
/// `parse_minus_expr` accepts it as an operand and the `match`
/// dispatch parses it cleanly — but FCS rejects `match` directly
/// after a prefix operator at the grammar level (minusExpr's operand
/// is minusExpr, not declExpr), so the recovery path must surface a
/// diagnostic rather than silently accept.
#[test]
fn minus_prefix_over_match_records_diagnostic() {
    let source = "- match x with A -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("`match`") && e.message.contains("prefix")),
        "expected a diagnostic about `match` after a prefix operator, got: {:?}",
        parse.errors,
    );
}

/// `& match x with A -> 1` — same as
/// [`minus_prefix_over_match_records_diagnostic`] but via the
/// `AMP`/`ADDRESS_OF_EXPR` path, mirroring
/// [`address_of_over_if_records_diagnostic`].
#[test]
fn address_of_over_match_records_diagnostic() {
    let source = "& match x with A -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("`match`") && e.message.contains("prefix")),
        "expected a diagnostic about `match` after a prefix operator, got: {:?}",
        parse.errors,
    );
}

/// `(if c then 1) + 2`: a no-else `if` inside parens, then an
/// infix continuation. The lexfilter swallows the `)` and emits
/// the then-body's `Virtual::BlockEnd` anchored at the `)` byte
/// position. The then-body close must not consume that `)` as
/// ERROR — `parse_paren_expr`'s `bump_swallowed_rparen` needs it.
/// Phase 5.2 promotes the no-else form to a clean parse, so the
/// expression now matches FCS's `SynExpr.IfThenElse(_, _, None,
/// …)` and produces zero diagnostics.
#[test]
fn no_else_if_inside_parens_then_infix_does_not_steal_rparen() {
    let source = "(if true then 1) + 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "no-else `if` inside parens should parse cleanly under Phase 5.2, got: {:?}",
        parse.errors,
    );

    // Structural sanity: the tree must contain a closed
    // PAREN_EXPR wrapping an IF_THEN_ELSE_EXPR. The stolen-`)`
    // bug produced a PAREN_EXPR with no RPAREN_TOK child, so
    // require an RPAREN_TOK descendant.
    let paren_kinds: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == crate::syntax::SyntaxKind::PAREN_EXPR)
        .collect();
    assert_eq!(
        paren_kinds.len(),
        1,
        "expected one PAREN_EXPR, got {paren_kinds:#?}",
    );
    let paren = &paren_kinds[0];
    assert!(
        paren
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == crate::syntax::SyntaxKind::RPAREN_TOK),
        "PAREN_EXPR must retain its RPAREN_TOK (lexfilter-swallowed `)` must not be stolen by the if's no-else close): {paren:#?}",
    );
    use crate::syntax::AstNode;
    let if_node = paren
        .children()
        .find(|n| n.kind() == crate::syntax::SyntaxKind::IF_THEN_ELSE_EXPR)
        .unwrap_or_else(|| panic!("PAREN_EXPR should wrap the IF_THEN_ELSE_EXPR: {paren:#?}"));
    let if_expr = crate::syntax::IfThenElseExpr::cast(if_node).expect("IfThenElseExpr");
    assert!(
        if_expr.else_branch().is_none(),
        "no-else form must produce IfThenElse with else_branch = None, got: {if_expr:#?}",
    );
}

/// `let x = if true then 1 else 2\n// c\nlet y = 3`: an
/// inter-declaration comment must NOT live inside the if's tree.
/// LSP consumers walk ancestors of a token to find the enclosing
/// declaration; trivia leaking into the prior decl points
/// ancestor queries at the wrong node. The convention (see
/// `parse_module_decl` and `drain_let_rhs_block`) is to leave
/// trailing virtual tokens unconsumed so the impl-file loop's
/// virtual-fallthrough drains inter-decl trivia at
/// `MODULE_OR_NAMESPACE` level. The if's trailing BlockEnd close
/// must follow the same convention.
#[test]
fn if_then_else_does_not_swallow_inter_decl_comment() {
    let source = "let x = if true then 1 else 2\n// c\nlet y = 3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "expected no errors, got {:?}",
        parse.errors
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");

    // The comment must sit outside both `let` decls, at
    // MODULE_OR_NAMESPACE level. A direct child-token search is
    // strong: if the comment lives anywhere inside a decl, it
    // won't show up as a direct token here.
    let module_comment = module
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.text() == "// c");
    assert!(
        module_comment.is_some(),
        "inter-decl comment `// c` should be a direct child of MODULE_OR_NAMESPACE (not buried inside a LET_DECL/IF_THEN_ELSE_EXPR). Tree: {:#?}",
        parse.root,
    );

    // And the first LET_DECL must not extend past the `2`. Its
    // text range should be `0..29` (covering `let x = if true
    // then 1 else 2`). Anything longer means trivia got pulled in.
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 2, "expected two LET_DECLs, got {decls:#?}");
    let crate::syntax::ModuleDecl::Let(let_x) = &decls[0] else {
        panic!("expected first decl to be Let, got {:?}", decls[0]);
    };
    let range = let_x.syntax().text_range();
    assert_eq!(
        usize::from(range.end()),
        29,
        "first LET_DECL should end at byte 29 (after `2`), got {range:?}",
    );
}

/// `if true then 1 else\n    2\n    3`: top-level if-then-else
/// with a multi-statement else body. The else SeqBlock contains
/// two statements separated by `Virtual::BlockSep`; both must
/// remain inside the if expression — otherwise the impl-file
/// loop sees the leaked tokens as fresh top-level decls. The
/// body parser wraps multiple statements in
/// [`SyntaxKind::SEQUENTIAL_EXPR`], so the resulting tree is
/// `IfThenElse(_, _, Some(Sequential([2, 3])))`.
#[test]
fn if_then_else_else_body_multi_statement_does_not_escape() {
    let source = "if true then 1 else\n    2\n    3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "multi-statement else body should parse cleanly via SEQUENTIAL_EXPR, got {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected exactly one top-level decl (the if-then-else expr); leaked body content would surface as extra decls. Got: {decls:#?}",
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(if_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected IfThenElse");
    };
    let else_branch = if_expr.else_branch().expect("else branch present");
    let crate::syntax::Expr::Sequential(seq) = else_branch else {
        panic!("else branch should be Sequential, got {else_branch:?}");
    };
    assert_eq!(
        seq.statements().count(),
        2,
        "expected two statements in the else SeqBlock",
    );
}

/// `if true then\n    1\n    2\nelse 3`: a multi-statement
/// then-body. After parsing `1`, the cursor is on a
/// `Virtual::BlockSep` followed by `Raw(Int("2"))`. The body
/// close must drain through the separator and `2` to reach the
/// matching `BlockEnd`, after which `Virtual::Else` is the next
/// token. Otherwise the body close sees `2` instead of
/// BlockEnd, falls into the no-else recovery, and the outer if
/// loses its else branch.
#[test]
fn if_then_else_then_body_multi_statement_keeps_else() {
    let source = "if true then\n    1\n    2\nelse 3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one top-level decl (the if-then-else expr), got {decls:#?}",
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(if_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected IfThenElse");
    };
    assert!(
        if_expr.else_branch().is_some(),
        "the if must keep its else branch even when the then-body has multi-statement content (which we report as unsupported). Got: {if_expr:#?}",
    );
}

/// `if a then\n  1\n  if b then 2 else 3\nelse 4`: a multi-statement
/// then-body containing a nested if-then-else. The naive
/// "drain until next `Virtual::BlockEnd`" recovery would mistake
/// the inner if's `BlockEnd` for the outer body's terminator
/// (and then steal the inner's `else` or fail to find the outer's).
/// Proper multi-statement parsing recursively descends into the
/// inner if, so its BlockEnd is consumed by the inner's own
/// scope rather than by the outer's drain.
#[test]
fn if_then_else_multi_statement_then_with_nested_if() {
    let source = "if a then\n  1\n  if b then 2 else 3\nelse 4\n";
    let parse = parse(source);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one top-level decl (the outer if-then-else); leaked content suggests the inner if's BlockEnd was mistaken for the outer body's. Got: {decls:#?}",
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    assert!(
        outer.else_branch().is_some(),
        "outer if must keep its else branch; the inner if's BlockEnd must not be mistaken for the outer body's terminator. Got: {outer:#?}",
    );

    // The then-body is a sequence of `1` then the inner if.
    let then_branch = outer.then_branch().expect("outer then branch");
    let crate::syntax::Expr::Sequential(seq) = then_branch else {
        panic!("expected outer then-branch to be a SequentialExpr, got {then_branch:?}");
    };
    let stmts: Vec<_> = seq.statements().collect();
    assert_eq!(stmts.len(), 2, "expected two statements, got {stmts:#?}");
    assert!(
        matches!(stmts[1], crate::syntax::Expr::IfThenElse(_)),
        "second statement should be the inner if-then-else, got {:?}",
        stmts[1],
    );
}

/// `if true then\n    if true then 1\nelse 2`: the outer if's
/// then-body is itself a no-else `if`. The inner if's BlockEnd
/// and the outer if's BlockEnd both sit between the inner's body
/// and the outer's `else`. The inner `parse_if_then_else` must
/// not greedily look past its own matching BlockEnd in search of
/// an `Else` — doing so finds the outer's else and steals it,
/// leaving the outer with no else. The contract is one drained
/// BlockEnd per opened BlockBegin: the inner releases exactly
/// the BlockEnd it opened, sees BlockEnd (not Else) next, and
/// bails to the no-else recovery; the outer then drains its own
/// BlockEnd, sees `Else`, and consumes it.
#[test]
fn if_with_nested_no_else_then_branch_preserves_outer_else() {
    let source = "if true then\n    if true then 1\nelse 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one top-level decl (the outer if-then-else), got {decls:#?}",
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr, got {:?}", decls[0]);
    };
    let expr = expr_decl.expr().expect("expr decl has an expr");
    let crate::syntax::Expr::IfThenElse(outer) = expr else {
        panic!("expected outer IfThenElse, got {expr:?}");
    };
    assert!(
        outer.else_branch().is_some(),
        "outer if must keep its else branch; the inner no-else if must not steal it. Got: {outer:#?}",
    );
    let then_branch = outer.then_branch().expect("outer then branch");
    assert!(
        matches!(then_branch, crate::syntax::Expr::IfThenElse(_)),
        "outer then-branch should be the inner IfThenElse, got {then_branch:?}",
    );
}

/// `if true then 1\nlet y = 2`: a no-else `if` followed by a
/// sibling `let` decl. The then-body's BlockEnd close must not
/// consume the surrounding module-level layout virtuals — those
/// belong to the impl-file loop so it can find the next decl.
/// Phase 5.2 promotes the no-else form to a clean parse:
/// `SynExpr.IfThenElse(_, _, None, …)` followed cleanly by the
/// sibling let.
#[test]
fn if_no_else_then_sibling_decl_does_not_swallow_outer_virtuals() {
    let source = "if true then 1\nlet y = 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "no-else `if` followed by sibling decl should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        2,
        "expected two top-level decls (an expr-decl for the if and a let), got {decls:#?}",
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected first decl to be an Expr, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(if_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected IfThenElse for first decl");
    };
    assert!(
        if_expr.else_branch().is_none(),
        "no-else `if` must produce IfThenElse with else_branch = None, got: {if_expr:#?}",
    );
    assert!(
        matches!(decls[1], crate::syntax::ModuleDecl::Let(_)),
        "expected the second decl to be a Let, got {:?}",
        decls[1],
    );
}

/// Phase 5.2 — `if true then 1`: the simplest no-else form,
/// matching FCS's `SynExpr.IfThenElse(_, _, None, …)`. The
/// `else_branch()` accessor must return `None`; no diagnostic
/// should fire.
#[test]
fn if_no_else_simple_parses_cleanly() {
    let source = "if true then 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "no-else `if` should parse cleanly under Phase 5.2, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one top-level decl, got {decls:#?}"
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(if_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected IfThenElse");
    };
    assert!(
        if_expr.condition().is_some(),
        "condition should be present, got {if_expr:#?}",
    );
    assert!(
        if_expr.then_branch().is_some(),
        "then_branch should be present, got {if_expr:#?}",
    );
    assert!(
        if_expr.else_branch().is_none(),
        "else_branch must be None for the no-else form, got {if_expr:#?}",
    );
}

/// Phase 5.2 — `let x = if true then 1`: no-else `if` on the
/// RHS of a `let`. The let's BlockEnd close (via
/// [`Self::drain_let_rhs_block`]) must work cleanly when the
/// if's then-body BlockEnd has already terminated the inner
/// scope. No diagnostic; `else_branch = None`.
#[test]
fn if_no_else_at_let_rhs_parses_cleanly() {
    let source = "let x = if true then 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "no-else `if` on a let RHS should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected one let-decl, got {decls:#?}");
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Let, got {:?}", decls[0]);
    };
    let binding = let_decl.bindings().next().expect("let has a binding");
    let rhs = binding.expr().expect("binding has a RHS");
    let crate::syntax::Expr::IfThenElse(if_expr) = rhs else {
        panic!("expected let RHS to be IfThenElse, got {rhs:?}");
    };
    assert!(
        if_expr.else_branch().is_none(),
        "let RHS no-else if must have else_branch = None, got {if_expr:#?}",
    );
}

/// Phase 5.2 — `if c1 then if c2 then 1`: two nested no-else
/// forms. The inner if's BlockEnd terminates the inner; the
/// outer's BlockEnd terminates the outer. Both must produce
/// `else_branch = None` with no diagnostics.
#[test]
fn nested_no_else_ifs_both_have_no_else_branch() {
    let source = "if c1 then if c2 then 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "nested no-else `if`s should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected one decl, got {decls:#?}");
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    assert!(
        outer.else_branch().is_none(),
        "outer if must have else_branch = None, got {outer:#?}",
    );
    let then_branch = outer.then_branch().expect("outer then branch");
    let crate::syntax::Expr::IfThenElse(inner) = then_branch else {
        panic!("expected inner IfThenElse in then-branch, got {then_branch:?}");
    };
    assert!(
        inner.else_branch().is_none(),
        "inner if must also have else_branch = None, got {inner:#?}",
    );
}

/// Phase 5.2 — `if true then\n    1\n    2`: a no-else `if` with
/// a multi-statement then-body. The body wraps in
/// `SEQUENTIAL_EXPR`; the BlockEnd close drains through the
/// `Virtual::BlockSep` between `1` and `2` before terminating
/// the if. `else_branch = None`.
#[test]
fn if_no_else_multi_statement_then_body() {
    let source = "if true then\n    1\n    2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "no-else `if` with multi-statement then-body should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one top-level decl (leaked body content would surface as extra decls), got {decls:#?}",
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(if_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected IfThenElse");
    };
    assert!(
        if_expr.else_branch().is_none(),
        "no-else if must have else_branch = None, got {if_expr:#?}",
    );
    let then_branch = if_expr.then_branch().expect("then branch");
    let crate::syntax::Expr::Sequential(seq) = then_branch else {
        panic!("multi-statement then-body should be Sequential, got {then_branch:?}");
    };
    assert_eq!(
        seq.statements().count(),
        2,
        "expected two statements in the then-body SeqBlock",
    );
}

/// Phase 5.3 — `if a then 1 elif b then 2 else 3`: a single
/// `elif` arm with a trailing `else`. FCS encodes the chain as
/// `IfThenElse(a, 1, Some (IfThenElse(b, 2, Some 3)))` — the
/// `elif` becomes a nested IfThenElseExpr sitting in the outer's
/// else-slot. Our shape mirrors that: outer's `else_branch()`
/// returns an `IfThenElse` whose `condition`/`then_branch`/
/// `else_branch` are all populated.
#[test]
fn elif_with_trailing_else_nests_in_else_slot() {
    let source = "if a then 1 elif b then 2 else 3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "basic elif chain should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one top-level decl, got {decls:#?}"
    );
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    assert!(outer.condition().is_some(), "outer condition");
    assert!(outer.then_branch().is_some(), "outer then");
    let outer_else = outer.else_branch().expect("outer else_branch");
    let crate::syntax::Expr::IfThenElse(inner) = outer_else else {
        panic!("expected nested IfThenElse in outer else slot, got {outer_else:?}");
    };
    assert!(inner.condition().is_some(), "inner condition");
    assert!(inner.then_branch().is_some(), "inner then");
    assert!(
        inner.else_branch().is_some(),
        "inner else_branch must be Some(3), got {inner:#?}",
    );
}

/// Phase 5.3 — `if a then 1 else if b then 2 else 3`: the
/// `else if` form. LexFilter merges adjacent `else` + `if` on
/// the same line into a single `Token::Elif` covering both
/// keywords' span. Structurally identical to bare `elif` after
/// the merge.
#[test]
fn merged_else_if_nests_in_else_slot() {
    let source = "if a then 1 else if b then 2 else 3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "merged `else if` should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    let outer_else = outer.else_branch().expect("outer else_branch");
    let crate::syntax::Expr::IfThenElse(inner) = outer_else else {
        panic!("expected nested IfThenElse in outer else, got {outer_else:?}");
    };
    assert!(
        inner.else_branch().is_some(),
        "inner else_branch must be Some(3)"
    );
}

/// Phase 5.3 — `if a then 1 elif b then 2 elif c then 3 else 4`:
/// two `elif` arms. Each elif nests in the previous if's else
/// slot, producing `IfThenElse(a, 1, Some(IfThenElse(b, 2,
/// Some(IfThenElse(c, 3, Some(4))))))`.
#[test]
fn two_elif_arms_nest_through_else_slots() {
    let source = "if a then 1 elif b then 2 elif c then 3 else 4\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "two-deep elif chain should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    let crate::syntax::Expr::IfThenElse(mid) = outer.else_branch().expect("outer else_branch")
    else {
        panic!("expected nested IfThenElse (mid)");
    };
    let crate::syntax::Expr::IfThenElse(innermost) = mid.else_branch().expect("mid else_branch")
    else {
        panic!("expected nested IfThenElse (innermost)");
    };
    assert!(
        innermost.else_branch().is_some(),
        "innermost else_branch must be Some(4)",
    );
}

/// Phase 5.3 — `if a then 1 elif b then 2`: elif chain with NO
/// trailing else. The inner (elif) IfThenElseExpr should have
/// `else_branch() = None`, just like the no-else form in
/// Phase 5.2. FCS gives
/// `IfThenElse(a, 1, Some(IfThenElse(b, 2, None)))`.
#[test]
fn elif_without_trailing_else_inner_is_no_else() {
    let source = "if a then 1 elif b then 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "elif chain without trailing else should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    let crate::syntax::Expr::IfThenElse(inner) = outer.else_branch().expect("outer else_branch")
    else {
        panic!("expected nested IfThenElse in outer else slot");
    };
    assert!(
        inner.else_branch().is_none(),
        "inner (elif arm) must have else_branch = None, got {inner:#?}",
    );
}

/// Phase 5.3 — `let x = if a then 1 elif b then 2 else 3`: elif
/// as the RHS of a `let`. Mirrors the Phase 5.2 let-RHS test;
/// confirms the BlockEnd accounting handles the nested elif's
/// SeqBlock without leaking the let's BlockEnd.
#[test]
fn elif_chain_at_let_rhs_parses_cleanly() {
    let source = "let x = if a then 1 elif b then 2 else 3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "elif at let RHS should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected one let-decl, got {decls:#?}");
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!("expected Let decl, got {:?}", decls[0]);
    };
    let binding = let_decl.bindings().next().expect("let has a binding");
    let rhs = binding.expr().expect("binding has a RHS");
    let crate::syntax::Expr::IfThenElse(outer) = rhs else {
        panic!("expected let RHS to be IfThenElse, got {rhs:?}");
    };
    let crate::syntax::Expr::IfThenElse(inner) = outer.else_branch().expect("outer else_branch")
    else {
        panic!("expected nested IfThenElse in outer else slot");
    };
    assert!(
        inner.else_branch().is_some(),
        "inner else_branch must be Some(3)"
    );
}

/// Phase 5.3 — `if a then 1 else (* c *) if b then 2 else 3`: a
/// block comment inside the merged `else if` keyword pair. LexFilter
/// rewrites both keywords into a single `Token::Elif` whose span
/// covers the entire run including the comment. The parser must
/// emit the keywords as distinct `ELSE_TOK` and `IF_TOK` tokens
/// (with the comment draining between them as its own
/// `COMMENT` token) — otherwise the comment is hidden inside a
/// single keyword text run and queries for it as a child token
/// fail.
#[test]
fn merged_else_if_preserves_intervening_block_comment() {
    let source = "if a then 1 else (* c *) if b then 2 else 3\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "merged `else if` with block comment should parse cleanly, got: {:?}",
        parse.errors,
    );

    // The comment must be addressable as a `BLOCK_COMMENT` token
    // in the green tree (not subsumed into a keyword's text).
    // Walking `descendants_with_tokens` and looking up its kind
    // catches the regression where the entire `else (* c *) if`
    // run was emitted as one `ELIF_TOK`.
    let comment_tok = parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == crate::syntax::SyntaxKind::BLOCK_COMMENT);
    assert!(
        comment_tok.is_some(),
        "block comment between merged `else if` keywords must survive as its own BLOCK_COMMENT token",
    );
    assert_eq!(
        comment_tok.unwrap().text(),
        "(* c *)",
        "comment text must match source verbatim",
    );
}

/// Phase 5.3 — bare `elif` produces a single `ELIF_TOK` leading
/// the nested `IF_THEN_ELSE_EXPR`, while the merged `else if`
/// form produces an `ELSE_TOK` (in the outer node) followed by a
/// nested `IF_THEN_ELSE_EXPR` whose first token is `IF_TOK`. This
/// shape distinction mirrors FCS's `SynExprIfThenElseTrivia.isElif`
/// flag (`true` for bare elif, `false` for the desugared merge),
/// and lets later passes recover the source-level form without
/// inspecting token text.
#[test]
fn bare_elif_versus_merged_else_if_have_distinct_leading_tokens() {
    use crate::syntax::AstNode;

    fn inner_first_token_kind(source: &str) -> crate::syntax::SyntaxKind {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "parse errors for `{source}`: {:?}",
            parse.errors
        );
        let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
        let module = file.modules().next().expect("module");
        let decls: Vec<_> = module.decls().collect();
        let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
            panic!("expected Expr decl");
        };
        let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("outer expr") else {
            panic!("expected outer IfThenElse");
        };
        let crate::syntax::Expr::IfThenElse(inner) =
            outer.else_branch().expect("outer else_branch")
        else {
            panic!("expected nested IfThenElse in outer else");
        };
        inner
            .syntax()
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| {
                !matches!(
                    t.kind(),
                    crate::syntax::SyntaxKind::WHITESPACE
                        | crate::syntax::SyntaxKind::NEWLINE
                        | crate::syntax::SyntaxKind::LINE_COMMENT
                        | crate::syntax::SyntaxKind::BLOCK_COMMENT
                )
            })
            .expect("inner has a non-trivia leading token")
            .kind()
    }

    assert_eq!(
        inner_first_token_kind("if a then 1 elif b then 2 else 3\n"),
        crate::syntax::SyntaxKind::ELIF_TOK,
        "bare `elif` must lead the nested node with ELIF_TOK",
    );
    assert_eq!(
        inner_first_token_kind("if a then 1 else if b then 2 else 3\n"),
        crate::syntax::SyntaxKind::IF_TOK,
        "merged `else if` must lead the nested node with IF_TOK (ELSE_TOK lives in the outer node)",
    );
}

/// Phase 5.3 — `if a then 1\n// c\nelif b then 2`: a comment
/// between arms of an elif chain. The trailing newline and the
/// comment must anchor as children of the OUTER `IF_THEN_ELSE_EXPR`,
/// not inside the nested elif's node — otherwise the inner node's
/// text range starts before the `elif` keyword and tree queries
/// for the comment's ancestors traverse through the elif arm.
/// Pins that `drain_raw_up_to(elif_span.start)` runs before the
/// inner `start_node`, mirroring the trivia anchoring used by the
/// `else` arm.
#[test]
fn elif_inner_node_starts_at_keyword_not_before_trivia() {
    let source = "if a then 1\n// c\nelif b then 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "elif chain with inter-arm comment should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::IfThenElse(outer) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected outer IfThenElse");
    };
    let crate::syntax::Expr::IfThenElse(inner) = outer.else_branch().expect("outer else_branch")
    else {
        panic!("expected nested IfThenElse in outer else slot");
    };
    let elif_offset = source.find("elif").expect("source must contain `elif`");
    let inner_start: usize = inner.syntax().text_range().start().into();
    assert_eq!(
        inner_start, elif_offset,
        "inner elif node must start at the `elif` keyword \
             (got start {inner_start}, expected {elif_offset}); \
             leading trivia/comment leaked into the nested node",
    );
}

/// Phase 5.3 — `let x = if a then 1 elif b then 2 else 3\nlet y = 4`:
/// elif chain at let RHS followed by a sibling decl. Confirms
/// the nested elif's BlockEnd cascade doesn't swallow the let's
/// closing BlockEnd, so the parser cleanly picks up `let y` as
/// a sibling rather than absorbing it into the first binding.
#[test]
fn elif_chain_at_let_rhs_then_sibling_decl() {
    let source = "let x = if a then 1 elif b then 2 else 3\nlet y = 4\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "elif at let RHS with sibling decl should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 2, "expected two let-decls, got {decls:#?}");
}

/// Phase 5.2 — `fun x y -> x`: curried two-argument form.
/// `FunExpr.args()` must yield two `Pat::Named` children in source
/// order. Since our green tree stores the args flat under one
/// `FUN_EXPR` (the moral equivalent of FCS's `parsedData` cache),
/// both args appear as siblings rather than via the curried
/// `Lambda(_, Lambda(_, …))` nesting.
#[test]
fn fun_curried_two_args() {
    let source = "fun x y -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "two-arg fun-lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 2, "expected 2 args, got {args:#?}");
    for (i, expected) in ["x", "y"].iter().enumerate() {
        let crate::syntax::Pat::Named(named) = &args[i] else {
            panic!("expected Named pat at arg #{i}, got {:?}", args[i]);
        };
        assert_eq!(
            named.ident().expect("named pat has ident").text(),
            *expected,
            "arg #{i} name mismatch",
        );
    }
}

/// Phase 5.2 — `fun () -> 1`: unit-parameter form. FCS parses `()`
/// as `SynPat.Paren(SynPat.Const(SynConst.Unit, …), …)` because
/// the unit form only ever reaches the pattern grammar wrapped in
/// parens (`pars.fsy:3929`). `FunExpr.args()` must yield one
/// `ParenPat`, whose `inner()` is a `ConstPat`.
#[test]
fn fun_unit_arg() {
    let source = "fun () -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "unit-arg fun-lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 1, "expected 1 arg for `()`, got {args:#?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("expected ParenPat for `()`, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("paren has inner");
    assert!(
        matches!(inner, crate::syntax::Pat::Const(_)),
        "unit pattern's inner must be a ConstPat, got {inner:?}",
    );
}

/// Phase 5.2 — `fun (x, y) -> x`: tuple-paren parameter form. The
/// tuple pattern only appears inside a `ParenPat` because the
/// fun-lambda's argument grammar takes `atomicPattern+`, and
/// `TuplePat` is not atomic — it must be wrapped. The wrapped
/// shape is `Paren(Tuple([x; y]))`, mirroring FCS exactly.
#[test]
fn fun_tuple_paren_arg() {
    let source = "fun (x, y) -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "tuple-paren-arg fun-lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(
        args.len(),
        1,
        "expected 1 (paren-wrapped) arg, got {args:#?}"
    );
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("expected ParenPat, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("paren has inner");
    let crate::syntax::Pat::Tuple(tuple) = inner else {
        panic!("expected TuplePat inside parens, got {inner:?}");
    };
    let elems: Vec<_> = tuple.elements().collect();
    assert_eq!(elems.len(), 2, "expected 2 tuple elements, got {elems:#?}");
}

/// Phase 5 Gap B — `fun (x) 0 -> y`: a simple named paren arg followed
/// by a non-simple const arg. The function-form arg sweep (reached when
/// `parse_paren_pat` parses the `(x)`) used to promote the paren to a
/// function-form head and fold `0` into a bogus arg list by peeking past
/// the swallowed `)`. The raw-stream gate stops at the `)`, so the two
/// args stay distinct: `[Paren(Named x), Const 0]`.
#[test]
fn lambda_paren_then_const_arg() {
    let source = "fun (x) 0 -> y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "paren-then-const-arg lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 2, "expected 2 curried args, got {args:#?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("arg #0 should be ParenPat, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("paren has inner");
    assert!(
        matches!(inner, crate::syntax::Pat::Named(_)),
        "arg #0 inner should be NAMED_PAT, got {inner:?}",
    );
    assert!(
        matches!(&args[1], crate::syntax::Pat::Const(_)),
        "arg #1 should be CONST_PAT, got {:?}",
        args[1],
    );
}

/// Phase 5 Gap B — `fun (x as y) 0 -> y`: an `as`-pat paren arg followed
/// by a const arg. The sweep must stop at the paren's swallowed `)`, so
/// the `0` is a separate const arg rather than being folded into the
/// `as`-pat. Green args: `[Paren(As(x, y)), Const 0]`.
#[test]
fn lambda_paren_as_then_const_arg() {
    let source = "fun (x as y) 0 -> y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "paren-as-then-const-arg lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 2, "expected 2 curried args, got {args:#?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("arg #0 should be ParenPat, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("paren has inner");
    assert!(
        matches!(inner, crate::syntax::Pat::As(_)),
        "arg #0 inner should be AS_PAT, got {inner:?}",
    );
    assert!(
        matches!(&args[1], crate::syntax::Pat::Const(_)),
        "arg #1 should be CONST_PAT, got {:?}",
        args[1],
    );
}

/// Phase 5 Gap B — `fun (Some x) y -> x`: a ctor-app paren arg followed
/// by a bare named arg. The sweep must stop at the paren's swallowed
/// `)`, leaving `y` as a second curried `NAMED_PAT` rather than folding
/// it into `Some`'s argument list. Green args: `[Paren(Some x), Named y]`.
#[test]
fn lambda_paren_ctor_then_named_arg() {
    let source = "fun (Some x) y -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "paren-ctor-then-named-arg lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 2, "expected 2 curried args, got {args:#?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("arg #0 should be ParenPat, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("paren has inner");
    assert!(
        matches!(inner, crate::syntax::Pat::LongIdent(_)),
        "arg #0 inner should be LONG_IDENT_PAT (`Some x`), got {inner:?}",
    );
    assert!(
        matches!(&args[1], crate::syntax::Pat::Named(_)),
        "arg #1 should be NAMED_PAT (`y`), got {:?}",
        args[1],
    );
}

/// Phase 5.X.6 — `fun (x : int) -> x`: a typed paren arg. The lambda-arg
/// path (`try_emit_atomic_pat` → `parse_paren_pat` →
/// `emit_paren_pat_element`) attaches the per-element `:` to the inner
/// pat, so the single arg is `Paren(Typed(Named x, int))` — the same
/// shape as the let-head typed-arg case, now pinned at the lambda site.
#[test]
fn lambda_typed_paren_arg() {
    let source = "fun (x : int) -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "typed-paren-arg lambda should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let crate::syntax::ModuleDecl::Expr(expr_decl) = &decls[0] else {
        panic!("expected Expr decl, got {:?}", decls[0]);
    };
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 1, "expected 1 typed arg, got {args:#?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("arg #0 should be ParenPat, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("paren has inner");
    let crate::syntax::Pat::Typed(typed) = inner else {
        panic!("arg #0 inner should be TYPED_PAT, got {inner:?}");
    };
    assert!(
        matches!(typed.pat(), Some(crate::syntax::Pat::Named(_))),
        "typed inner pat should be NAMED_PAT (`x`), got {:?}",
        typed.pat(),
    );
    assert!(
        matches!(typed.ty(), Some(crate::syntax::Type::LongIdent(_))),
        "typed annotation should be LONG_IDENT_TYPE (`int`), got {:?}",
        typed.ty(),
    );
}

/// Phase 5.2 — `let f = fun x -> x`: lambda on the RHS of a
/// let-binding. The let's RHS-block close (via
/// [`Self::drain_let_rhs_block`]) must work cleanly when the
/// lambda's trailing virtuals are already consumed by
/// `parse_fun_expr`'s drain loop. The binding's `expr()` must
/// resolve to a `FunExpr`, not e.g. swallow the lambda as part of
/// a sequential or app context.
#[test]
fn fun_lambda_as_let_rhs() {
    let source = "let f = fun x -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "lambda-as-let-RHS should parse cleanly, got: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected one let-decl, got {decls:#?}");
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Let, got {:?}", decls[0]);
    };
    let binding = let_decl.bindings().next().expect("let has a binding");
    let rhs = binding.expr().expect("binding has a RHS");
    let crate::syntax::Expr::Fun(fun_expr) = rhs else {
        panic!("expected let RHS to be FunExpr, got {rhs:?}");
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 1, "expected 1 arg in lambda RHS, got {args:#?}");
}

#[test]
fn match_single_clause_tree_shape() {
    let source = "match x with A -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "single-clause match should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..20
  MODULE_OR_NAMESPACE@0..20
    EXPR_DECL@0..19
      MATCH_EXPR@0..19
        MATCH_TOK@0..5 \"match\"
        IDENT_EXPR@5..7
          WHITESPACE@5..6 \" \"
          IDENT_TOK@6..7 \"x\"
        WHITESPACE@7..8 \" \"
        WITH_TOK@8..12 \"with\"
        MATCH_CLAUSE@12..19
          LONG_IDENT_PAT@12..14
            LONG_IDENT@12..14
              WHITESPACE@12..13 \" \"
              IDENT_TOK@13..14 \"A\"
          WHITESPACE@14..15 \" \"
          RARROW_TOK@15..17 \"->\"
          CONST_EXPR@17..19
            WHITESPACE@17..18 \" \"
            INT32_LIT@18..19 \"1\"
          ERROR@19..19 \"\"
        ERROR@19..19 \"\"
    NEWLINE@19..20 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 5.M.1 — `match x with Some y -> y`: ctor-application clause
/// pattern. The head-binding entry produces a `LONG_IDENT_PAT` whose
/// trailing argument is a `NAMED_PAT` (`SynPat.LongIdent` with one
/// arg pattern). Exercised via the facade accessors rather than a
/// full shape pin.
#[test]
fn match_ctor_clause_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match x with Some y -> y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(m.scrutinee(), Some(Expr::Ident(_))),
        "scrutinee should be `x`",
    );
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 1, "single clause");
    assert!(
        matches!(clauses[0].pat(), Some(Pat::LongIdent(_))),
        "clause pat should be ctor-app LONG_IDENT_PAT, got {:?}",
        clauses[0].pat(),
    );
    assert!(
        matches!(clauses[0].result(), Some(Expr::Ident(_))),
        "clause result should be ident `y`",
    );
}

/// Phase 5.M.1 — `match (a, b) with x, y -> x`: a tuple scrutinee
/// (`PAREN_EXPR > TUPLE_EXPR`) with a top-level tuple clause pattern
/// (`TUPLE_PAT`). Confirms `parenPattern` reaches the comma-tuple
/// production and that the scrutinee `parse_expr` stops at the
/// `Virtual::With`.
#[test]
fn match_tuple_clause_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match (a, b) with x, y -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr");
    };
    assert!(
        matches!(m.scrutinee(), Some(Expr::Paren(_))),
        "scrutinee should be the paren-tuple `(a, b)`, got {:?}",
        m.scrutinee(),
    );
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 1);
    assert!(
        matches!(clauses[0].pat(), Some(Pat::Tuple(_))),
        "clause pat should be TUPLE_PAT, got {:?}",
        clauses[0].pat(),
    );
}

/// Dotted DU clause pattern — `match foo with | Foo.Bar -> ()`: a *nullary*
/// multi-segment long-ident pattern. FCS's `atomicPatternLongIdent: pathOp`
/// sweeps the whole `Foo.Bar` path into one `SynPat.LongIdent` whose
/// `SynLongIdent` carries both segments (multi-segment ⇒ `LongIdent`, never
/// `Named`). The clause pat must be a `LONG_IDENT_PAT` whose head `LONG_IDENT`
/// yields both `Foo` and `Bar`, with no curried args and no parse errors.
#[test]
fn match_dotted_nullary_du_clause() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match foo with | Foo.Bar -> ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 1, "single clause");
    let Some(Pat::LongIdent(li)) = clauses[0].pat() else {
        panic!(
            "clause pat should be LONG_IDENT_PAT, got {:?}",
            clauses[0].pat()
        );
    };
    let segs: Vec<String> = li
        .head()
        .expect("LONG_IDENT_PAT head")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo", "Bar"], "head path segments");
    assert_eq!(li.args().count(), 0, "nullary — no curried args");
}

/// Dotted DU clause pattern with an argument — `match foo with | Foo.Bar x ->
/// ()`: the multi-segment head sweeps `Foo.Bar`, then the function-form arg
/// sweep collects the trailing `x` as a single `NAMED_PAT` curried argument
/// (FCS's `atomicPatternLongIdent atomicPatsOrNamePatPairs`).
#[test]
fn match_dotted_applied_du_clause() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match foo with | Foo.Bar x -> ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    let Some(Pat::LongIdent(li)) = clauses[0].pat() else {
        panic!(
            "clause pat should be LONG_IDENT_PAT, got {:?}",
            clauses[0].pat()
        );
    };
    let segs: Vec<String> = li
        .head()
        .expect("LONG_IDENT_PAT head")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo", "Bar"], "head path segments");
    let args: Vec<_> = li.args().collect();
    assert_eq!(args.len(), 1, "one curried arg, got {args:?}");
    assert!(
        matches!(&args[0], Pat::Named(_)),
        "arg #0 should be NAMED_PAT, got {:?}",
        args[0],
    );
}

/// Dotted DU clause pattern, three segments — `match foo with | A.B.C -> ()`:
/// the dot-continuation sweeps every segment into the head `LONG_IDENT`.
#[test]
fn match_dotted_three_segment_du_clause() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match foo with | A.B.C -> ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    let Some(Pat::LongIdent(li)) = clauses[0].pat() else {
        panic!(
            "clause pat should be LONG_IDENT_PAT, got {:?}",
            clauses[0].pat()
        );
    };
    let segs: Vec<String> = li
        .head()
        .expect("LONG_IDENT_PAT head")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["A", "B", "C"], "head path segments");
}

/// `global.`-rooted dotted pattern (FCS's `GLOBAL DOT pathOp`) — now a
/// recognised pattern head (the formerly-deferred slice, sibling of `_.M`). It
/// parses as a `LONG_IDENT_PAT` whose first segment is the reused keyword
/// `global`, mirroring the dotted-DU clause above. Cross-checked against FCS in
/// `tests/all/parser_diff_global_pat.rs`.
#[test]
fn match_global_rooted_dotted_pat_is_long_ident() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match v with global.N.Case -> ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    let Some(Pat::LongIdent(li)) = clauses[0].pat() else {
        panic!(
            "clause pat should be LONG_IDENT_PAT, got {:?}",
            clauses[0].pat()
        );
    };
    let segs: Vec<String> = li
        .head()
        .expect("LONG_IDENT_PAT head")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["global", "N", "Case"], "head path segments");
}

/// A bare lowercase ident clause pattern stays a `NAMED_PAT` (single segment,
/// lowercase, no args) — guards against the dotted-head change over-promoting
/// the common variable-binding clause to `LONG_IDENT_PAT`.
#[test]
fn match_bare_lowercase_clause_stays_named() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match foo with | x -> ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    assert!(
        matches!(clauses[0].pat(), Some(Pat::Named(_))),
        "bare lowercase clause pat should be NAMED_PAT, got {:?}",
        clauses[0].pat(),
    );
}

/// Phase 5.M.1 — `let f x = match x with A -> 1`: a match expression
/// on a let-binding RHS. The binding's `expr()` must resolve to a
/// `MatchExpr`, confirming `parse_match_expr`'s single-pair drain of
/// its own `RightBlockEnd`/`End` leaves the enclosing let's
/// `BlockEnd`/`DeclEnd` virtuals intact for `parse_let_binding`.
/// Mirrors `fun_lambda_as_let_rhs`.
#[test]
fn match_as_let_rhs() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "let f x = match x with A -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "match-as-let-RHS should parse cleanly, got: {:?}",
        parse.errors,
    );

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Let(let_decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Let");
    };
    let binding = let_decl.bindings().next().expect("binding");
    let Expr::Match(m) = binding.expr().expect("binding RHS") else {
        panic!("expected let RHS to be MatchExpr, got {:?}", binding.expr());
    };
    assert_eq!(m.clauses().count(), 1, "single clause");
}

/// Phase 5.M.2 — `match x with A -> 1 | B -> 2`: two single-line clauses
/// separated by a bare `|`. Pins the full green shape. The first clause
/// carries no `BAR_TOK`; the second clause owns its `BAR_TOK` (mirroring
/// FCS's per-clause `BarRange`). The single-line LexFilter shape emits
/// only ONE trailing `RightBlockEnd` (drained as the zero-width `ERROR`
/// inside the *last* clause) and one `End` (the `ERROR` directly under
/// `MATCH_EXPR`).
#[test]
fn match_two_clauses_tree_shape() {
    let source = "match x with A -> 1 | B -> 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "two-clause match should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..29
  MODULE_OR_NAMESPACE@0..29
    EXPR_DECL@0..28
      MATCH_EXPR@0..28
        MATCH_TOK@0..5 \"match\"
        IDENT_EXPR@5..7
          WHITESPACE@5..6 \" \"
          IDENT_TOK@6..7 \"x\"
        WHITESPACE@7..8 \" \"
        WITH_TOK@8..12 \"with\"
        MATCH_CLAUSE@12..19
          LONG_IDENT_PAT@12..14
            LONG_IDENT@12..14
              WHITESPACE@12..13 \" \"
              IDENT_TOK@13..14 \"A\"
          WHITESPACE@14..15 \" \"
          RARROW_TOK@15..17 \"->\"
          CONST_EXPR@17..19
            WHITESPACE@17..18 \" \"
            INT32_LIT@18..19 \"1\"
        MATCH_CLAUSE@19..28
          WHITESPACE@19..20 \" \"
          BAR_TOK@20..21 \"|\"
          LONG_IDENT_PAT@21..23
            LONG_IDENT@21..23
              WHITESPACE@21..22 \" \"
              IDENT_TOK@22..23 \"B\"
          WHITESPACE@23..24 \" \"
          RARROW_TOK@24..26 \"->\"
          CONST_EXPR@26..28
            WHITESPACE@26..27 \" \"
            INT32_LIT@27..28 \"2\"
          ERROR@28..28 \"\"
        ERROR@28..28 \"\"
    NEWLINE@28..29 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 5.M.2 — `match x with | A -> 1 | B -> 2`: an optional *leading*
/// bar before the first clause. Both clauses then own a `BAR_TOK`, but
/// the projection must be identical to the no-leading-bar form (FCS
/// elides the leading-bar range). Exercised via accessors: two clauses,
/// each with a ctor-ref `LONG_IDENT_PAT`.
#[test]
fn match_leading_bar_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match x with | A -> 1 | B -> 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(
        clauses.len(),
        2,
        "leading bar must not produce an extra clause"
    );
    for (i, clause) in clauses.iter().enumerate() {
        assert!(
            matches!(clause.pat(), Some(Pat::LongIdent(_))),
            "clause {i} pat should be ctor-ref LONG_IDENT_PAT, got {:?}",
            clause.pat(),
        );
        assert!(
            matches!(clause.result(), Some(Expr::Const(_))),
            "clause {i} result should be a Const, got {:?}",
            clause.result(),
        );
    }
}

/// Phase 5.M.3 — `match x with A when y -> 1`: the minimal `when`-guard
/// form. Pins the full green shape: a `WHEN_TOK` + guard `Expr` slot in
/// between the clause pattern and `RARROW_TOK`. The guard is a plain
/// `IDENT_EXPR` (`y`); the result is a `CONST_EXPR` (`1`). The two
/// trailing zero-width `ERROR` leaves are the same `RightBlockEnd` / `End`
/// drains as the no-guard form.
#[test]
fn match_when_guard_tree_shape() {
    let source = "match x with A when y -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "guarded match should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..27
  MODULE_OR_NAMESPACE@0..27
    EXPR_DECL@0..26
      MATCH_EXPR@0..26
        MATCH_TOK@0..5 \"match\"
        IDENT_EXPR@5..7
          WHITESPACE@5..6 \" \"
          IDENT_TOK@6..7 \"x\"
        WHITESPACE@7..8 \" \"
        WITH_TOK@8..12 \"with\"
        MATCH_CLAUSE@12..26
          LONG_IDENT_PAT@12..14
            LONG_IDENT@12..14
              WHITESPACE@12..13 \" \"
              IDENT_TOK@13..14 \"A\"
          WHITESPACE@14..15 \" \"
          WHEN_TOK@15..19 \"when\"
          IDENT_EXPR@19..21
            WHITESPACE@19..20 \" \"
            IDENT_TOK@20..21 \"y\"
          WHITESPACE@21..22 \" \"
          RARROW_TOK@22..24 \"->\"
          CONST_EXPR@24..26
            WHITESPACE@24..25 \" \"
            INT32_LIT@25..26 \"1\"
          ERROR@26..26 \"\"
        ERROR@26..26 \"\"
    NEWLINE@26..27 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 5.M.3 — `match x with A when y -> 1 | B -> 2`: a guard on the
/// first clause only. Exercises the facade disambiguation: `guard()`
/// returns the leading `Expr` only when a `WHEN_TOK` is present, and
/// `result()` returns the *trailing* `Expr` (the result, not the guard)
/// in both the guarded and unguarded clauses.
#[test]
fn match_when_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "match x with A when y -> 1 | B -> 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 2);

    // Guarded clause: `A when y -> 1`.
    assert!(
        matches!(clauses[0].guard(), Some(Expr::Ident(_))),
        "clause 0 guard should be ident `y`, got {:?}",
        clauses[0].guard(),
    );
    assert!(
        matches!(clauses[0].result(), Some(Expr::Const(_))),
        "clause 0 result should be `1` (not the guard), got {:?}",
        clauses[0].result(),
    );

    // Unguarded clause: `B -> 2`.
    assert!(
        clauses[1].guard().is_none(),
        "clause 1 has no guard, got {:?}",
        clauses[1].guard(),
    );
    assert!(
        matches!(clauses[1].result(), Some(Expr::Const(_))),
        "clause 1 result should be `2`, got {:?}",
        clauses[1].result(),
    );
}

/// Phase 10.4c — `match! x with A -> 1`: full green-shape pin. Identical to
/// `match_single_clause_tree_shape` apart from the `MATCH_BANG_EXPR` node and
/// the `MATCH_BANG_TOK` keyword (`match!`, six chars), confirming `match!`
/// reuses the `match` body verbatim (scrutinee + `with` + the shared clause
/// list, including the trailing `RightBlockEnd`/`End` zero-width ERROR pair).
#[test]
fn match_bang_single_clause_tree_shape() {
    let source = "match! x with A -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "single-clause match! should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    EXPR_DECL@0..20
      MATCH_BANG_EXPR@0..20
        MATCH_BANG_TOK@0..6 \"match!\"
        IDENT_EXPR@6..8
          WHITESPACE@6..7 \" \"
          IDENT_TOK@7..8 \"x\"
        WHITESPACE@8..9 \" \"
        WITH_TOK@9..13 \"with\"
        MATCH_CLAUSE@13..20
          LONG_IDENT_PAT@13..15
            LONG_IDENT@13..15
              WHITESPACE@13..14 \" \"
              IDENT_TOK@14..15 \"A\"
          WHITESPACE@15..16 \" \"
          RARROW_TOK@16..18 \"->\"
          CONST_EXPR@18..20
            WHITESPACE@18..19 \" \"
            INT32_LIT@19..20 \"1\"
          ERROR@20..20 \"\"
        ERROR@20..20 \"\"
    NEWLINE@20..21 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.4c — `match! x with Some y -> y`: exercise the
/// [`MatchBangExpr`](crate::syntax::MatchBangExpr) facade accessors
/// (`scrutinee`/`clauses`), reusing [`MatchClause`](crate::syntax::MatchClause)
/// verbatim. Mirrors `match_ctor_clause_via_accessors`.
#[test]
fn match_bang_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "match! x with Some y -> y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::MatchBang(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchBangExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(m.scrutinee(), Some(Expr::Ident(_))),
        "scrutinee should be `x`",
    );
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 1, "single clause");
    assert!(
        matches!(clauses[0].pat(), Some(Pat::LongIdent(_))),
        "clause pat should be ctor-app LONG_IDENT_PAT, got {:?}",
        clauses[0].pat(),
    );
    assert!(
        matches!(clauses[0].result(), Some(Expr::Ident(_))),
        "clause result should be ident `y`",
    );
}

/// Phase 10.4d — `while c do f`: full green-shape pin. The condition is the
/// leading `Expr` child, the body the trailing one; `DO_TOK` separates them and
/// the `BlockBegin`/`BlockEnd`/`DeclEnd` SeqBlock scaffolding is consumed as
/// `ERROR` leaves (the same shape `do!` produces).
#[test]
fn while_tree_shape() {
    let source = "while c do f\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "while loop should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..13
  MODULE_OR_NAMESPACE@0..13
    EXPR_DECL@0..12
      WHILE_EXPR@0..12
        WHILE_TOK@0..5 \"while\"
        IDENT_EXPR@5..7
          WHITESPACE@5..6 \" \"
          IDENT_TOK@6..7 \"c\"
        WHITESPACE@7..8 \" \"
        DO_TOK@8..10 \"do\"
        WHITESPACE@10..11 \" \"
        ERROR@11..11 \"\"
        IDENT_EXPR@11..12
          IDENT_TOK@11..12 \"f\"
        ERROR@12..12 \"\"
        ERROR@12..12 \"\"
    NEWLINE@12..13 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.4d — `while c do g x`: exercise the
/// [`WhileExpr`](crate::syntax::WhileExpr) facade accessors. `cond()` is the
/// leading `Expr` (the condition `c`), `body()` the trailing one (the
/// application `g x`).
#[test]
fn while_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "while c do g x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::While(w) = expr_decl.expr().expect("expr") else {
        panic!("expected WhileExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(w.cond(), Some(Expr::Ident(_))),
        "cond should be ident `c`, got {:?}",
        w.cond(),
    );
    assert!(
        matches!(w.body(), Some(Expr::App(_))),
        "body should be the application `g x`, got {:?}",
        w.body(),
    );
}

/// Top-level `do f`: full green-shape pin. The decl is a `SynModuleDecl.Expr`
/// (`EXPR_DECL`) wrapping a `SynExpr.Do` (`DO_EXPR`); `DO_TOK` leads, the body
/// is the trailing `Expr` child, and the `BlockBegin`/`BlockEnd`/`DeclEnd`
/// SeqBlock scaffolding is consumed as `ERROR` leaves (the `do!`/`while` shape).
#[test]
fn top_level_do_tree_shape() {
    let source = "do f\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "top-level `do` should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      DO_EXPR@0..4
        DO_TOK@0..2 \"do\"
        WHITESPACE@2..3 \" \"
        ERROR@3..3 \"\"
        IDENT_EXPR@3..4
          IDENT_TOK@3..4 \"f\"
        ERROR@4..4 \"\"
        ERROR@4..4 \"\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Top-level `do g x`: exercise the [`DoExpr`](crate::syntax::DoExpr) facade.
/// The decl is an `Expr` module-decl whose expression is a `DoExpr`; `inner()`
/// is the body application `g x`.
#[test]
fn top_level_do_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "do g x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Do(d) = expr_decl.expr().expect("expr") else {
        panic!("expected DoExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(d.inner(), Some(Expr::App(_))),
        "body should be the application `g x`, got {:?}",
        d.inner(),
    );
}

/// Phase 10.4d — `while c do f done`: the explicit verbose-syntax `done`
/// terminator is claimed as `DONE_TOK` (not left as an unsupported leftover).
/// LexFilter relabels the raw `done` to the body's closing `Virtual::DeclEnd`
/// at the `done` span (and a coincident zero-width `BlockEnd`); the parser
/// claims the backing raw `Token::Done`, so the parse is clean and
/// `text(tree) == source`.
#[test]
fn while_done_terminator_tree_shape() {
    let source = "while c do f done\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "`while … do … done` should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    EXPR_DECL@0..17
      WHILE_EXPR@0..17
        WHILE_TOK@0..5 \"while\"
        IDENT_EXPR@5..7
          WHITESPACE@5..6 \" \"
          IDENT_TOK@6..7 \"c\"
        WHITESPACE@7..8 \" \"
        DO_TOK@8..10 \"do\"
        WHITESPACE@10..11 \" \"
        ERROR@11..11 \"\"
        IDENT_EXPR@11..12
          IDENT_TOK@11..12 \"f\"
        ERROR@12..12 \"\"
        WHITESPACE@12..13 \" \"
        DONE_TOK@13..17 \"done\"
    NEWLINE@17..18 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.4e — `while! c do f`: full green-shape pin. Identical to
/// `while_tree_shape` apart from the `WHILE_BANG_EXPR` node and the
/// `WHILE_BANG_TOK` keyword (`while!`, six chars), confirming `while!` routes
/// through the same `parse_while_loop` as plain `while`.
#[test]
fn while_bang_tree_shape() {
    let source = "while! c do f\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "`while!` should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..14
  MODULE_OR_NAMESPACE@0..14
    EXPR_DECL@0..13
      WHILE_BANG_EXPR@0..13
        WHILE_BANG_TOK@0..6 \"while!\"
        IDENT_EXPR@6..8
          WHITESPACE@6..7 \" \"
          IDENT_TOK@7..8 \"c\"
        WHITESPACE@8..9 \" \"
        DO_TOK@9..11 \"do\"
        WHITESPACE@11..12 \" \"
        ERROR@12..12 \"\"
        IDENT_EXPR@12..13
          IDENT_TOK@12..13 \"f\"
        ERROR@13..13 \"\"
        ERROR@13..13 \"\"
    NEWLINE@13..14 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.4e — `while! c do g x`: exercise the
/// [`WhileBangExpr`](crate::syntax::WhileBangExpr) facade accessors. `cond()`
/// is the leading `Expr` (the condition `c`), `body()` the trailing one (the
/// application `g x`).
#[test]
fn while_bang_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "while! c do g x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::WhileBang(w) = expr_decl.expr().expect("expr") else {
        panic!("expected WhileBangExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(w.cond(), Some(Expr::Ident(_))),
        "cond should be ident `c`, got {:?}",
        w.cond(),
    );
    assert!(
        matches!(w.body(), Some(Expr::App(_))),
        "body should be the application `g x`, got {:?}",
        w.body(),
    );
}

/// `for x in xs do f`: full green-shape pin of a `ForEach`. The binder
/// pattern is a `NAMED_PAT` child, `IN_TOK` separates it from the enumerable
/// collection (the leading `Expr` child), `DO_TOK` from the body (the trailing
/// `Expr` child), and the `BlockBegin`/`BlockEnd`/`DeclEnd` SeqBlock scaffolding
/// is consumed as `ERROR` leaves — exactly the shape `while` produces.
#[test]
fn for_each_tree_shape() {
    let source = "for x in xs do f\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "for loop should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..17
  MODULE_OR_NAMESPACE@0..17
    EXPR_DECL@0..16
      FOR_EACH_EXPR@0..16
        FOR_TOK@0..3 \"for\"
        NAMED_PAT@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        WHITESPACE@5..6 \" \"
        IN_TOK@6..8 \"in\"
        IDENT_EXPR@8..11
          WHITESPACE@8..9 \" \"
          IDENT_TOK@9..11 \"xs\"
        WHITESPACE@11..12 \" \"
        DO_TOK@12..14 \"do\"
        WHITESPACE@14..15 \" \"
        ERROR@15..15 \"\"
        IDENT_EXPR@15..16
          IDENT_TOK@15..16 \"f\"
        ERROR@16..16 \"\"
        ERROR@16..16 \"\"
    NEWLINE@16..17 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// `for x in xs do g x`: exercise the [`ForEachExpr`](crate::syntax::ForEachExpr)
/// facade accessors. `pat()` is the binder (`x`), `enum_expr()` the leading
/// `Expr` (the collection `xs`), `body()` the trailing one (the application
/// `g x`).
#[test]
fn for_each_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "for x in xs do g x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::ForEach(f) = expr_decl.expr().expect("expr") else {
        panic!("expected ForEachExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(f.pat(), Some(Pat::Named(_))),
        "pat should be the named binder `x`, got {:?}",
        f.pat(),
    );
    assert!(
        matches!(f.enum_expr(), Some(Expr::Ident(_))),
        "enum_expr should be ident `xs`, got {:?}",
        f.enum_expr(),
    );
    assert!(
        matches!(f.body(), Some(Expr::App(_))),
        "body should be the application `g x`, got {:?}",
        f.body(),
    );
}

/// `for x in xs -> g x`: full green-shape pin of the comprehension arrow form.
/// The body is a `YIELD_OR_RETURN_EXPR > [RARROW_TOK, <expr>]` (FCS's
/// `YieldOrReturn((true, false), …)`), and the one-sided SeqBlock close trails
/// as a single zero-width `ERROR`.
#[test]
fn for_each_arrow_tree_shape() {
    let source = "for x in xs -> g x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "arrow comprehension should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..19
  MODULE_OR_NAMESPACE@0..19
    EXPR_DECL@0..18
      FOR_EACH_EXPR@0..18
        FOR_TOK@0..3 \"for\"
        NAMED_PAT@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        WHITESPACE@5..6 \" \"
        IN_TOK@6..8 \"in\"
        IDENT_EXPR@8..11
          WHITESPACE@8..9 \" \"
          IDENT_TOK@9..11 \"xs\"
        YIELD_OR_RETURN_EXPR@11..18
          WHITESPACE@11..12 \" \"
          RARROW_TOK@12..14 \"->\"
          APP_EXPR@14..18
            IDENT_EXPR@14..16
              WHITESPACE@14..15 \" \"
              IDENT_TOK@15..16 \"g\"
            IDENT_EXPR@16..18
              WHITESPACE@16..17 \" \"
              IDENT_TOK@17..18 \"x\"
        ERROR@18..18 \"\"
    NEWLINE@18..19 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// `for x in xs -> g x` via accessors: the `ForEachExpr` body is the
/// yield-wrapped expression, and that `YieldExpr` reads as an implicit `yield`
/// (`is_yield()`, not `is_from()`) — matching FCS's `YieldOrReturn((true,
/// false), …)`.
#[test]
fn for_each_arrow_body_is_yield() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "for x in xs -> g x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::ForEach(f) = expr_decl.expr().expect("expr") else {
        panic!("expected ForEachExpr, got {:?}", expr_decl.expr());
    };
    let Some(Expr::Yield(y)) = f.body() else {
        panic!("arrow body should be a YieldExpr, got {:?}", f.body());
    };
    assert!(y.is_yield(), "the `->` body is an implicit yield");
    assert!(!y.is_from(), "the `->` body is YieldOrReturn, not …From");
    assert!(
        matches!(y.inner(), Some(Expr::App(_))),
        "yielded expression should be the application `g x`, got {:?}",
        y.inner(),
    );
}

/// `for i = 1 to 10 do f`: full green-shape pin of a range `For`. The loop
/// variable is a bare `IDENT_TOK`, `EQUALS_TOK` precedes the start bound, the
/// `TO_TOK` direction keyword the end bound, and `DO_TOK` the body; the three
/// `Expr` children are start, end, body in order, with the SeqBlock scaffolding
/// as `ERROR` leaves.
#[test]
fn for_range_tree_shape() {
    let source = "for i = 1 to 10 do f\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "for range loop should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    EXPR_DECL@0..20
      FOR_EXPR@0..20
        FOR_TOK@0..3 \"for\"
        WHITESPACE@3..4 \" \"
        IDENT_TOK@4..5 \"i\"
        WHITESPACE@5..6 \" \"
        EQUALS_TOK@6..7 \"=\"
        CONST_EXPR@7..9
          WHITESPACE@7..8 \" \"
          INT32_LIT@8..9 \"1\"
        WHITESPACE@9..10 \" \"
        TO_TOK@10..12 \"to\"
        CONST_EXPR@12..15
          WHITESPACE@12..13 \" \"
          INT32_LIT@13..15 \"10\"
        WHITESPACE@15..16 \" \"
        DO_TOK@16..18 \"do\"
        WHITESPACE@18..19 \" \"
        ERROR@19..19 \"\"
        IDENT_EXPR@19..20
          IDENT_TOK@19..20 \"f\"
        ERROR@20..20 \"\"
        ERROR@20..20 \"\"
    NEWLINE@20..21 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// `for i = a downto b do g i`: exercise the [`ForExpr`](crate::syntax::ForExpr)
/// facade accessors. `ident()` is the loop variable `i`, `is_ascending()` is
/// `false` (a `downto` loop), `from_expr()`/`to_expr()` the two bounds (idents
/// `a`/`b`), and `body()` the trailing application `g i`.
#[test]
fn for_range_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "for i = a downto b do g i\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::For(f) = expr_decl.expr().expect("expr") else {
        panic!("expected ForExpr, got {:?}", expr_decl.expr());
    };
    assert_eq!(
        f.ident().map(|t| t.text().to_string()),
        Some("i".to_string()),
        "loop variable should be `i`",
    );
    assert!(!f.is_ascending(), "`downto` loop should not be ascending");
    assert!(
        matches!(f.from_expr(), Some(Expr::Ident(_))),
        "from_expr should be ident `a`, got {:?}",
        f.from_expr(),
    );
    assert!(
        matches!(f.to_expr(), Some(Expr::Ident(_))),
        "to_expr should be ident `b`, got {:?}",
        f.to_expr(),
    );
    assert!(
        matches!(f.body(), Some(Expr::App(_))),
        "body should be the application `g i`, got {:?}",
        f.body(),
    );
}

/// Phase 5.M.4 — `function A -> 1`: the minimal `function` (MatchLambda)
/// form. Pins the full green shape:
/// `MATCH_LAMBDA_EXPR > [FUNCTION_TOK, MATCH_CLAUSE > [<pat>,
/// RARROW_TOK, <result>, ε]]` with no scrutinee and no `with`. The two
/// zero-width `ERROR` leaves are the drained `Virtual::RightBlockEnd`
/// (clause SeqBlock close) and `Virtual::End` (`CtxtMatchClauses`
/// close) — the same trailing pair `match` produces.
#[test]
fn function_single_clause_tree_shape() {
    let source = "function A -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "single-clause function should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..16
  MODULE_OR_NAMESPACE@0..16
    EXPR_DECL@0..15
      MATCH_LAMBDA_EXPR@0..15
        FUNCTION_TOK@0..8 \"function\"
        MATCH_CLAUSE@8..15
          LONG_IDENT_PAT@8..10
            LONG_IDENT@8..10
              WHITESPACE@8..9 \" \"
              IDENT_TOK@9..10 \"A\"
          WHITESPACE@10..11 \" \"
          RARROW_TOK@11..13 \"->\"
          CONST_EXPR@13..15
            WHITESPACE@13..14 \" \"
            INT32_LIT@14..15 \"1\"
          ERROR@15..15 \"\"
        ERROR@15..15 \"\"
    NEWLINE@15..16 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 5.M.4 — `function A when y -> 1 | B -> 2`: a guard on the first
/// clause of a MatchLambda. Exercises the facade: `MatchLambdaExpr` has
/// no scrutinee, its `clauses()` reuse `MatchClause`, and the guarded
/// clause's `guard()`/`result()` disambiguate just as they do under
/// `match`.
#[test]
fn function_when_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "function A when y -> 1 | B -> 2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::MatchLambda(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchLambdaExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 2);

    // Guarded clause: `A when y -> 1`.
    assert!(
        matches!(clauses[0].guard(), Some(Expr::Ident(_))),
        "clause 0 guard should be ident `y`, got {:?}",
        clauses[0].guard(),
    );
    assert!(
        matches!(clauses[0].result(), Some(Expr::Const(_))),
        "clause 0 result should be `1` (not the guard), got {:?}",
        clauses[0].result(),
    );

    // Unguarded clause: `B -> 2`.
    assert!(
        clauses[1].guard().is_none(),
        "clause 1 has no guard, got {:?}",
        clauses[1].guard(),
    );
    assert!(
        matches!(clauses[1].result(), Some(Expr::Const(_))),
        "clause 1 result should be `2`, got {:?}",
        clauses[1].result(),
    );
}

/// Phase 5.M.5 — an offside multi-statement clause body must surface as a
/// `SEQUENTIAL_EXPR` in the clause's `result()` slot, with the statements
/// recoverable in source order. Wrapping the body must not perturb the
/// `guard()`/`result()` positional disambiguation (no guard here, so the
/// sole `Expr` child is the result).
#[test]
fn match_seq_body_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "match x with\n| A ->\n    e1\n    e2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Match(m) = expr_decl.expr().expect("expr") else {
        panic!("expected MatchExpr, got {:?}", expr_decl.expr());
    };
    let clauses: Vec<_> = m.clauses().collect();
    assert_eq!(clauses.len(), 1);

    let Some(Expr::Sequential(seq)) = clauses[0].result() else {
        panic!(
            "clause result should be a SEQUENTIAL_EXPR, got {:?}",
            clauses[0].result(),
        );
    };
    let stmts: Vec<_> = seq.statements().collect();
    assert_eq!(
        stmts.len(),
        2,
        "sequential body should have two statements, got {stmts:?}",
    );
    assert!(
        stmts.iter().all(|e| matches!(e, Expr::Ident(_))),
        "both statements should be idents, got {stmts:?}",
    );
    assert!(
        clauses[0].guard().is_none(),
        "clause has no guard, got {:?}",
        clauses[0].guard(),
    );
}

/// Phase 5.M.5 — full green-tree shape pin for an offside sequential
/// clause body. The two statements and the `Virtual::BlockSep`-derived
/// zero-width ERROR between them are wrapped in a single
/// `SEQUENTIAL_EXPR` under the `MATCH_CLAUSE`, followed by the clause's
/// own zero-width `RightBlockEnd` ERROR and the clause-list `End` ERROR.
#[test]
fn match_seq_body_tree_shape() {
    let source = "match x with\n| A ->\n    e1\n    e2\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..34
  MODULE_OR_NAMESPACE@0..34
    EXPR_DECL@0..33
      MATCH_EXPR@0..33
        MATCH_TOK@0..5 \"match\"
        IDENT_EXPR@5..7
          WHITESPACE@5..6 \" \"
          IDENT_TOK@6..7 \"x\"
        WHITESPACE@7..8 \" \"
        WITH_TOK@8..12 \"with\"
        MATCH_CLAUSE@12..33
          NEWLINE@12..13 \"\\n\"
          BAR_TOK@13..14 \"|\"
          LONG_IDENT_PAT@14..16
            LONG_IDENT@14..16
              WHITESPACE@14..15 \" \"
              IDENT_TOK@15..16 \"A\"
          WHITESPACE@16..17 \" \"
          RARROW_TOK@17..19 \"->\"
          SEQUENTIAL_EXPR@19..33
            IDENT_EXPR@19..26
              NEWLINE@19..20 \"\\n\"
              WHITESPACE@20..24 \"    \"
              IDENT_TOK@24..26 \"e1\"
            NEWLINE@26..27 \"\\n\"
            WHITESPACE@27..31 \"    \"
            ERROR@31..31 \"\"
            IDENT_EXPR@31..33
              IDENT_TOK@31..33 \"e2\"
          ERROR@33..33 \"\"
        ERROR@33..33 \"\"
    NEWLINE@33..34 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.20a — `try x with _ -> 0`: the single-line `try`/`with` exception
/// handler. Pins the green-tree shape `TRY_EXPR > [TRY_TOK, <body>,
/// ERROR(RightBlockEnd), WITH_TOK, MATCH_CLAUSE, ERROR(End)]` — the body is the
/// leading `Expr` child, the one-sided SeqBlock close and the trailing
/// `CtxtMatchClauses` close are zero-width ERROR leaves, and the `with`-clause
/// list reuses `MATCH_CLAUSE` verbatim (same shape as `match … with`).
#[test]
fn try_with_single_clause_tree_shape() {
    let source = "try x with _ -> 0\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "single-clause try/with should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    EXPR_DECL@0..17
      TRY_EXPR@0..17
        TRY_TOK@0..3 \"try\"
        IDENT_EXPR@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        ERROR@5..5 \"\"
        WHITESPACE@5..6 \" \"
        WITH_TOK@6..10 \"with\"
        MATCH_CLAUSE@10..17
          WILDCARD_PAT@10..12
            WHITESPACE@10..11 \" \"
            UNDERSCORE_TOK@11..12 \"_\"
          WHITESPACE@12..13 \" \"
          RARROW_TOK@13..15 \"->\"
          CONST_EXPR@15..17
            WHITESPACE@15..16 \" \"
            INT32_LIT@16..17 \"0\"
          ERROR@17..17 \"\"
        ERROR@17..17 \"\"
    NEWLINE@17..18 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.20a — `try`/`with` via the facade accessors. Confirms `try_expr()`
/// returns the protected body (the sole non-clause `Expr` child) and
/// `with_clauses()` yields the handler arms, with the body and clause results
/// correctly disambiguated.
#[test]
fn try_with_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl, Pat};
    let source = "try f x with | Failure m -> 0 | _ -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Try(t) = expr_decl.expr().expect("expr") else {
        panic!("expected TryExpr, got {:?}", expr_decl.expr());
    };
    assert!(
        matches!(t.try_expr(), Some(Expr::App(_))),
        "body should be the application `f x`, got {:?}",
        t.try_expr(),
    );
    let clauses: Vec<_> = t.with_clauses().collect();
    assert_eq!(clauses.len(), 2, "two handler clauses");
    assert!(
        matches!(clauses[0].pat(), Some(Pat::LongIdent(_))),
        "first clause pat should be the ctor-app `Failure m`, got {:?}",
        clauses[0].pat(),
    );
    assert!(
        matches!(clauses[1].pat(), Some(Pat::Wildcard(_))),
        "second clause pat should be the wildcard `_`, got {:?}",
        clauses[1].pat(),
    );
}

/// Phase 10.20a — multi-line offside `try`/`with` (body and `with` on separate
/// lines, aligned). Exercises the one-sided SeqBlock body draining its own
/// `RightBlockEnd` before the offside `with`. Checked via accessors so the
/// indentation-specific layout trivia isn't pinned.
#[test]
fn try_with_offside_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "let f x =\n    try g x\n    with _ -> 0\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Let(let_decl) = module.decls().next().expect("decl") else {
        panic!("expected Let decl");
    };
    let binding = let_decl.bindings().next().expect("binding");
    let Some(Expr::Try(t)) = binding.expr() else {
        panic!("expected try-expr RHS, got {:?}", binding.expr());
    };
    assert!(
        matches!(t.try_expr(), Some(Expr::App(_))),
        "body should be `g x`, got {:?}",
        t.try_expr(),
    );
    assert_eq!(t.with_clauses().count(), 1, "one handler clause");
}

/// Phase 10.20a — a `try` is *not* an atomic-arg starter, so `- try …` is
/// rejected at the grammar level by FCS. We recover (no panic) and surface the
/// same `maybe_warn_keyword_after_prefix` diagnostic the other control keywords
/// get, mirroring `- match …` / `- if …`.
#[test]
fn minus_prefix_over_try_records_diagnostic() {
    let source = "let y = - try x with _ -> 0\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.iter().any(|e| e
            .message
            .contains("`try` cannot appear directly after a prefix operator")),
        "expected the prefix-keyword diagnostic, got: {:?}",
        parse.errors,
    );
}

/// Phase 10.20b — `try x finally ()`: the single-line `try`/`finally`
/// expression. Pins the green-tree shape `TRY_EXPR > [TRY_TOK, <body>,
/// ERROR(RightBlockEnd), FINALLY_TOK, ERROR(BlockBegin), <finally-body>, …]` —
/// the try body is the leading `Expr` child, `finally` a raw passthrough, and
/// the finally body a regular block (here the unit literal `()`).
#[test]
fn try_finally_single_line_tree_shape() {
    let source = "try x finally ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "single-line try/finally should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..17
  MODULE_OR_NAMESPACE@0..17
    EXPR_DECL@0..16
      TRY_EXPR@0..16
        TRY_TOK@0..3 \"try\"
        IDENT_EXPR@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        ERROR@5..5 \"\"
        WHITESPACE@5..6 \" \"
        FINALLY_TOK@6..13 \"finally\"
        WHITESPACE@13..14 \" \"
        ERROR@14..14 \"\"
        CONST_EXPR@14..16
          LPAREN_TOK@14..15 \"(\"
          RPAREN_TOK@15..16 \")\"
        ERROR@16..16 \"\"
    NEWLINE@16..17 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.20b — `try`/`finally` via the facade accessors: `try_expr()` is the
/// protected body, `finally_expr()` the cleanup, `is_try_finally()` is `true`,
/// and `with_clauses()` is empty.
#[test]
fn try_finally_via_accessors() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "let f x =\n    try g x\n    finally cleanup ()\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Let(let_decl) = module.decls().next().expect("decl") else {
        panic!("expected Let decl");
    };
    let binding = let_decl.bindings().next().expect("binding");
    let Some(Expr::Try(t)) = binding.expr() else {
        panic!("expected try-expr RHS, got {:?}", binding.expr());
    };
    assert!(t.is_try_finally(), "should be the try/finally form");
    assert!(
        matches!(t.try_expr(), Some(Expr::App(_))),
        "body should be `g x`, got {:?}",
        t.try_expr(),
    );
    assert!(
        matches!(t.finally_expr(), Some(Expr::App(_))),
        "finally body should be `cleanup ()`, got {:?}",
        t.finally_expr(),
    );
    assert_eq!(
        t.with_clauses().count(),
        0,
        "no handler clauses on try/finally"
    );
}

/// Phase 10.20b — the `try … with …` form reports `is_try_finally() == false`
/// and a `None` `finally_expr()`, confirming the discriminant cleanly separates
/// the two forms sharing the `TRY_EXPR` node.
#[test]
fn try_with_is_not_try_finally() {
    use crate::syntax::{AstNode, Expr, ModuleDecl};
    let source = "try x with _ -> 0\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(expr_decl) = module.decls().next().expect("decl") else {
        panic!("expected Expr decl");
    };
    let Expr::Try(t) = expr_decl.expr().expect("expr") else {
        panic!("expected TryExpr");
    };
    assert!(!t.is_try_finally(), "try/with is not try/finally");
    assert!(t.finally_expr().is_none(), "try/with has no finally body");
    assert_eq!(t.with_clauses().count(), 1, "one handler clause");
}
