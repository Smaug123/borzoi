use super::super::*;
use super::*;

/// `( \` )` has a lex-error token between the parens. The unit
/// lookahead must treat the `Err` as a non-trivia stopper — if it
/// skipped errors and found the trailing `RParen`, `parse_const_expr`
/// would commit to unit and `bump_swallowed_rparen` would
/// `unreachable!` on the error token it didn't expect to see.
#[test]
fn paren_with_lex_error_inside_does_not_panic() {
    // The bare backtick `` ` `` is a `LexError` for our lexer — no
    // regex matches it. Wrap it between parens; we must NOT commit
    // to unit on the trailing `)`.
    let source = "( ` )\n";
    let parse = parse(source);
    // Either an error gets reported, or we don't commit to unit.
    // The crucial thing is the parser doesn't panic — the assert
    // above on `assert_lossless` would too.
    assert_lossless(source, &parse);
}

/// `( 1 )` has a non-trivia interior — this is a paren *expression*
/// (`SynExpr.Paren`), not a unit literal. The dispatch peers past
/// the `LParen` into the raw stream, finds a `1`, and routes to
/// `parse_paren_expr` instead of `parse_const_expr`'s unit arm.
/// Shape: `PAREN_EXPR > [LPAREN_TOK, <inner CONST_EXPR>, RPAREN_TOK]`
/// with the surrounding spaces as trivia children of `PAREN_EXPR`.
#[test]
fn paren_expression_with_int_inside() {
    let source = "( 1 )\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      PAREN_EXPR@0..5
        LPAREN_TOK@0..1 \"(\"
        WHITESPACE@1..2 \" \"
        CONST_EXPR@2..3
          INT32_LIT@2..3 \"1\"
        WHITESPACE@3..4 \" \"
        RPAREN_TOK@4..5 \")\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Nested `(( 1 ))` — the recursive `parse_expr` call inside the
/// outer `parse_paren_expr` re-runs `peek_is_expr_start`, which peers
/// past the inner `LParen` and finds another expression-starter.
/// Two `PAREN_EXPR` nodes nest under each other.
#[test]
fn nested_paren_expression() {
    let source = "((1))\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      PAREN_EXPR@0..5
        LPAREN_TOK@0..1 \"(\"
        PAREN_EXPR@1..4
          LPAREN_TOK@1..2 \"(\"
          CONST_EXPR@2..3
            INT32_LIT@2..3 \"1\"
          RPAREN_TOK@3..4 \")\"
        RPAREN_TOK@4..5 \")\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1, 2` — top-level two-tuple `SynExpr.Tuple(isStruct=false,
/// exprs=[1; 2], …)`. The first `CONST_EXPR` is wrapped under
/// `TUPLE_EXPR` via `start_node_at`/`Checkpoint` after the comma
/// is observed; the trailing space before `2` lands inside the
/// second `CONST_EXPR` (consistent with the asymmetric trivia
/// placement we already accepted for paren-expressions).
#[test]
fn lone_two_tuple_at_top_level() {
    let source = "1, 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      TUPLE_EXPR@0..4
        CONST_EXPR@0..1
          INT32_LIT@0..1 \"1\"
        COMMA_TOK@1..2 \",\"
        CONST_EXPR@2..4
          WHITESPACE@2..3 \" \"
          INT32_LIT@3..4 \"2\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1, 2, 3` — three-element tuple. Confirms the `while` loop
/// in `parse_expr` keeps accumulating elements until commas stop.
#[test]
fn lone_three_tuple_at_top_level() {
    let source = "1, 2, 3\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      TUPLE_EXPR@0..7
        CONST_EXPR@0..1
          INT32_LIT@0..1 \"1\"
        COMMA_TOK@1..2 \",\"
        CONST_EXPR@2..4
          WHITESPACE@2..3 \" \"
          INT32_LIT@3..4 \"2\"
        COMMA_TOK@4..5 \",\"
        CONST_EXPR@5..7
          WHITESPACE@5..6 \" \"
          INT32_LIT@6..7 \"3\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `(1, 2)` — tuple inside parens. FCS shape:
/// `SynExpr.Paren(SynExpr.Tuple([1; 2], …), …)`. Our tree nests
/// `TUPLE_EXPR` directly under `PAREN_EXPR`.
#[test]
fn tuple_inside_paren() {
    let source = "(1, 2)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..7
  MODULE_OR_NAMESPACE@0..7
    EXPR_DECL@0..6
      PAREN_EXPR@0..6
        LPAREN_TOK@0..1 \"(\"
        TUPLE_EXPR@1..5
          CONST_EXPR@1..2
            INT32_LIT@1..2 \"1\"
          COMMA_TOK@2..3 \",\"
          CONST_EXPR@3..5
            WHITESPACE@3..4 \" \"
            INT32_LIT@4..5 \"2\"
        RPAREN_TOK@5..6 \")\"
    NEWLINE@6..7 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `x, y` — tuple of idents. Confirms tuple wrapping works with
/// `IDENT_EXPR` atoms too, not just `CONST_EXPR`.
#[test]
fn tuple_of_idents() {
    let source = "x, y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      TUPLE_EXPR@0..4
        IDENT_EXPR@0..1
          IDENT_TOK@0..1 \"x\"
        COMMA_TOK@1..2 \",\"
        IDENT_EXPR@2..4
          WHITESPACE@2..3 \" \"
          IDENT_TOK@3..4 \"y\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `1,` — trailing comma with nothing after it. `parse_expr` enters
/// the tuple branch on seeing the comma, emits `COMMA_TOK`, then
/// finds no expression-starter for the next element. Records an
/// error and breaks rather than panicking or consuming nothing in
/// an infinite loop.
#[test]
fn tuple_trailing_comma_recovers() {
    let source = "1,\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected an error for trailing-comma tuple",
    );
    assert_lossless(source, &parse);
}

/// `(1), 2` — the parenthesised first element of an outer tuple.
/// Inside `parse_paren_expr` the recursive `parse_expr` peeks the
/// filtered stream and sees the outer Comma directly (the closing
/// `)` is swallowed by LexFilter and only lives in the raw stream).
/// Without an extra raw-stream check, the inner tuple loop would
/// consume that outer comma, drain `)` as ERROR, and produce the
/// wrong shape. Regression for codex review of phase 3.2.
#[test]
fn paren_then_comma_is_outer_tuple() {
    let source = "(1), 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::Tuple(tuple) = decl.expr().expect("expr") else {
        panic!("expected outer tuple, got {:?}", decl.expr());
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 2);
    assert!(
        matches!(&elements[0], crate::syntax::Expr::Paren(_)),
        "first element should be paren-wrapped, got {:?}",
        elements[0]
    );
    assert!(
        matches!(&elements[1], crate::syntax::Expr::Const(_)),
        "second element should be const, got {:?}",
        elements[1]
    );
}

/// `(1, (2), 3)` — middle element is a parenthesised expression. The
/// outer paren's inner `parse_expr` builds a 3-tuple; the middle
/// element's nested `parse_expr` must not eat the outer comma after
/// its own swallowed `)`. Same regression class as
/// [`paren_then_comma_is_outer_tuple`].
#[test]
fn nested_paren_in_middle_of_tuple() {
    let source = "(1, (2), 3)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::Paren(paren) = decl.expr().expect("expr") else {
        panic!("expected outer paren");
    };
    let crate::syntax::Expr::Tuple(tuple) = paren.inner().expect("inner") else {
        panic!("expected tuple inside paren");
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 3);
    assert!(matches!(&elements[1], crate::syntax::Expr::Paren(_)));
}

/// Multi-line tuple inside parens. F# accepts the layout
/// `( 1, \n  2 )` because LexFilter's paren context suppresses
/// offside-driven `BlockSep` insertion across the comma. But if a
/// `Virtual::BlockSep` were to appear after the comma, the tuple loop
/// must skip it before deciding the element is missing. Regression
/// for codex review of phase 3.2.
#[test]
fn multiline_tuple_in_paren() {
    let source = "(\n    1,\n    2\n)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    // Top-level shape: EXPR_DECL > PAREN_EXPR > TUPLE_EXPR > [Int, Int].
    // We don't pin trivia placement byte-by-byte (the layout has many
    // newlines/indents); the point is to *not* lose the second element
    // out of the tuple.
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::Paren(paren) = decl.expr().expect("expr") else {
        panic!("expected paren expr at top level");
    };
    let crate::syntax::Expr::Tuple(tuple) = paren.inner().expect("paren inner") else {
        panic!("expected tuple inside paren");
    };
    assert_eq!(tuple.elements().count(), 2);
}

/// `f x` — bare function application. FCS shape:
/// `SynExpr.App(NonAtomic, false, Ident "f", Ident "x", …)`. Our tree
/// nests both idents directly under `APP_EXPR`; the whitespace between
/// them sticks to the argument expression (consistent with how
/// `parse_atomic_expr`'s `IDENT_EXPR` drains its own leading trivia).
#[test]
fn app_two_idents() {
    let source = "f x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      APP_EXPR@0..3
        IDENT_EXPR@0..1
          IDENT_TOK@0..1 \"f\"
        IDENT_EXPR@1..3
          WHITESPACE@1..2 \" \"
          IDENT_TOK@2..3 \"x\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `f x y` — three-segment application. F# applications are
/// left-associative: `f x y` is `(f x) y`, i.e.
/// `App(App(f, x), y)`. The same `Checkpoint` reused each iteration
/// of `parse_app_expr` produces the nested shape.
#[test]
fn app_three_idents_left_assoc() {
    let source = "f x y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      APP_EXPR@0..5
        APP_EXPR@0..3
          IDENT_EXPR@0..1
            IDENT_TOK@0..1 \"f\"
          IDENT_EXPR@1..3
            WHITESPACE@1..2 \" \"
            IDENT_TOK@2..3 \"x\"
        IDENT_EXPR@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"y\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `f x, g y` — application binds tighter than the comma. The result
/// is `Tuple(App(f, x), App(g, y))`, not `App(f, Tuple(x, g, y))`
/// or anything similarly wrong. This is the crucial precedence
/// interaction between `parse_expr`'s tuple loop and
/// `parse_app_expr`'s greedy atom-eating.
#[test]
fn app_and_tuple_precedence() {
    let source = "f x, g y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::Tuple(tuple) = decl.expr().expect("expr") else {
        panic!("expected outer tuple, got {:?}", decl.expr());
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 2);
    for el in &elements {
        assert!(
            matches!(el, crate::syntax::Expr::App(_)),
            "tuple element should be App, got {el:?}",
        );
    }
    assert_lossless(source, &parse);
}

/// `f (g x)` — application whose argument is a paren-wrapped
/// sub-application. The outer `parse_app_expr` parses `f` as the
/// head, then `( g x )` as one atom (`parse_atomic_expr` dispatches
/// LParen to `parse_paren_expr` whose inner `parse_expr` recurses
/// into application). Result: `App(f, Paren(App(g, x)))`.
#[test]
fn app_with_paren_arg() {
    let source = "f (g x)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::App(app) = decl.expr().expect("expr") else {
        panic!("expected outer App, got {:?}", decl.expr());
    };
    assert!(matches!(app.func(), Some(crate::syntax::Expr::Ident(_))));
    let crate::syntax::Expr::Paren(paren) = app.arg().expect("arg") else {
        panic!("expected paren arg, got {:?}", app.arg());
    };
    assert!(matches!(paren.inner(), Some(crate::syntax::Expr::App(_))));
    assert_lossless(source, &parse);
}

/// `(f) x` — paren-wrapped first element of an outer application.
/// LexFilter swallows `)`, so the inner `parse_app_expr` peeks the
/// filtered stream and sees the *outer* `x` directly. Without an
/// extra raw-stream gate, the inner app loop would consume `x` as
/// part of the parenthesised expression and `bump_swallowed_rparen`
/// would then fail to find its `)`. Regression for codex review of
/// phase 3.3.
#[test]
fn paren_then_ident_is_outer_app() {
    let source = "(f) x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::App(app) = decl.expr().expect("expr") else {
        panic!("expected outer App, got {:?}", decl.expr());
    };
    assert!(
        matches!(app.func(), Some(crate::syntax::Expr::Paren(_))),
        "func should be Paren, got {:?}",
        app.func(),
    );
    assert!(
        matches!(app.arg(), Some(crate::syntax::Expr::Ident(_))),
        "arg should be Ident, got {:?}",
        app.arg(),
    );
    assert_lossless(source, &parse);
}

/// `f (g) x` — middle argument is a parenthesised ident. The outer
/// app is left-associative: `App(App(f, Paren(g)), x)`. The inner
/// `parse_app_expr` (running inside `parse_paren_expr`) must not
/// suck the outer `x` into the paren. Same regression class as
/// [`paren_then_ident_is_outer_app`].
#[test]
fn paren_in_middle_of_app_does_not_swallow_outer_arg() {
    let source = "f (g) x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App, got {:?}", decl.expr());
    };
    let crate::syntax::Expr::App(inner) = outer.func().expect("outer.func") else {
        panic!(
            "expected nested App in func position, got {:?}",
            outer.func()
        );
    };
    assert!(
        matches!(inner.func(), Some(crate::syntax::Expr::Ident(_))),
        "innermost func should be Ident `f`, got {:?}",
        inner.func(),
    );
    let crate::syntax::Expr::Paren(paren) = inner.arg().expect("inner.arg") else {
        panic!("middle arg should be Paren, got {:?}", inner.arg());
    };
    assert!(matches!(paren.inner(), Some(crate::syntax::Expr::Ident(_))));
    assert!(
        matches!(outer.arg(), Some(crate::syntax::Expr::Ident(_))),
        "outer arg should be Ident `x`, got {:?}",
        outer.arg(),
    );
    assert_lossless(source, &parse);
}

