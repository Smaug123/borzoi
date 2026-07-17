//! Function-arrow (`a -> b`) and tuple (`a * b`) types, and their precedence interaction.
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.3 — `(f : int -> string)`: single function arrow. Pins
/// the `FUN_TYPE > [LONG_IDENT_TYPE, RARROW_TOK, LONG_IDENT_TYPE]`
/// shape produced by `parse_type`'s checkpoint-and-wrap path.
#[test]
fn typed_paren_expr_with_fun_type_shape() {
    let source = "(f : int -> string)\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "expected no parser errors; got {:?}\n{}",
        parse.errors,
        debug_tree(&parse.root),
    );
    let fun_type = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FUN_TYPE)
        .expect("FUN_TYPE present for `int -> string`");

    let type_children: Vec<_> = fun_type
        .children()
        .filter(|c| {
            matches!(
                c.kind(),
                SyntaxKind::LONG_IDENT_TYPE
                    | SyntaxKind::PAREN_TYPE
                    | SyntaxKind::ANON_TYPE
                    | SyntaxKind::VAR_TYPE
                    | SyntaxKind::FUN_TYPE
            )
        })
        .collect();
    assert_eq!(
        type_children.len(),
        2,
        "FUN_TYPE has two Type children; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_eq!(type_children[0].kind(), SyntaxKind::LONG_IDENT_TYPE);
    assert_eq!(type_children[1].kind(), SyntaxKind::LONG_IDENT_TYPE);

    let arrow = fun_type
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::RARROW_TOK)
        .expect("RARROW_TOK present inside FUN_TYPE");
    assert_eq!(arrow.text(), "->");

    assert_lossless(source, &parse);
}

/// Phase 7.3 — `(f : int ->) y\n`: the `->` sigil sits just before
/// the swallowed `)` and the next *filtered* token is the outer
/// `y`. Without a raw-stream boundary check on the arrow lookahead
/// (mirroring the 7.2 typar-ident fix), `parse_type` would treat
/// the outer `y` as the return type and drag the real `)` in as
/// `ERROR`, corrupting the outer parse.
///
/// Pins: the outer PAREN_EXPR keeps its closing `)`; the outer `y`
/// does NOT become a return-type ident inside the type annotation;
/// a parser error is recorded for the missing return type.
#[test]
fn fun_type_arrow_lookahead_does_not_cross_swallowed_rparen() {
    let source = "(f : int ->) y\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(f : int ->)`");
    let outer_has_rparen = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        outer_has_rparen,
        "outer PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let stole_y_as_return_type = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::FUN_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y_as_return_type,
        "outer `y` must not be absorbed as the return-type ident; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a parser error for missing return-type after `->`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.4 — `(x : int * string)`: binary tuple type. Pins the
/// `TUPLE_TYPE > [LONG_IDENT_TYPE, STAR_TOK, LONG_IDENT_TYPE]`
/// shape from `parse_tuple_type`'s checkpoint-and-wrap loop. The
/// flat-segment invariant (no nested pairs) is verified separately
/// in `tuple_type_is_flat_for_ternary`.
#[test]
fn typed_paren_expr_with_tuple_type_shape() {
    use crate::syntax::{AstNode, Type};
    let source = "(x : int * string)\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "expected no parser errors; got {:?}\n{}",
        parse.errors,
        debug_tree(&parse.root),
    );
    let tuple = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TUPLE_TYPE)
        .expect("TUPLE_TYPE present for `int * string`");

    let type_children: Vec<_> = tuple
        .children()
        .filter(|c| Type::can_cast(c.kind()))
        .collect();
    assert_eq!(
        type_children.len(),
        2,
        "binary TUPLE_TYPE has two Type children; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_eq!(type_children[0].kind(), SyntaxKind::LONG_IDENT_TYPE);
    assert_eq!(type_children[1].kind(), SyntaxKind::LONG_IDENT_TYPE);

    let stars: Vec<_> = tuple
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::STAR_TOK)
        .collect();
    assert_eq!(
        stars.len(),
        1,
        "binary TUPLE_TYPE has one STAR_TOK separator; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_eq!(stars[0].text(), "*");

    assert_lossless(source, &parse);
}

