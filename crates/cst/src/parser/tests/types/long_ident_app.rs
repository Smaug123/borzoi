//! Long-ident application types with a paren/app root (`(int).Foo`, dotted continuations).
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.10 — `(x : (int).Foo)`: minimal LongIdentApp accepted
/// by FCS. The head is a paren-type (a non-`path` atomic, so the
/// dot-chain loop in `parse_atomic_type` retro-wraps it), and the
/// post-dot path is a single ident with no type-args. Green
/// shape: `LONG_IDENT_APP_TYPE > [PAREN_TYPE, DOT_TOK,
/// LONG_IDENT[Foo]]`. Pins the gate: FCS's LR tables empirically
/// admit Paren as the LHS but reject bare typar (`'T.Foo`), which
/// is why this test uses `(int).Foo` rather than `'T.Foo`.
#[test]
fn long_ident_app_type_paren_int_root_no_args() {
    use crate::syntax::{AstNode, LongIdentAppType, Type};
    let source = "(x : (int).Foo)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present for `(int).Foo`");
    let lia = LongIdentAppType::cast(node).expect("LONG_IDENT_APP_TYPE casts");
    match lia.root().expect("root present") {
        Type::Paren(_) => {}
        other => panic!("root must be Paren(int); got {other:?}"),
    }
    let path: Vec<_> = lia
        .path()
        .expect("path present")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(path, vec!["Foo".to_string()]);
    assert!(lia.type_args().is_empty(), "no type-args for bare `.Foo`");
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : (int).Foo<string>)`: LongIdentApp with
/// type-args after the path. The HPA virtual between the path's
/// last ident and the `<` is consumed as zero-width ERROR; the
/// body parses `LESS … GREATER` exactly like the prefix-app wrap.
#[test]
fn long_ident_app_type_paren_root_with_args() {
    use crate::syntax::{AstNode, LongIdentAppType, Type};
    let source = "(x : (int).Foo<string>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    let lia = LongIdentAppType::cast(node).expect("LONG_IDENT_APP_TYPE casts");
    match lia.root().expect("root present") {
        Type::Paren(_) => {}
        other => panic!("root must be Paren(int); got {other:?}"),
    }
    let path: Vec<_> = lia
        .path()
        .expect("path present")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(path, vec!["Foo".to_string()]);
    let args = lia.type_args();
    assert_eq!(args.len(), 1);
    assert!(
        matches!(args[0], Type::LongIdent(_)),
        "sole arg must be LongIdent(string); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : (int).Foo.Bar)`: multi-segment dotted path