/// `(x)` — paren around an ident. Confirms paren-expr also works
/// over `IDENT_EXPR` interiors, not just `CONST_EXPR`.
#[test]
fn paren_expression_around_ident() {
    let source = "(x)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      PAREN_EXPR@0..3
        LPAREN_TOK@0..1 \"(\"
        IDENT_EXPR@1..2
          IDENT_TOK@1..2 \"x\"
        RPAREN_TOK@2..3 \")\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `(1+2)` — paren interior is `1 + 2`, a complete infix expression.
/// Confirms the Pratt climber's RHS recursion runs cleanly inside a
/// paren whose inner `parse_expr` doesn't see the LexFilter-swallowed
/// `RParen`. Pins the shape against the FCS-faithful `mkSynInfix`
/// nesting: outer `PAREN_EXPR` wrapping the same `APP_EXPR >
/// INFIX_APP_EXPR` shape as a bare `1 + 2` decl.
#[test]
fn paren_around_infix_expression() {
    let source = "(1+2)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      PAREN_EXPR@0..5
        LPAREN_TOK@0..1 \"(\"
        APP_EXPR@1..4
          INFIX_APP_EXPR@1..3
            CONST_EXPR@1..2
              INT32_LIT@1..2 \"1\"
            LONG_IDENT_EXPR@2..3
              LONG_IDENT@2..3
                IDENT_TOK@2..3 \"+\"
          CONST_EXPR@3..4
            INT32_LIT@3..4 \"2\"
        RPAREN_TOK@4..5 \")\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `(1` — the inner `1` parses but EOF arrives before the closing
/// `)`. `bump_swallowed_rparen` must hit the EOF arm and record an
/// error rather than panicking.
#[test]
fn paren_expression_unterminated_recovers() {
    let source = "(1";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected an unterminated-paren error",
    );
    assert_lossless(source, &parse);
}

/// `((` — outer LParen passes `peek_is_expr_start` (inner `(` is in
/// `raw_starts_atomic_expr`), but the *inner* position has no valid
/// expression starter. The inner `peek_is_expr_start` check inside
/// `parse_paren_expr` must fail and record an error rather than
/// recursing into `parse_expr` (which would hit `unreachable!`).
#[test]
fn nested_open_paren_no_inner_recovers() {
    let source = "((";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected an error for nested open paren with no inner expr",
    );
    assert_lossless(source, &parse);
}

/// `((+))` — the inner `(+)` is a parenthesised operator-value
/// (FCS's `opName`), so the whole thing is `Paren(LongIdent(["+"]))` —
/// error-free, *not* a recovery case. (Before operator-values were
/// parsed, `parse_paren_expr` saw the bare `+` as a missing-operand
/// prefix and recovered with an error; the operator-value
/// reinterpretation now fires first.)
#[test]
fn nested_paren_around_operator_value() {
    let source = "((+))\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      PAREN_EXPR@0..5
        LPAREN_TOK@0..1 \"(\"
        LONG_IDENT_EXPR@1..4
          LONG_IDENT@1..4
            LPAREN_TOK@1..2 \"(\"
            IDENT_TOK@2..3 \"+\"
            RPAREN_TOK@3..4 \")\"
        RPAREN_TOK@4..5 \")\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `(f x)` — paren around an application. `parse_app_expr` consumes
/// both `f` and `x`, wrapping them under `APP_EXPR`; the result then
/// sits inside `PAREN_EXPR`. Confirms application is the inside-paren
/// production and that the closing `)` is still found correctly.
#[test]
fn paren_expression_around_app() {
    let source = "(f x)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      PAREN_EXPR@0..5
        LPAREN_TOK@0..1 \"(\"
        APP_EXPR@1..4
          IDENT_EXPR@1..2
            IDENT_TOK@1..2 \"f\"
          IDENT_EXPR@2..4
            WHITESPACE@2..3 \" \"
            IDENT_TOK@3..4 \"x\"
        RPAREN_TOK@4..5 \")\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A bare identifier `x` is `SynExpr.Ident` — FCS's optimised
/// representation for a one-segment `SynLongIdent` (`SyntaxTree.fsi:805`).
/// Shape: `EXPR_DECL > IDENT_EXPR > IDENT_TOK`.
#[test]
fn lone_ident_expression() {
    let source = "x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..2
  MODULE_OR_NAMESPACE@0..2
    EXPR_DECL@0..1
      IDENT_EXPR@0..1
        IDENT_TOK@0..1 \"x\"
    NEWLINE@1..2 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A backticked identifier `` ``foo bar`` `` is also `SynExpr.Ident`. The
/// green tree preserves the backticks (lossless); the FCS-side `idText`
/// strips them, so the differential normaliser must match. Pins that the
/// `QuotedIdent` lexer token funnels into `IDENT_TOK` just like a plain
/// `Ident`.
#[test]
fn lone_backticked_ident_expression() {
    let source = "``foo bar``\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..12
  MODULE_OR_NAMESPACE@0..12
    EXPR_DECL@0..11
      IDENT_EXPR@0..11
        IDENT_TOK@0..11 \"``foo bar``\"
    NEWLINE@11..12 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A second literal on a *later* line that's *indented past* the first
/// is a continuation of the first expression in F#'s offside layout —
/// LexFilter does not emit a `Virtual::BlockSep`, so `parse_app_expr`
/// keeps consuming. The result is the same `App(42, 43)` as the
/// same-line form, just with the layout trivia preserved.
#[test]
fn indented_continuation_is_app() {
    let source = "42\n  43\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let mut decls = module.decls();
    let decl = decls.next().expect("one decl");
    assert!(decls.next().is_none(), "expected exactly one decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::App(app) = decl.expr().expect("expr") else {
        panic!("expected App, got {:?}", decl.expr());
    };
    assert!(matches!(app.func(), Some(crate::syntax::Expr::Const(_))));
    assert!(matches!(app.arg(), Some(crate::syntax::Expr::Const(_))));
    assert_lossless(source, &parse);
}

/// `a + b` — the minimal infix. Confirms the two-tier shape FCS's
/// `mkSynInfix` builds: an outer `APP_EXPR` whose first child is
/// the inner `INFIX_APP_EXPR (lhs, op-as-long-ident)` and whose
/// second child is the RHS. The operator token rides under
/// `LONG_IDENT_EXPR > LONG_IDENT > IDENT_TOK`, carrying the source
/// text "+" directly (FCS additionally mangles to `op_Addition` +
/// `OriginalNotation "+"` trivia; the FCS-side normaliser unwraps
/// the trivia so the diff lines up).
#[test]
fn infix_plus_two_idents() {
    let source = "a + b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..5
      APP_EXPR@0..5
        INFIX_APP_EXPR@0..3
          IDENT_EXPR@0..1
            IDENT_TOK@0..1 \"a\"
          LONG_IDENT_EXPR@1..3
            LONG_IDENT@1..3
              WHITESPACE@1..2 \" \"
              IDENT_TOK@2..3 \"+\"
        IDENT_EXPR@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"b\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `a + b * c` — `*` (lbp=70) binds tighter than `+` (lbp=60), so
/// the RHS of `+` is itself an `App(*, b, c)` subtree rather than
/// just `b`. The Pratt climber's `parse_pratt_expr(rbp)` recursive
/// call with `rbp = 61` swallows higher-precedence operators on the
/// right.
#[test]
fn infix_precedence_plus_times() {
    let source = "a + b * c\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AppExpr, AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App");
    };
    assert!(!outer.is_infix(), "outer is the plain-form App");
    let Expr::App(rhs) = outer.arg().expect("arg") else {
        panic!("expected RHS to be App(*, b, c), got {:?}", outer.arg());
    };
    assert!(!rhs.is_infix(), "RHS is the outer plain-form App");
    let Expr::App(rhs_inner) = rhs.func().expect("func") else {
        panic!("expected RHS func to be INFIX_APP_EXPR");
    };
    assert!(rhs_inner.is_infix(), "RHS func is the inner infix App");
    let Expr::LongIdent(op) = rhs_inner.func().expect("op") else {
        panic!("expected RHS infix op to be LongIdent");
    };
    let op_text: String = op
        .long_ident()
        .expect("long_ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(op_text, "*");
    // And the top-level op is `+`.
    let Expr::App(outer_inner) = outer.func().expect("func") else {
        panic!("expected outer func to be INFIX_APP_EXPR");
    };
    assert!(outer_inner.is_infix());
    let Expr::LongIdent(top_op) = outer_inner.func().expect("op") else {
        panic!("expected top op LongIdent");
    };
    let top_text: String = top_op
        .long_ident()
        .expect("long_ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(top_text, "+");
    // Sanity: cast the AppExpr type to confirm AppExpr accepts both kinds.
    let _: AppExpr = outer.clone();
    assert_lossless(source, &parse);
}

/// `a + b + c` — `+` is left-associative (`rbp = lbp - 1 = 59`), so
/// the inner recursive `parse_pratt_expr(60)` stops at the second `+`
/// and the outer iteration wraps `App(+, a, b)` as the LHS of the
/// next infix. Result: `App(+, App(+, a, b), c)`.
#[test]
fn infix_plus_left_associative() {
    let source = "a + b + c\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App");
    };
    // outer.arg() must be `c`, not an App.
    let Expr::Ident(c) = outer.arg().expect("arg") else {
        panic!("expected outer arg to be Ident `c`, got {:?}", outer.arg());
    };
    assert_eq!(c.ident().expect("ident").text(), "c");
    // outer.func() = inner INFIX_APP_EXPR; its arg() must be the
    // already-built `App(+, a, b)`.
    let Expr::App(infix) = outer.func().expect("func") else {
        panic!("expected outer func to be infix App");
    };
    assert!(infix.is_infix());
    let Expr::App(lhs) = infix.arg().expect("arg") else {
        panic!(
            "expected infix arg to be App(+, a, b), got {:?}",
            infix.arg()
        );
    };
    assert!(!lhs.is_infix(), "lhs is the plain-form outer App for a + b");
    assert_lossless(source, &parse);
}

/// `a, b + c` — infix `+` (lbp=60) binds tighter than tuple `,`
/// (handled at `parse_expr`'s tuple loop, which calls
/// `parse_pratt_expr(0)` per element). The tuple has two elements
/// `a` and `App(+, b, c)`, not three.
#[test]
fn infix_tighter_than_tuple_comma() {
    let source = "a, b + c\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::Tuple(t) = decl.expr().expect("expr") else {
        panic!("expected Tuple");
    };
    let els: Vec<_> = t.elements().collect();
    assert_eq!(els.len(), 2, "tuple should have 2 elements, got {els:?}");
    assert!(
        matches!(els[1], Expr::App(ref a) if !a.is_infix()),
        "second element should be the outer plain App for `b + c`, got {:?}",
        els[1],
    );
    assert_lossless(source, &parse);
}

/// `f a + b` — application (`expr_app` in pars.fsy) binds tighter
/// than any infix operator. The Pratt climber calls
/// `parse_app_expr` as the atom, so `f a` is one greedy left-assoc
/// App chain that becomes the LHS of `+`. Result:
/// `App(+, App(f, a), b)`.
#[test]
fn app_tighter_than_infix() {
    let source = "f a + b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App");
    };
    let Expr::App(infix) = outer.func().expect("func") else {
        panic!("expected outer func to be infix App");
    };
    assert!(infix.is_infix());
    let Expr::App(fa) = infix.arg().expect("arg") else {
        panic!("expected infix LHS to be App(f, a), got {:?}", infix.arg());
    };
    assert!(!fa.is_infix(), "f a is plain application");
    assert_lossless(source, &parse);
}

/// `(a) + b` — a parenthesised LHS must not pull the post-`)` `+`
/// into the inner `parse_expr`. The `peek_infix_continuation`
/// raw-stream gate (mirror of `at_tuple_continuation` /
/// `at_app_continuation`) checks for a LexFilter-swallowed `RParen`
/// before any next non-trivia raw and bails. Without the gate this
/// would parse as `Paren(App(+, a, b))`; with it the outer
/// `parse_pratt_expr` picks up `+` correctly.
#[test]
fn paren_lhs_does_not_eat_outer_infix() {
    let source = "(a) + b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App, got {:?}", decl.expr());
    };
    // outer.func().arg() must be a Paren wrapping the lone `a` —
    // i.e. the `+` did not get swallowed by parse_paren_expr.
    let Expr::App(infix) = outer.func().expect("func") else {
        panic!("expected outer func to be infix");
    };
    assert!(matches!(infix.arg(), Some(Expr::Paren(_))));
    assert_lossless(source, &parse);
}