/// Phase 7.4 — `(x : int * string * bool)`: pins the flat-segment
/// invariant. The single `TUPLE_TYPE` must contain three Type
/// children and two `STAR_TOK`s in source order, *not* a nested
/// `Tuple(int, Tuple(string, bool))` shape. Matches FCS's
/// `SynTupleTypeSegment` list which is itself flat.
#[test]
fn tuple_type_is_flat_for_ternary() {
    use crate::syntax::{AstNode, Type};
    let source = "(x : int * string * bool)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let tuples: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::TUPLE_TYPE)
        .collect();
    assert_eq!(
        tuples.len(),
        1,
        "ternary tuple must produce exactly one TUPLE_TYPE (flat, not nested); \
             got tree:\n{}",
        debug_tree(&parse.root),
    );

    let tuple = &tuples[0];
    let type_children: Vec<_> = tuple
        .children()
        .filter(|c| Type::can_cast(c.kind()))
        .collect();
    let stars: Vec<_> = tuple
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::STAR_TOK)
        .collect();
    assert_eq!(type_children.len(), 3, "ternary path has three types");
    assert_eq!(stars.len(), 2, "ternary path has two STAR_TOK separators");
    assert_lossless(source, &parse);
}

/// Phase 7.4 — `(f : int * int -> int)`: `*` binds tighter than
/// `->`, so the LHS of `FUN_TYPE` is the `TUPLE_TYPE`. Pins the
/// precedence layering: `parse_type` recurses through
/// `parse_tuple_type` before checking for the arrow.
#[test]
fn tuple_binds_tighter_than_fun_arrow() {
    let source = "(f : int * int -> int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let fun = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FUN_TYPE)
        .expect("outer FUN_TYPE present");
    use crate::syntax::AstNode;
    let arg = crate::syntax::FunType::cast(fun)
        .expect("FUN_TYPE casts to facade")
        .arg()
        .expect("outer FUN_TYPE has an argument-type child");
    assert!(
        matches!(arg, crate::syntax::Type::Tuple(_)),
        "FUN_TYPE's argument must be a Tuple (precedence pinned); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.4 — `(x : int *) y\n`: the `*` sigil sits just before
/// the swallowed `)` and the next *filtered* token is the outer
/// `y`. Without a raw-stream boundary check after the `*` (same
/// shape as the 7.3 arrow boundary fix), `parse_tuple_type` would
/// absorb the outer `y` as the next segment and drag the real `)`
/// in as `ERROR`, corrupting the outer parse.
///
/// Pins: the outer PAREN_EXPR keeps its closing `)`; the outer
/// `y` does NOT become a tuple segment; a parser error is
/// recorded for the missing post-`*` type.
#[test]
fn tuple_type_post_star_lookahead_does_not_cross_swallowed_rparen() {
    let source = "(x : int *) y\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : int *)`");
    let outer_has_rparen = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        outer_has_rparen,
        "outer PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let stole_y_as_segment = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::TUPLE_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y_as_segment,
        "outer `y` must not be absorbed as a tuple segment; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a parser error for missing type after `*`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// `struct (int * string)` — the struct-tuple type. The single `TUPLE_TYPE`
/// carries the leading `STRUCT_TOK` (read by `TupleType::is_struct`) and the
/// `(`/`)` directly (no `Paren` wrapper); its `segments()` is the same flat
/// `[Type, Star, Type]` a plain tuple yields.
#[test]
fn struct_tuple_type_shape() {
    use crate::syntax::{AstNode, TupleType};
    let source = "(x : struct (int * string))\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let tuple_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TUPLE_TYPE)
        .expect("TUPLE_TYPE for `struct (int * string)`");
    let tuple = TupleType::cast(tuple_node).expect("TupleType");
    assert!(tuple.is_struct(), "is_struct must be true; got: {tuple:?}");
    let type_segs = tuple
        .segments()
        .into_iter()
        .filter(|s| matches!(s, crate::syntax::TupleSegment::Type(_)))
        .count();
    assert_eq!(type_segs, 2, "two element types under one flat TUPLE_TYPE");
    // The `struct`/parens are kept as tokens but are not segments.
    assert!(
        tuple
            .syntax()
            .children_with_tokens()
            .any(|el| el.kind() == SyntaxKind::STRUCT_TOK),
        "the STRUCT_TOK marker is kept under the node",
    );
    assert_lossless(source, &parse);
}

/// `struct (int)` — a single-element struct tuple. FCS rejects it ("a struct
/// tuple needs ≥2 elements"); we mirror with a clean error and a lossless tree
/// (the `TUPLE_TYPE` is still built, with `is_struct` set), never a panic.
#[test]
fn struct_tuple_type_single_element_is_clean_error() {
    let source = "(x : struct (int))\n";
    let parse = parse(source);
    assert!(
        parse.errors.iter().any(|e| e
            .message
            .contains("struct tuple type needs at least two elements")),
        "expected the ≥2-element error; got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// `struct (int / string)` — a `/` divisor as the *first* separator. FCS's
/// production mandates a leading `*` (`STRUCT LPAREN appType STAR …`), so the
/// `/`-first form is a parse error; we mirror it (the `/` is still consumed for
/// losslessness) rather than silently accepting it as a struct tuple.
#[test]
fn struct_tuple_type_slash_first_separator_is_clean_error() {
    let source = "(x : struct (int / string))\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("first separator must be `*`")),
        "expected the first-separator-`*` error; got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Deeply nested struct tuples re-enter `parse_struct_tuple_type` through
/// `parse_atomic_type` below `parse_type`'s own guard; the dispatch wraps each
/// level in `with_depth`, so a pathological nest hits `MAX_PARSE_DEPTH` and
/// drains to EOF instead of overflowing the stack. Pins that the parse
/// terminates (no panic) and stays lossless.
#[test]
fn struct_tuple_type_deep_nesting_does_not_overflow() {
    let depth = 2000;
    let mut source = String::from("(x : ");
    for _ in 0..depth {
        source.push_str("struct (");
    }
    source.push_str("int * int");
    for _ in 0..depth {
        source.push_str(" * int)");
    }
    source.push_str(")\n");
    // The assertion is simply that this returns (no stack overflow / panic).
    let parse = parse(&source);
    assert_lossless(&source, &parse);
}

/// `(x : int *) struct (int * int)` — a `*` just before the swallowed `)` of the
/// inner annotation, with `struct (` on the *outside*. The struct-tuple
/// lookahead (`peek_starts_struct_tuple_type`) is raw-aligned, so the
/// tuple-separator recovery does not see the outer `struct` through the
/// swallowed `)`: the outer `)` is kept and no struct tuple is built across it.
#[test]
fn struct_tuple_lookahead_does_not_cross_swallowed_rparen() {
    let source = "(x : int *) struct (int * int)\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : int *)`");
    assert!(
        paren
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::RPAREN_TOK),
        "outer PAREN_EXPR must keep its `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// `(x : struct ()) y` — an empty (invalid) struct tuple whose `)` is swallowed,
/// followed by the outer `y`. The first-element gate consults the raw stream
/// *before* draining (like the `PAREN_TYPE` arm), so the swallowed close is not
/// crossed: the outer `y` is not stolen as the first element and the parse stays
/// lossless with a recovery error.
#[test]
fn empty_struct_tuple_does_not_steal_outer_token() {
    let source = "(x : struct ()) y\n";
    let parse = parse(source);
    let stole_y = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::TUPLE_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y,
        "outer `y` must not be absorbed as a struct-tuple element; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a recovery error for the empty struct tuple; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}
