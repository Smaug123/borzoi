use super::super::*;
use super::*;

/// Phase 5 Gap A — `let x as y = w`: minimal top-level `as`-pattern.
/// Asserts the green shape `AS_PAT > [NAMED_PAT, AS_TOK, NAMED_PAT]`.
#[test]
fn as_pat_top_level_minimal() {
    let source = "let x as y = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::As(as_pat) = pat else {
        panic!("expected AS_PAT head, got {pat:?}");
    };
    assert!(
        matches!(as_pat.lhs(), Some(crate::syntax::Pat::Named(_))),
        "as lhs should be NAMED_PAT, got {:?}",
        as_pat.lhs(),
    );
    assert!(
        matches!(as_pat.rhs(), Some(crate::syntax::Pat::Named(_))),
        "as rhs should be NAMED_PAT, got {:?}",
        as_pat.rhs(),
    );
    assert!(as_pat.as_token().is_some(), "AS_PAT must carry an AS_TOK");
}

/// Phase 5 Gap A — `let x, y as z = w`: `as` is the lowest precedence,
/// so it binds the whole tuple to its left → `AS_PAT > [TUPLE_PAT,
/// AS_TOK, NAMED_PAT]`, not `Tuple[x, As(y,z)]`.
#[test]
fn as_pat_top_level_tuple_lhs() {
    let source = "let x, y as z = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::As(as_pat) = pat else {
        panic!("expected AS_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::Tuple(tuple) = as_pat.lhs().expect("as lhs") else {
        panic!("as lhs should be TUPLE_PAT, got {:?}", as_pat.lhs());
    };
    assert_eq!(
        tuple.elements().count(),
        2,
        "tuple lhs of `as` has two elements",
    );
    assert!(
        matches!(as_pat.rhs(), Some(crate::syntax::Pat::Named(_))),
        "as rhs should be NAMED_PAT, got {:?}",
        as_pat.rhs(),
    );
}

/// Phase 5 Gap A — `let x as y as z = w`: chained `as` is left-nested
/// (`As(As(x,y),z)`), mirroring FCS's left-recursive grammar. The
/// outer AS_PAT's lhs is itself an AS_PAT.
#[test]
fn as_pat_chained() {
    let source = "let x as y as z = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::As(outer) = pat else {
        panic!("expected outer AS_PAT, got {pat:?}");
    };
    let crate::syntax::Pat::As(inner) = outer.lhs().expect("outer as lhs") else {
        panic!(
            "chained `as` must be left-nested: outer lhs should be AS_PAT, got {:?}",
            outer.lhs()
        );
    };
    assert!(
        matches!(inner.lhs(), Some(crate::syntax::Pat::Named(_)))
            && matches!(inner.rhs(), Some(crate::syntax::Pat::Named(_))),
        "inner AS_PAT should be `x as y`",
    );
    assert!(
        matches!(outer.rhs(), Some(crate::syntax::Pat::Named(_))),
        "outer as rhs should be NAMED_PAT (`z`), got {:?}",
        outer.rhs(),
    );
}

/// Phase 5 Gap A — `let (x as y) = w`: in-paren `as`. Outer head is
/// `PAREN_PAT` wrapping `AS_PAT > [NAMED_PAT, AS_TOK, NAMED_PAT]`.
#[test]
fn as_pat_paren_minimal() {
    let source = "let (x as y) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::As(as_pat) = paren.inner().expect("paren inner") else {
        panic!("expected AS_PAT inside PAREN_PAT, got {:?}", paren.inner());
    };
    assert!(
        matches!(as_pat.lhs(), Some(crate::syntax::Pat::Named(_)))
            && matches!(as_pat.rhs(), Some(crate::syntax::Pat::Named(_))),
        "paren `as` should be `x as y`",
    );
}

/// Phase 5 Gap A — `let (Some x as y) = w`: the `as` lhs is a
/// function-form `LONG_IDENT_PAT` (`Some x`), confirming the RHS-only
/// `constrPattern` restriction doesn't apply to the lhs.
#[test]
fn as_pat_paren_ctor_lhs() {
    let source = "let (Some x as y) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::As(as_pat) = paren.inner().expect("paren inner") else {
        panic!("expected AS_PAT inside PAREN_PAT, got {:?}", paren.inner());
    };
    assert!(
        matches!(as_pat.lhs(), Some(crate::syntax::Pat::LongIdent(_))),
        "as lhs should be LONG_IDENT_PAT (`Some x`), got {:?}",
        as_pat.lhs(),
    );
}

/// Phase 5 Gap A — `let (x, y as z) = w`: tuple binds tighter than
/// `as` inside parens too → `Paren(As(Tuple[x,y], z))`.
#[test]
fn as_pat_paren_tuple_lhs() {
    let source = "let (x, y as z) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::As(as_pat) = paren.inner().expect("paren inner") else {
        panic!("expected AS_PAT inside PAREN_PAT, got {:?}", paren.inner());
    };
    assert!(
        matches!(as_pat.lhs(), Some(crate::syntax::Pat::Tuple(_))),
        "as lhs should be TUPLE_PAT, got {:?}",
        as_pat.lhs(),
    );
}

/// Phase 5 Gap A — `let (x : int as y) = w`: a per-element `:` binds
/// the lhs typed-pat, then `as` wraps it → `Paren(As(Typed(x,int), y))`.
#[test]
fn as_pat_paren_typed_lhs() {
    let source = "let (x : int as y) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::As(as_pat) = paren.inner().expect("paren inner") else {
        panic!("expected AS_PAT inside PAREN_PAT, got {:?}", paren.inner());
    };
    assert!(
        matches!(as_pat.lhs(), Some(crate::syntax::Pat::Typed(_))),
        "as lhs should be TYPED_PAT, got {:?}",
        as_pat.lhs(),
    );
}

/// Phase 5 Gap A — `let (x as y : int) = w`: a trailing `:` after the
/// whole `as` wraps it → `Paren(Typed(As(x,y), int))`. The inner is a
/// TYPED_PAT whose own inner is the AS_PAT.
#[test]
fn as_pat_paren_trailing_colon() {
    let source = "let (x as y : int) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::Typed(typed) = paren.inner().expect("paren inner") else {
        panic!(
            "expected TYPED_PAT inside PAREN_PAT, got {:?}",
            paren.inner()
        );
    };
    assert!(
        matches!(typed.pat(), Some(crate::syntax::Pat::As(_))),
        "trailing-colon typed-pat should wrap an AS_PAT, got {:?}",
        typed.pat(),
    );
}

/// Phase 5 Gap A — `let x as y, z = w`: the `as`-pat is a tuple
/// *element*, not the whole head. The comma-reduce outranks the `as`
/// shift, so `x as y` reduces first → head is `TUPLE_PAT` whose first
/// element is `AS_PAT` and second is `NAMED_PAT`. (Regression for the
/// codex P2 where the one-shot tuple-then-`as` left the comma orphaned.)
#[test]
fn as_pat_top_level_as_then_comma() {
    let source = "let x as y, z = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Tuple(tuple) = pat else {
        panic!("expected TUPLE_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 2, "head tuple has two elements");
    assert!(
        matches!(elements[0], crate::syntax::Pat::As(_)),
        "first tuple element should be AS_PAT (`x as y`), got {:?}",
        elements[0],
    );
    assert!(
        matches!(elements[1], crate::syntax::Pat::Named(_)),
        "second tuple element should be NAMED_PAT (`z`), got {:?}",
        elements[1],
    );
}

/// Phase 5 Gap A — `let x, y as z, w = v`: the comma-run `x, y` reduces
/// to a flat tuple before the `as` wraps it, then a trailing comma opens
/// a fresh outer tuple → `Tuple[As(Tuple[x,y], z), w]`. Pins the
/// nested-tuple-inside-`as`-inside-tuple shape produced by re-wrapping
/// the single checkpoint in token order.
#[test]
fn as_pat_top_level_comma_as_comma() {
    let source = "let x, y as z, w = v\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Tuple(outer) = pat else {
        panic!("expected outer TUPLE_PAT head, got {pat:?}");
    };
    let outer_elems: Vec<_> = outer.elements().collect();
    assert_eq!(outer_elems.len(), 2, "outer tuple has two elements");
    let crate::syntax::Pat::As(as_pat) = &outer_elems[0] else {
        panic!(
            "first outer element should be AS_PAT, got {:?}",
            outer_elems[0]
        );
    };
    let crate::syntax::Pat::Tuple(inner) = as_pat.lhs().expect("as lhs") else {
        panic!(
            "as lhs should be the inner TUPLE_PAT (`x, y`), got {:?}",
            as_pat.lhs()
        );
    };
    assert_eq!(inner.elements().count(), 2, "inner tuple has two elements");
    assert!(
        matches!(outer_elems[1], crate::syntax::Pat::Named(_)),
        "second outer element should be NAMED_PAT (`w`), got {:?}",
        outer_elems[1],
    );
}

/// Phase 5 Gap A — `let (x as y, z) = w`: the same `as`-then-comma
/// interleave inside parens → `Paren(Tuple[As(x,y), z])`. The first
/// tuple element is the `AS_PAT`.
#[test]
fn as_pat_paren_as_then_comma() {
    let source = "let (x as y, z) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::Tuple(tuple) = paren.inner().expect("paren inner") else {
        panic!(
            "expected TUPLE_PAT inside PAREN_PAT, got {:?}",
            paren.inner()
        );
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 2, "paren tuple has two elements");
    assert!(
        matches!(elements[0], crate::syntax::Pat::As(_)),
        "first tuple element should be AS_PAT (`x as y`), got {:?}",
        elements[0],
    );
}