/// `!`-prefixed ops (other than `!=`-headed) are `PREFIX_OP` per
/// lex.fsl line 986, not infix. `a !+ b` parses as
/// `App(a, App(!+, b))` — the outer App is a plain (non-infix)
/// application of `a` to the prefix-op subexpression `!+ b`, which is
/// itself a plain App whose func is the `!+` long-ident.
#[test]
fn bang_prefixed_op_does_not_form_infix() {
    let source = "a !+ b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App, got {:?}", decl.expr());
    };
    assert!(!outer.is_infix(), "outer App `a (!+ b)` must not be infix");
    // outer.func() is the bare `a`.
    assert!(
        matches!(outer.func(), Some(Expr::Ident(_))),
        "outer App func should be `a`, got {:?}",
        outer.func(),
    );
    // outer.arg() is the prefix application `!+ b`.
    let Expr::App(prefix) = outer.arg().expect("outer arg") else {
        panic!("outer App arg should be App(!+, b), got {:?}", outer.arg());
    };
    assert!(!prefix.is_infix(), "prefix `!+ b` must not be infix");
    assert_lossless(source, &parse);
}

/// Bare `&` is the address-of prefix (pars.fsy:5162 `AMP minusExpr`)
/// and the conjunction binder in patterns (lines 3650/4000) — it has
/// no `declExpr AMP declExpr` rule, and LexFilter only rewrites `&`
/// to `ADJACENT_PREFIX_OP` when it sits adjacent to the preceding
/// token (no leading whitespace). With spaces around `&` neither
/// path applies, so `a & b` is a syntax error in FCS: a single decl
/// `a` plus error 10 ("Unexpected symbol `&`"). Verified against the
/// FCS oracle directly. Our `parse_impl_file` separator gate (see
/// the `needs_sep` flag) preserves that 1-decl-plus-error shape
/// rather than mistakenly starting a second decl at `&b`. For the
/// adjacent-prefix variant (`f &x`) see
/// [`adjacent_ampersand_in_arg_position`].
#[test]
fn bare_ampersand_with_spaces_is_syntax_error() {
    let source = "a & b\n";
    let parse = parse(source);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "`a & b` must produce one decl (FCS shape), got {}: {decls:#?}",
        decls.len(),
    );
    let ModuleDecl::Expr(decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr")
    };
    assert!(
        matches!(decl.expr(), Some(Expr::Ident(_))),
        "sole decl should be Ident `a`, got {:?}",
        decl.expr(),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected an `unexpected token` error at the dangling `&`",
    );
    assert_lossless(source, &parse);
}

/// `a&b` (no whitespace). LexFilter rewrites `&` to
/// `ADJACENT_PREFIX_OP` only when adjacent-right AND non-adjacent-left
/// (LexFilter.fs:2694) — here the `&` is adjacent on *both* sides, so
/// the rewrite doesn't fire and we're back to a bare `AMP` in a
/// position with no continuation rule. Same end shape as the
/// whitespace-around variant: one decl `a` plus an "unexpected token"
/// error. Pins the separator-gate fix against the no-space sibling.
#[test]
fn adjacent_ampersand_after_ident_is_syntax_error() {
    let source = "a&b\n";
    let parse = parse(source);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "`a&b` must produce one decl (FCS shape), got {}: {decls:#?}",
        decls.len(),
    );
    let ModuleDecl::Expr(decl) = &decls[0] else {
        panic!("expected ModuleDecl::Expr")
    };
    assert!(
        matches!(decl.expr(), Some(Expr::Ident(_))),
        "sole decl should be Ident `a`, got {:?}",
        decl.expr(),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected an `unexpected token` error at the dangling `&`",
    );
    assert_lossless(source, &parse);
}

/// Adjacent `&` (no whitespace between `f` and `&x`) — LexFilter
/// rewrites the `&` to `ADJACENT_PREFIX_OP` and the parser admits
/// it at arg position (pars.fsy:5197 `argExpr: ADJACENT_PREFIX_OP
/// atomicExpr`). Result: `App(f, AddressOf(x))`.
#[test]
fn adjacent_ampersand_in_arg_position() {
    let source = "f &x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App, got {:?}", decl.expr());
    };
    assert!(!outer.is_infix(), "outer App `f &x` must not be infix");
    let Expr::AddressOf(addr) = outer.arg().expect("outer arg") else {
        panic!(
            "outer App arg should be AddressOf(x), got {:?}",
            outer.arg()
        );
    };
    assert!(addr.is_byref(), "single `&` is the byref form");
    assert_lossless(source, &parse);
}

/// `:=` (`COLON_EQUALS`) sits *below* COMMA in pars.fsy's precedence
/// table (line 344 `%right COLON_EQUALS` vs line 346 `%left COMMA`),
/// so `r := a, b` parses as `(r) := (a, b)` — `:=` is parsed *above*
/// the tuple loop in [`Parser::parse_expr`], wrapping the whole tuple
/// on each side. The `mkSynInfix` shape is identical to an ordinary
/// infix op: outer plain `App(App(:=, r), Tuple(a, b))` whose func is
/// the inner infix `App` carrying the `:=` operator. (Earlier phases
/// deferred this by keeping `Token::ColonEquals` out of
/// [`Parser::peek_infix_op`], because the Pratt climber runs *under*
/// the tuple loop and would mis-nest `r := a, b` as `(r := a), b`.)
#[test]
fn colon_equals_forms_infix_above_tuple() {
    let source = "r := a, b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    // Outer App is the plain (`isInfix = false`) `mkSynInfix` wrapper.
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App for `:=`");
    };
    assert!(!outer.is_infix(), "outer is the plain-form App");
    // Its func is the inner infix App whose own func is the `:=` operator.
    let Expr::App(inner) = outer.func().expect("func") else {
        panic!("expected outer func to be the inner infix App");
    };
    assert!(inner.is_infix(), "inner is the infix App carrying `:=`");
    let Expr::LongIdent(op) = inner.func().expect("op") else {
        panic!("expected the `:=` op to be a LongIdent");
    };
    let op_text: String = op
        .long_ident()
        .expect("long_ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(op_text, ":=");
    // The LHS (inner arg) is the bare `r`; the RHS (outer arg) is the tuple.
    assert!(
        matches!(inner.arg(), Some(Expr::Ident(_) | Expr::LongIdent(_))),
        "`:=` LHS is the bare `r`, got {:?}",
        inner.arg()
    );
    assert!(
        matches!(outer.arg(), Some(Expr::Tuple(_))),
        "`:=` RHS is the whole `a, b` tuple, got {:?}",
        outer.arg()
    );
    assert_lossless(source, &parse);
}

/// A `:=` whose RHS is missing (EOF, a closer, or a separator) must *not*
/// recurse into the RHS parse — that path assumes a verified expression start
/// and would reach `parse_const_payload`'s `unreachable!` and panic. The
/// `peek_is_expr_start` guard (mirroring [`Parser::parse_assign_rhs`]'s `<-`
/// arm) records a recovery error instead; the `:=` operator itself stays in the
/// tree. An LSP parser sees half-typed input constantly, so this is the
/// load-bearing invariant: no panic, fully lossless.
#[test]
fn colon_equals_missing_rhs_recovers_without_panicking() {
    for source in [
        "r :=\n",            // EOF after the operator
        "r := )\n",          // a closer follows
        "let f () = r :=\n", // incomplete binding RHS
        "[ r := ]\n",        // a list-element closer follows
        "r := ; b\n",        // a separator follows
        // Missing RHS *inside* parens: the `)` is swallowed from the filtered
        // stream, so without the `!at_swallowed_seq_closer` RHS guard
        // `peek_is_expr_start` would see the token past the closer and recurse
        // across it — `(r := )..3` would reach `parse_const_payload`'s
        // `unreachable!`. The same hazard (and guard) applies to `<-`.
        "(r := )..3\n",
        "(r := ) x\n",
        "(x <- )..3\n",
    ] {
        let parse = parse(source);
        // The contract is recovery, not a clean parse — only that it neither
        // panics nor loses bytes.
        assert_lossless(source, &parse);
    }
}

/// `!=` *is* INFIX_COMPARE_OP per lex.fsl line 978 — the `!=` literal
/// is whitelisted into the compare-op family. Regression guard against
/// over-aggressive `!`-prefix rejection from
/// [`bang_prefixed_op_does_not_form_infix`].
#[test]
fn bang_equals_still_classifies_as_infix() {
    let source = "a != b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App");
    };
    let Expr::App(infix) = outer.func().expect("func") else {
        panic!("expected outer func to be infix App");
    };
    assert!(infix.is_infix());
    assert_lossless(source, &parse);
}

/// `a ?? b` — `QMARK_QMARK` appears in pars.fsy only as a `%token`
/// (line 88) and in `%left QMARK_QMARK` (line 367) for precedence;
/// there is no `declExpr QMARK_QMARK declExpr` production. FCS
/// rejects this input with "Unexpected symbol '??'", so our parser
/// must NOT fabricate an infix App for it. Phase 3.4 had wrongly
/// listed `Token::QMarkQMark` in `peek_infix_op`; this guards the
/// fix.
#[test]
fn double_qmark_is_not_infix() {
    let source = "a ?? b\n";
    let parse = parse(source);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::INFIX_APP_EXPR),
        "must not classify `??` as infix anywhere in tree"
    );
    assert_lossless(source, &parse);
}

/// `f -1` — FCS's LexFilter (`SyntaxTree/LexFilter.fs:2694`) rewrites
/// the `-` here to `ADJACENT_PREFIX_OP` because it's adjacent-right
/// (no whitespace between `-` and `1`) AND non-adjacent-left (gap
/// between `f` and `-`). The result FCS produces is `App(f, -1)`
/// where `-1` is a signed Int32 literal — NOT `f - 1` binary
/// subtraction. Phase 3.5 will handle the prefix form; for now
/// `peek_infix_continuation` must refuse to treat `-` as infix
/// here. The tree must contain NO `INFIX_APP_EXPR` node.
#[test]
fn adjacent_prefix_minus_not_treated_as_infix() {
    let source = "f -1\n";
    let parse = parse(source);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::INFIX_APP_EXPR),
        "must not classify adjacent-prefix `-` as infix anywhere in tree"
    );
    assert_lossless(source, &parse);
}

/// `1 +2` — symmetric to [`adjacent_prefix_minus_not_treated_as_infix`].
/// Same LexFilter rewrite (adjacent-right `+` becomes
/// `ADJACENT_PREFIX_OP`), so we must not produce `1 + 2` infix.
#[test]
fn adjacent_prefix_plus_not_treated_as_infix() {
    let source = "1 +2\n";
    let parse = parse(source);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::INFIX_APP_EXPR),
        "must not classify adjacent-prefix `+` as infix anywhere in tree"
    );
    assert_lossless(source, &parse);
}

/// `f - 1` (spaces both sides) — the normal infix subtraction shape.
/// Regression guard: the adjacency gate added for
/// [`adjacent_prefix_minus_not_treated_as_infix`] must NOT swallow
/// the well-spaced infix case.
#[test]
fn spaced_minus_is_still_infix() {
    let source = "f - 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App");
    };
    let Expr::App(infix) = outer.func().expect("func") else {
        panic!("expected outer func to be infix App");
    };
    assert!(infix.is_infix());
    assert_lossless(source, &parse);
}

/// `f-1` (no whitespace at all) — FCS's rewrite triggers only when
/// the `-` is adjacent-right AND has a gap from the LHS, so no
/// gap on either side stays as plain `MINUS` (infix). Regression
/// guard for the adjacency gate's correct shape.
#[test]
fn unspaced_minus_is_still_infix() {
    let source = "f-1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Expr(decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Expr")
    };
    let Expr::App(outer) = decl.expr().expect("expr") else {
        panic!("expected outer App");
    };
    let Expr::App(infix) = outer.func().expect("func") else {
        panic!("expected outer func to be infix App");
    };
    assert!(infix.is_infix());
    assert_lossless(source, &parse);
}

/// `1true` — FCS's lex.fsl (line 515) treats `(int | xint | float)
/// ident_char+` as a single malformed numeric literal and emits an
/// error. Our lexer doesn't merge them: it produces `Int("1")` then
/// `True`. Without a parser-level guard, `parse_app_expr` would
/// happily build `App(1, true)` — silently accepting input FCS
/// rejects. The guard in `at_app_continuation` must refuse to
/// continue an application when the LHS's last raw token is a
/// numeric literal AND the next non-trivia raw is adjacent (no
/// whitespace gap). This phase-3.4 guard doesn't mirror FCS's
/// single-token shape, but it does ensure we don't *silently
/// accept* `App(1, true)` — we surface a parse error instead.
#[test]
fn adjacent_numeric_then_keyword_not_app() {
    let source = "1true\n";
    let parse = parse(source);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "must not build App(1, true) for malformed numeric `1true`",
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for malformed numeric `1true`",
    );
    assert_lossless(source, &parse);
}

/// `123abc` — same as [`adjacent_numeric_then_keyword_not_app`] but
/// with an identifier suffix instead of a keyword. The lexer splits
/// at the digit/ident boundary giving `Int("123")` then `Ident("abc")`;
/// the adjacency guard must refuse to App these.
#[test]
fn adjacent_numeric_then_ident_not_app() {
    let source = "123abc\n";
    let parse = parse(source);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "must not build App(123, abc) for malformed numeric `123abc`",
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for malformed numeric `123abc`",
    );
    assert_lossless(source, &parse);
}

/// `1 abc` (with whitespace) — FCS treats this as `App(1, abc)`,
/// a valid application of a numeric "function" to an ident arg.
/// Regression guard: the adjacency guard must NOT swallow the
/// whitespace-separated case. Whitespace is the discriminator.
#[test]
fn spaced_numeric_then_ident_is_app() {
    let source = "1 abc\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "`1 abc` should still build App(1, abc) when whitespace separates them",
    );
    assert_lossless(source, &parse);
}

