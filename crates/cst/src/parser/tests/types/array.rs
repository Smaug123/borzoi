//! Array-suffix types (`int[]`, multi-rank, chained).
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.7 — `(x : int[])`: minimal IDENT-adjacent array suffix.
/// LexFilter emits `Virtual::HighPrecedenceBrackApp` between the
/// `int` ident and the `[`; the array branch of `parse_app_type`'s
/// suffix loop consumes that virtual as a zero-width `ERROR` and
/// then bumps `LBRACK_TOK RBRACK_TOK`. Tree shape:
/// `ARRAY_TYPE > [LONG_IDENT_TYPE(int), ERROR(HPBA), LBRACK_TOK,
/// RBRACK_TOK]` with rank 1.
#[test]
fn typed_paren_expr_with_array_type_rank_one_shape() {
    use crate::syntax::{ArrayType, AstNode, Type};
    let source = "(x : int[])\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let arr_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ARRAY_TYPE)
        .expect("ARRAY_TYPE present for `int[]`");
    let arr = ArrayType::cast(arr_node).expect("ARRAY_TYPE casts");
    assert_eq!(arr.rank(), 1, "`int[]` rank is 1");
    match arr.element_type().expect("element type present") {
        Type::LongIdent(_) => {}
        other => panic!("element type must be LongIdent(int), got {other:?}"),
    }

    let toks: Vec<_> = arr
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect();
    assert_eq!(
        toks,
        vec![
            SyntaxKind::ERROR,
            SyntaxKind::LBRACK_TOK,
            SyntaxKind::RBRACK_TOK
        ],
        "IDENT-adjacent array must carry an HPBA placeholder before the brackets; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.7 — `(x : int[,])` / `int[,,]`: multi-rank suffix. Each
/// extra comma between the brackets bumps the rank. `ArrayType::rank`
/// counts `COMMA_TOK` children + 1.
#[test]
fn array_type_multi_rank_counts_commas() {
    use crate::syntax::{ArrayType, AstNode};
    for (source, expected) in [("(x : int[,])\n", 2usize), ("(x : int[,,])\n", 3)] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?}: got errors: {:?}",
            parse.errors,
        );
        let arr_node = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ARRAY_TYPE)
            .unwrap_or_else(|| panic!("{source:?}: ARRAY_TYPE present"));
        let arr = ArrayType::cast(arr_node).expect("ARRAY_TYPE casts");
        assert_eq!(
            arr.rank(),
            expected,
            "{source:?}: rank mismatch; got tree:\n{}",
            debug_tree(&parse.root),
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 7.7 — `(x : int[][])`: jagged array. Both array suffixes
/// wrap from the shared `cp` in `parse_app_type`, so the outer
/// `ARRAY_TYPE` contains the inner one as its element-type child —
/// left-associative, mirroring FCS's `appTypeWithoutNull
/// arrayTypeSuffix` left-recursion.
#[test]
fn array_type_chained_suffix_nests_left_associative() {
    use crate::syntax::{ArrayType, AstNode, Type};
    let source = "(x : int[][])\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let arrays: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ARRAY_TYPE)
        .collect();
    assert_eq!(
        arrays.len(),
        2,
        "expected two nested ARRAY_TYPE nodes; got tree:\n{}",
        debug_tree(&parse.root),
    );

    // `descendants()` yields outer-first.
    let outer = ArrayType::cast(arrays[0].clone()).expect("outer casts");
    let inner_outer = outer.element_type().expect("outer element-type");
    match inner_outer {
        Type::Array(inner) => match inner.element_type().expect("inner element-type") {
            Type::LongIdent(_) => {}
            other => panic!("inner element must be LongIdent(int), got {other:?}"),
        },
        other => panic!("outer element must be Array(_), got {other:?}"),
    }
}

/// Phase 7.7 — `(x : int list[])`: array-of-postfix-app. The
/// postfix-app loop wraps `int list` into `APP_TYPE` first; then
/// the array loop wraps that into `ARRAY_TYPE`. Both branches
/// share `parse_app_type`'s checkpoint so the resulting shape is
/// `ARRAY_TYPE > [APP_TYPE(list, [int])]`, matching FCS's
/// `Array(rank=1, App(list, [int], postfix))`.
#[test]
fn array_type_over_postfix_app_nests_correctly() {
    use crate::syntax::{AppType, ArrayType, AstNode, Type};
    let source = "(x : int list[])\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let arr_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ARRAY_TYPE)
        .expect("ARRAY_TYPE present");
    let arr = ArrayType::cast(arr_node).expect("ARRAY_TYPE casts");
    assert_eq!(arr.rank(), 1);
    let elem = arr.element_type().expect("element type present");
    let Type::App(app) = elem else {
        panic!(
            "array element should be APP_TYPE, got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    assert!(app.is_postfix(), "inner App must be postfix `int list`");
    match app.type_name().expect("App head") {
        Type::LongIdent(_) => {}
        other => panic!("App head must be LongIdent(list), got {other:?}"),
    }
    let inner_args = app.type_args();
    assert_eq!(inner_args.len(), 1);
    assert!(matches!(inner_args[0], Type::LongIdent(_)));
    let _ = AppType::cast(app.syntax().clone());
    assert_lossless(source, &parse);
}

/// Phase 7.7 — `(x : (int)[])`: paren-headed array, no HPBA. The
/// `]` of the inner paren-type isn't an IDENT so LexFilter doesn't
/// emit `HighPrecedenceBrackApp`; the array loop fires from the
/// bare `LBrack` next-non-trivia raw and just bumps `LBRACK_TOK
/// RBRACK_TOK`, with no zero-width `ERROR` placeholder. Pins both
/// arms of `arrayTypeSuffix` (`pars.fsy:6371-6376`).
#[test]
fn array_type_paren_head_omits_hpba_placeholder() {
    use crate::syntax::{ArrayType, AstNode, Type};
    let source = "(x : (int)[])\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let arr_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ARRAY_TYPE)
        .expect("ARRAY_TYPE present");
    let arr = ArrayType::cast(arr_node).expect("ARRAY_TYPE casts");
    assert_eq!(arr.rank(), 1);
    match arr.element_type().expect("element type present") {
        Type::Paren(_) => {}
        other => panic!("element must be Paren(int), got {other:?}"),
    }

    let toks: Vec<_> = arr
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect();
    assert_eq!(
        toks,
        vec![SyntaxKind::LBRACK_TOK, SyntaxKind::RBRACK_TOK],
        "paren-headed array carries no HPBA placeholder; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.7 regression — malformed `(x : int[), y)`: the inner
/// type's `[` is closed by `)` instead of `]`. LexFilter swallows
/// the inner `)` (it matches the outer paren expression's `(`), so
/// a filtered `peek()` after the `LBRACK_TOK` returns the outer
/// tuple's `,`. The array-suffix loop must not consume that comma
/// as a rank-separator nor drag the outer `)` in as `RBRACK_TOK` /
/// `ERROR` — gating both on `next_non_trivia_raw_at_pos` (which
/// surfaces the swallowed `)`) keeps the array-suffix scope
/// confined to its real bracket span.
#[test]
fn array_suffix_does_not_consume_past_swallowed_rparen() {
    let source = "(x : int[), y)\n";
    let parse = parse(source);
    // Source is malformed; we don't care about the diagnostic shape,
    // only that the array-suffix scope is properly bounded.
    let arr = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ARRAY_TYPE);
    let Some(arr) = arr else {
        return;
    };
    let toks: Vec<_> = arr
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect();
    assert!(
        !toks.contains(&SyntaxKind::COMMA_TOK),
        "ARRAY_TYPE must not absorb the outer tuple's `,` as a rank-separator; got tokens {toks:?} in tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !toks.contains(&SyntaxKind::RBRACK_TOK),
        "ARRAY_TYPE must not absorb anything past the swallowed `)` as its `]`; got tokens {toks:?} in tree:\n{}",
        debug_tree(&parse.root),
    );
}