/// Phase 5 Gap A — `let (x as ) = w`: missing rhs after `as`. The
/// parser records an error and does not panic; the tree stays
/// lossless. We don't pin the exact recovery shape.
#[test]
fn as_pat_rhs_missing_recovers() {
    let source = "let (x as ) = w\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "missing `as` rhs should record a parse error",
    );
    assert_lossless(source, &parse);
}

/// Phase 5.2 — `fun x -> x`: the simplest single-arg lambda.
/// `FunExpr.args()` must yield exactly one `Pat::Named`; the body
/// must resolve to an identifier expression. No diagnostics.
#[test]
fn fun_single_arg_named_pat() {
    let source = "fun x -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "single-arg fun-lambda should parse cleanly, got: {:?}",
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
    let crate::syntax::Expr::Fun(fun_expr) = expr_decl.expr().expect("expr decl has an expr")
    else {
        panic!("expected FunExpr, got {:?}", expr_decl.expr());
    };
    let args: Vec<_> = fun_expr.args().collect();
    assert_eq!(args.len(), 1, "expected 1 arg, got {args:#?}");
    let crate::syntax::Pat::Named(named) = &args[0] else {
        panic!("expected Named pat for `x`, got {:?}", args[0]);
    };
    assert_eq!(
        named.ident().expect("named pat has ident").text(),
        "x",
        "arg name should be `x`"
    );
    let body = fun_expr.body().expect("FunExpr.body");
    assert!(
        matches!(body, crate::syntax::Expr::Ident(_)),
        "body should be an identifier expression, got {body:?}",
    );
}

/// Phase 6 — `let [x; y] = z`: minimal list pattern. Green shape
/// `ARRAY_OR_LIST_PAT > [LBRACK_TOK, NAMED_PAT, SEMI_TOK, NAMED_PAT,
/// RBRACK_TOK]`; `is_array() == false`, two `NAMED_PAT` elements, no
/// errors.
#[test]
fn list_pat_minimal() {
    use crate::syntax::AstNode;
    let source = "let [x; y] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    assert!(!arr.is_array(), "`[…]` is a list, not an array");
    let elements: Vec<_> = arr.elements().collect();
    assert_eq!(elements.len(), 2, "two elements, got {elements:?}");
    assert!(
        elements
            .iter()
            .all(|e| matches!(e, crate::syntax::Pat::Named(_))),
        "both elements should be NAMED_PAT, got {elements:?}",
    );
    assert_eq!(count_tok(arr.syntax(), SyntaxKind::LBRACK_TOK), 1);
    assert_eq!(count_tok(arr.syntax(), SyntaxKind::SEMI_TOK), 1);
    assert_eq!(count_tok(arr.syntax(), SyntaxKind::RBRACK_TOK), 1);
}

/// Phase 6 — `let [| a; b |] = arr`: minimal array pattern. The
/// delimiters are `LBRACK_BAR_TOK` / `BAR_RBRACK_TOK`, so
/// `is_array() == true`.
#[test]
fn array_pat_minimal() {
    use crate::syntax::AstNode;
    let source = "let [| a; b |] = arr\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    assert!(arr.is_array(), "`[| … |]` is an array");
    assert_eq!(arr.elements().count(), 2, "two elements");
    assert_eq!(count_tok(arr.syntax(), SyntaxKind::LBRACK_BAR_TOK), 1);
    assert_eq!(count_tok(arr.syntax(), SyntaxKind::BAR_RBRACK_TOK), 1);
}

/// Phase 6 — `let [] = z`: an empty list pattern is valid (no
/// "expected element" error, unlike anon-record types). Zero
/// elements.
#[test]
fn list_pat_empty() {
    let source = "let [] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "empty list pattern should parse cleanly, got: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    assert!(!arr.is_array());
    assert_eq!(arr.elements().count(), 0, "empty list has no elements");
}

/// Phase 6 — `let [||] = z`: an empty array pattern. Confirms `[||]`
/// lexes as `LBrackBar` + `BarRBrack` and that empty is valid.
#[test]
fn array_pat_empty() {
    let source = "let [||] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "empty array pattern should parse cleanly, got: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    assert!(arr.is_array(), "`[||]` is an empty array");
    assert_eq!(arr.elements().count(), 0, "empty array has no elements");
}

/// Phase 6 — `let [x] = z`: a single-element list.
#[test]
fn list_pat_single() {
    let source = "let [x] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = arr.elements().collect();
    assert_eq!(elements.len(), 1, "one element, got {elements:?}");
    assert!(matches!(elements[0], crate::syntax::Pat::Named(_)));
}

/// Phase 6 — `let [a, b] = z`: the comma builds a tuple *within* the
/// single element, so the list has ONE `TUPLE_PAT` element — not two.
/// `;` is the element separator; `,` is consumed into the element.
#[test]
fn list_pat_tuple_element() {
    let source = "let [a, b] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = arr.elements().collect();
    assert_eq!(
        elements.len(),
        1,
        "`[a, b]` is a one-element list (the element is a tuple), got {elements:?}",
    );
    let crate::syntax::Pat::Tuple(tuple) = &elements[0] else {
        panic!(
            "the sole element should be TUPLE_PAT, got {:?}",
            elements[0]
        );
    };
    assert_eq!(tuple.elements().count(), 2, "tuple element has two parts");
}

/// A *repeated* element separator inside a list/array pattern is invalid. FCS's
/// `seps` is a single separator group, so `let [a; ; b] = z` and
/// `let [| a; ; b |] = z` are parse errors (`ParseHadErrors: true`, verified
/// against `fcs-dump ast`). The parser consumes exactly one group per gap, so
/// the stray second `;` trips the element parser's recovery — pinning that we
/// do *not* silently accept the malformed run. A single/trailing `;` and offside
/// (`BlockSep`) separation stay valid.
#[test]
fn list_array_pat_repeated_separator_errors() {
    for source in ["let [a; ; b] = z\n", "let [| a; ; b |] = z\n"] {
        let parse = parse(source);
        assert_lossless(source, &parse);
        assert!(
            !parse.errors.is_empty(),
            "a repeated list/array-pattern separator in {source:?} must record a parse error",
        );
    }
}

/// Phase 6 — `let [x; y; ] = z`: a trailing separator before the close
/// is tolerated with no error.
#[test]
fn list_pat_trailing_semi() {
    let source = "let [x; y; ] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "trailing `;` should be tolerated, got: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    assert_eq!(
        arr.elements().count(),
        2,
        "trailing `;` adds no phantom element",
    );
}

/// Phase 6 — `let [[x]; y] = z`: a nested list pattern. The first
/// element is itself an `ARRAY_OR_LIST_PAT`.
#[test]
fn list_pat_nested() {
    let source = "let [[x]; y] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = arr.elements().collect();
    assert_eq!(elements.len(), 2, "two elements, got {elements:?}");
    let crate::syntax::Pat::ArrayOrList(inner) = &elements[0] else {
        panic!(
            "first element should be a nested ARRAY_OR_LIST_PAT, got {:?}",
            elements[0]
        );
    };
    assert_eq!(inner.elements().count(), 1, "inner list has one element");
}

/// Phase 6 — `let [Some x; None] = z`: each element is an applPat, so a
/// ctor-application element projects to `LONG_IDENT_PAT`.
#[test]
fn list_pat_ctor_element() {
    let source = "let [Some x; None] = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = arr.elements().collect();
    assert_eq!(elements.len(), 2, "two elements, got {elements:?}");
    assert!(
        elements
            .iter()
            .all(|e| matches!(e, crate::syntax::Pat::LongIdent(_))),
        "both elements should be LONG_IDENT_PAT, got {elements:?}",
    );
}

/// Phase 6 — `let f [x] = x`: a bracket arg promotes the head to
/// function form, with the list pattern as the single curried arg.
#[test]
fn list_pat_function_form_arg() {
    let source = "let f [x] = x\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT (function form), got {pat:?}");
    };
    let args: Vec<_> = li.args().collect();
    assert_eq!(args.len(), 1, "one curried arg, got {args:?}");
    let crate::syntax::Pat::ArrayOrList(arr) = &args[0] else {
        panic!("arg #0 should be ARRAY_OR_LIST_PAT, got {:?}", args[0]);
    };
    assert_eq!(arr.elements().count(), 1);
}

/// Phase 6 — `fun [x] -> x`: a list pattern as a lambda arg. The arg
/// surface (`try_emit_atomic_pat` → `parse_array_or_list_pat`) accepts
/// a single bracket arg with no errors.
#[test]
fn list_pat_lambda_arg() {
    use crate::syntax::AstNode;
    let source = "fun [x] -> x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "list-pattern lambda arg should parse cleanly, got: {:?}",
        parse.errors,
    );

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
    assert_eq!(args.len(), 1, "one list-pattern arg, got {args:?}");
    assert!(
        matches!(&args[0], crate::syntax::Pat::ArrayOrList(_)),
        "arg #0 should be ARRAY_OR_LIST_PAT, got {:?}",
        args[0],
    );
}