/// `1(2)` — FCS treats numeric-then-paren as a valid application
/// (`1` applied to `(2)`, matching the Atomic-flag form). FCS's
/// malformed-numeric rule (`lex.fsl:515`) only matches
/// `(int|xint|float) ident_char+`; `(` is not an ident_char, so
/// this is a clean App. Regression guard: my round-4 adjacency
/// guard must only refuse continuation when the follower starts
/// with an ident_char, not for arbitrary adjacent atoms.
#[test]
fn adjacent_numeric_then_paren_is_app() {
    let source = "1(2)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "`1(2)` should parse as App(1, (2))",
    );
    assert_lossless(source, &parse);
}

/// `f(x)` — identifier-adjacent paren call. LexFilter now emits
/// `Virtual::HighPrecedenceParenApp` between `f` and `(`. The parser
/// must consume the marker and continue the application; otherwise
/// `f(x)` regresses into two separate decls (`f` and `(x)`).
#[test]
fn ident_adjacent_paren_call_is_app() {
    let source = "f(x)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR),
        "`f(x)` should parse as App(f, (x)); tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// `arr[0]` — identifier-adjacent bracket indexer (phase 10.16c). LexFilter
/// emits `Virtual::HighPrecedenceBrackApp` between the ident and the `[`; the
/// postfix tail consumes it as the zero-width
/// [`SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK`] marker and wraps the head + the
/// `[0]` list literal in an atomic `APP_EXPR` (FCS `App(Atomic, arr,
/// ArrayOrListComputed[0])`). Parses cleanly (no diagnostics) and losslessly.
#[test]
fn ident_adjacent_lbrack_marker_is_consumed_losslessly() {
    let source = "arr[0]\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "`arr[0]` should parse cleanly; errors:\n{:?}\ntree:\n{}",
        parse.errors,
        debug_tree(&parse.root),
    );
    // The HPB marker is a zero-width *token*, so it is invisible to the
    // node-only `tree_contains_kind`; scan tokens directly.
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK),
        "`arr[0]` must carry the HPB marker; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::ARRAY_OR_LIST_EXPR),
        "`arr[0]`'s index must be an ARRAY_OR_LIST_EXPR; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// `f(` — identifier adjacent to an unclosed/empty `(`. LexFilter
/// still emits `Virtual::HighPrecedenceParenApp`, but the `(` has no
/// well-formed contents. Without the well-formedness gate in
/// `at_app_continuation`, the App loop would dive into
/// `parse_atomic_expr`'s LParen-dispatch and hit `unreachable!`.
/// The guard here is that parsing terminates with errors (no panic).
#[test]
fn ident_adjacent_unclosed_paren_does_not_panic() {
    let source = "f(\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected parse errors for `f(`; got none",
    );
    assert_lossless(source, &parse);
}

/// `f(+)` — an identifier adjacent to a parenthesised operator-value.
/// LexFilter emits the `HighPrecedenceParenApp` marker between `f` and
/// `(`, and the `(+)` argument is a `LONG_IDENT_EXPR` operator-value, so
/// the whole thing is the atomic application `App(Atomic, f, (+))` —
/// error-free. (Before operator-values were parsed, the `(+)` argument
/// failed to start an atomic expression and the construct recovered with
/// an error.)
#[test]
fn ident_adjacent_paren_around_operator_value() {
    let source = "f(+)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      APP_EXPR@0..4
        IDENT_EXPR@0..1
          IDENT_TOK@0..1 \"f\"
        HIGH_PRECEDENCE_PAREN_APP_TOK@1..1 \"\"
        LONG_IDENT_EXPR@1..4
          LONG_IDENT@1..4
            LPAREN_TOK@1..2 \"(\"
            IDENT_TOK@2..3 \"+\"
            RPAREN_TOK@3..4 \")\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A `LexError` surfacing through the filtered stream must reach
/// `Parse.errors` with its specific message — generic "unexpected token"
/// drops the cause. An unterminated string is the canonical case
/// (`logos`'s `Err(LexError::default())` for the fallback).
#[test]
fn lex_error_preserved_in_parse_errors() {
    // `"foo` is unterminated — lexer's StringLit pattern requires a
    // closing quote.
    let source = "\"foo\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.starts_with("lex error")),
        "expected a 'lex error' ParseError; got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `f(if true then 1 else 2)` — the adjacent paren-app form
/// (LexFilter emits `Virtual::HighPrecedenceParenApp` between
/// `f` and `(`). The paren body is a full expression (`declExpr`
/// in pars.fsy), so `at_app_continuation`'s LParen lookahead must
/// accept the same starter set as `peek_is_expr_start` — i.e.
/// `raw_starts_minus_expr`, not `raw_starts_atomic_expr`. With the
/// narrower predicate, the spaced form `f (if …)` worked while the
/// adjacent form fell off the app-continuation loop and merged into
/// neighbouring decls. A full FCS diff is blocked on the deferred
/// `is_atomic` projection (FCS reports `Atomic` for the adjacent
/// form, our App normaliser hardcodes `false`), so this checks
/// structural shape only: `App > [Ident("f"), Paren(IfThenElse)]`.
#[test]
fn adjacent_paren_app_accepts_if_then_else_body() {
    let source = "f(if true then 1 else 2)\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "expected a clean parse, got errors: {:?}",
        parse.errors,
    );

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(expr_decl) = decl else {
        panic!("expected ModuleDecl::Expr, got {decl:?}");
    };
    let expr = expr_decl.expr().expect("expr decl has an expr");
    let crate::syntax::Expr::App(app) = expr else {
        panic!("expected App, got {expr:?}");
    };
    assert!(!app.is_infix(), "adjacent paren-app must not be infix");
    let func = app.func().expect("App.func");
    assert!(
        matches!(
            func,
            crate::syntax::Expr::LongIdent(_) | crate::syntax::Expr::Ident(_)
        ),
        "func should be `f`, got {func:?}",
    );
    let arg = app.arg().expect("App.arg");
    let crate::syntax::Expr::Paren(paren) = arg else {
        panic!("expected Paren-wrapped arg, got {arg:?}");
    };
    let inner = paren.inner().expect("Paren.inner");
    assert!(
        matches!(inner, crate::syntax::Expr::IfThenElse(_)),
        "paren body should be an IfThenElse, got {inner:?}",
    );
}

/// `null` is FCS's `SynExpr.Null` — a distinct atomic expression, *not*
/// a `SynConst`. A bare top-level `null` is an `EXPR_DECL` wrapping a
/// `NULL_EXPR > [NULL_TOK]`, with no parse errors.
#[test]
fn bare_null_expression() {
    let source = "null\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    EXPR_DECL@0..4
      NULL_EXPR@0..4
        NULL_TOK@0..4 \"null\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `let x = null` — `null` stands on a binding RHS like any other atom.
/// The `=`-opened offside block (`OBLOCKBEGIN`) lands as the usual
/// zero-width `ERROR` placeholder; the RHS is the `NULL_EXPR` atom.
#[test]
fn let_binding_null_rhs() {
    let source = "let x = null\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..13
  MODULE_OR_NAMESPACE@0..13
    LET_DECL@0..12
      LET_TOK@0..3 \"let\"
      BINDING@3..12
        NAMED_PAT@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        WHITESPACE@5..6 \" \"
        EQUALS_TOK@6..7 \"=\"
        WHITESPACE@7..8 \" \"
        ERROR@8..8 \"\"
        NULL_EXPR@8..12
          NULL_TOK@8..12 \"null\"
    NEWLINE@12..13 \"\\n\"
    ERROR@13..13 \"\"
    ERROR@13..13 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `null` in application-argument position — `f null` is
/// `App(f, Null)`, confirming `null` is admitted as an `argExpr`
/// (`raw_starts_atomic_expr`), not just as a top-level atom.
#[test]
fn null_as_app_arg() {
    let source = "f null\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Expr(decl) = decl else {
        panic!("expected ModuleDecl::Expr")
    };
    let crate::syntax::Expr::App(app) = decl.expr().expect("expr") else {
        panic!("expected App, got {:?}", decl.expr());
    };
    assert!(
        matches!(app.arg(), Some(crate::syntax::Expr::Null(_))),
        "arg should be Null, got {:?}",
        app.arg(),
    );
    assert_lossless(source, &parse);
}

// ---- type-relation operators (`:?` / `:>` / `:?>`) -----------------------

/// Extract the RHS expression of the first `let` binding.
fn first_binding_rhs(root: &SyntaxNode) -> crate::syntax::Expr {
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let crate::syntax::ModuleDecl::Let(let_decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Let");
    };
    let binding = let_decl.bindings().next().expect("binding");
    binding.expr().expect("binding rhs expr")
}

/// `a :?> b` — the downcast operator builds a `DOWNCAST_EXPR` whose children
/// are the cast expression, the `:?>` token, and the target type.
#[test]
fn downcast_expr_shape() {
    use crate::syntax::AstNode;
    let source = "let foo = a :?> b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let crate::syntax::Expr::Downcast(d) = first_binding_rhs(&parse.root) else {
        panic!(
            "expected Downcast, got {:?}",
            first_binding_rhs(&parse.root)
        );
    };
    assert!(
        matches!(d.expr(), Some(crate::syntax::Expr::Ident(_))),
        "downcast inner expr should be the ident `a`, got {:?}",
        d.expr(),
    );
    assert!(
        matches!(d.ty(), Some(crate::syntax::Type::LongIdent(_))),
        "downcast target type should be the long-ident `b`, got {:?}",
        d.ty(),
    );
    assert_eq!(
        count_tok(d.syntax(), SyntaxKind::COLON_QMARK_GREATER_TOK),
        1
    );
    assert_lossless(source, &parse);
}

/// `a :> b` — the upcast operator builds an `UPCAST_EXPR`.
#[test]
fn upcast_expr_shape() {
    use crate::syntax::AstNode;
    let source = "let foo = a :> b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let crate::syntax::Expr::Upcast(u) = first_binding_rhs(&parse.root) else {
        panic!("expected Upcast, got {:?}", first_binding_rhs(&parse.root));
    };
    assert!(matches!(u.expr(), Some(crate::syntax::Expr::Ident(_))));
    assert!(matches!(u.ty(), Some(crate::syntax::Type::LongIdent(_))));
    assert_eq!(count_tok(u.syntax(), SyntaxKind::COLON_GREATER_TOK), 1);
    assert_lossless(source, &parse);
}

/// `a :? b` — the type-test operator builds a `TYPE_TEST_EXPR`.
#[test]
fn type_test_expr_shape() {
    use crate::syntax::AstNode;
    let source = "let foo = a :? b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let crate::syntax::Expr::TypeTest(t) = first_binding_rhs(&parse.root) else {
        panic!(
            "expected TypeTest, got {:?}",
            first_binding_rhs(&parse.root)
        );
    };
    assert!(matches!(t.expr(), Some(crate::syntax::Expr::Ident(_))));
    assert!(matches!(t.ty(), Some(crate::syntax::Type::LongIdent(_))));
    assert_eq!(count_tok(t.syntax(), SyntaxKind::COLON_QMARK_TOK), 1);
    assert_lossless(source, &parse);
}

/// `a :?> b :?> c` — left-associative, so the outer node wraps an inner
/// `DOWNCAST_EXPR` on its expression side: `(a :?> b) :?> c`.
#[test]
fn downcast_is_left_associative() {
    let source = "let foo = a :?> b :?> c\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let crate::syntax::Expr::Downcast(outer) = first_binding_rhs(&parse.root) else {
        panic!("expected outer Downcast");
    };
    assert!(
        matches!(outer.expr(), Some(crate::syntax::Expr::Downcast(_))),
        "left-assoc: outer downcast's expr must be the inner downcast, got {:?}",
        outer.expr(),
    );
    assert_lossless(source, &parse);
}

/// `let foo = a :?> ` — the `COLON_QMARK_GREATER recover` arm: a missing type
/// must still emit a `DOWNCAST_EXPR` (carrying the expr + operator), record an
/// "expected type" error, stay lossless, and never panic.
#[test]
fn downcast_missing_type_recovers_without_panic() {
    let source = "let foo = a :?> \n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for the type-less `{source}`",
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::DOWNCAST_EXPR),
        "the recovery path must still emit a DOWNCAST_EXPR; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// `let foo = a : b` — a bare `:` **is** a type annotation (`SynExpr.Typed`):
