//! Postfix (`int list`) and prefix (`list<int>`) type-application forms.
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.5 — `(x : int list)`: minimal postfix app. Pins the
/// `APP_TYPE > [LONG_IDENT_TYPE(int), LONG_IDENT_TYPE(list)]`
/// shape from `parse_app_type`'s checkpoint-and-wrap loop: the
/// first Type child is the arg, the second is the head. No
/// `LESS_TOK` is present so `AppType::is_postfix()` keys off the
/// 7.5 default of `true`.
#[test]
fn typed_paren_expr_with_postfix_app_type_shape() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : int list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for `int list`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts to facade");

    let arg = app.type_args();
    assert_eq!(
        arg.len(),
        1,
        "postfix app has exactly one type-arg; got tree:\n{}",
        debug_tree(&parse.root),
    );
    match &arg[0] {
        Type::LongIdent(_) => {}
        other => panic!("postfix arg must be LongIdent(int), got {other:?}"),
    }
    match app.type_name().expect("APP_TYPE has a head") {
        Type::LongIdent(_) => {}
        other => panic!("postfix head must be LongIdent(list), got {other:?}"),
    }
    assert!(app.is_postfix(), "postfix flag must be true");
    assert_lossless(source, &parse);
}

/// Phase 7.5 — `(x : int list option)`: pins left-associative
/// nesting. The outer `APP_TYPE` must contain another `APP_TYPE`
/// as its first (arg) Type child, *not* a flat list of three
/// LongIdents. Mirrors FCS's `App(option, [App(list, [int])])`.
#[test]
fn postfix_app_type_is_left_associative() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : int list option)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let app_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::APP_TYPE)
        .collect();
    assert_eq!(
        app_nodes.len(),
        2,
        "chained postfix app must produce two APP_TYPE nodes (nested, not flat); \
             got tree:\n{}",
        debug_tree(&parse.root),
    );

    // The outer node is the one whose parent is *not* an APP_TYPE.
    let outer_node = app_nodes
        .iter()
        .find(|n| n.parent().map(|p| p.kind()) != Some(SyntaxKind::APP_TYPE))
        .expect("one outer APP_TYPE must exist")
        .clone();
    let outer = AppType::cast(outer_node).expect("outer APP_TYPE casts to facade");
    let outer_args = outer.type_args();
    assert_eq!(outer_args.len(), 1, "postfix outer has one type-arg");
    assert!(
        matches!(outer_args[0], Type::App(_)),
        "outer APP_TYPE's type-arg must itself be an APP_TYPE \
             (left-associative); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.5 — `(x : int list * string list)`: pins app > tuple
/// precedence. The single `TUPLE_TYPE` must hold two `APP_TYPE`
/// children as its type segments, not the other way round (a
/// single APP_TYPE wrapping a TUPLE_TYPE arg would mean tuple
/// bound tighter than app, which it does not).
#[test]
fn tuple_of_postfix_app_types_pins_app_tighter_than_tuple() {
    use crate::syntax::{AstNode, Type};
    let source = "(x : int list * string list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let tuple = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TUPLE_TYPE)
        .expect("TUPLE_TYPE present");
    let type_children: Vec<_> = tuple.children().filter_map(Type::cast).collect();
    assert_eq!(
        type_children.len(),
        2,
        "binary TUPLE_TYPE has two Type children",
    );
    for (i, ty) in type_children.iter().enumerate() {
        assert!(
            matches!(ty, Type::App(_)),
            "tuple segment {i} must be an APP_TYPE (app > tuple); got tree:\n{}",
            debug_tree(&parse.root),
        );
    }
    assert_lossless(source, &parse);
}

/// Phase 7.5 — `(f : int -> int list)`: pins app > arrow
/// precedence. The outer `FUN_TYPE`'s return-type child must be
/// an `APP_TYPE`, not a LongIdent of `int` with a stray `list`.
/// Together with `tuple_of_postfix_app_types_pins_app_tighter_than_tuple`
/// this gives both directions of the precedence layering.
#[test]
fn postfix_app_in_fun_type_return_pins_app_tighter_than_arrow() {
    use crate::syntax::{AstNode, FunType, Type};
    let source = "(f : int -> int list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let fun = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FUN_TYPE)
        .expect("FUN_TYPE present");
    let ret = FunType::cast(fun)
        .expect("FUN_TYPE casts to facade")
        .ret()
        .expect("FUN_TYPE has a return-type child");
    assert!(
        matches!(ret, Type::App(_)),
        "FUN_TYPE's return must be an APP_TYPE (app > arrow); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.5 — `(x : int) list\n`: the head-position `int` sits
/// just before the LexFilter-swallowed `)` and the next *filtered*
/// token is the outer `list`. Without a raw-stream boundary check
/// on the postfix-head lookahead (mirroring the 7.3 arrow and 7.4
/// star gates), `parse_app_type` would absorb the outer `list` as
/// a postfix head and drag the real `)` in as `ERROR`, corrupting
/// the outer parse.
///
/// Pins: the outer PAREN_EXPR keeps its closing `)`; the outer
/// `list` does NOT become a postfix head of the inner type; no
/// APP_TYPE is emitted.
#[test]
fn app_type_post_head_lookahead_does_not_cross_swallowed_rparen() {
    let source = "(x : int) list\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : int)`");
    let outer_has_rparen = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        outer_has_rparen,
        "outer PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let stole_list_as_postfix_head = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::APP_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "list")
            })
    });
    assert!(
        !stole_list_as_postfix_head,
        "outer `list` must not be absorbed as a postfix head; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// A lambda body's trailing annotation type, followed by a fresh expression
/// statement on the next line: `let f = fun x -> x : int⏎y`. The
/// `ORIGHT_BLOCK_END` virtual closing the `->` one-sided block parks at the
/// filtered cursor just after the annotation type `int`, while the raw stream
/// skips past it to the next line's `y`. `parse_app_type`'s postfix-app loop
/// gates the continuation on the *raw* stream (so a LexFilter-swallowed `)`
/// isn't crossed — see
/// [`app_type_post_head_lookahead_does_not_cross_swallowed_rparen`]), so it
/// must *also* confirm the *filtered* cursor is itself on the postfix head;
/// otherwise it dispatches `parse_app_type_con_power` onto the parked virtual
/// and trips its `unreachable!`.
///
/// Pins: no panic; `y` is NOT absorbed as a postfix head of `int` (no
/// `APP_TYPE`); the annotation type stays the bare `int`; lossless round-trip.
#[test]
fn lambda_body_annotation_does_not_absorb_next_statement() {
    let source = "let f = fun x -> x : int\ny\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let absorbed_next_statement = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::APP_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !absorbed_next_statement,
        "next-line `y` must not be absorbed as a postfix head; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let has_int_type = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::LONG_IDENT_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "int")
            })
    });
    assert!(
        has_int_type,
        "annotation type `int` must be present as a LONG_IDENT_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : List<int>)`: minimal prefix app. Pins the
/// `APP_TYPE > [LONG_IDENT_TYPE(List), ERROR(HPA), LESS_TOK,
/// LONG_IDENT_TYPE(int), GREATER_TOK]` shape from
/// `parse_app_type`'s prefix branch. The head is the *first* Type
/// child and `is_postfix()` returns `false` because the
/// `LESS_TOK` child gates the discrimination.
#[test]
fn typed_paren_expr_with_prefix_app_type_shape() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : List<int>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for `List<int>`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts to facade");
    assert!(!app.is_postfix(), "prefix flag must be false");

    let args = app.type_args();
    assert_eq!(
        args.len(),
        1,
        "prefix app with one arg has exactly one type-arg; got tree:\n{}",
        debug_tree(&parse.root),
    );
    match &args[0] {
        Type::LongIdent(_) => {}
        other => panic!("prefix arg must be LongIdent(int), got {other:?}"),
    }
    match app.type_name().expect("APP_TYPE has a head") {
        Type::LongIdent(_) => {}
        other => panic!("prefix head must be LongIdent(List), got {other:?}"),
    }

    let has_less = app
        .syntax()
        .children_with_tokens()
        .any(|el| el.kind() == SyntaxKind::LESS_TOK);
    let has_greater = app
        .syntax()
        .children_with_tokens()
        .any(|el| el.kind() == SyntaxKind::GREATER_TOK);
    assert!(
        has_less && has_greater,
        "prefix APP_TYPE must carry LESS_TOK and GREATER_TOK"
    );
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : Dictionary<string, int>)`: pins the
/// comma-separated multi-arg shape. Two Type-args interleaved with
/// one `COMMA_TOK` between them.
#[test]
fn prefix_app_type_multiple_args() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : Dictionary<string, int>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for `Dictionary<string, int>`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts to facade");
    assert!(!app.is_postfix());

    let args = app.type_args();
    assert_eq!(
        args.len(),
        2,
        "two type-args for Dictionary<string, int>; got tree:\n{}",
        debug_tree(&parse.root),
    );
    for (i, ty) in args.iter().enumerate() {
        assert!(
            matches!(ty, Type::LongIdent(_)),
            "arg {i} must be a LongIdent, got {ty:?}",
        );
    }

    let commas = app
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMA_TOK)
        .count();
    assert_eq!(commas, 1, "one COMMA_TOK between two args");
    assert_lossless(source, &parse);
}

/// Empty type-arg list `Foo< >` — FCS's `typeArgsActual: LESS
/// GREATER` arm (`pars.fsy:6649`) yields zero args with no parse
/// error. (Adjacent `<>` lexes as the `<>` inequality operator, so
/// the empty form only arises spaced.) The prefix-app arg loop must
/// skip `parse_type` when the close `>` is already next rather than
/// recording a spurious "expected type" diagnostic.
#[test]
fn prefix_app_type_empty_type_args() {
    use crate::syntax::{AppType, AstNode};
    let source = "(x : Foo< >)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for `Foo< >`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts to facade");
    assert!(!app.is_postfix(), "prefix form carries LESS_TOK");
    assert!(
        app.type_args().is_empty(),
        "empty `< >` yields zero type-args; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let has_greater = app
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::GREATER_TOK);
    assert!(has_greater, "the close `>` is consumed as GREATER_TOK");
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : List<List<int>>)`: pins the nested-generics
/// case where LexFilter's `smash_typar_token` splits a trailing
/// `>>` into two separate `>` tokens. Two APP_TYPE nodes, both
/// prefix, with the inner one as the outer's only type-arg.
#[test]
fn prefix_app_type_nested_generics_split_trailing_greater_greater() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : List<List<int>>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let app_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::APP_TYPE)
        .collect();
    assert_eq!(
        app_nodes.len(),
        2,
        "nested generics produce two prefix APP_TYPE nodes; got tree:\n{}",
        debug_tree(&parse.root),
    );

    let outer_node = app_nodes
        .iter()
        .find(|n| n.parent().map(|p| p.kind()) != Some(SyntaxKind::APP_TYPE))
        .expect("one outer APP_TYPE")
        .clone();
    let outer = AppType::cast(outer_node).expect("outer APP_TYPE");
    assert!(!outer.is_postfix(), "outer must be prefix");

    let outer_args = outer.type_args();
    assert_eq!(outer_args.len(), 1, "outer has one arg (inner List<int>)");
    match &outer_args[0] {
        Type::App(inner) => {
            assert!(!inner.is_postfix(), "inner must also be prefix");
        }
        other => panic!("outer arg must be App, got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : Foo<int> list)`: mixed prefix + postfix.
