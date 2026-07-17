//! Nullable types (`string | null`).
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.11 — `(x : string | null)`: the minimal nullable type.
/// FCS's `appTypeCanBeNullable: appTypeWithoutNull
/// BAR_JUST_BEFORE_NULL NULL` (`pars.fsy:6357`) projects to
/// `SynType.WithNull(LongIdent string, false, _, { BarRange })`.
/// Green shape: `WITH_NULL_TYPE > [LONG_IDENT_TYPE[string], BAR_TOK,
/// NULL_TOK]`. The inner type is the sole `Type` child; the `|` is
/// exposed via `bar_token`.
#[test]
fn with_null_type_minimal() {
    use crate::syntax::{AstNode, Type, WithNullType};
    let source = "(x : string | null)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("WITH_NULL_TYPE present for `string | null`");
    let wn = WithNullType::cast(node).expect("WITH_NULL_TYPE casts");
    match wn.inner().expect("inner present") {
        Type::LongIdent(_) => {}
        other => panic!("inner must be LongIdent(string); got {other:?}"),
    }
    assert_eq!(
        wn.bar_token().expect("bar token present").text(),
        "|",
        "the BAR_TOK child must be the `|`",
    );
    assert_lossless(source, &parse);
}

/// Phase 7.11 — `(x : int list | null)`: the postfix application
/// binds *inside* the nullable wrap, because FCS's nullable
/// production wraps an `appTypeWithoutNull` (which already includes
/// the postfix `list`). So the inner type is the postfix
/// `APP_TYPE`, not the other way around.
#[test]
fn with_null_type_over_postfix_app() {
    use crate::syntax::{AstNode, Type, WithNullType};
    let source = "(x : int list | null)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("WITH_NULL_TYPE present");
    let wn = WithNullType::cast(node).expect("casts");
    let Type::App(app) = wn.inner().expect("inner") else {
        panic!(
            "inner must be App(list, [int], postfix); got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    assert!(app.is_postfix(), "inner App must be postfix `int list`");
    assert_lossless(source, &parse);
}

/// Phase 7.11 — `(x : string | null * int)`: the nullable binds
/// tighter than the tuple `*` (FCS's `tupleType:
/// appTypeCanBeNullable STAR …`), so the first tuple element is the
/// `WITH_NULL_TYPE`, not the whole `string | (null * int)`. Pins the
/// layering: `WITH_NULL_TYPE` is a *child* of `TUPLE_TYPE`.
#[test]
fn with_null_type_first_tuple_element() {
    let source = "(x : string | null * int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let wn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("WITH_NULL_TYPE present");
    assert_eq!(
        wn.parent().map(|p| p.kind()),
        Some(SyntaxKind::TUPLE_TYPE),
        "WITH_NULL_TYPE must be the first tuple element; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.11 — `(x : int * string | null)`: the nullable applies to
/// the *second* tuple element. Mirrors
/// [`Self::with_null_type_first_tuple_element`] but exercises the
/// post-`*` element call site in `parse_tuple_type`'s loop.
#[test]
fn with_null_type_second_tuple_element() {
    let source = "(x : int * string | null)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let wn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("WITH_NULL_TYPE present");
    assert_eq!(
        wn.parent().map(|p| p.kind()),
        Some(SyntaxKind::TUPLE_TYPE),
        "WITH_NULL_TYPE must sit directly under the TUPLE_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.11 — `(x : string | null -> int)`: the nullable binds
/// tighter than the arrow (FCS's `typ: tupleType RARROW typ`, and
/// `tupleType` reaches `appTypeCanBeNullable`). So the function
/// argument is the `WITH_NULL_TYPE`; it sits directly under the
/// `FUN_TYPE`.
#[test]
fn with_null_type_arrow_operand() {
    let source = "(x : string | null -> int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let wn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("WITH_NULL_TYPE present");
    assert_eq!(
        wn.parent().map(|p| p.kind()),
        Some(SyntaxKind::FUN_TYPE),
        "WITH_NULL_TYPE must be the arrow's argument type; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.11 — `(x : Foo<string | null>)`: a nullable type as a
/// generic type-argument. FCS's type-arg list is `typ`, which
/// reaches `appTypeCanBeNullable`, so `string | null` is admitted
/// inside the `<…>`. The inner of the WithNull is `LongIdent`.
#[test]
fn with_null_type_in_generic_arg() {
    use crate::syntax::{AstNode, Type, WithNullType};
    let source = "(x : Foo<string | null>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("WITH_NULL_TYPE present inside `<…>`");
    let wn = WithNullType::cast(node).expect("casts");
    match wn.inner().expect("inner") {
        Type::LongIdent(_) => {}
        other => panic!("inner must be LongIdent(string); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.11 — `(x : string | int)`: a bare `|` *not* followed by
/// `null` must not trigger the nullable wrap (FCS only relabels
/// `BAR`→`BAR_JUST_BEFORE_NULL` when the next token is `NULL`). We
/// don't pin the exact recovery diagnostics here — only that no
/// `WITH_NULL_TYPE` node is produced and the parser doesn't panic.
#[test]
fn with_null_not_triggered_without_null() {
    let source = "(x : string | int)\n";
    let parse = parse(source);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE),
        "`string | int` must not produce WITH_NULL_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 7.11 — the nullable wrap must not reach *across* a
/// LexFilter-swallowed `)`. In `(x : string) | null` the `)` closes
/// the typed-paren annotation and LexFilter swallows it, so after the
/// inner `string` the *filtered* cursor already points at the outer
/// `|` (the swallowed `)` is gone from the filtered stream but still
/// present in the raw stream). Gating only on the filtered `peek`
/// would wrap `string` as `WITH_NULL_TYPE` and drain the real `)` as
/// an `ERROR`, stealing `| null` into the annotation. The raw-stream
/// gate (the next *raw* non-trivia token must itself be `|`) rejects
/// this: the raw token after `string` is `)`, not `|`. We pin only
/// the structural property (no `WITH_NULL_TYPE`), not recovery
/// diagnostics.
#[test]
fn with_null_not_triggered_across_swallowed_paren() {
    let source = "(x : string) | null\n";
    let parse = parse(source);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE),
        "`(x : string) | null` must not wrap the annotation as WITH_NULL_TYPE \
             across the swallowed `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
}