/// FCS's `typedSequentialExpr: sequentialExpr COLON typ` (`pars.fsy:4088`)
/// applies at every body / block-RHS position, including a `let` RHS. Verified
/// via `fcs-dump`: FCS parses this cleanly (`ParseHadErrors = false`) into a
/// `Typed` node. (Earlier this was wrongly rejected — the annotation was only
/// recognised inside a typed paren.) It must be a `TYPED_EXPR`, *not* a cast
/// node — adding `:?` / `:>` / `:?>` must keep a bare `:` out of the cast branch.
#[test]
fn bare_colon_is_a_type_annotation() {
    let source = "let foo = a : b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::TYPED_EXPR),
        "bare `:` must produce a TYPED_EXPR; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::TYPE_TEST_EXPR)
            && !tree_contains_kind(&parse.root, SyntaxKind::UPCAST_EXPR)
            && !tree_contains_kind(&parse.root, SyntaxKind::DOWNCAST_EXPR),
        "bare `:` must not fabricate a cast node; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// A bare `: T` inside `[ … ]` / `[| … |]` is **not** a type annotation: FCS's
/// `listExprElements` / `arrayExprElements` are `sequentialExpr`, not
/// `typedSequentialExpr`, so `[1 : int]` / `[|1 : int|]` are rejected (verified
/// via `fcs-dump`: `ParseHadErrors = true`). The annotation needs parens
/// (`[(1 : int)]`). Guards the `typedSequentialExpr` hook against firing in the
/// list/array element path.
#[test]
fn bare_colon_in_brackets_is_error() {
    for source in ["let xs = [1 : int]\n", "let xs = [|1 : int|]\n"] {
        let parse = parse(source);
        // The essential FCS-matching behaviour: it must be *rejected*, not
        // silently accepted as a valid annotated element. (The exact recovery
        // shape is incidental — since both sides error, the corpus harness never
        // diffs the trees.)
        assert!(
            !parse.errors.is_empty(),
            "{source:?}: a bare `:` in brackets must be an error",
        );
        assert_lossless(source, &parse);
    }
}

/// Negative control: a *parenthesised* annotation inside brackets is fine —
/// `[(1 : int)]` is `[ Typed(1, int) ]`, no error.
#[test]
fn parenthesized_colon_in_brackets_is_ok() {
    let source = "let xs = [(1 : int)]\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::TYPED_EXPR),
        "the parenthesised `: int` must still be a TYPED_EXPR; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Object expression `{ new T with member … }` — the reported bug. Pins the
/// green-tree shape: an `OBJ_EXPR` carrying the `NEW_EXPR` base call, the `with`,
/// and the member, with no errors. (The normalised shape is diffed against FCS
/// in `parser_diff_obj_expr.rs`.)
#[test]
fn obj_expr_member_form_green_tree() {
    let source = "let x =\n    { new IDisposable with\n        member x.Dispose () = () }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    let obj = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::OBJ_EXPR)
        .expect("expected an OBJ_EXPR node");
    // The base call is the leading NEW_EXPR child; no spurious missing-argument
    // error (the bare `new IDisposable` has no parens — `argOptions = None`).
    assert!(
        obj.children().any(|n| n.kind() == SyntaxKind::NEW_EXPR),
        "OBJ_EXPR must carry a NEW_EXPR base call",
    );
    // The `with` keyword and exactly one member.
    assert_eq!(count_tok(&obj, SyntaxKind::WITH_TOK), 1, "one `with` token");
    assert_eq!(
        obj.children()
            .filter(|n| n.kind() == SyntaxKind::MEMBER_DEFN)
            .count(),
        1,
        "exactly one member",
    );
    // The closing brace is recovered.
    assert_eq!(
        count_tok(&obj, SyntaxKind::RBRACE_TOK),
        1,
        "one closing-brace token",
    );
}

/// A `new`-headed brace with constructor args but **no** `with`/interface block
/// is a computation expression wrapping a `SynExpr.New`, not an object
/// expression — `{ new T(1, 2) }`. Guards the disambiguation that keeps the
/// existing computation-expression behaviour.
#[test]
fn new_in_brace_without_with_is_computation() {
    let source = "let x = { new T(1, 2) }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::OBJ_EXPR),
        "a `with`-less `{{ new T(args) }}` must not be an object expression",
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::COMPUTATION_EXPR)
            && tree_contains_kind(&parse.root, SyntaxKind::NEW_EXPR),
        "it is a COMPUTATION_EXPR wrapping a NEW_EXPR",
    );
}

/// `a :: b` — the cons operator builds a dedicated `CONS_EXPR` green node
/// `[<lhs>, COLON_COLON_TOK, <rhs>]` (lossless, mirroring the pattern-side
/// `LIST_CONS_PAT`). The FCS-faithful App-of-Tuple projection is exercised by
/// the differential tests in `tests/all/parser_diff_cons.rs`; here we pin the green
/// shape.
#[test]
fn cons_expr_green_shape() {
    let source = "let xs = a :: b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..16
  MODULE_OR_NAMESPACE@0..16
    LET_DECL@0..15
      LET_TOK@0..3 \"let\"
      BINDING@3..15
        NAMED_PAT@3..6
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..6 \"xs\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        CONS_EXPR@9..15
          IDENT_EXPR@9..10
            IDENT_TOK@9..10 \"a\"
          WHITESPACE@10..11 \" \"
          COLON_COLON_TOK@11..13 \"::\"
          IDENT_EXPR@13..15
            WHITESPACE@13..14 \" \"
            IDENT_TOK@14..15 \"b\"
    NEWLINE@15..16 \"\\n\"
    ERROR@16..16 \"\"
    ERROR@16..16 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `(a ::) y` — incomplete input: the `::` inside the paren has no RHS before
/// the LexFilter-swallowed `)`. The RHS lookahead must **not** peer past that
/// swallowed closer and grab the enclosing `y` as the tail (which would build
/// the cons across the paren and drain the real `)` as `ERROR`, mis-nesting the
/// body). Instead the `::` is left for enclosing recovery: no `CONS_EXPR` is
/// built and the parse stays lossless. Pins the after-operator swallowed-closer
/// gate in `peek_cons_continuation`.
#[test]
fn cons_expr_missing_rhs_before_swallowed_close_recovers() {
    let source = "let x = (a ::) y\n";
    let parse = parse(source);
    // Incomplete input → errors are expected; the contract is no panic, a
    // lossless tree, and no mis-built cons across the closer.
    assert_lossless(source, &parse);
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::CONS_EXPR),
        "a `::` with no RHS before the swallowed `)` must not build a CONS_EXPR; tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// `a :: b :: c` is right-associative: the *outer* `CONS_EXPR`'s tail (`rhs`)
/// is itself a `CONS_EXPR`, not its head. Pins the `rbp == lbp` recursion in
/// the Pratt cons branch via the typed facade.
#[test]
fn cons_expr_right_associative_nesting() {
    use crate::syntax::{AstNode, ConsExpr, Expr};
    let source = "let xs = a :: b :: c\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let outer = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::CONS_EXPR)
        .and_then(ConsExpr::cast)
        .expect("a CONS_EXPR node");
    assert!(
        matches!(outer.lhs(), Some(Expr::Ident(_))),
        "outer head is the bare `a`, got {:?}",
        outer.lhs(),
    );
    assert!(
        matches!(outer.rhs(), Some(Expr::Cons(_))),
        "outer tail nests `b :: c`, got {:?}",
        outer.rhs(),
    );
}

/// The canonical inline-IL expression `(# "foo" : int #)`
/// (`SynExpr.LibraryOnlyILAssembly`). FCS reaches it via `parenExpr`, so the
/// shape is `Paren(LibraryOnlyILAssembly)`: a `PAREN_EXPR` owning the `(`/`)`
/// (the closing `)` is LexFilter-swallowed and recovered as `RPAREN_TOK`) around
/// an `INLINE_IL_EXPR` holding the `# … #` body. The instruction string is a
/// bare `STRING_LIT` (not a `CONST_EXPR`); the `: int` return type is a real
/// `LONG_IDENT_TYPE`.
#[test]
fn inline_il_basic_return_type() {
    let source = "(# \"foo\" : int #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    EXPR_DECL@0..17
      PAREN_EXPR@0..17
        LPAREN_TOK@0..1 \"(\"
        INLINE_IL_EXPR@1..16
          HASH_TOK@1..2 \"#\"
          WHITESPACE@2..3 \" \"
          STRING_LIT@3..8 \"\\\"foo\\\"\"
          WHITESPACE@8..9 \" \"
          COLON_TOK@9..10 \":\"
          WHITESPACE@10..11 \" \"
          LONG_IDENT_TYPE@11..14
            LONG_IDENT@11..14
              IDENT_TOK@11..14 \"int\"
          WHITESPACE@14..15 \" \"
          HASH_TOK@15..16 \"#\"
        RPAREN_TOK@16..17 \")\"
    NEWLINE@17..18 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Curried value arguments (`optCurriedArgExprs`): `(# "add" a b : int #)`
/// carries two `IDENT_EXPR` arguments between the instruction string and the
/// `:` return type. Each is a separate `argExpr`, not an application.
#[test]
fn inline_il_with_arguments() {
    let source = "(# \"add\" a b : int #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..22
  MODULE_OR_NAMESPACE@0..22
    EXPR_DECL@0..21
      PAREN_EXPR@0..21
        LPAREN_TOK@0..1 \"(\"
        INLINE_IL_EXPR@1..20
          HASH_TOK@1..2 \"#\"
          WHITESPACE@2..3 \" \"
          STRING_LIT@3..8 \"\\\"add\\\"\"
          IDENT_EXPR@8..10
            WHITESPACE@8..9 \" \"
            IDENT_TOK@9..10 \"a\"
          IDENT_EXPR@10..12
            WHITESPACE@10..11 \" \"
            IDENT_TOK@11..12 \"b\"
          WHITESPACE@12..13 \" \"
          COLON_TOK@13..14 \":\"
          WHITESPACE@14..15 \" \"
          LONG_IDENT_TYPE@15..18
            LONG_IDENT@15..18
              IDENT_TOK@15..18 \"int\"
          WHITESPACE@18..19 \" \"
          HASH_TOK@19..20 \"#\"
        RPAREN_TOK@20..21 \")\"
    NEWLINE@21..22 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// No arguments and no return type — `(# "!0[]" #)` is just the instruction
/// string between the delimiters (`optInlineAssemblyReturnTypes` empty).
#[test]
fn inline_il_string_only() {
    let source = "(# \"!0[]\" #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..13
  MODULE_OR_NAMESPACE@0..13
    EXPR_DECL@0..12
      PAREN_EXPR@0..12
        LPAREN_TOK@0..1 \"(\"
        INLINE_IL_EXPR@1..11
          HASH_TOK@1..2 \"#\"
          WHITESPACE@2..3 \" \"
          STRING_LIT@3..9 \"\\\"!0[]\\\"\"
          WHITESPACE@9..10 \" \"
          HASH_TOK@10..11 \"#\"
        RPAREN_TOK@11..12 \")\"
    NEWLINE@12..13 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The `type (T)` type argument (`opt_inlineAssemblyTypeArg`). The `type`
/// keyword is LexFilter-swallowed and recovered as `TYPE_TOK`; the
/// parenthesised type's `)` is swallowed too and recovered as `RPAREN_TOK`.
/// Verifies both the spaced (`type ('T)`) reading here and that the inner type
/// is a real `VAR_TYPE`.
#[test]
fn inline_il_type_argument() {
    let source = "(# \"sizeof !0\" type ('T) : nativeint #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..40
  MODULE_OR_NAMESPACE@0..40
    EXPR_DECL@0..39
      PAREN_EXPR@0..39
        LPAREN_TOK@0..1 \"(\"
        INLINE_IL_EXPR@1..38
          HASH_TOK@1..2 \"#\"
          WHITESPACE@2..3 \" \"
          STRING_LIT@3..14 \"\\\"sizeof !0\\\"\"
          WHITESPACE@14..15 \" \"
          TYPE_TOK@15..19 \"type\"
          WHITESPACE@19..20 \" \"
          LPAREN_TOK@20..21 \"(\"
          VAR_TYPE@21..23
            QUOTE_TOK@21..22 \"'\"
            IDENT_TOK@22..23 \"T\"
          RPAREN_TOK@23..24 \")\"
          WHITESPACE@24..25 \" \"
          COLON_TOK@25..26 \":\"
          WHITESPACE@26..27 \" \"
          LONG_IDENT_TYPE@27..36
            LONG_IDENT@27..36
              IDENT_TOK@27..36 \"nativeint\"
          WHITESPACE@36..37 \" \"
          HASH_TOK@37..38 \"#\"
        RPAREN_TOK@38..39 \")\"
    NEWLINE@39..40 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The unit return form `: ()` (FCS's `COLON LPAREN rparen`, an *empty*
/// `retTy`) — distinct from `: unit`. Inside the `INLINE_IL_EXPR` it is
/// `COLON_TOK LPAREN_TOK RPAREN_TOK` with no inner type (that `)` is swallowed
/// and recovered); the outer `)` belongs to the wrapping `PAREN_EXPR`.
#[test]
fn inline_il_unit_return() {
    let source = "(# \"pop\" x : () #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..19
  MODULE_OR_NAMESPACE@0..19
    EXPR_DECL@0..18
      PAREN_EXPR@0..18
        LPAREN_TOK@0..1 \"(\"
        INLINE_IL_EXPR@1..17
          HASH_TOK@1..2 \"#\"
          WHITESPACE@2..3 \" \"
          STRING_LIT@3..8 \"\\\"pop\\\"\"
          IDENT_EXPR@8..10
            WHITESPACE@8..9 \" \"
            IDENT_TOK@9..10 \"x\"
          WHITESPACE@10..11 \" \"
          COLON_TOK@11..12 \":\"
          WHITESPACE@12..13 \" \"
          LPAREN_TOK@13..14 \"(\"
          RPAREN_TOK@14..15 \")\"
          WHITESPACE@15..16 \" \"
          HASH_TOK@16..17 \"#\"
        RPAREN_TOK@17..18 \")\"
    NEWLINE@18..19 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Inline IL is an `atomicExpr`, so it stands as a high-precedence application
/// argument: `f (# "ldnull" : 'T #)` is `App(f, InlineIl)`.
#[test]
fn inline_il_in_argument_position() {
    let source = "f (# \"ldnull\" : 'T #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(tree_contains_kind(&parse.root, SyntaxKind::INLINE_IL_EXPR));
    assert!(tree_contains_kind(&parse.root, SyntaxKind::APP_EXPR));
    assert_lossless(source, &parse);
}

/// A round-trip + no-error sweep over the real inline-IL shapes that appear in
/// FSharp.Core: compact `type('T)`, parenthesised arguments, generic and
/// postfix return types, the `type (T) arg₀ … value` no-return form, and a
/// `let inline` binding context. Each must parse cleanly (no `ParseError`) and
/// losslessly, and contain exactly one `INLINE_IL_EXPR`.
#[test]
fn inline_il_real_world_shapes_are_clean() {
    for source in [
        "(# \"ldlen.multi 3 0\" array : int #)\n",
        "(# \"newarr !0\" type ('T) n : 'T array #)\n",
        "(# \"ldelem.multi 2 !0\" type ('T) array index1 index2 : 'T #)\n",
        "(# \"stobj !0\" type('T) address value #)\n",
        "(# \"localloc\" (count * x) : nativeptr<'T> #)\n",
        "(# \"\" address : 'T byref  #)\n",
        "(# \"cpblk\" destination source (count * x) #)\n",
        "let inline neg (x: 'T) : 'T = (# \"neg\" x : 'T #)\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} produced errors: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);
        let count = parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::INLINE_IL_EXPR)
            .count();
        assert_eq!(
            count, 1,
            "{source:?} should have exactly one INLINE_IL_EXPR"
        );
    }
}

/// Inline IL split across lines parses cleanly regardless of the continuation
/// column. At some columns the lex-filter inserts an offside `Virtual::BlockSep`
/// between the inline-IL tokens (at others it does not); the parser skips those
/// spurious separators, so every layout below is error-free and lossless and
/// holds exactly one `INLINE_IL_EXPR`. The varied indentations exercise both the
/// separator-present and separator-absent cases at the string→arg, arg→`:`,
/// `:`→type, and type→`#` boundaries.
#[test]
fn inline_il_multiline_is_clean() {
    for source in [
        "(# \"neg\"\n x : int #)\n",      // 1-space continuation (emits BlockSep)
        "(# \"neg\"\n      x : int #)\n", // deep continuation (no BlockSep)
        "(# \"neg\"\nx : int #)\n",       // column-0 continuation
        "(# \"add\"\n a\n b : int #)\n",  // each argument on its own line
        "(# \"neg\" x\n : int #)\n",      // newline before `:`
        "(# \"neg\" x :\n int #)\n",      // newline before the return type
        "(# \"sizeof !0\" type ('T)\n : nativeint #)\n", // newline after `type (T)`
        "let y =\n  (# \"add\"\n     a\n     b : int #)\n", // inside a `let` RHS block
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} produced errors: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);
        let count = parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::INLINE_IL_EXPR)
            .count();
        assert_eq!(
            count, 1,
            "{source:?} should have exactly one INLINE_IL_EXPR"
        );
    }
}

/// Malformed inline IL must degrade gracefully — a recovery error, never a
/// panic, and always lossless (`text(tree) == source`). Covers a missing
/// instruction string, a missing closing `#`, an unterminated expression,
/// `type` without its `(`, and a byte-string instruction (FCS rejects it; byte
/// strings lex as `BYTEARRAY`, not the `string` nonterminal).
#[test]
fn inline_il_malformed_recovers_losslessly() {
    for source in [
        "(# #)\n",                // no instruction string
        "(# \"foo\" : int )\n",   // missing closing `#`
        "(# \"foo\"\n",           // unterminated (EOF before `#)`)
        "(# \"foo\" type x #)\n", // `type` not followed by `(`
        "(# \"ld\"B #)\n",        // byte-string instruction
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should report a recovery error"
        );
        assert_lossless(source, &parse);
    }
}

/// Recovery must never reach *past* the LexFilter-swallowed closing `)`. When
/// the `#` is missing, that `)` is absent from the *filtered* stream, so every
/// optional-part lookahead (the instruction string, `type (…)`, the arguments,
/// the `:` return type) could otherwise leap across it and consume the
/// following expression. Each must instead stop at the `)`, which is recovered
/// as the wrapping `PAREN_EXPR`'s closing `RPAREN_TOK` — leaving the trailing
/// tokens a sibling. Covers a missing `#` at each grammar position.
#[test]
fn inline_il_missing_close_does_not_swallow_following_token() {
    for source in [
        "(# \"foo\") x\n",       // bare arg position
        "(# \"add\" a) z\n",     // after an argument
        "(# \"foo\" : int) y\n", // after a return type
        "(#) \"s\"\n",           // missing instruction string, string follows
        "(# \"x\" type) (T)\n",  // `type` not followed by `(`, paren follows
        "(# \"x\" :) ()\n",      // dangling `:`, `()` follows
    ] {
        let parse = parse(source);
        // The missing `#)` is a real error, but recovery stays clean.
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should error on missing `#`"
        );
        assert_lossless(source, &parse);
        // The inline IL must still be produced, inside its `PAREN_EXPR` wrapper.
        let il = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::INLINE_IL_EXPR)
            .unwrap_or_else(|| panic!("{source:?} should still produce an INLINE_IL_EXPR"));
        let paren = il
            .parent()
            .filter(|p| p.kind() == SyntaxKind::PAREN_EXPR)
            .unwrap_or_else(|| panic!("{source:?}: INLINE_IL_EXPR should sit under a PAREN_EXPR"));
        // The wrapper closed at its own `)` …
        assert!(
            paren.text().to_string().ends_with(')'),
            "{source:?}: the inline-IL PAREN_EXPR should end at its `)` ({:?})",
            paren.text().to_string(),
        );
        // … and there is sibling content *after* it (the token it must not have
        // swallowed). `trim_end` drops the trailing newline.
        assert!(
            usize::from(paren.text_range().end()) < source.trim_end().len(),
            "{source:?}: content after the inline IL was swallowed ({:?})",
            paren.text().to_string(),
        );
    }
}

