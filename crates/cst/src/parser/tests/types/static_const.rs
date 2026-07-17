//! Type-provider static arguments — `SynType.StaticConstant` /
//! `StaticConstantExpr` / `StaticConstantNamed` / `StaticConstantNull`, plus
//! the `/` measure-division `SynTupleTypeSegment.Slash` (phase 10.9). Reached
//! through the prefix-app `Foo<…>` / dotted `(int).Foo<…>` type-argument
//! surfaces and (for the bare literal / `null` forms) anywhere a type is
//! expected.

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

use crate::syntax::{AstNode, Expr, Type, TypeDefn};

/// Find the sole node of `kind` in a parse.
fn node_of_kind(parse: &crate::parser::Parse, kind: SyntaxKind) -> SyntaxNode {
    parse
        .root
        .descendants()
        .find(|n| n.kind() == kind)
        .unwrap_or_else(|| panic!("{kind:?} present in:\n{}", debug_tree(&parse.root)))
}

/// `Foo<42>` → the single arg is a `STATIC_CONST_TYPE` whose `literal()` is the
/// `INT32_LIT("42")`. Pins the prefix-app arg routing into the static-const
/// arm.
#[test]
fn static_const_int_green_shape() {
    use crate::syntax::StaticConstType;
    let source = "(x : Foo<42>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE);
    let sc = StaticConstType::cast(node).expect("casts");
    let lit = sc.literal().expect("literal token");
    assert_eq!(lit.kind(), SyntaxKind::INT32_LIT);
    assert_eq!(lit.text(), "42");
    assert_lossless(source, &parse);
}

/// `Foo<"literal">` → the arg is a `STATIC_CONST_TYPE` over a `STRING_LIT`.
#[test]
fn static_const_string_green_shape() {
    use crate::syntax::StaticConstType;
    let source = "(x : Foo<\"lit\">)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let sc =
        StaticConstType::cast(node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE)).expect("casts");
    assert_eq!(
        sc.literal().expect("literal").kind(),
        SyntaxKind::STRING_LIT
    );
    assert_lossless(source, &parse);
}

/// `Foo<true>` → a `STATIC_CONST_TYPE` over a `BOOL_LIT` (FCS's dedicated
/// `TRUE` `atomicType` arm; our parser shares the const-payload `BOOL_LIT`).
#[test]
fn static_const_bool_green_shape() {
    use crate::syntax::StaticConstType;
    let source = "(x : Foo<true>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let sc =
        StaticConstType::cast(node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE)).expect("casts");
    assert_eq!(sc.literal().expect("literal").text(), "true");
    assert_lossless(source, &parse);
}

/// `Foo<null>` → a `STATIC_CONST_NULL_TYPE > [NULL_TOK]`.
#[test]
fn static_const_null_green_shape() {
    let source = "(x : Foo<null>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = node_of_kind(&parse, SyntaxKind::STATIC_CONST_NULL_TYPE);
    let toks: Vec<_> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect();
    assert_eq!(toks, vec![SyntaxKind::NULL_TOK]);
    assert_lossless(source, &parse);
}