/// The outer postfix `list` checkpoints at the same position as
/// the inner prefix `Foo<int>`, so the result nests as
/// `App(App(Foo, [int]), list)` — outer postfix wraps inner
/// prefix as its arg. Mirrors FCS's left-associative chain across
/// surface forms.
#[test]
fn mixed_prefix_postfix_app_type_nests_left_associative() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : Foo<int> list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let app_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::APP_TYPE)
        .collect();
    assert_eq!(
        app_nodes.len(),
        2,
        "mixed prefix+postfix produces two APP_TYPE nodes; got tree:\n{}",
        debug_tree(&parse.root),
    );

    let outer_node = app_nodes
        .iter()
        .find(|n| n.parent().map(|p| p.kind()) != Some(SyntaxKind::APP_TYPE))
        .expect("one outer APP_TYPE")
        .clone();
    let outer = AppType::cast(outer_node).expect("outer APP_TYPE");
    assert!(
        outer.is_postfix(),
        "outer `list` must produce a postfix APP_TYPE",
    );
    let outer_args = outer.type_args();
    assert_eq!(outer_args.len(), 1);
    match &outer_args[0] {
        Type::App(inner) => {
            assert!(
                !inner.is_postfix(),
                "inner `Foo<int>` must produce a prefix APP_TYPE",
            );
        }
        other => panic!("outer arg must wrap the inner App, got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : Foo<^T>)`: pins SRTP first-arg through the
/// LexFilter `<^` opener split. The raw lexer tokenises `<^` as
/// a single `Op("<^")`; LexFilter's `smash_typar_token` splits
/// the filtered stream into `Less(true) + Op("^")`, but the
/// raw-stream gate `raw_starts_atomic_type` only sees the
/// unsplit `Op("<^")` until both filtered halves are consumed.
/// Without explicit handling, `parse_type` would reject the
/// first arg with "expected type".
#[test]
fn prefix_app_type_with_srtp_first_arg_after_fused_less_caret() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : Foo<^T>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present");
    let app = AppType::cast(app_node).expect("APP_TYPE casts");
    assert!(!app.is_postfix());
    let args = app.type_args();
    assert_eq!(args.len(), 1);
    assert!(
        matches!(args[0], Type::Var(_)),
        "arg must be a VAR_TYPE for ^T; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : List<List<int>> option)`: nested generics
/// whose trailing `>>` is split by LexFilter, *followed* by a
/// postfix-app head. After the inner `>` is bumped, the filtered
/// cursor sits on the second `>` (the outer closer). The
/// raw-stream postfix-app lookahead must not look past the
/// pending split tail and claim `option` is the next postfix
/// head while the filtered token is still `Greater`.
#[test]
fn nested_prefix_app_with_following_postfix_does_not_skip_split_close() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : List<List<int>> option)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);

    let app_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::APP_TYPE)
        .collect();
    assert_eq!(
        app_nodes.len(),
        3,
        "expect three APP_TYPE nodes (outer postfix `option` over outer prefix `List` over \
             inner prefix `List<int>`); got tree:\n{}",
        debug_tree(&parse.root),
    );

    let outer_node = app_nodes
        .iter()
        .find(|n| n.parent().map(|p| p.kind()) != Some(SyntaxKind::APP_TYPE))
        .expect("one outer APP_TYPE")
        .clone();
    let outer = AppType::cast(outer_node).expect("outer APP_TYPE");
    assert!(outer.is_postfix(), "outer must be postfix (`option`)");
    let outer_args = outer.type_args();
    assert_eq!(outer_args.len(), 1);
    match &outer_args[0] {
        Type::App(mid) => assert!(!mid.is_postfix(), "middle must be prefix (`List<…>`)"),
        other => panic!("outer arg must wrap the middle App, got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.6 — `(x : List<int -> int>)`: pins that a full `typ`
/// (including arrows) is admitted as a type-arg inside the
/// brackets — matching FCS's `typeArgActual := typ`. The inner
/// arg must be a `FUN_TYPE`, not a `LongIdent` of `int` followed
/// by a stray `-> int`.
#[test]
fn prefix_app_type_arg_admits_function_type() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : List<int -> int>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present");
    let app = AppType::cast(app_node).expect("APP_TYPE casts");
    let args = app.type_args();
    assert_eq!(args.len(), 1, "one arg (a function type)");
    assert!(
        matches!(args[0], Type::Fun(_)),
        "arg must be a FUN_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}