/// An incomplete query join (`query { a in } b`) must not reach *past* the
/// LexFilter-swallowed `}` for the join RHS. The `}` is absent from the
/// *filtered* stream, so a naive RHS parse (`peek_is_expr_start`) would peer
/// across it, take the outer `b` as the join's right operand, and drain the
/// real `}` as an error *inside* the `JOIN_IN_EXPR` — mis-nesting the CE. The
/// RHS path must instead apply the same raw swallowed-closer gate as the other
/// Pratt continuations: stop at the `}`, record a missing-operand error, and
/// leave the brace to close with `b` a sibling outside the CE.
#[test]
fn join_in_missing_rhs_does_not_swallow_following_token() {
    let source = "query { a in } b\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "incomplete join should report a recovery error"
    );
    assert_lossless(source, &parse);
    let join = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::JOIN_IN_EXPR)
        .expect("should still produce a JOIN_IN_EXPR");
    let ce = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::COMPUTATION_EXPR)
        .expect("should still produce a COMPUTATION_EXPR");
    // The join did not reach past the swallowed `}`: it ends at or before the
    // brace, and the brace closes before the trailing `b`.
    let brace = source.find('}').unwrap();
    let b = source.find('b').unwrap();
    assert!(
        usize::from(join.text_range().end()) <= brace,
        "JOIN_IN_EXPR reached past the closing brace ({:?})",
        join.text().to_string(),
    );
    assert!(
        usize::from(ce.text_range().end()) <= b,
        "the trailing `b` was swallowed into the computation expression ({:?})",
        ce.text().to_string(),
    );
}

/// A join RHS that is a bare `..` with no upper bound (`query { a in .. }`)
/// must recover, not panic: `peek_is_expr_start` admits a leading `..`, but
/// `parse_pratt_expr` cannot consume one (it would hit the atomic const
/// parser's `unreachable!`). The RHS path delegates a leading `..` to the
/// open-lower-range production, which reports a missing-upper error and still
/// builds the node losslessly. FCS likewise errors here. (Codex review 2026-06.)
#[test]
fn join_in_open_range_rhs_recovers_without_panic() {
    for source in [
        "query { a in .. }\n",  // bare `..` — missing upper
        "query { a in ..b }\n", // valid open-lower range RHS
    ] {
        let parse = parse(source);
        // No panic is implicit in reaching here; pin losslessness.
        assert_lossless(source, &parse);
        assert!(
            parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::JOIN_IN_EXPR),
            "{source:?} should still produce a JOIN_IN_EXPR"
        );
    }
}

/// A trait-call member signature missing its `static`/`abstract`/`member`/`new`
/// introducer (`(^a : (M : …) x)`) is an FCS error — so the trait-call gate must
/// *not* commit. The parser recovers (errors, no panic, lossless) and crucially
/// produces **no** `TRAIT_CALL_EXPR` for these unsupported forms. (Operator-named
/// member sigs are supported — see `parser_diff_trait_call`'s
/// `diff_trait_call_operator_member` — as are `inline` ones, FCS accepting them:
/// `diff_trait_call_inline_member` / `diff_trait_call_inline_method`.)
#[test]
fn unsupported_trait_call_member_sig_does_not_commit() {
    let source = "let inline g (x: ^a) = (^a : (M : ^a -> int) x)\n"; // no introducer
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "{source:?} should error (FCS rejects the member sig)"
    );
    assert_lossless(source, &parse);
    assert!(
        parse
            .root
            .descendants()
            .all(|n| n.kind() != SyntaxKind::TRAIT_CALL_EXPR),
        "{source:?} should not commit to a TRAIT_CALL_EXPR"
    );
}

/// The support alternatives `( ^a or … )` are FCS's `typarAlts`
/// (`typar (OR appTypeCanBeNullable)*`): a *typar* first, then `or`-separated
/// `appType`s. Shapes that are not that list — a token where the separator or
/// closer is due, an empty alternative — must **not** commit to a
/// `TRAIT_CALL_EXPR` (FCS rejects them all); the parser must still error
/// cleanly, losslessly, and without panicking.
#[test]
fn malformed_trait_call_support_alts_do_not_commit() {
    for source in [
        // A token where the separator/closer is due.
        "let inline f (x: ^a) = ((^a b) : (static member M : ^a -> int) x)\n",
        // Empty alternatives.
        "let inline f (x: ^a) = ((^a or) : (static member M : ^a -> int) x)\n",
        "let inline f (x: ^a) = ((^a or or int) : (static member M : ^a -> int) x)\n",
        // A non-typar *first* alternative (`typarAlts`' base case is a typar).
        "let inline f (x: ^a) = ((int or ^a) : (static member M : ^a -> int) x)\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should error (FCS rejects the support alternatives)"
        );
        assert_lossless(source, &parse);
        assert!(
            parse
                .root
                .descendants()
                .all(|n| n.kind() != SyntaxKind::TRAIT_CALL_EXPR),
            "{source:?} should not commit to a TRAIT_CALL_EXPR"
        );
    }
}

/// An alternative that runs *past* what an `appType` can consume
/// (`(^a or int -> string)` — FCS: "Unexpected symbol '->' … Expected 'or', ')'")
/// is beyond what the token-level commit scan can see, so the parser does commit.
/// It must then recover on the operand rather than panic or run away: an error,
/// and the paren expression still closes at its own `)` (nothing past the closer
/// is dragged in).
#[test]
fn trait_call_support_alt_beyond_app_type_recovers() {
    let source =
        "let inline f (x: ^a) = ((^a or int -> string) : (static member M : ^a -> int) x)\n";
    let parse = parse(source);
    assert!(!parse.errors.is_empty(), "{source:?} should error");
    assert_lossless(source, &parse);
}

/// A trait call whose argument is missing and whose outer `)` was swallowed by
/// LexFilter (`(^a : (static member M : …)) x`): the argument-expression gate
/// must stop at the swallowed closer rather than dragging the following token
/// (`x`) in as the argument. The `TRAIT_CALL_EXPR` is still produced (the member
/// sig is well-formed), the outer `PAREN_EXPR` closes at its `)`, and `x`
/// remains a sibling after it.
#[test]
fn trait_call_missing_argument_does_not_swallow_following_token() {
    let source = "let inline g (x: ^a) = (^a : (static member M : ^a -> int)) x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    let tc = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TRAIT_CALL_EXPR)
        .expect("the well-formed member sig should still produce a TRAIT_CALL_EXPR");
    let paren = tc
        .parent()
        .filter(|p| p.kind() == SyntaxKind::PAREN_EXPR)
        .expect("TRAIT_CALL_EXPR should sit under a PAREN_EXPR");
    // The wrapper closed at its own `)`, and there is sibling content (`x`)
    // after it that was not swallowed as the argument.
    assert!(
        usize::from(paren.text_range().end()) < source.trim_end().len(),
        "the trailing `x` was swallowed into the trait call ({:?})",
        paren.text().to_string(),
    );
}

/// An incomplete dynamic lookup inside a paren (`(a?)b`, `a?()b`) must recover
/// cleanly: the `?` argument gate is raw-adjacency–guarded, so it does not drag
/// the *outside* token across the LexFilter-swallowed `)` into the
/// `DYNAMIC_EXPR`. Both are FCS errors; we must error, stay lossless, and close
/// the `PAREN_EXPR` at its own `)` with the following token left a sibling.
#[test]
fn incomplete_dynamic_does_not_swallow_past_closer() {
    for source in ["let x = (a?)b\n", "let x = a?()b\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should error on the incomplete dynamic argument"
        );
        assert_lossless(source, &parse);
        if let Some(dyn_node) = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::DYNAMIC_EXPR)
        {
            // The trailing `b` must be left *outside* the dynamic node — it was
            // not dragged in across the swallowed `)`. (The `b` is the last
            // non-newline char, so the node must end before it.)
            assert!(
                usize::from(dyn_node.text_range().end()) < source.trim_end().len(),
                "{source:?}: DYNAMIC_EXPR absorbed the token past the swallowed closer ({:?})",
                dyn_node.text().to_string(),
            );
        }
    }
}