/// `Foo<const E>` → `STATIC_CONST_EXPR_TYPE > [CONST_TOK, IDENT_EXPR(E)]`; the
/// facade `expr()` casts the inner atomic expression.
#[test]
fn static_const_expr_green_shape() {
    use crate::syntax::StaticConstExprType;
    let source = "(x : Foo<const E>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = node_of_kind(&parse, SyntaxKind::STATIC_CONST_EXPR_TYPE);
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::CONST_TOK),
        "CONST_TOK present"
    );
    let sce = StaticConstExprType::cast(node).expect("casts");
    match sce.expr().expect("inner expr") {
        Expr::Ident(_) => {}
        other => panic!("inner expr must be Ident(E); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// `Foo<N=42>` → `STATIC_CONST_NAMED_TYPE > [LONG_IDENT_TYPE(N), EQUALS_TOK,
/// STATIC_CONST_TYPE(42)]`; `ident()` is the name type, `value()` is the
/// (static-constant) value type.
#[test]
fn static_const_named_green_shape() {
    use crate::syntax::StaticConstNamedType;
    let source = "(x : Foo<N=42>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = node_of_kind(&parse, SyntaxKind::STATIC_CONST_NAMED_TYPE);
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::EQUALS_TOK),
        "EQUALS_TOK present"
    );
    let named = StaticConstNamedType::cast(node).expect("casts");
    match named.ident().expect("ident type") {
        Type::LongIdent(_) => {}
        other => panic!("ident must be LongIdent(N); got {other:?}"),
    }
    match named.value().expect("value type") {
        Type::StaticConst(_) => {}
        other => panic!("value must be StaticConst(42); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// `(x : 42)` → a bare `STATIC_CONST_TYPE` reached outside any `<…>` surface,
/// confirming the arm lives at the `atomType` layer (FCS-faithful), not gated
/// to the type-arg loop.
#[test]
fn static_const_bare_outside_type_args() {
    use crate::syntax::StaticConstType;
    let source = "(x : 42)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let sc =
        StaticConstType::cast(node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE)).expect("casts");
    assert_eq!(sc.literal().expect("literal").text(), "42");
    assert_lossless(source, &parse);
}

/// `float<1/s>` → the arg is a `TUPLE_TYPE` whose `segments()` are
/// `[Type(StaticConst 1), Slash, Type(LongIdent s)]`. Pins the `/`
/// measure-division `SynTupleTypeSegment.Slash` and that the leading `1` is a
/// `StaticConstant` (not a `LongIdent`).
#[test]
fn measure_division_slash_segment() {
    use crate::syntax::{TupleSegment, TupleType};
    let source = "(x : float<1/s>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let tuple =
        TupleType::cast(node_of_kind(&parse, SyntaxKind::TUPLE_TYPE)).expect("TUPLE_TYPE casts");
    let segments = tuple.segments();
    assert_eq!(segments.len(), 3, "1 / s is three segments");
    match &segments[0] {
        TupleSegment::Type(Type::StaticConst(_)) => {}
        other => panic!("first segment must be StaticConst(1); got {other:?}"),
    }
    assert!(
        matches!(segments[1], TupleSegment::Slash(_)),
        "middle segment must be Slash; got {:?}",
        segments[1]
    );
    match &segments[2] {
        TupleSegment::Type(Type::LongIdent(_)) => {}
        other => panic!("last segment must be LongIdent(s); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// `type T = Foo<42>` — a static-const arg in a type-abbreviation body, to
/// confirm the arg routing is not specific to the typed-paren caller.
#[test]
fn static_const_in_type_abbrev() {
    use crate::syntax::StaticConstType;
    let source = "type T = Foo<42>\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    // The abbreviation carries the App; the arg is the static constant.
    let _ = TypeDefn::cast(node_of_kind(&parse, SyntaxKind::TYPE_DEFN)).expect("TYPE_DEFN casts");
    let sc =
        StaticConstType::cast(node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE)).expect("casts");
    assert_eq!(sc.literal().expect("literal").text(), "42");
    assert_lossless(source, &parse);
}

/// Recovery: a named arg with a missing value (`Foo<N=>`) must not panic; the
/// parser records an error and still round-trips losslessly (full recovery
/// shape is phase 11). Pins only that the malformed surface is handled
/// gracefully, since the named-form `=` bump is unconditional once detected.
#[test]
fn static_const_named_missing_value_recovers() {
    for source in ["(x : Foo<N=>)\n", "(x : Foo< >)\n", "(x : Foo<>)\n"] {
        let parse = parse(source);
        // No assertion on error presence (some are valid empty-arg forms); the
        // contract under test is "no panic + lossless".
        assert_lossless(source, &parse);
    }
}

/// Recovery: the `const`-expr gate must not panic or steal tokens past a
/// LexFilter-swallowed closer, and must reject non-atomic starters cleanly.
/// `(x : const) y` would otherwise drain the swallowed `)` and parse the outer
/// `y` as the static-const expression; `const if …` / a bare `const -` would
/// otherwise reach `parse_atomic_expr`'s `parse_const_payload` `unreachable!`
/// arm. All three error gracefully and round-trip losslessly (FCS errors too;
/// the recovery *shape* is phase 11).
#[test]
fn static_const_expr_bad_operand_recovers() {
    for source in [
        "let z = (x : const) y\n",
        "(x : Foo<const if true then 1 else 2>)\n",
        "(x : Foo<const ->)\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "expected an error for {source:?}, tree:\n{}",
            debug_tree(&parse.root)
        );
        assert_lossless(source, &parse);
    }
}

/// `Foo<const -1>` — the `const` operand may be a sign-folded negative literal.
/// Pins that the gate admits the fold (filtered `INT32_LIT("-1")`, raw
/// `Op("-")`) and parses it as the inner atomic expression with no errors.
#[test]
fn static_const_expr_negative_literal_green_shape() {
    use crate::syntax::StaticConstExprType;
    let source = "(x : Foo<const -1>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let sce = StaticConstExprType::cast(node_of_kind(&parse, SyntaxKind::STATIC_CONST_EXPR_TYPE))
        .expect("casts");
    match sce.expr().expect("inner expr") {
        Expr::Const(_) => {}
        other => panic!("inner expr must be Const(-1); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Recovery: an *un-spaced* `<-` / `=-` fuses at the lexer (longest match)
/// into the back-arrow / an infix op, so `Foo<-1>` and `Foo<N=-1>` are not the
/// signed-static-arg forms (which need a space: `Foo< -1>` / `Foo<N= -1>`).
/// FCS errors on both; our parser must error gracefully (no panic) and
/// round-trip losslessly. The spaced, *valid* signed forms are pinned by the
/// `diff_ast_static_const_*negative*` diff tests.
#[test]
fn unspaced_sign_after_angle_is_not_a_static_arg() {
    for source in ["(x : Foo<-1>)\n", "(x : Foo<N=-1>)\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "expected an error for {source:?}, tree:\n{}",
            debug_tree(&parse.root)
        );
        assert_lossless(source, &parse);
    }
}

/// `(x : -1)` → a bare `STATIC_CONST_TYPE` over a sign-folded `INT32_LIT("-1")`.
/// Pins that the type-start gate admits the fold (raw cursor `Op("-")`,
/// filtered `INT32_LIT("-1")`) and the literal carries the sign.
#[test]
fn static_const_negative_green_shape() {
    use crate::syntax::StaticConstType;
    let source = "(x : -1)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let sc =
        StaticConstType::cast(node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE)).expect("casts");
    let lit = sc.literal().expect("literal");
    assert_eq!(lit.kind(), SyntaxKind::INT32_LIT);
    assert_eq!(lit.text(), "-1");
    assert_lossless(source, &parse);
}

/// `float</s>` → the arg is a `TUPLE_TYPE` whose first segment is a `Slash`
/// (no leading `Type`), then `Type(LongIdent s)`. Pins the leading-slash
/// measure form.
#[test]
fn leading_slash_measure_green_shape() {
    use crate::syntax::{TupleSegment, TupleType};
    let source = "(x : float</s>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let tuple =
        TupleType::cast(node_of_kind(&parse, SyntaxKind::TUPLE_TYPE)).expect("TUPLE_TYPE casts");
    let segments = tuple.segments();
    assert_eq!(segments.len(), 2, "/ s is two segments (leading Slash)");
    assert!(
        matches!(segments[0], TupleSegment::Slash(_)),
        "first segment must be the leading Slash; got {:?}",
        segments[0]
    );
    match &segments[1] {
        TupleSegment::Type(Type::LongIdent(_)) => {}
        other => panic!("second segment must be LongIdent(s); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Recovery: a leading `*` is *not* a happy-path tuple start (FCS reports
/// "Expecting type" and recovers with a `FromParseError`, which is phase 11).
/// Our parser must error and round-trip losslessly, not panic.
#[test]
fn leading_star_is_error() {
    let source = "(x : *s)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "leading `*` must error, tree:\n{}",
        debug_tree(&parse.root)
    );
    assert_lossless(source, &parse);
}

/// Recovery: malformed / boundary forms exposed by the static-arg gates must
/// error gracefully (no panic), round-tripping losslessly. `Foo<const (>)>`
/// would otherwise hit `parse_atomic_expr`'s LParen-dispatch `unreachable!`;
/// `(x : ) -1` would otherwise cross the swallowed `)` and steal the outer
/// `-1` as the annotation; `(x : ) /s` is the leading-slash analogue.
#[test]
fn static_arg_gate_boundaries_recover() {
    for source in [
        "(x : Foo<const (>)>)\n",
        "let z = (x : ) -1\n",
        "let z = (x : ) /s\n",
        // `const` with no operand, then an outer paren expr: the swallowed `)`
        // of `(x : const)` must not be crossed to consume `(1)` as the operand.
        "let z = (x : const) (1)\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "expected an error for {source:?}, tree:\n{}",
            debug_tree(&parse.root)
        );
        assert_lossless(source, &parse);
    }
}

/// `(x : (/s))` — the leading-slash measure inside a paren type parses cleanly
/// (the paren-inner gate reaches `parse_type`). Pins the `Paren > Tuple` with a
/// leading `Slash` segment.
#[test]
fn paren_leading_slash_green_shape() {
    use crate::syntax::{TupleSegment, TupleType};
    let source = "(x : (/s))\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let tuple =
        TupleType::cast(node_of_kind(&parse, SyntaxKind::TUPLE_TYPE)).expect("TUPLE_TYPE casts");
    let segments = tuple.segments();
    assert!(
        matches!(segments.first(), Some(TupleSegment::Slash(_))),
        "first segment must be the leading Slash; got {segments:?}"
    );
    assert_lossless(source, &parse);
}

/// `(x : 42<int>)` — LexFilter emits an HPA virtual after the numeric literal,
/// but a static constant is not an `appTypeCon`, so FCS rejects the `<int>` as
/// an "unexpected type application" (a bare `StaticConstant 42`). Our parser
/// must therefore leave the head as a `STATIC_CONST_TYPE` — **not** wrap it in
/// an `APP_TYPE` — and record an error; it round-trips losslessly.
#[test]
fn static_const_is_not_a_type_app_head() {
    let source = "(x : 42<int>)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected an error for the stray `<int>`, tree:\n{}",
        debug_tree(&parse.root)
    );
    // The literal head stays a `STATIC_CONST_TYPE`.
    let sc = node_of_kind(&parse, SyntaxKind::STATIC_CONST_TYPE);
    assert_eq!(
        sc.children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| !t.kind().is_trivia())
            .map(|t| t.text().to_string()),
        Some("42".to_string())
    );
    // It is NOT wrapped in an APP_TYPE (the bug this guards against).
    assert!(
        parse
            .root
            .descendants()
            .all(|n| n.kind() != SyntaxKind::APP_TYPE),
        "static-const head must not be wrapped as APP_TYPE, tree:\n{}",
        debug_tree(&parse.root)
    );
    assert_lossless(source, &parse);
}