/// Phase 6.5 — `let { X = a; M.Y = b } = r`: record pattern via the
/// facade. Two fields in source order; the first name is single-segment
/// `X`, the second qualified `M.Y`; each value is a `NAMED_PAT`. Pins the
/// `RecordPat::fields` / `RecordPatField::name`/`pat` accessors and that
/// the swallowed `}` is reclaimed (lossless, no errors).
#[test]
fn record_pat_via_accessors() {
    let source = "let { X = a; M.Y = b } = r\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Record(rec) = pat else {
        panic!("expected RECORD_PAT head, got {pat:?}");
    };
    let fields: Vec<_> = rec.fields().collect();
    assert_eq!(fields.len(), 2, "two fields, got {fields:?}");

    let name0: Vec<String> = fields[0]
        .name()
        .expect("field 0 name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(name0, vec!["X".to_string()]);
    assert!(
        matches!(fields[0].pat(), Some(crate::syntax::Pat::Named(_))),
        "field 0 value should be NAMED_PAT, got {:?}",
        fields[0].pat(),
    );

    let name1: Vec<String> = fields[1]
        .name()
        .expect("field 1 name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(name1, vec!["M".to_string(), "Y".to_string()]);
    assert!(
        matches!(fields[1].pat(), Some(crate::syntax::Pat::Named(_))),
        "field 1 value should be NAMED_PAT, got {:?}",
        fields[1].pat(),
    );
}

/// Phase 6.5 — full green-tree shape pin for `let { X = a } = r`. The
/// `RECORD_PAT` wraps `LBRACE_TOK`, one `RECORD_PAT_FIELD` (`LONG_IDENT`
/// name, `EQUALS_TOK`, value `NAMED_PAT`), and the reclaimed `RBRACE_TOK`.
#[test]
fn record_pat_tree_shape() {
    let source = "let { X = a } = r\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    LET_DECL@0..17
      LET_TOK@0..3 \"let\"
      BINDING@3..17
        RECORD_PAT@3..13
          WHITESPACE@3..4 \" \"
          LBRACE_TOK@4..5 \"{\"
          RECORD_PAT_FIELD@5..11
            WHITESPACE@5..6 \" \"
            LONG_IDENT@6..7
              IDENT_TOK@6..7 \"X\"
            WHITESPACE@7..8 \" \"
            EQUALS_TOK@8..9 \"=\"
            NAMED_PAT@9..11
              WHITESPACE@9..10 \" \"
              IDENT_TOK@10..11 \"a\"
          WHITESPACE@11..12 \" \"
          RBRACE_TOK@12..13 \"}\"
        WHITESPACE@13..14 \" \"
        EQUALS_TOK@14..15 \"=\"
        WHITESPACE@15..16 \" \"
        ERROR@16..16 \"\"
        IDENT_EXPR@16..17
          IDENT_TOK@16..17 \"r\"
    NEWLINE@17..18 \"\\n\"
    ERROR@18..18 \"\"
    ERROR@18..18 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 6.5 — an empty record pattern `{ }` is a parse *error*: FCS has
/// no empty-record production and reaches an empty `SynPat.Record` only
/// via its `LBRACE error rbrace` recovery rule (unlike `[]`/`[||]`, which
/// are valid). The parser must diagnose it (and stay lossless) rather than
/// silently accepting it.
#[test]
fn record_pat_empty_errors() {
    let source = "let { } = r\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an empty record pattern `{{ }}` must record a parse error",
    );
}

/// A *repeated* field separator inside a record pattern is invalid. FCS's
/// `seps_block` is a single separator group, so `let { F = a; ; G = b } = r` is
/// a parse error (`ParseHadErrors: true`, verified against `fcs-dump ast`). The
/// parser consumes exactly one group per gap, so the stray second `;` trips the
/// field parser's recovery — pinning that we do *not* silently accept the
/// malformed run. A single separator and the `}`-on-own-line layout stay valid.
#[test]
fn record_pat_repeated_separator_errors() {
    let source = "let { F = a; ; G = b } = r\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a repeated record-pattern separator must record a parse error",
    );
}

/// Phase 6.6 — `let (:? int) = x`: the IsInst pattern via the facade.
/// The binding head is a `PAREN_PAT` wrapping an `IS_INST_PAT`; the
/// `IsInstPat::ty()` accessor returns the tested `LONG_IDENT_TYPE`.
#[test]
fn isinst_pat_via_accessors() {
    let source = "let (:? int) = x\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT inner");
    let crate::syntax::Pat::IsInst(isinst) = inner else {
        panic!("expected IS_INST_PAT inner, got {inner:?}");
    };
    let ty = isinst.ty().expect("IS_INST_PAT must contain a tested type");
    assert!(
        matches!(ty, crate::syntax::Type::LongIdent(_)),
        "tested type should be LONG_IDENT_TYPE, got {ty:?}",
    );
    use crate::syntax::AstNode;
    assert_eq!(ty.syntax().text().to_string().trim(), "int");
}