/// on the right of a non-`path` head. The inner path-loop walks
/// the `.Bar` after the initial `.Foo`, so the `LONG_IDENT` child
/// has two ident segments.
#[test]
fn long_ident_app_type_paren_root_multi_segment_path() {
    use crate::syntax::{AstNode, LongIdentAppType};
    let source = "(x : (int).Foo.Bar)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let liau_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .collect();
    assert_eq!(
        liau_nodes.len(),
        1,
        "`(int).Foo.Bar` is one LongIdentApp with a 2-segment path, not two nested \
             LongIdentApps; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let lia = LongIdentAppType::cast(liau_nodes[0].clone()).expect("casts");
    let path: Vec<_> = lia
        .path()
        .expect("path")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(path, vec!["Foo".to_string(), "Bar".to_string()]);
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : Foo<int>.Bar)`: prefix-app head retro-wrapped
/// by the dot-chain. The HPA-prefix wrap above lifts the head
/// `Foo` into `APP_TYPE`; the dot-chain then sees `APP_TYPE` as
/// the LHS via the shared checkpoint and wraps it as the root of
/// a `LONG_IDENT_APP_TYPE`. Pins the layering: `Foo.Bar<int>` is
/// *not* the same shape (that one is plain `App(Foo.Bar, [int])`).
#[test]
fn long_ident_app_type_app_prefix_root() {
    use crate::syntax::{AstNode, LongIdentAppType, Type};
    let source = "(x : Foo<int>.Bar)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    let lia = LongIdentAppType::cast(node).expect("casts");
    let Type::App(app) = lia.root().expect("root") else {
        panic!(
            "root must be App(Foo, [int]); got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    assert!(!app.is_postfix(), "root App must be prefix `Foo<int>`");
    let path: Vec<_> = lia
        .path()
        .expect("path")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(path, vec!["Bar".to_string()]);
    assert!(lia.type_args().is_empty());
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : (int list).Foo)`: parenthesised type as the
/// LongIdentApp root. The `PAREN_TYPE` is the captured head; the
/// dot-chain loop fires after the closing paren and retro-wraps.
#[test]
fn long_ident_app_type_paren_root() {
    use crate::syntax::{AstNode, LongIdentAppType, Type};
    let source = "(x : (int list).Foo)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    let lia = LongIdentAppType::cast(node).expect("casts");
    match lia.root().expect("root") {
        Type::Paren(_) => {}
        other => panic!("root must be Paren(_); got {other:?}"),
    }
    let path: Vec<_> = lia
        .path()
        .expect("path")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(path, vec!["Foo".to_string()]);
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : (int).Foo<string>.Bar)`: chained. After the
/// first iteration wraps `(int).Foo<string>` as a
/// `LONG_IDENT_APP_TYPE`, the loop's second iteration sees that
/// node as the LHS at the same checkpoint and retro-wraps it as
/// the root of an outer `LONG_IDENT_APP_TYPE`. Pins left-
/// associative nesting.
#[test]
fn long_ident_app_type_chained() {
    use crate::syntax::{AstNode, LongIdentAppType, Type};
    let source = "(x : (int).Foo<string>.Bar)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let liau_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .collect();
    assert_eq!(
        liau_nodes.len(),
        2,
        "expected two nested LONG_IDENT_APP_TYPE nodes; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The outermost node is the one whose parent is not itself a
    // LONG_IDENT_APP_TYPE — i.e. the last-applied `.Bar` wrap.
    let outer_node = liau_nodes
        .iter()
        .find(|n| n.parent().map(|p| p.kind()) != Some(SyntaxKind::LONG_IDENT_APP_TYPE))
        .expect("one outer LongIdentApp")
        .clone();
    let outer = LongIdentAppType::cast(outer_node).expect("outer casts");
    let outer_path: Vec<_> = outer
        .path()
        .expect("outer path")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(outer_path, vec!["Bar".to_string()]);
    let Type::LongIdentApp(inner) = outer.root().expect("outer root") else {
        panic!(
            "outer root must wrap the inner LongIdentApp; got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    let inner_path: Vec<_> = inner
        .path()
        .expect("inner path")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(inner_path, vec!["Foo".to_string()]);
    match inner.root().expect("inner root") {
        Type::Paren(_) => {}
        other => panic!("inner root must be Paren(int); got {other:?}"),
    }
    assert_eq!(inner.type_args().len(), 1, "inner has `<string>`");
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : (int).Foo list)`: postfix-app outside the
/// LongIdentApp. The dot-chain loop runs at the atomic layer and
/// returns; control resumes in `parse_app_type`, which then sees
/// `list` and wraps the `LONG_IDENT_APP_TYPE` into a postfix
/// `APP_TYPE` from the *outer* checkpoint. Pins the layering
/// against the reverse misparse (`LongIdentApp(App(list, …),
/// Foo)`).
#[test]
fn long_ident_app_type_under_postfix() {
    use crate::syntax::{AppType, AstNode, LongIdentAppType, Type};
    let source = "(x : (int).Foo list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for the postfix `list`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts");
    assert!(app.is_postfix(), "outer must be postfix `T list`");
    let args = app.type_args();
    assert_eq!(args.len(), 1);
    let Type::LongIdentApp(lia) = &args[0] else {
        panic!(
            "App's sole arg must be the LongIdentApp; got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    let _ = LongIdentAppType::cast(lia.syntax().clone());
    match lia.root().expect("inner root") {
        Type::Paren(_) => {}
        other => panic!("inner root must be Paren(int); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : 'T.Foo)`: FCS's LR tables empirically
/// reject bare typar followed by `.path` (the parser reduces past
/// `atomType` before the DOT-shift state, then errors on the
/// trailing `.Foo`). Our parser must mirror that: no
/// `LONG_IDENT_APP_TYPE` for this surface, and the trailing
/// `.Foo` must surface as a parse error rather than being
/// silently absorbed. Pins the `head_can_chain = false` gate
/// for the Quote/Hat (typar) arm of `parse_atomic_type`.
#[test]
fn long_ident_app_type_typar_root_does_not_chain() {
    let source = "(x : 'T.Foo)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "bare-typar `.path` head must error; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE),
        "bare typar must not produce LONG_IDENT_APP_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : Foo.Bar.Baz)`: a plain dotted path does *not*
/// produce a `LongIdentApp` — `parse_app_type_con_power` eagerly
/// walks the `DOT IDENT` chain into a single `LONG_IDENT_TYPE`
/// before the dot-chain loop ever runs. Pins the layering split
/// between the two methods.
#[test]
fn long_ident_app_type_plain_path_is_not_long_ident_app() {
    let source = "(x : Foo.Bar.Baz)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE),
        "plain dotted path must not produce LONG_IDENT_APP_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let li_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_TYPE)
        .expect("LONG_IDENT_TYPE present");
    let inner = li_node
        .children()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT)
        .expect("LONG_IDENT child");
    let path: Vec<_> = inner
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(
        path,
        vec!["Foo".to_string(), "Bar".to_string(), "Baz".to_string()],
    );
    assert_lossless(source, &parse);
}

/// Phase 7.10 recovery — `(x : (int).Foo.)`: trailing dot inside the
/// dot-chain's inner path-loop. The parser must push a
/// "trailing dot in long identifier path" error and *not* panic,
/// then leave the LongIdentApp in place with its partial path.
/// Mirrors `parse_app_type_con_power`'s same recovery shape.
///
/// Uses `(int)` as the head because FCS's LR tables only admit
/// `atomType DOT path` when the LHS is Paren / Anon / HPA-wrapped App
/// (the typar-rooted form is rejected — see
/// [`Self::long_ident_app_type_typar_root_does_not_chain`]).
#[test]
fn long_ident_app_type_trailing_dot_recovers() {
    let source = "(x : (int).Foo.)\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("trailing dot in long identifier path")),
        "expected trailing-dot error, got: {:?}",
        parse.errors,
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE),
        "LongIdentApp node must still be present after recovery; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.10 regression — `let f x = (x : (int).Foo) < y`: the
/// outer `<` belongs to the enclosing comparison expression, not
/// to a deprecated `typeArgsNoHpaDeprecated` block on the type
/// annotation. LexFilter swallows the typed-expr `)` so the
/// filtered cursor reaches the outer `<` while the raw cursor is
/// still at `RParen`. Without a raw-stream adjacency gate on the
/// optional `< ... >` arm, the parser drains the `)` as ERROR
/// and consumes `y` as a type-arg. Pins the raw-stream gate
/// (`raw_pos` at `<`) before accepting `< … >` after the path.
#[test]
fn long_ident_app_type_outer_less_than_not_type_args() {
    let source = "let f x = (x : (int).Foo) < y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let lia = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    // The LongIdentApp must NOT contain a LESS_TOK / GREATER_TOK
    // child: the outer `<` belongs to the comparison, not to the
    // type's optional `<...>` arm.
    assert!(
        !lia.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::LESS_TOK),
        "LongIdentApp must not absorb the outer `<`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // And `y` must remain available for the enclosing
    // expression: it appears as a token *outside* the type
    // annotation. The whole binding must parse cleanly.
    assert_lossless(source, &parse);
}

/// Phase 7.10 regression — `let f x = (x : (int).Foo<Bar) > y`: the
/// closing `>` of the type-arg list belongs to the enclosing
/// comparison, not to the `<Bar …>` block. LexFilter swallows the
/// typed-expr `)`, so once `Bar` is parsed the filtered cursor
/// already exposes the outer `>` while the raw cursor is still at
/// `RParen`. The `GREATER` bump inside the `< … >` arm must gate on
/// the raw stream (the next raw token is `)`, not `>`): otherwise it
/// drains the `)` as a text-carrying ERROR and steals the outer `>`.
#[test]
fn long_ident_app_type_inner_greater_outside_paren_not_consumed() {
    let source = "let f x = (x : (int).Foo<Bar) > y\n";
    let parse = parse(source);
    let lia = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    // The outer `>` must not be absorbed as the list's closing
    // `GREATER_TOK`.
    assert!(
        !lia.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::GREATER_TOK),
        "LongIdentApp must not absorb the outer `>`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The swallowed `)` must not be drained as a text-carrying ERROR.
    assert_no_nonempty_error_tokens(&parse);
    assert_lossless(source, &parse);
}

/// Phase 7.10 regression — `let f x = (x : (int).Foo<Bar) , y`: the
/// comma separates the enclosing tuple, not the type-arg list. Same
/// swallowed-`)` shape as
/// [`Self::long_ident_app_type_inner_greater_outside_paren_not_consumed`]:
/// after `Bar` the filtered cursor is at the outer `,` while the raw
/// cursor is at `RParen`. The `COMMA` bump inside the `< … >` arm
/// must gate on the raw stream so it doesn't drain the `)` as ERROR
/// and pull `y` in as a second type argument.
#[test]
fn long_ident_app_type_inner_comma_outside_paren_not_consumed() {
    let source = "let f x = (x : (int).Foo<Bar) , y\n";
    let parse = parse(source);
    let lia = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    // The outer `,` must not be absorbed as a type-arg separator.
    assert!(
        !lia.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::COMMA_TOK),
        "LongIdentApp must not absorb the outer `,`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_no_nonempty_error_tokens(&parse);
    assert_lossless(source, &parse);
}

/// Phase 7.10 — `(x : (int).Foo< >)`: empty type-arg list on a
/// dot-chain. FCS's `typeArgsNoHpaDeprecated → typeArgsActual` admits
/// a `LESS GREATER` arm (`pars.fsy:6649`) producing zero args with no
/// error. (Adjacent `<>` fuses into the `<>` inequality operator, so
/// the empty form only arises spaced.) The `< … >` arm must skip the
/// arg loop when `>` is already next instead of recording a spurious
/// "expected type" diagnostic.
#[test]
fn long_ident_app_type_empty_type_args() {
    use crate::syntax::{AstNode, LongIdentAppType};
    let source = "(x : (int).Foo< >)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let lia_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present for `(int).Foo< >`");
    let lia = LongIdentAppType::cast(lia_node).expect("casts to facade");
    assert!(
        lia.type_args().is_empty(),
        "empty `< >` yields zero type-args; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let toks: Vec<_> = lia
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| t.kind())
        .collect();
    assert!(
        toks.contains(&SyntaxKind::LESS_TOK) && toks.contains(&SyntaxKind::GREATER_TOK),
        "the `< >` pair is consumed as LESS_TOK + GREATER_TOK; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.10 regression — `let f x = (x : (int).Foo.) y`: a
/// trailing dot in the *inner* path loop, followed by a swallowed
/// typed-expr `)`. LexFilter drops the `)`, so after the inner dot
/// is bumped the filtered cursor exposes the outer `y` while the
/// raw cursor is still at `RParen`. Without the same raw-after-dot
/// boundary gate the entry dot uses, the loop steals `y` as a path
/// `IDENT_TOK` and drains the real `)` as `ERROR`. Pins the gate:
/// `y` must stay outside the type, the path is just `[Foo]`, and a
/// trailing-dot error is reported.
#[test]
fn long_ident_app_type_inner_trailing_dot_swallowed_paren_no_steal() {
    let source = "let f x = (x : (int).Foo.) y\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("trailing dot in long identifier path")),
        "expected trailing-dot error, got: {:?}",
        parse.errors,
    );
    let lia = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
        .expect("LONG_IDENT_APP_TYPE present");
    use crate::syntax::{AstNode, LongIdentAppType};
    let segs: Vec<_> = LongIdentAppType::cast(lia)
        .expect("casts")
        .path()
        .expect("path present")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(
        segs,
        vec!["Foo".to_string()],
        "path must stop at the trailing dot, not steal the outer `y`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The real `)` must not be drained as ERROR. A drained token
    // carries its source text (`drain_raw_up_to` → `emit_text`), so
    // a non-empty ERROR is the corruption; the swallowed-`)` paren
    // recovery legitimately leaves *zero-width* ERROR markers even
    // for a clean parse, so those are allowed.
    assert_no_nonempty_error_tokens(&parse);
    assert_lossless(source, &parse);
}

/// Phase 7.10 regression — layout break between the leading `.`
/// and the path's first ident inside an anon-record field
/// (`{| F : (int).` newline `Foo |}`, with `Foo` at the field's
/// offside column). LexFilter inserts a `Virtual(BlockSep)`
/// between the `.` and `Foo`, so after the dot-chain bumps the
/// dot, `peek()` is the BlockSep while the raw cursor is at the
/// ident. The first ident-bump must not consume that layout
/// virtual as `IDENT_TOK`: no path token in the tree may be a
/// zero-width (virtual-backed) emission.
#[test]
fn long_ident_app_type_dot_then_layout_break_no_virtual_consumed() {
    // `Foo` sits at column 14, the same column as field `F`, so
    // the offside rule emits a `BlockSep` before it (a deeper
    // indent would make it a continuation with no virtual).
    let source = "let f (x : {| F : (int).\n              Foo |}) = x\n";
    let parse = parse(source);
    assert_no_empty_path_tokens(&parse);
    assert_lossless(source, &parse);
}

/// Phase 7.10 regression — layout break between a consumed path
/// segment and a continuation `.` inside an anon-record field
/// (`{| F : (int).Foo` newline `.Bar |}`, with `.Bar` at the
/// field's offside column). LexFilter inserts a `Virtual(BlockSep)`
/// between `Foo` and the second `.`, so the inner `DOT IDENT` loop
/// sees the raw `.` ahead while `peek()` is the BlockSep. The loop
/// must break at the layout boundary rather than bumping the
/// virtual as `DOT_TOK` — FCS itself rejects this surface
/// ("Unexpected symbol '.' in field declaration"), but the
/// recovery must not corrupt the tree.
#[test]
fn long_ident_app_type_segment_then_layout_break_dot_not_consumed() {
    // `.Bar` sits at column 14 (field `F`'s column) → `BlockSep`.
    let source = "let f (x : {| F : (int).Foo\n              .Bar |}) = x\n";
    let parse = parse(source);
    assert_no_empty_path_tokens(&parse);
    // `.Bar` is past the layout boundary, so the LongIdentApp's
    // path must be just `[Foo]`.
    if let Some(node) = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_APP_TYPE)
    {
        use crate::syntax::{AstNode, LongIdentAppType};
        let lia = LongIdentAppType::cast(node).expect("casts");
        let segs: Vec<_> = lia
            .path()
            .expect("path present")
            .idents()
            .map(|t| t.text().to_string())
            .collect();
        assert_eq!(
            segs,
            vec!["Foo".to_string()],
            "path must stop at the layout boundary; got tree:\n{}",
            debug_tree(&parse.root),
        );
    }
    assert_lossless(source, &parse);
}

/// Same layout-virtual hazard as
/// [`Self::long_ident_app_type_segment_then_layout_break_dot_not_consumed`],
/// but for the plain long-ident path loop in
/// [`Parser::parse_app_type_con_power`] (`Foo` newline `.Bar` at
/// the field's offside column). The `BlockSep` between `Foo` and
/// the `.` must not be bumped as a zero-width `DOT_TOK`.
#[test]
fn long_ident_type_segment_then_layout_break_dot_not_consumed() {
    let source = "let f (x : {| F : Foo\n              .Bar |}) = x\n";
    let parse = parse(source);
    assert_no_empty_path_tokens(&parse);
    assert_lossless(source, &parse);
}

/// Same swallowed-`)` hazard as
/// [`Self::long_ident_app_type_inner_trailing_dot_swallowed_paren_no_steal`],
/// but for the plain long-ident path loop in
/// [`Parser::parse_app_type_con_power`]: `let f x = (x : Foo.) y`.
/// After the trailing dot is bumped, LexFilter's swallowed typed-
/// expr `)` exposes the outer `y` at the filtered cursor while the
/// raw stream still has the `)`. The loop must report a trailing
/// dot and break rather than stealing `y` and draining the real
/// `)` as ERROR.
#[test]
fn long_ident_type_inner_trailing_dot_swallowed_paren_no_steal() {
    let source = "let f x = (x : Foo.) y\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("trailing dot in long identifier path")),
        "expected trailing-dot error, got: {:?}",
        parse.errors,
    );
    let path: Vec<_> = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT_TYPE)
        .expect("LONG_IDENT_TYPE present")
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(
        path,
        vec!["Foo".to_string()],
        "path must stop at the trailing dot, not steal the outer `y`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_no_nonempty_error_tokens(&parse);
    assert_lossless(source, &parse);
}