/// A bare SRTP trait-call body directly as a dynamic argument
/// (`a?(^T : (static member M : …) x)`) is an FCS error: the `dynamicArg` `(`
/// body is a `typedSequentialExpr`, not a `parenExprBody`, so the trait-call
/// form is not admitted there (it must be nested as `a?((…))`). We must error
/// (the `^` head typar is not an expression start) and stay lossless, *not*
/// commit to a `TRAIT_CALL_EXPR` inside the dynamic argument.
#[test]
fn trait_call_is_not_a_bare_dynamic_argument() {
    let source = "let f a x = a?(^T : (static member M : ^T -> int) x)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a bare trait-call dynamic argument should be a parse error"
    );
    assert_lossless(source, &parse);
    // The dynamic node's argument paren must not contain a committed trait call.
    let dyn_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::DYNAMIC_EXPR)
        .expect("the `a?(…)` should still produce a DYNAMIC_EXPR");
    assert!(
        dyn_node
            .descendants()
            .all(|n| n.kind() != SyntaxKind::TRAIT_CALL_EXPR),
        "the dynamic argument must not commit to a TRAIT_CALL_EXPR",
    );
}

/// A bare `base` with no `.` qualification is an FCS parse error (FCS's grammar
/// has only `BASE DOT atomicExprQualification`, no lone `BASE`). We must error
/// and stay lossless.
#[test]
fn bare_base_is_a_parse_error() {
    let source = "let x = base\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a bare `base` (no `.` qualification) should be a parse error"
    );
    assert_lossless(source, &parse);
}

/// An unparenthesised `base.M` in after-type argument position
/// (`new T base.M`, `inherit B base.M`) is an FCS error: `atomicExprAfterType`
/// omits the `BASE DOT` production. `base` is excluded from
/// `raw_starts_attribute_arg`, so it must *not* be consumed as an adjacent
/// constructor / inherit argument (the parenthesised `new T(base.M)` is fine).
#[test]
fn base_is_not_an_unparenthesised_after_type_argument() {
    for source in ["let g = new T base.M\n", "type C() =\n  inherit B base.M\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?}: unparenthesised `base.M` after a type should be a parse error"
        );
        assert_lossless(source, &parse);
    }
}

/// An unparenthesised `global.M` in after-type argument position
/// (`new T global.M`, `inherit B global.M`, `[<A global.X>]`) is an FCS error
/// for the same reason as `base`: `atomicExprAfterType` omits the `GLOBAL DOT`
/// production. So although `global` is now a full atomic-expression head, it is
/// excluded from `raw_starts_attribute_arg` and must *not* be consumed as an
/// adjacent constructor / inherit / attribute argument (the parenthesised
/// `new T(global.M)` is fine — its head token is `(`).
#[test]
fn global_is_not_an_unparenthesised_after_type_argument() {
    for source in [
        "let g = new T global.M\n",
        "type C() =\n  inherit B global.M\n",
        "[<A global.X>]\nlet x = 1\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?}: unparenthesised `global.M` after a type should be a parse error"
        );
        assert_lossless(source, &parse);
    }
}

mod type_op_props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any chain of the three type-relation operators between simple
        /// atoms parses without panicking and round-trips losslessly
        /// (`text(tree) == source`), error-free or not. Guards the new Pratt
        /// branch against corruption / panics on arbitrary operator orderings
        /// and chain lengths, complementing the FCS-oracle shape tests in
        /// `tests/all/parser_diff_type_relation.rs`.
        #[test]
        fn cast_chains_are_lossless_and_panic_free(
            head in "[a-z]",
            tail in proptest::collection::vec(
                (prop_oneof![Just(":?"), Just(":>"), Just(":?>")], "[a-z]"),
                0..6,
            ),
        ) {
            let mut src = format!("let foo = {head}");
            for (op, atom) in &tail {
                src.push_str(&format!(" {op} {atom}"));
            }
            src.push('\n');
            let parse = parse(&src);
            // No panic is implicit in reaching here; pin losslessness.
            assert_lossless(&src, &parse);
        }

        /// Any chain of `::` (interleaved with the type-relation operators and
        /// `+`) between simple atoms parses without panicking and round-trips
        /// losslessly. Guards the cons Pratt branch against corruption on
        /// arbitrary operator orderings / chain lengths, complementing the
        /// FCS-oracle shape tests in `tests/all/parser_diff_cons.rs`.
        #[test]
        fn cons_chains_are_lossless_and_panic_free(
            head in "[a-z]",
            tail in proptest::collection::vec(
                (prop_oneof![Just("::"), Just(":?"), Just(":>"), Just("+")], "[a-z]"),
                0..6,
            ),
        ) {
            let mut src = format!("let foo = {head}");
            for (op, atom) in &tail {
                src.push_str(&format!(" {op} {atom}"));
            }
            src.push('\n');
            let parse = parse(&src);
            assert_lossless(&src, &parse);
        }
    }
}

/// `(-)` — a bare parenthesised operator-value (FCS's `opName`). Emits
/// `LONG_IDENT_EXPR > LONG_IDENT > [LPAREN_TOK, IDENT_TOK("-"), RPAREN_TOK]`,
/// matching FCS's `SynExpr.LongIdent(["op_Subtraction"])` once the
/// `OriginalNotationWithParen "-"` trivia is unwrapped. The `-` is the
/// otherwise prefix-able `Op("-")`, so the operator-value path must win over
/// the prefix-application path.
#[test]
fn paren_operator_value_minus() {
    let source = "(-)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      LONG_IDENT_EXPR@0..3
        LONG_IDENT@0..3
          LPAREN_TOK@0..1 \"(\"
          IDENT_TOK@1..2 \"-\"
          RPAREN_TOK@2..3 \")\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `Checked.(-)` — a *qualified* operator-value. `mkSynDot` folds the
/// operator onto the `Checked` long-ident, so the operator becomes a
/// trailing `LONG_IDENT` segment (`( - )`) after the `.`, all inside the
/// one `LONG_IDENT_EXPR`. FCS:
/// `LongIdent(["Checked"; "op_Subtraction"])`.
#[test]
fn qualified_paren_operator_value() {
    let source = "Checked.(-)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..12
  MODULE_OR_NAMESPACE@0..12
    EXPR_DECL@0..11
      LONG_IDENT_EXPR@0..11
        LONG_IDENT@0..11
          IDENT_TOK@0..7 \"Checked\"
          DOT_TOK@7..8 \".\"
          LPAREN_TOK@8..9 \"(\"
          IDENT_TOK@9..10 \"-\"
          RPAREN_TOK@10..11 \")\"
    NEWLINE@11..12 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The motivating real-world form — a qualified operator-value applied to
/// two arguments inside a `for … to … do` loop. Before this fix the `.` of
/// `Checked.(-)` was the first parse failure. Pin that it now parses
/// error-free and round-trips.
#[test]
fn qualified_operator_value_in_for_loop() {
    let source = "let f (array: int[]) = for i = 0 to Checked.(-) array.Length 1 do ()\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
}

/// `(*)` — the glued multiply operator-value. The lexer emits a single
/// `LParenStarRParen` token for `(*)`; we split it back into
/// `[LPAREN_TOK, IDENT_TOK("*"), RPAREN_TOK]` so the operator reads as `*`,
/// matching FCS's `op_Multiply`. (Contrast the spaced `( * )`, which stays
/// the whole-dimension wildcard.)
#[test]
fn paren_operator_value_star_glued() {
    let source = "(*)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..3
      LONG_IDENT_EXPR@0..3
        LONG_IDENT@0..3
          LPAREN_TOK@0..1 \"(\"
          IDENT_TOK@1..2 \"*\"
          RPAREN_TOK@2..3 \")\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `atomicExprAfterType` argument contexts (`new T(…)`, `inherit T(…)`,
/// `[<Attr(…)>]`) exclude FCS's `identExpr: opName` alternative, so a bare
/// parenthesised operator-value is **not** a valid argument there — these are
/// FCS parse errors even though `(+)` is a fine expression elsewhere. Pin that
/// our parser still reports an error (rather than silently accepting the
/// operator-value) and stays lossless. Mirrors FCS's `ParseHadErrors = true`
/// for each. (A paren-*wrapped* value `new C((+))` *is* accepted — its head is
/// the outer `(`; covered by the differential suite.)
#[test]
fn aftertype_arg_rejects_paren_operator_value() {
    for source in [
        "new C(+)\n",
        "new C(=)\n",
        "new C(*)\n",
        "[<A(+)>]\nlet x = 1\n",
        "[<A(=)>]\nlet x = 1\n",
        "[<A(*)>]\nlet x = 1\n",
        "type T() =\n    inherit C(+)\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "expected a parse error for after-type operator-value arg {source:?}; got none",
        );
        assert_lossless(source, &parse);
    }
}

/// A measure literal with a bare measure-variable sigil and no name
/// (`1.0<'>`) is malformed — FCS's `typar` production requires an identifier
/// after `'`/`^`. We must fail loud (emit an error) rather than silently
/// accept it, while keeping the parse lossless.
#[test]
fn measure_var_sigil_without_name_errors() {
    for source in ["let x = 1.0<'>\n", "let x = 1.0<^>\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "expected a parse error for a nameless measure sigil {source:?}; got none",
        );
        assert_lossless(source, &parse);
    }
}

/// `?ident` optional named argument: a positive structural check plus the two
/// dual-stream recovery guards.
///
/// `M(?opt)` builds `LONG_IDENT_EXPR > [QMARK_TOK, LONG_IDENT > [IDENT_TOK]]`
/// (FCS's `SynExpr.LongIdent(isOptional = true, …)`). The head must *not* commit
/// when an identifier only *appears* to follow the `?`:
/// * **offside** (`?⏎opt`) — a layout virtual sits between them in the filtered
///   stream; committing would bump the virtual as the name.
/// * **swallowed closer** (`(1 + ?) opt`) — a LexFilter-swallowed `)` sits
///   between them in the raw stream; committing would drain the `)` and steal
///   `opt` from the enclosing paren.
///
/// Both must recover with an error and **no** `QMARK_TOK`.
#[test]
fn optional_named_arg_does_not_commit_across_virtual_or_swallowed_closer() {
    // Positive: the well-formed head emits a QMARK_TOK + IDENT_TOK name.
    let ok = parse("let r = M(?opt)\n");
    assert!(ok.errors.is_empty(), "errors: {:?}", ok.errors);
    assert!(
        ok.root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::QMARK_TOK),
        "the well-formed `?opt` head must emit a QMARK_TOK marker",
    );

    for source in [
        "let r =\n    ?\n    opt\n", // offside virtual between `?` and `opt`
        "let r = (1 + ?) opt\n",     // swallowed `)` between `?` and `opt`
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} must not parse as an optional-arg head",
        );
        assert!(
            parse
                .root
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .all(|t| t.kind() != SyntaxKind::QMARK_TOK),
            "{source:?}: the `?` must not commit to an optional-arg head (no QMARK_TOK)",
        );
        assert_lossless(source, &parse);
    }
}

/// Recovery: a body-trailing operator immediately before a LexFilter-swallowed
/// closer (`(1 +) x`, `{1 +} y`) has a *missing operand*, so the closer ends the
/// body and the following token stays **outside** it. Without the closer-after
/// gates the recovery treats the operator as an operator-value/app-arg and
/// drains the swallowed `)`/`}` to pull the trailing token inside the body.
///
/// Mirrors the cons case (`(a ::) y`, already guarded by
/// `peek_cons_continuation`) — checked here as a regression guard.
#[test]
fn trailing_operator_before_swallowed_closer_does_not_absorb_following_token() {
    // The trailing token (`x` / `y`) must NOT be a descendant of the body node.
    let body_absorbs = |source: &str, body_kind: SyntaxKind, trailing: &str| {
        let parse = parse(source);
        assert!(!parse.errors.is_empty(), "{source:?} should error");
        assert_lossless(source, &parse);
        let body = parse
            .root
            .descendants()
            .find(|n| n.kind() == body_kind)
            .unwrap_or_else(|| panic!("{source:?}: expected a {body_kind:?} node"));
        body.descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == trailing)
    };

    for (source, trailing) in [
        ("let r = (1 +) x\n", "x"),
        ("let r = (1 *) x\n", "x"),
        ("let r = (a |> ) z\n", "z"),
        // The address-of `&` / `&&` are dedicated tokens (not `Op`), also
        // admitted as adjacent-prefix app args — they need the guard too.
        ("let r = (f &) x\n", "x"),
        ("let r = (f &&) x\n", "x"),
        ("let r = (a ::) y\n", "y"), // regression: cons already correct
    ] {
        assert!(
            !body_absorbs(source, SyntaxKind::PAREN_EXPR, trailing),
            "{source:?}: `{trailing}` must stay outside the PAREN_EXPR",
        );
    }
    assert!(
        !body_absorbs("let r = {1 +} y\n", SyntaxKind::COMPUTATION_EXPR, "y"),
        "`{{1 +}} y`: `y` must stay outside the COMPUTATION_EXPR",
    );
}