/// Phase 6.6 — full green-tree shape pin for `let (:? int) = x`. The
/// `PAREN_PAT` wraps `LPAREN_TOK`, the `IS_INST_PAT` (`COLON_QMARK_TOK`
/// + `LONG_IDENT_TYPE`), and the reclaimed `RPAREN_TOK`.
#[test]
fn isinst_pat_tree_shape() {
    let source = "let (:? int) = x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..17
  MODULE_OR_NAMESPACE@0..17
    LET_DECL@0..16
      LET_TOK@0..3 \"let\"
      BINDING@3..16
        PAREN_PAT@3..12
          WHITESPACE@3..4 \" \"
          LPAREN_TOK@4..5 \"(\"
          IS_INST_PAT@5..11
            COLON_QMARK_TOK@5..7 \":?\"
            LONG_IDENT_TYPE@7..11
              LONG_IDENT@7..11
                WHITESPACE@7..8 \" \"
                IDENT_TOK@8..11 \"int\"
          RPAREN_TOK@11..12 \")\"
        WHITESPACE@12..13 \" \"
        EQUALS_TOK@13..14 \"=\"
        WHITESPACE@14..15 \" \"
        ERROR@15..15 \"\"
        IDENT_EXPR@15..16
          IDENT_TOK@15..16 \"x\"
    NEWLINE@16..17 \"\\n\"
    ERROR@17..17 \"\"
    ERROR@17..17 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 6.6 — a `:?` with no following type is a parse error (FCS's
/// `COLON_QMARK recover` / bare `COLON_QMARK` arms, which yield
/// `IsInst(FromParseError)`). The parser must diagnose it and stay
/// lossless rather than panic or silently accept it.
#[test]
fn isinst_pat_missing_type_errors() {
    let source = "match x with :? -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a `:?` with no following type must record a parse error",
    );
}

/// The optional-value pattern `?x` via the facade — `let f (?x) = x`. The
/// binding head is a function-form `LONG_IDENT_PAT` whose single arg is a
/// `PAREN_PAT` wrapping an `OPTIONAL_VAL_PAT`; the `OptionalValPat::ident`
/// accessor returns the named `IDENT_TOK` (`x`).
#[test]
fn optional_val_pat_via_accessors() {
    let source = "let f (?x) = x\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let arg = long.args().next().expect("head must have one arg");
    let crate::syntax::Pat::Paren(paren) = arg else {
        panic!("expected PAREN_PAT arg, got {arg:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT inner");
    let crate::syntax::Pat::OptionalVal(opt) = inner else {
        panic!("expected OPTIONAL_VAL_PAT inner, got {inner:?}");
    };
    let ident = opt
        .ident()
        .expect("OPTIONAL_VAL_PAT must contain an IDENT_TOK");
    assert_eq!(ident.text(), "x");
}

/// Full green-tree shape pin for `let f (?x) = x`. The `OPTIONAL_VAL_PAT`
/// holds the `QMARK_TOK` sigil and the named `IDENT_TOK`, nested under the
/// `PAREN_PAT` argument of the function-form binding head.
#[test]
fn optional_val_pat_tree_shape() {
    let source = "let f (?x) = x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..15
  MODULE_OR_NAMESPACE@0..15
    LET_DECL@0..14
      LET_TOK@0..3 \"let\"
      BINDING@3..14
        LONG_IDENT_PAT@3..10
          LONG_IDENT@3..5
            WHITESPACE@3..4 \" \"
            IDENT_TOK@4..5 \"f\"
          PAREN_PAT@5..10
            WHITESPACE@5..6 \" \"
            LPAREN_TOK@6..7 \"(\"
            OPTIONAL_VAL_PAT@7..9
              QMARK_TOK@7..8 \"?\"
              IDENT_TOK@8..9 \"x\"
            RPAREN_TOK@9..10 \")\"
        WHITESPACE@10..11 \" \"
        EQUALS_TOK@11..12 \"=\"
        WHITESPACE@12..13 \" \"
        ERROR@13..13 \"\"
        IDENT_EXPR@13..14
          IDENT_TOK@13..14 \"x\"
    NEWLINE@14..15 \"\\n\"
    ERROR@15..15 \"\"
    ERROR@15..15 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// `(?)` is the parenthesised `op_Dynamic` operator name (FCS's `opName: QMARK`
/// → `SynPat.Named(op_Dynamic, OriginalNotationWithParen "?")`), *not* a
/// malformed optional-value pattern: `?` only forms an `OPTIONAL_VAL_PAT` when an
/// identifier follows (`?x`). Ground-truthed via `fcs-dump`: `let f (?) = x`
/// parses with **no** diagnostics, with the arg the `op_Dynamic` operator value.
/// (This case used to be a clean error before operator-value patterns were
/// modelled; it is now FCS-faithful. Differential coverage:
/// `diff_ast_let_dynamic_operator_arg`.)
#[test]
fn paren_question_is_dynamic_operator_value() {
    let source = "let f (?) = x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    // The single arg is the `op_Dynamic` operator value — a NAMED_PAT whose
    // IDENT_TOK is the bare `?` (the parens are sibling LPAREN/RPAREN notation).
    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = long.args().collect();
    assert_eq!(args.len(), 1, "head should have one arg, got {args:?}");
    let crate::syntax::Pat::Named(named) = &args[0] else {
        panic!("arg should be the op_Dynamic Named, got {:?}", args[0]);
    };
    assert_eq!(named.ident().expect("NAMED_PAT ident").text(), "?");
}

/// The `op_Dynamic` operator value `(?)` as a curried argument does not swallow
/// the following argument — `let f (?) y = y` has *two* args: `Named "?"`
/// (op_Dynamic) and a separate `Named y`. `consume_paren_op_value` consumes the
/// LexFilter-swallowed `)` itself, so `y` survives as the second argument rather
/// than being pulled across the closer. FCS parses this with no diagnostics
/// (ground-truthed via `fcs-dump`).
#[test]
fn paren_question_operator_arg_keeps_next_arg() {
    let source = "let f (?) y = y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    // Two curried args: the `op_Dynamic` Named `(?)` and a separate `Named y`.
    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = long.args().collect();
    assert_eq!(
        args.len(),
        2,
        "head should have two curried args, got {args:?}"
    );
    let crate::syntax::Pat::Named(dynamic) = &args[0] else {
        panic!(
            "first arg should be the op_Dynamic Named, got {:?}",
            args[0]
        );
    };
    assert_eq!(dynamic.ident().expect("NAMED_PAT ident").text(), "?");
    let crate::syntax::Pat::Named(named) = &args[1] else {
        panic!("second arg should be Named y, got {:?}", args[1]);
    };
    assert_eq!(
        named.ident().expect("NAMED_PAT ident").text(),
        "y",
        "the post-closer `y` must survive as the second argument"
    );
}

/// `(or)` is the parenthesised ML-compat boolean operator name — FCS's historic
/// `operatorName: OR { "or" }` — *not* a malformed pattern. It binds exactly like
/// any other `( op )` head (e.g. `(&)`, `(?)`): `let (or) e1 e2 = …` is a
/// function definition whose head name is `or` and whose curried args are `e1`,
/// `e2`. The `or` token lexes to the `Token::Or` keyword, so the only thing
/// distinguishing it from `(&)` (already modelled) is that keyword's admission as
/// a paren operator name (`is_paren_operator_name`).
///
/// We accept it as part of the permissive union surface: it is real, shipped
/// FSharp.Core source (via SourceLink) and parses cleanly under FsAutoComplete
/// (which only *warns* FS0086 "should not normally be redefined" at the semantic
/// layer). Recent FCS removed `OR` from its grammar, so this is a deliberate,
/// documented divergence — there is no differential coverage because FCS errors.
#[test]
fn paren_or_operator_binding_head() {
    let source = "let (or) e1 e2 = e1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    // Head is the `or` operator name with two curried args — `LONG_IDENT_PAT`.
    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let name: Vec<String> = long
        .head()
        .expect("operator head ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(name, vec!["or".to_string()], "head name should be `or`");
    let args: Vec<_> = long.args().collect();
    assert_eq!(
        args.len(),
        2,
        "head should have two curried args, got {args:?}"
    );
    for (arg, want) in args.iter().zip(["e1", "e2"]) {
        let crate::syntax::Pat::Named(named) = arg else {
            panic!("arg should be a Named, got {arg:?}");
        };
        assert_eq!(named.ident().expect("NAMED_PAT ident").text(), want);
    }
}

/// Full-fidelity guard for the two-token range-step operator name
/// (`op_RangeStep`): a comment *between* the two `..` of `let (.. (*c*) ..) a b =
/// a` stays an ordinary [`SyntaxKind::BLOCK_COMMENT`] trivia token *inside* the
/// [`SyntaxKind::RANGE_STEP_OP`] node — not absorbed into a merged operator leaf.
/// So `is_trivia()`/token-at-offset inside the comment behave correctly, and the
/// node wraps exactly two `DOT_DOT_TOK` leaves around the preserved trivia.
#[test]
fn range_step_operator_preserves_inter_dot_comment_as_trivia() {
    let source = "let (.. (*c*) ..) a b = a\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == crate::syntax::SyntaxKind::RANGE_STEP_OP)
        .expect("tree should contain a RANGE_STEP_OP node");
    let kinds: Vec<_> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| t.kind())
        .collect();
    assert_eq!(
        kinds,
        vec![
            crate::syntax::SyntaxKind::DOT_DOT_TOK,
            crate::syntax::SyntaxKind::WHITESPACE,
            crate::syntax::SyntaxKind::BLOCK_COMMENT,
            crate::syntax::SyntaxKind::WHITESPACE,
            crate::syntax::SyntaxKind::DOT_DOT_TOK,
        ],
        "the comment must survive as trivia between the two `..` leaves",
    );
    assert!(
        crate::syntax::SyntaxKind::BLOCK_COMMENT.is_trivia(),
        "BLOCK_COMMENT is trivia, so token-at-offset in the comment is not the operator",
    );
}

/// A nullary `let (or) = z` binds the bare `or` operator value — the singleton
/// `NAMED_PAT` form (FCS's `atomicPattern: atomicPatternLongIdent` →
/// `SynPat.Named`), identical to `let (&) = z` modulo the operator spelling.
#[test]
fn paren_or_nullary_binding() {
    let source = "let (or) = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Named(named) = pat else {
        panic!("expected NAMED_PAT head, got {pat:?}");
    };
    assert_eq!(named.ident().expect("NAMED_PAT ident").text(), "or");
}

/// The `or` operator value `(or)` as a curried argument does not swallow the
/// following argument — `let f (or) y = y` has *two* args: `Named "or"` and a
/// separate `Named y`. `consume_paren_op_value` consumes the LexFilter-swallowed
/// `)` itself, so `y` survives (mirrors the `(?)` op_Dynamic case).
#[test]
fn paren_or_operator_arg_keeps_next_arg() {
    let source = "let f (or) y = y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = long.args().collect();
    assert_eq!(
        args.len(),
        2,
        "head should have two curried args, got {args:?}"
    );
    let crate::syntax::Pat::Named(or_op) = &args[0] else {
        panic!(
            "first arg should be the `or` operator Named, got {:?}",
            args[0]
        );
    };
    assert_eq!(or_op.ident().expect("NAMED_PAT ident").text(), "or");
    let crate::syntax::Pat::Named(named) = &args[1] else {
        panic!("second arg should be Named y, got {:?}", args[1]);
    };
    assert_eq!(
        named.ident().expect("NAMED_PAT ident").text(),
        "y",
        "the post-closer `y` must survive as the second argument",
    );
}

/// `(or)` in expression position is the bare operator value — FCS's historic
/// `identExpr: opName` → a single-segment `SynExpr.LongIdent`. `let x = (or)`
/// parses cleanly with the RHS a `LONG_IDENT_EXPR` whose ident is `or`.
#[test]
fn paren_or_operator_value_expr() {
    let source = "let x = (or)\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::LONG_IDENT_EXPR),
        "RHS should be a LONG_IDENT_EXPR operator value",
    );
    let has_or = parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "or");
    assert!(has_or, "the operator value should carry an `or` IDENT_TOK");
}

/// Phase 6.6 — a `:?` whose type is pushed onto the next line *inside a
/// list pattern* (`[ :?⏎    int ]`) parks a `Virtual::BlockSep` between
/// the `:?` and the type on the filtered stream. The type-start gate must
/// reject that layout virtual (a raw-stream-only peek would skip it, find
/// the `int`, and dispatch into `parse_atomic_type` with the cursor still
/// on the virtual → its `unreachable!` arm). FCS rejects this offside form
/// too ("Incomplete structured construct"); we must record an error and
/// stay lossless rather than panic.
#[test]
fn isinst_pat_list_element_offside_recovers() {
    let source = "match x with\n| [ :?\n    int ] -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an offside `:?` type inside a list pattern must record a parse error",
    );
}

/// Phase 6.6 — a `:?` whose type is pushed onto an offside line that
/// *closes the match-clause context* (`match x with :?⏎  int -> 1`) leaves
/// a `Virtual::End` (not a `BlockSep`/`BlockEnd`) at the filtered cursor
/// while the raw lookahead still sees `int`. The type-start gate must
/// reject *any* virtual at the cursor, not just the layout-separator ones,
/// or `parse_atomic_type` dispatches onto the `Virtual::End` and panics.
/// FCS rejects this too ("Expecting type"); we must record an error and
/// stay lossless rather than panic.
#[test]
fn isinst_pat_offside_clause_close_recovers() {
    let source = "match x with :?\n  int -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a `:?` whose type is offside past the clause close must record a parse error",
    );
}

/// Phase 6.7 — `let h :: t = z`: a cons pattern via the facade. The
/// binding head is a `LIST_CONS_PAT` whose `lhs`/`rhs` are the head/tail
/// `NAMED_PAT`s; the `::` token is exposed via `cons_token`.
#[test]
fn cons_pat_via_accessors() {
    let source = "let h :: t = z\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ListCons(cons) = pat else {
        panic!("expected LIST_CONS_PAT head, got {pat:?}");
    };
    assert!(
        matches!(cons.lhs(), Some(crate::syntax::Pat::Named(_))),
        "lhs should be NAMED_PAT, got {:?}",
        cons.lhs()
    );
    assert!(
        matches!(cons.rhs(), Some(crate::syntax::Pat::Named(_))),
        "rhs should be NAMED_PAT, got {:?}",
        cons.rhs()
    );
    assert_eq!(
        cons.cons_token().map(|t| t.text().to_string()),
        Some("::".to_string()),
    );
}

/// Phase 6.7 — `let a :: b :: c = z`: right-associativity is reflected in
/// the green tree — the outer `LIST_CONS_PAT`'s `rhs` is itself a
/// `LIST_CONS_PAT` (`ListCons(a, ListCons(b, c))`), not left-nested.
#[test]
fn cons_pat_right_assoc_shape() {
    let source = "let a :: b :: c = z\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ListCons(outer) = pat else {
        panic!("expected outer LIST_CONS_PAT, got {pat:?}");
    };
    assert!(
        matches!(outer.lhs(), Some(crate::syntax::Pat::Named(_))),
        "outer lhs should be NAMED_PAT (a), got {:?}",
        outer.lhs()
    );
    let crate::syntax::Pat::ListCons(inner) = outer.rhs().expect("outer rhs") else {
        panic!("outer rhs should be a nested LIST_CONS_PAT (right-assoc)");
    };
    assert!(matches!(inner.lhs(), Some(crate::syntax::Pat::Named(_))));
    assert!(matches!(inner.rhs(), Some(crate::syntax::Pat::Named(_))));
}

/// Phase 6.7 — full green-tree shape pin for `let h :: t = z`. The
/// `LIST_CONS_PAT` wraps the head `NAMED_PAT`, `COLON_COLON_TOK`, and the
/// tail `NAMED_PAT`.
#[test]
fn cons_pat_tree_shape() {
    let source = "let h :: t = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..15
  MODULE_OR_NAMESPACE@0..15
    LET_DECL@0..14
      LET_TOK@0..3 \"let\"
      BINDING@3..14
        LIST_CONS_PAT@3..10
          NAMED_PAT@3..5
            WHITESPACE@3..4 \" \"
            IDENT_TOK@4..5 \"h\"
          WHITESPACE@5..6 \" \"
          COLON_COLON_TOK@6..8 \"::\"
          NAMED_PAT@8..10
            WHITESPACE@8..9 \" \"
            IDENT_TOK@9..10 \"t\"
        WHITESPACE@10..11 \" \"
        EQUALS_TOK@11..12 \"=\"
        WHITESPACE@12..13 \" \"
        ERROR@13..13 \"\"
        IDENT_EXPR@13..14
          IDENT_TOK@13..14 \"z\"
    NEWLINE@14..15 \"\\n\"
    ERROR@15..15 \"\"
    ERROR@15..15 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 6.7 — a `::` with no following pattern (`h :: -> 1`) records an
/// error (FCS's `parenPattern COLON_COLON` recovery arm) and stays
/// lossless, without panicking. The climb only recurses on a successful
/// `emit_pat_atom`, so the missing rhs is a clean bail.
#[test]
fn cons_pat_missing_rhs_recovers() {
    let source = "match v with h :: -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a `::` with no following pattern must record a parse error",
    );
}

/// Phase 6.7 — the cons rhs pushed onto an offside line that closes the
/// clause context (`h ::⏎  t`) parks a `Virtual::End` at the filtered
/// cursor after `::`. `emit_pat_atom` gates on `is_atomic_pat_start`
/// (which rejects virtuals), so the climb bails cleanly rather than
/// dispatching onto the virtual — no panic, error recorded, lossless.
#[test]
fn cons_pat_offside_rhs_recovers() {
    let source = "match v with h ::\n  t -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an offside `::` rhs past the clause close must record a parse error",
    );
}

/// An offside `::` rhs inside a list pattern (`[ h ::⏎    t ]`) is a **valid**
/// continuation: a trailing infix `::` opens a fresh block for its rhs (the
/// lex-filter's "r.h.s. of an infix token begins a new block" rule —
/// `Filter::infix_rhs_pushes`), so `t` continues the cons rather than starting a
/// new statement. FCS accepts it; the cons pattern `h :: t` is parsed with no
/// error. (Was a deferred limitation that parked a `Virtual::BlockSep` after the
/// `::` and bailed — superseded once trailing-infix continuation landed.)
#[test]
fn cons_pat_list_offside_rhs_continues() {
    let source = "match v with\n| [ h ::\n    t ] -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "an offside `::` rhs inside a list pattern continues the cons, no error: {:?}",
        parse.errors,
    );
}

/// Phase 6.7 — a `::` whose tail is missing before a LexFilter-swallowed
/// `)` (`let (h ::) next = z`). The `)` is gone from the filtered stream,
/// so the filtered cursor after `::` is already at `next`; the cons rhs
/// must **not** consume `next` (it belongs to the enclosing binding). The
/// rhs emit is gated on the *raw* next token, which surfaces the swallowed
/// `)` — so the `LIST_CONS_PAT` has no rhs, the `)` closes the paren, and
/// an error is recorded. Mirrors the swallowed-`)` discipline of the
/// function-form promotion gate.
#[test]
fn cons_pat_swallowed_close_recovers() {
    use crate::syntax::AstNode;
    let source = "let (h ::) next = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a `::` with no tail before `)` must record a parse error",
    );
    let cons = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LIST_CONS_PAT)
        .and_then(crate::syntax::ListConsPat::cast)
        .expect("a LIST_CONS_PAT node");
    assert!(
        cons.rhs().is_none(),
        "the cons tail must not consume the token past the swallowed `)`; got {:?}",
        cons.rhs(),
    );
}

/// Phase 6.8 — `let a & b = z`: a conjunction pattern via the facade. The
/// binding head is an `ANDS_PAT` whose `operands` are the two `NAMED_PAT`s, in
/// source order.
#[test]
fn ands_pat_via_accessors() {
    let source = "let a & b = z\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Ands(ands) = pat else {
        panic!("expected ANDS_PAT head, got {pat:?}");
    };
    let operands: Vec<_> = ands.operands().collect();
    assert_eq!(operands.len(), 2, "two operands, got {operands:?}");
    assert!(
        operands
            .iter()
            .all(|p| matches!(p, crate::syntax::Pat::Named(_))),
        "both operands should be NAMED_PAT, got {operands:?}",
    );
}

/// Phase 6.8 — `let a & b & c = z`: the `Ands` list is *flat* (n-ary) — three
/// `NAMED_PAT` operands under one `ANDS_PAT`, not nested.
#[test]
fn ands_pat_flat_three() {
    let source = "let a & b & c = z\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Ands(ands) = pat else {
        panic!("expected ANDS_PAT head, got {pat:?}");
    };
    let operands: Vec<_> = ands.operands().collect();
    assert_eq!(
        operands.len(),
        3,
        "flat Ands of three operands, got {operands:?}",
    );
}

/// Phase 6.8 — full green-tree shape pin for `let a & b = z`. The `ANDS_PAT`
/// wraps the two operand `NAMED_PAT`s with the `AMP_TOK` between them.
#[test]
fn ands_pat_tree_shape() {
    let source = "let a & b = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..14
  MODULE_OR_NAMESPACE@0..14
    LET_DECL@0..13
      LET_TOK@0..3 \"let\"
      BINDING@3..13
        ANDS_PAT@3..9
          NAMED_PAT@3..5
            WHITESPACE@3..4 \" \"
            IDENT_TOK@4..5 \"a\"
          WHITESPACE@5..6 \" \"
          AMP_TOK@6..7 \"&\"
          NAMED_PAT@7..9
            WHITESPACE@7..8 \" \"
            IDENT_TOK@8..9 \"b\"
        WHITESPACE@9..10 \" \"
        EQUALS_TOK@10..11 \"=\"
        WHITESPACE@11..12 \" \"
        ERROR@12..12 \"\"
        IDENT_EXPR@12..13
          IDENT_TOK@12..13 \"z\"
    NEWLINE@13..14 \"\\n\"
    ERROR@14..14 \"\"
    ERROR@14..14 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 6.8 — an `&` with no following operand before a LexFilter-swallowed
/// `)` (`let (a &) next = z`). The `&` continuation goes through the
/// raw-stream-gated `emit_pat_atom`, so it bails cleanly (no extra operand,
/// the `)` closes the paren) and does not consume `next` — the same
/// swallowed-`)` discipline as the `::`/`,` rhs.
#[test]
fn ands_pat_swallowed_close_recovers() {
    let source = "let (a &) next = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an `&` with no operand before `)` must record a parse error",
    );
}

/// Phase 6.9 — `let A | B = z`: an or-pattern via the facade. The binding head
/// is an `OR_PAT` whose `lhs`/`rhs` are the two branch patterns; the `|` token
/// is exposed via `bar_token`.
#[test]
fn or_pat_via_accessors() {
    let source = "let A | B = z\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Or(or) = pat else {
        panic!("expected OR_PAT head, got {pat:?}");
    };
    // `A`/`B` are uppercase, so each branch is a nullary `LONG_IDENT_PAT`.
    assert!(
        matches!(or.lhs(), Some(crate::syntax::Pat::LongIdent(_))),
        "lhs should be LONG_IDENT_PAT, got {:?}",
        or.lhs()
    );
    assert!(
        matches!(or.rhs(), Some(crate::syntax::Pat::LongIdent(_))),
        "rhs should be LONG_IDENT_PAT, got {:?}",
        or.rhs()
    );
    assert_eq!(
        or.bar_token().map(|t| t.text().to_string()),
        Some("|".to_string()),
    );
}