/// Positive guard for the closer-after gates: well-formed bodies where the last
/// element legitimately precedes the closer must be unchanged — the gate keys on
/// a body-trailing *operator*, never a complete operand.
#[test]
fn trailing_operand_before_closer_still_parses() {
    // `(f x)` — `x` is the last (complete) arg, then `)`; both stay inside.
    let p = parse("let r = (f x)\n");
    assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    let paren = p
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR present");
    assert!(
        paren
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "x"),
        "`(f x)`: the complete arg `x` must stay inside the paren",
    );

    // `(1 + 2)` and `(a) + b` unchanged (no errors).
    for source in ["let r = (1 + 2)\n", "let r = (a) + b\n"] {
        let p = parse(source);
        assert!(p.errors.is_empty(), "{source:?} errors: {:?}", p.errors);
    }
}

/// FSharp.Core's static-optimization binding RHS — `mainExpr when 'T : ty =
/// branch` (`SynExpr.LibraryOnlyStaticOptimization`, `pars.fsy:3391`). The whole
/// RHS is one `STATIC_OPTIMIZATION_EXPR`: the fallthrough main expression, the
/// offside `BlockSep` before the clause (a zero-width `ERROR`), then a
/// `STATIC_OPT_WHEN_CLAUSE` of `[WHEN_TOK, STATIC_OPT_CONDITION, EQUALS_TOK,
/// <branch>]`. The condition subject is a reused `TYPAR_DECL`.
#[test]
fn static_optimization_green_shape() {
    let source = "let inline f x =\n    g x\n    when 'T : int = h x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..49
  MODULE_OR_NAMESPACE@0..49
    LET_DECL@0..48
      LET_TOK@0..3 \"let\"
      BINDING@3..48
        WHITESPACE@3..4 \" \"
        INLINE_TOK@4..10 \"inline\"
        LONG_IDENT_PAT@10..14
          LONG_IDENT@10..12
            WHITESPACE@10..11 \" \"
            IDENT_TOK@11..12 \"f\"
          NAMED_PAT@12..14
            WHITESPACE@12..13 \" \"
            IDENT_TOK@13..14 \"x\"
        WHITESPACE@14..15 \" \"
        EQUALS_TOK@15..16 \"=\"
        NEWLINE@16..17 \"\\n\"
        WHITESPACE@17..21 \"    \"
        ERROR@21..21 \"\"
        STATIC_OPTIMIZATION_EXPR@21..48
          APP_EXPR@21..24
            IDENT_EXPR@21..22
              IDENT_TOK@21..22 \"g\"
            IDENT_EXPR@22..24
              WHITESPACE@22..23 \" \"
              IDENT_TOK@23..24 \"x\"
          NEWLINE@24..25 \"\\n\"
          WHITESPACE@25..29 \"    \"
          ERROR@29..29 \"\"
          STATIC_OPT_WHEN_CLAUSE@29..48
            WHEN_TOK@29..33 \"when\"
            STATIC_OPT_CONDITION@33..42
              TYPAR_DECL@33..36
                WHITESPACE@33..34 \" \"
                QUOTE_TOK@34..35 \"'\"
                IDENT_TOK@35..36 \"T\"
              WHITESPACE@36..37 \" \"
              COLON_TOK@37..38 \":\"
              WHITESPACE@38..39 \" \"
              LONG_IDENT_TYPE@39..42
                LONG_IDENT@39..42
                  IDENT_TOK@39..42 \"int\"
            WHITESPACE@42..43 \" \"
            EQUALS_TOK@43..44 \"=\"
            APP_EXPR@44..48
              IDENT_EXPR@44..46
                WHITESPACE@44..45 \" \"
                IDENT_TOK@45..46 \"h\"
              IDENT_EXPR@46..48
                WHITESPACE@46..47 \" \"
                IDENT_TOK@47..48 \"x\"
    NEWLINE@48..49 \"\\n\"
    ERROR@49..49 \"\"
    ERROR@49..49 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A round-trip + no-error sweep over the real static-optimization shapes in
/// `prim-types.fs`: multiple clauses, `and`-chained conditions, the bare `'T
/// struct` form, a generic-type condition, inline-IL branch bodies (with infix
/// `-`), and a multi-line `if`/`else` branch. Each must parse cleanly (no
/// `ParseError`) and losslessly, with exactly one `STATIC_OPTIMIZATION_EXPR`.
#[test]
fn static_optimization_real_world_shapes_are_clean() {
    for source in [
        "let inline f x =\n    g x\n    when 'T : int = a x\n    when 'T : float = b x\n",
        "let inline f x =\n    g x\n    when 'T : int and 'U : float = h x\n",
        "let inline f v =\n    g v\n    when 'T struct =\n        match box v with\n        | _ -> 0\n",
        "let inline f x =\n    g x\n    when 'T : list<int> = h x\n",
        "let inline f x y =\n    g x y\n    when 'T : sbyte = (# \"cgt\" x y : int #) - (# \"clt\" x y : int #)\n",
        "let inline f x =\n    g x\n    when 'T : int = if a then b\n                    else c\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} produced errors: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);
        let count = parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::STATIC_OPTIMIZATION_EXPR)
            .count();
        assert_eq!(
            count, 1,
            "{source:?} should have one STATIC_OPTIMIZATION_EXPR"
        );
    }
}

/// The static-optimization repr is reachable through the typed AST: a binding
/// whose RHS `Expr` is `Expr::StaticOptimization`, exposing the main expression,
/// the clauses, and each clause's conditions (subject typar + `: ty` / bare
/// `struct`) and branch.
#[test]
fn static_optimization_reaches_ast() {
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let source = "let inline f x =\n    g x\n    when 'T : int = a x\n    when 'U struct = b x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Let(let_decl) = module.decls().next().expect("a decl") else {
        panic!("expected a LET_DECL");
    };
    let binding = let_decl.bindings().next().expect("a binding");
    let Some(Expr::StaticOptimization(so)) = binding.expr() else {
        panic!(
            "the RHS should be a static optimization, got {:?}",
            binding.expr()
        );
    };
    assert!(
        matches!(so.main_expr(), Some(Expr::App(_))),
        "main expr is the `g x` application",
    );
    let clauses: Vec<_> = so.clauses().collect();
    assert_eq!(clauses.len(), 2, "two `when` clauses");

    // Clause 1: `'T : int` — a typed condition with a real type, no `struct`.
    let c1: Vec<_> = clauses[0].conditions().collect();
    assert_eq!(c1.len(), 1);
    assert!(!c1[0].is_struct(), "`'T : int` is not the struct form");
    assert!(c1[0].ty().is_some(), "`'T : int` carries a type");
    assert_eq!(
        c1[0]
            .typar()
            .and_then(|t| t.ident())
            .map(|i| i.text().to_string()),
        Some("T".to_string()),
    );
    assert!(matches!(clauses[0].branch(), Some(Expr::App(_))));

    // Clause 2: `'U struct` — the bare struct form, no type.
    let c2: Vec<_> = clauses[1].conditions().collect();
    assert_eq!(c2.len(), 1);
    assert!(c2[0].is_struct(), "`'U struct` is the struct form");
    assert!(c2[0].ty().is_none(), "`'U struct` carries no type");
}

/// Static optimization is a `localBinding`-only production (`pars.fsy:3327`):
/// FCS allows `when 'T : ty = …` clauses *only* on `let`/`use` binding RHSs, not
/// on member methods (`memberCore`), constructors, `val`/auto-properties, or
/// computation-expression binders (`let!`/`use!`, `ceBindingCore`) — those use a
/// plain `typedSequentialExprBlock`, so a trailing `when` is a parse error. Our
/// parser must mirror that: the `when` is *not* consumed as a static
/// optimization in those contexts (no `STATIC_OPTIMIZATION_EXPR`), and the stray
/// `when` is reported, matching FCS rather than silently accepting divergent
/// syntax.
#[test]
fn static_optimization_only_on_local_bindings() {
    for source in [
        // A member method body.
        "type T() =\n    member x.M y =\n        g y\n        when 'T : int = h y\n",
        // A computation-expression `let!` binder.
        "let c = async {\n    let! x =\n        g 1\n        when 'T : int = h 1\n    return x }\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should reject the non-local-binding `when` (FCS does)",
        );
        assert!(
            !parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::STATIC_OPTIMIZATION_EXPR),
            "{source:?} must not parse a static optimization outside a local binding",
        );
        assert_lossless(source, &parse);
    }

    // A class-local `let` binding *is* a `localBinding`, so static optimization
    // is allowed there (FCS routes `classDefnBindings` through `localBindings`).
    let ok = "type T() =\n    let inline f x =\n        g x\n        when 'T : int = h x\n    member _.M = 0\n";
    let parse = parse(ok);
    assert!(
        parse.errors.is_empty(),
        "class-local let errors: {:?}",
        parse.errors
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::STATIC_OPTIMIZATION_EXPR),
        "a class-local `let inline` admits static optimization",
    );
}

/// FSharp.Core's library-only cons-cell field read `c.( :: ).0`
/// (`SynExpr.LibraryOnlyUnionCaseFieldGet`, `pars.fsy:5351`). The object stays a
/// single `IDENT_EXPR` (FCS `Ident`, not a one-segment long-ident); the
/// qualification is `[DOT_TOK, LPAREN_TOK, COLON_COLON_TOK, RPAREN_TOK (swallowed,
/// recovered), DOT_TOK, INT32_LIT]`. FCS flags it library-only (FS0042); we mirror
/// that single diagnostic over the whole expression while still building the tree.
#[test]
fn cons_field_get_green_shape() {
    let source = "let f c = c.( :: ).0\n";
    let parse = parse(source);
    assert_eq!(
        parse.errors.len(),
        1,
        "exactly the library-only diagnostic: {:?}",
        parse.errors,
    );
    assert_eq!(
        &source[parse.errors[0].span.clone()],
        "c.( :: ).0",
        "the FS0042-equivalent diagnostic spans the whole expression",
    );
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    LET_DECL@0..20
      LET_TOK@0..3 \"let\"
      BINDING@3..20
        LONG_IDENT_PAT@3..7
          LONG_IDENT@3..5
            WHITESPACE@3..4 \" \"
            IDENT_TOK@4..5 \"f\"
          NAMED_PAT@5..7
            WHITESPACE@5..6 \" \"
            IDENT_TOK@6..7 \"c\"
        WHITESPACE@7..8 \" \"
        EQUALS_TOK@8..9 \"=\"
        WHITESPACE@9..10 \" \"
        ERROR@10..10 \"\"
        LIBRARY_ONLY_FIELD_GET_EXPR@10..20
          IDENT_EXPR@10..11
            IDENT_TOK@10..11 \"c\"
          DOT_TOK@11..12 \".\"
          LPAREN_TOK@12..13 \"(\"
          WHITESPACE@13..14 \" \"
          COLON_COLON_TOK@14..16 \"::\"
          WHITESPACE@16..17 \" \"
          RPAREN_TOK@17..18 \")\"
          DOT_TOK@18..19 \".\"
          INT32_LIT@19..20 \"0\"
    NEWLINE@20..21 \"\\n\"
    ERROR@21..21 \"\"
    ERROR@21..21 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The set form `c.( :: ).1 <- t` is an `ASSIGN_EXPR` over the cons-field get
/// (the get is the LHS target; FCS's `mkSynAssign` collapses it to
/// `LibraryOnlyUnionCaseFieldSet`). Reaches the AST: the object and field number
/// are exposed off the `Expr::LibraryOnlyFieldGet` target.
#[test]
fn cons_field_set_reaches_ast() {
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let source = "let f c t = c.( :: ).1 <- t\n";
    let parse = parse(source);
    assert_eq!(
        parse.errors.len(),
        1,
        "the library-only diagnostic: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let ModuleDecl::Let(let_decl) = module.decls().next().expect("a decl") else {
        panic!("expected a LET_DECL");
    };
    let binding = let_decl.bindings().next().expect("a binding");
    let Some(Expr::Assign(assign)) = binding.expr() else {
        panic!("the RHS is an assignment, got {:?}", binding.expr());
    };
    let target = assign.target().expect("an assign target");
    let Expr::LibraryOnlyFieldGet(g) = target else {
        panic!("the assign target is a cons-field get, got {target:?}");
    };
    assert!(matches!(g.object(), Some(Expr::Ident(_))), "object is `c`");
    assert_eq!(g.field_num(), Some(1), "field number is 1");
}

/// A *signed* field number is not a cons-field access: `cons.( :: ).-1` lexes
/// the `.-` as a single operator token (not `DOT` then a sign-folded int), so
/// the `.( :: ).<int>` lookahead declines — no `LIBRARY_ONLY_FIELD_GET_EXPR`,
/// just recovery errors, matching FCS (which rejects `.-1` as FS0010, not a
/// field-get). Guards against admitting a non-`INT32` field token (and the
/// `field_num` panic that a wrongly-admitted signed token would cause).
#[test]
fn signed_cons_field_number_is_not_a_field_get() {
    for source in ["let f c = c.( :: ).-1\n", "let f c = c.( :: ).-1l\n"] {
        let parse = parse(source);
        assert!(
            !parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::LIBRARY_ONLY_FIELD_GET_EXPR),
            "{source:?} must not parse as a cons-field access",
        );
        assert!(!parse.errors.is_empty(), "{source:?} is a recovery case");
        assert_lossless(source, &parse);
    }
}