/// Phase 6.9 — `let A | B | C = z`: left-associativity in the green tree — the
/// outer `OR_PAT`'s `lhs` is itself an `OR_PAT` (`Or(Or(A,B), C)`), not
/// right-nested.
#[test]
fn or_pat_left_assoc_shape() {
    let source = "let A | B | C = z\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Or(outer) = pat else {
        panic!("expected outer OR_PAT, got {pat:?}");
    };
    let crate::syntax::Pat::Or(inner) = outer.lhs().expect("outer lhs") else {
        panic!("outer lhs should be a nested OR_PAT (left-assoc)");
    };
    assert!(matches!(
        inner.lhs(),
        Some(crate::syntax::Pat::LongIdent(_))
    ));
    assert!(matches!(
        inner.rhs(),
        Some(crate::syntax::Pat::LongIdent(_))
    ));
    assert!(matches!(
        outer.rhs(),
        Some(crate::syntax::Pat::LongIdent(_))
    ));
}

/// Phase 6.9 — full green-tree shape pin for `let A | B = z`. The `OR_PAT`
/// wraps the two branch `LONG_IDENT_PAT`s with the `BAR_TOK` between them.
#[test]
fn or_pat_tree_shape() {
    let source = "let A | B = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..14
  MODULE_OR_NAMESPACE@0..14
    LET_DECL@0..13
      LET_TOK@0..3 \"let\"
      BINDING@3..13
        OR_PAT@3..9
          LONG_IDENT_PAT@3..5
            LONG_IDENT@3..5
              WHITESPACE@3..4 \" \"
              IDENT_TOK@4..5 \"A\"
          WHITESPACE@5..6 \" \"
          BAR_TOK@6..7 \"|\"
          LONG_IDENT_PAT@7..9
            LONG_IDENT@7..9
              WHITESPACE@7..8 \" \"
              IDENT_TOK@8..9 \"B\"
        WHITESPACE@9..10 \" \"
        EQUALS_TOK@10..11 \"=\"
        WHITESPACE@11..12 \" \"
        ERROR@12..12 \"\"
        IDENT_EXPR@12..13
          IDENT_TOK@12..13 \"z\"
    NEWLINE@13..14 \"\\n\"
    ERROR@14..14 \"\"
    ERROR@14..14 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 6.9 — an `|` with no following operand before a LexFilter-swallowed
/// `)` (`let (A |) next = z`). The rhs goes through the raw-gated
/// `emit_pat_atom`, so it bails cleanly (no rhs, the `)` closes the paren) and
/// does not consume `next` — the same swallowed-`)` discipline as `::`/`,`/`&`.
#[test]
fn or_pat_swallowed_close_recovers() {
    use crate::syntax::AstNode;
    let source = "let (A |) next = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an `|` with no operand before `)` must record a parse error",
    );
    let or = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::OR_PAT)
        .and_then(crate::syntax::OrPat::cast)
        .expect("an OR_PAT node");
    assert!(
        or.rhs().is_none(),
        "the or rhs must not consume the token past the swallowed `)`; got {:?}",
        or.rhs(),
    );
}

/// An `as` whose rhs is missing before a LexFilter-swallowed `)`
/// (`let (h as) next = z`) — the same swallowed-`)` hazard the cons rhs
/// guards against, on the `as` arm of the pattern-tail climber. The `)` is
/// gone from the filtered stream, so the filtered cursor after `as` is
/// already at `next`; the rhs must **not** consume `next` (it belongs to
/// the enclosing binding). Gating the rhs on the *raw* next token surfaces
/// the swallowed `)`, so the `AS_PAT` gets no rhs, the `)` closes the
/// paren, and an error is recorded. FCS recovers `Paren(As(h, Wild))` here
/// and likewise leaves `next` outside the pattern.
#[test]
fn as_pat_swallowed_close_recovers() {
    use crate::syntax::AstNode;
    let source = "let (h as) next = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an `as` with no rhs before `)` must record a parse error",
    );
    let as_pat = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::AS_PAT)
        .and_then(crate::syntax::AsPat::cast)
        .expect("an AS_PAT node");
    assert!(
        as_pat.rhs().is_none(),
        "the `as` rhs must not consume the token past the swallowed `)`; got {:?}",
        as_pat.rhs(),
    );
}

/// Phase 6 — `let [x = z`: a missing close bracket records an error
/// (no panic) and stays lossless. The exact recovery shape isn't
/// pinned — only that the parser survives.
#[test]
fn list_pat_missing_close_recovers() {
    let source = "let [x = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a missing `]` must record a parse error",
    );
}

/// Phase 6 — offside-separated list elements must stay distinct
/// `NAMED_PAT`s, not be mis-promoted to function form. In
/// `[ x⏎ y ]` the filtered stream carries a `Virtual::BlockSep`
/// between `x` and `y`; the function-form promotion check must
/// reject that intervening layout virtual rather than skip it on the
/// raw stream and fold `y` into `x`'s curried-arg sweep.
#[test]
fn list_pat_offside_separated() {
    let source = "let [ x\n      y ] = z\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::ArrayOrList(arr) = pat else {
        panic!("expected ARRAY_OR_LIST_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = arr.elements().collect();
    assert_eq!(elements.len(), 2, "two elements, got {elements:?}");
    assert!(
        elements
            .iter()
            .all(|e| matches!(e, crate::syntax::Pat::Named(_))),
        "both elements should be NAMED_PAT (not function-form promoted), got {elements:?}",
    );
}

/// Phase 10.6 — the single paren-pattern argument of a function-form head:
/// `let f ([<Foo>] x) = w` reaches `LONG_IDENT_PAT > [LONG_IDENT, PAREN_PAT >
/// ATTRIB_PAT > [ATTRIBUTE_LIST, NAMED_PAT]]`. Pins the green shape and the
/// facade accessors (`AttribPat::pat()` / `attributes()`).
#[test]
fn attrib_pat_green_shape() {
    let source = "let f ([<Foo>] x) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let arg = li.args().next().expect("one head argument");
    let crate::syntax::Pat::Paren(paren) = arg else {
        panic!("expected PAREN_PAT argument, got {arg:?}");
    };
    let crate::syntax::Pat::Attrib(attrib) = paren.inner().expect("paren inner") else {
        panic!("expected ATTRIB_PAT inside the parens");
    };
    assert!(
        matches!(attrib.pat(), Some(crate::syntax::Pat::Named(_))),
        "ATTRIB_PAT must wrap the NAMED_PAT, got {:?}",
        attrib.pat(),
    );
    let lists: Vec<_> = attrib.attributes().collect();
    assert_eq!(lists.len(), 1, "one attribute list, got {lists:?}");
    let attrs: Vec<_> = lists[0].attributes().collect();
    assert_eq!(attrs.len(), 1, "one attribute in the list");
}

/// Phase 10.6 — two *adjacent* `[<…>]` lists prefix one `ATTRIB_PAT` carrying
/// both `ATTRIBUTE_LIST` children (FCS groups `attributes: attributeList
/// attributes` into a single `SynAttributes`).
#[test]
fn attrib_pat_two_adjacent_lists() {
    let source = "let f ([<A>] [<B>] x) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::Paren(paren) = li.args().next().expect("arg") else {
        panic!("expected PAREN_PAT argument");
    };
    let crate::syntax::Pat::Attrib(attrib) = paren.inner().expect("paren inner") else {
        panic!("expected ATTRIB_PAT inside the parens");
    };
    let lists: Vec<_> = attrib.attributes().collect();
    assert_eq!(
        lists.len(),
        2,
        "two adjacent attribute lists, got {lists:?}"
    );
}

/// Phase 10.6 precedence — `::` binds *inside* the attrib, so
/// `([<Foo>] h :: t)` is `ATTRIB_PAT > [ATTRIBUTE_LIST, LIST_CONS_PAT]`, not a
/// `LIST_CONS_PAT` wrapping the `ATTRIB_PAT`.
#[test]
fn attrib_pat_cons_binds_inside() {
    let source = "let f ([<Foo>] h :: t) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::Paren(paren) = li.args().next().expect("arg") else {
        panic!("expected PAREN_PAT argument");
    };
    let crate::syntax::Pat::Attrib(attrib) = paren.inner().expect("paren inner") else {
        panic!("expected ATTRIB_PAT inside the parens");
    };
    assert!(
        matches!(attrib.pat(), Some(crate::syntax::Pat::ListCons(_))),
        "the `::` must bind inside the attrib (Attrib(ListCons …)), got {:?}",
        attrib.pat(),
    );
}

/// Phase 10.6 precedence — `,` binds *outside* the attrib, so
/// `([<Foo>] x, y)` is `Tuple[Attrib(x), y]`: the parens wrap a `TUPLE_PAT`
/// whose first element is the `ATTRIB_PAT`.
#[test]
fn attrib_pat_comma_binds_outside() {
    let source = "let f ([<Foo>] x, y) = w\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let crate::syntax::Pat::Paren(paren) = li.args().next().expect("arg") else {
        panic!("expected PAREN_PAT argument");
    };
    let crate::syntax::Pat::Tuple(tuple) = paren.inner().expect("paren inner") else {
        panic!("expected TUPLE_PAT inside the parens (`,` binds outside the attrib)");
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 2, "two tuple elements, got {elements:?}");
    assert!(
        matches!(elements[0], crate::syntax::Pat::Attrib(_)),
        "first tuple element must be the ATTRIB_PAT, got {:?}",
        elements[0],
    );
    assert!(
        matches!(elements[1], crate::syntax::Pat::Named(_)),
        "second tuple element must be the plain NAMED_PAT, got {:?}",
        elements[1],
    );
}

/// Phase 10.6 recovery — an attribute list with no inner pattern (`([<A>])`)
/// is malformed, but the recovery must NOT steal the following argument. A
/// LexFilter-swallowed `)` sits between the attribute list and `x` on the raw
/// stream; the raw-gated inner emit bails on the `)` so the `ATTRIB_PAT` gets
/// no inner pat, the `)` closes the parens, and `x` survives as the second
/// curried argument.
#[test]
fn attrib_pat_empty_recovers_next_arg() {
    let source = "let f ([<A>]) x = w\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "an attribute list with no inner pattern must record an error",
    );

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = li.args().collect();
    assert_eq!(
        args.len(),
        2,
        "the `x` must survive as a second argument, got {args:?}",
    );
    assert!(
        matches!(&args[0], crate::syntax::Pat::Paren(_)),
        "first arg is the (malformed) paren-attrib, got {:?}",
        args[0],
    );
    assert!(
        matches!(&args[1], crate::syntax::Pat::Named(_)),
        "second arg must be the surviving NAMED_PAT `x`, got {:?}",
        args[1],
    );
}

/// Phase 10.6 — an attribute prefix on an *offside-continued* `::`-rhs operand
/// (`[ h ::⏎    [<A>] t ]`). The trailing infix `::` now opens a fresh block for
/// its rhs (`Filter::infix_rhs_pushes`), so `[<A>] t` continues the cons as a
/// well-formed attributed operand — FCS accepts it with no error. The standing
/// guard the fix pins: every emitted `LBRACK_LESS_TOK` carries the real `"[<"`
/// text — never a zero-width mislabelled virtual (a `Virtual::BlockSep` mistaken
/// for the `[<` opener). (Before trailing-infix continuation this form bailed
/// with a recoverable "expected pattern after `::`" error.)
#[test]
fn attrib_pat_offside_cons_rhs_no_mislabel() {
    let source = "match v with\n| [ h ::\n    [<A>] t ] -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "the offside attributed `::`-rhs continues the cons, no error: {:?}",
        parse.errors,
    );
    let bad_opener = parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::LBRACK_LESS_TOK)
        .any(|t| t.text() != "[<");
    assert!(
        !bad_opener,
        "every `[<` opener must be the real token, not a mislabelled zero-width virtual",
    );
}

/// Phase 10.6 — the swallowed-`)` dual of the offside guard. With a missing
/// `::`-rhs right before a LexFilter-swallowed `)` and an attribute just
/// outside (`match v with ( a :: ) [<B>] c -> 1`), the filtered cursor lands on
/// the outside `[<` while the raw cursor is still on the swallowed `)`. The
/// attrib dispatch must require `[<` on *both* cursors, so it declines and the
/// `)` is reclaimed as the paren's closer rather than drained into an
/// `ATTRIB_PAT` — i.e. no `ATTRIB_PAT` subtree may contain a `)` token.
#[test]
fn attrib_pat_swallowed_rparen_not_stolen() {
    let source = "match v with ( a :: ) [<B>] c -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    let attrib_steals_rparen = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ATTRIB_PAT)
        .any(|a| {
            a.descendants_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.text() == ")")
        });
    assert!(
        !attrib_steals_rparen,
        "the attributed-operand dispatch must not drain the swallowed `)` into an ATTRIB_PAT",
    );
}

/// Phase 10.6 — the same swallowed-closer guard at the `emit_paren_pat_element`
/// dispatch (record-field values, not just the `emit_pat_atom` tail). With a
/// record-field value missing before the LexFilter-swallowed `}` and an
/// attribute outside (`match v with { X = } [<A>] y -> 1`), the filtered cursor
/// lands on the outside `[<` while the raw cursor is still on `}`. The shared
/// `at_attribute_list_start` both-cursors gate declines, so the `}` is reclaimed
/// by `bump_swallowed_rbrace` rather than drained into an `ATTRIB_PAT`.
#[test]
fn attrib_pat_swallowed_rbrace_not_stolen() {
    let source = "match v with { X = } [<A>] y -> 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    let attrib_steals_rbrace = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ATTRIB_PAT)
        .any(|a| {
            a.descendants_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.text() == "}")
        });
    assert!(
        !attrib_steals_rbrace,
        "the record-field attrib dispatch must not drain the swallowed `}}` into an ATTRIB_PAT",
    );
}

/// Phase 10.6 — a *typed clause head* whose annotation sits directly before the
/// clause arrow (`match v with [<A>] x : int -> x`) is a parse error in FCS, not
/// a valid attributed pattern: the clause `:` arm (now on for `PatCtx::Clause`)
/// parses the annotation *greedily*, so it absorbs `int -> x` as a function type
/// (`Typed(x, Fun(int, x))`), leaving no clause `->` (verified
/// `ParseHadErrors: true`, "Expected '->'"). It errors *identically* to the
/// non-attributed `match v with x : int -> x`; our parser now consumes the same
/// greedy typed pattern and errors too. (A type bounded by `::`/`as`, e.g.
/// `| h: int :: t ->`, *does* parse — see the `parser_diff_match` differential
/// tests.) Pinned lossless + erroring on both forms.
#[test]
fn attrib_pat_typed_clause_head_errors_like_nonattrib() {
    for source in [
        "match v with x : int -> x\n",
        "match v with [<A>] x : int -> x\n",
    ] {
        let parse = parse(source);
        assert_lossless(source, &parse);
        assert!(
            !parse.errors.is_empty(),
            "the clause `:` must error (as FCS does), got a clean parse for {source:?}",
        );
    }
}

/// A bare (unparenthesised) typed clause pattern cannot carry a `when` guard:
/// the clause `:` annotation is parsed with the *greedy*
/// `typeWithTypeConstraints`, which consumes the following `when` as a
/// type-constraint clause (`int when …`) rather than leaving it for the match
/// guard — so `| y: int when y > 0 -> y` errors on BOTH sides (verified
/// `ParseHadErrors: true` against FCS). A guard needs parentheses
/// (`| (y: int) when y > 0 -> _`) or a `::`/`as`-bounded annotation
/// (`| h: int :: t when g -> _`, covered by the `parser_diff_match` diffs). This
/// pins the both-error behaviour: "fixing" it to accept the guard would create a
/// we-accept/FCS-reject divergence, since FCS rejects it too.
#[test]
fn clause_typed_pat_bare_when_guard_errors_like_fcs() {
    let source = "match x with | y: int when y > 0 -> y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a bare typed clause pattern + `when` guard must error (as FCS does)",
    );
}

/// A per-element access modifier is only legal before a `pathOp` (an
/// ident/name), matching FCS's `access pathOp`. Before a non-`pathOp` element —
/// a wildcard (`let a, private _`) or a literal (`let a, private 1`) — there is
/// no `access` production, so FCS errors and so must we: the tuple-element
/// admittance lets the modifier reach the head-element parser, which declines to
/// consume it and the element bails. Lossless + both-error (the recovery *shape*
/// differs from FCS, so this is a unit guard, not a diff oracle — see the
/// positive `parser_diff_let_bindings` cases). Guards against over-accepting a
/// modifier where FCS has no production.
#[test]
fn tuple_element_access_before_non_pathop_errors_like_fcs() {
    for source in ["let a, private _ = 1, 2\n", "let a, private 1 = 1, 2\n"] {
        let parse = parse(source);
        assert_lossless(source, &parse);
        assert!(
            !parse.errors.is_empty(),
            "a modifier before a non-pathOp element must error (as FCS does): {source:?}",
        );
    }
}

/// Active-pattern definition head with a curried arg — `let (|Foo|Bar|) x = x`.
/// FCS folds the name into a single-segment `SynPat.LongIdent`; we mirror that
/// as `LONG_IDENT_PAT > [ACTIVE_PAT_NAME, NAMED_PAT]`. Pins the green shape,
/// the reconstructed case list, and losslessness (every `(`/`|`/`)` kept).
#[test]
fn active_pat_name_function_form_tree_shape() {
    let source = "let (|Foo|Bar|) x = x\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head for a function-form active pattern, got {pat:?}");
    };
    assert!(
        li.head().is_none(),
        "an active-pattern head carries an ACTIVE_PAT_NAME, not a LONG_IDENT",
    );
    let active = li.active_pat_name().expect("ACTIVE_PAT_NAME head");
    let cases: Vec<String> = active.case_tokens().map(|t| t.text().to_string()).collect();
    assert_eq!(cases, vec!["Foo".to_string(), "Bar".to_string()]);
    // One curried arg, a NAMED_PAT.
    let args: Vec<_> = li.args().collect();
    assert_eq!(args.len(), 1, "one curried arg");
    assert!(
        matches!(args[0], crate::syntax::Pat::Named(_)),
        "arg should be NAMED_PAT, got {:?}",
        args[0],
    );
}

/// A *nullary* active pattern collapses to `SynPat.Named` (FCS's maybe-var
/// rule, since the `idText` leads with `|`). Green shape `NAMED_PAT >
/// [ACTIVE_PAT_NAME]`; `ident()` is `None` (the name is not an `IDENT_TOK`).
#[test]
fn active_pat_name_nullary_is_named() {
    let source = "let (|Foo|Bar|) = id\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::Named(named) = pat else {
        panic!("expected NAMED_PAT head for a nullary active pattern, got {pat:?}");
    };
    assert!(
        named.ident().is_none(),
        "a nullary active pattern's name is an ACTIVE_PAT_NAME, not an IDENT_TOK",
    );
    let active = named.active_pat_name().expect("ACTIVE_PAT_NAME child");
    let cases: Vec<String> = active.case_tokens().map(|t| t.text().to_string()).collect();
    assert_eq!(cases, vec!["Foo".to_string(), "Bar".to_string()]);
}

/// A partial active pattern keeps its trailing `_` as a case token (an
/// `UNDERSCORE_TOK`), so the reconstructed `idText` is `|Foo|_|`.
#[test]
fn active_pat_name_partial_underscore() {
    let source = "let (|Foo|_|) x = None\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(li) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let active = li.active_pat_name().expect("ACTIVE_PAT_NAME head");
    let cases: Vec<String> = active.case_tokens().map(|t| t.text().to_string()).collect();
    assert_eq!(cases, vec!["Foo".to_string(), "_".to_string()]);
}

/// `name_range()` spans the `|Foo|Bar|` text from the first `|` to the last `|`,
/// with the surrounding parens excluded — matching FCS's `SynLongIdent` idText
/// range for the active-pattern value.
#[test]
fn active_pat_name_range_excludes_parens() {
    for source in ["let (|Foo|Bar|) x = x\n", "let (|Foo|_|) x = None\n"] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "unexpected errors: {:?}",
            parse.errors
        );
        let pat = first_binding_head_pat(&parse.root);
        let active = match pat {
            crate::syntax::Pat::LongIdent(li) => li.active_pat_name(),
            crate::syntax::Pat::Named(n) => n.active_pat_name(),
            other => panic!("expected an active-pattern head, got {other:?}"),
        }
        .expect("ACTIVE_PAT_NAME");
        let range = active.name_range().expect("a name range");
        let text = &source[usize::from(range.start())..usize::from(range.end())];
        // First and last char are the bars; the parens are not included.
        assert!(
            text.starts_with('|') && text.ends_with('|'),
            "name range {text:?} should be the |..| span (parens excluded)"
        );
        assert!(
            !text.contains('(') && !text.contains(')'),
            "name range {text:?} must exclude the surrounding parens"
        );
    }
}

/// `global.`-rooted pattern head — FCS's `GLOBAL DOT pathOp`
/// (`SynPat.LongIdent(["global"; …])`), the pattern twin of the `global`-rooted
/// expression path. A dotted `global.` head is a `LONG_IDENT_PAT` whose first
/// segment text is the reused keyword `global`. Cross-checked against FCS in
/// `tests/all/parser_diff_global_pat.rs`; this pins the local green shape.
#[test]
fn global_rooted_pat_head_is_long_ident() {
    let source = "match x with\n| global.A.B -> 1\n| _ -> 2\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);
    // The first match clause's pattern is a LONG_IDENT_PAT led by `global`.
    let li = parse
        .root
        .descendants()
        .find(|n| n.kind() == crate::syntax::SyntaxKind::LONG_IDENT_PAT)
        .expect("a LONG_IDENT_PAT");
    let first_seg = li
        .descendants_with_tokens()
        .filter_map(|nt| nt.into_token())
        .find(|t| t.kind() == crate::syntax::SyntaxKind::IDENT_TOK)
        .expect("an IDENT_TOK segment");
    assert_eq!(first_seg.text(), "global");
}

/// Explicit value typars after a *dotted* pattern head (`A.B.Case<'T> y`) —
/// FCS's `constrPattern: atomicPatternLongIdent explicitValTyparDecls
/// atomicPatsOrNamePatPairs`, which takes the typars after the whole `pathOp`.
/// They land in a `TYPAR_DECLS` child sitting between the head `LONG_IDENT` and
/// the argument patterns, exactly as for a single-ident head (`Case<'T> y`).
/// Cross-checked against FCS in `tests/all/parser_diff_pat_typars.rs`; this pins the
/// child order, which the normaliser reads to fill `SynPat.LongIdent.typars`.
#[test]
fn dotted_pat_head_carries_typar_decls() {
    for source in [
        "match x with\n| A.B.Case<'T> y -> 1\n| _ -> 0\n",
        "match x with\n| global.A.Case<'T> y -> 1\n| _ -> 0\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "unexpected errors for {source:?}: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);
        let li = parse
            .root
            .descendants()
            .find(|n| n.kind() == crate::syntax::SyntaxKind::LONG_IDENT_PAT)
            .expect("a LONG_IDENT_PAT");
        let children: Vec<crate::syntax::SyntaxKind> = li.children().map(|n| n.kind()).collect();
        assert_eq!(
            children,
            vec![
                crate::syntax::SyntaxKind::LONG_IDENT,
                crate::syntax::SyntaxKind::TYPAR_DECLS,
                crate::syntax::SyntaxKind::NAMED_PAT,
            ],
            "for {source:?}"
        );
    }
}

/// A pattern path ending in an *operator* `opName` (`A.B.(+) y`) — FCS's
/// `pathOp: ident DOT pathOp` with a final `pathOp: opName`. The `( op )` tokens
/// live *inside* the head `LONG_IDENT`, so its `idents()` reads
/// `["A", "B", "+"]`, matching FCS's `["A"; "B"; "op_Addition"]` (the normaliser
/// compares the source spelling). Cross-checked against FCS in
/// `tests/all/parser_diff_pat_opname_path.rs`.
#[test]
fn dotted_operator_name_pat_path_is_one_long_ident() {
    let source = "match x with\n| A.B.(+) y -> 1\n| _ -> 0\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);
    let li = parse
        .root
        .descendants()
        .find(|n| n.kind() == crate::syntax::SyntaxKind::LONG_IDENT)
        .expect("a LONG_IDENT");
    let segs: Vec<String> = li
        .children_with_tokens()
        .filter_map(|nt| nt.into_token())
        .filter(|t| t.kind() == crate::syntax::SyntaxKind::IDENT_TOK)
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["A", "B", "+"]);
}

/// A pattern path ending in an *active-pattern* `opName` (`A.B.(|Foo|_|) y`).
/// Unlike the operator form, the name is a sibling `ACTIVE_PAT_NAME` node (FCS
/// folds it into the final `SynLongIdent` segment `"|Foo|_|"`, which the
/// normaliser rebuilds from the case tokens) — the same shape the dotted
/// *member* head (`member x.(|Foo|Bar|)`) already produced.
#[test]
fn dotted_active_pat_name_pat_path_has_sibling_name_node() {
    let source = "match x with\n| A.B.(|Foo|_|) y -> 1\n| _ -> 0\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);
    let li = parse
        .root
        .descendants()
        .find(|n| n.kind() == crate::syntax::SyntaxKind::LONG_IDENT_PAT)
        .expect("a LONG_IDENT_PAT");
    let children: Vec<crate::syntax::SyntaxKind> = li.children().map(|n| n.kind()).collect();
    assert_eq!(
        children,
        vec![
            crate::syntax::SyntaxKind::LONG_IDENT,
            crate::syntax::SyntaxKind::ACTIVE_PAT_NAME,
            crate::syntax::SyntaxKind::NAMED_PAT,
        ],
    );
}

/// A dotted `opName` path in *atomic* position (a curried argument) is FCS's
/// `atomicPattern: atomicPatternLongIdent` — nullary. The head must not sweep the
/// following argument into itself: `let f A.B.(+) y = 1` binds *two* args.
#[test]
fn atomic_dotted_opname_pat_does_not_swallow_the_next_arg() {
    let source = "let f A.B.(+) y = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);
    // The binding head is `f` with two argument patterns: the `A.B.(+)` path and
    // the named `y` — siblings under the head `LONG_IDENT_PAT`, not nested.
    let head = parse
        .root
        .descendants()
        .find(|n| n.kind() == crate::syntax::SyntaxKind::LONG_IDENT_PAT)
        .expect("the head LONG_IDENT_PAT");
    let children: Vec<crate::syntax::SyntaxKind> = head.children().map(|n| n.kind()).collect();
    assert_eq!(
        children,
        vec![
            crate::syntax::SyntaxKind::LONG_IDENT,
            crate::syntax::SyntaxKind::LONG_IDENT_PAT,
            crate::syntax::SyntaxKind::NAMED_PAT,
        ],
    );
}

/// A *bare* `global` is a valid expression but **not** a valid pattern (FCS
/// FS0010). We must reject it (an error, not a silent accept), matching FCS —
/// never widen the reused keyword into a bare pattern name.
#[test]
fn bare_global_pattern_is_rejected() {
    for source in [
        "match x with\n| global -> 1\n| _ -> 2\n",
        "let f global = 1\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "bare `global` pattern must error, but parsed clean: {source:?}"
        );
        assert_lossless(source, &parse);
    }
}

/// `_.M` — FCS's `UNDERSCORE DOT pathOp` — is gated on the F# 4.7
/// `SingleUnderscorePattern` feature and is deferred here (it lands with its
/// language-version gate in a later slice), so a bare `_` stays the wildcard and
/// the dotted `_.M` form is a clean lossless parse error rather than a wrong
/// tree — the recorded boundary for a future implementation to flip.
#[test]
fn underscore_rooted_pat_is_deferred_error() {
    let source = "match x with\n| _.M -> 1\n| _ -> 2\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "`_.M` is deferred — expected a parse error, got a clean parse",
    );
    assert_lossless(source, &parse);
}
