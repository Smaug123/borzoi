//! Constraint-intersection types (`#A & #B`, `'T & #A`) — FCS's
//! `SynType.Intersection` (`intersectionType`, `pars.fsy:6328-6335`, phase
//! 10.10). One submodule per `parse_type` grammar form, mirroring the
//! sibling type-test files.

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Helper: the sole `INTERSECTION_TYPE` node in a parse, cast to the facade.
fn intersection(parse: &crate::parser::Parse) -> crate::syntax::IntersectionType {
    use crate::syntax::{AstNode, IntersectionType};
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::INTERSECTION_TYPE)
        .unwrap_or_else(|| {
            panic!(
                "INTERSECTION_TYPE present; got tree:\n{}",
                debug_tree(&parse.root)
            )
        });
    IntersectionType::cast(node).expect("INTERSECTION_TYPE casts")
}

/// Phase 10.10 — `(x : #A & #B)`: the minimal `hashConstraint AMP …` form.
/// `typar` is `None` and both operands (including the leading `#A`) are in
/// `types`. Green shape: `INTERSECTION_TYPE > [HASH_CONSTRAINT_TYPE, AMP_TOK,
/// HASH_CONSTRAINT_TYPE]`.
#[test]
fn intersection_two_hash_minimal() {
    use crate::syntax::Type;
    let source = "(x : #A & #B)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    assert!(i.typar().is_none(), "hash-head form has no head typar");
    let types: Vec<Type> = i.types().collect();
    assert_eq!(
        types.len(),
        2,
        "two operands; got tree:\n{}",
        debug_tree(&parse.root)
    );
    assert!(
        types.iter().all(|t| matches!(t, Type::Hash(_))),
        "both operands are hash constraints; got {types:?}",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : #A & #B & #C)`: three operands flatten into one
/// `types` list (no nesting).
#[test]
fn intersection_three_hash() {
    let source = "(x : #A & #B & #C)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    assert_eq!(intersection(&parse).types().count(), 3);
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : 'T & #A)`: the `typar AMP …` form. The head typar `'T`
/// lands in the dedicated `typar` slot (`is_head_type() == false`) and is
/// *excluded* from `types`, leaving the single `#A` operand.
#[test]
fn intersection_typar_head() {
    use crate::syntax::Type;
    let source = "(x : 'T & #A)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    let typar = i.typar().expect("head typar present for `'T & …`");
    assert_eq!(typar.ident().expect("typar ident").text(), "T");
    assert!(
        !typar.is_head_type(),
        "`'T` is the plain (non-head) typar form"
    );
    let types: Vec<Type> = i.types().collect();
    assert_eq!(types.len(), 1, "the head typar is not in `types`");
    assert!(
        matches!(types[0], Type::Hash(_)),
        "the lone operand is `#A`"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : ^T & #A)`: a head-typar (`^`-sigil, statically-resolved)
/// head. `is_head_type()` is `true`.
#[test]
fn intersection_head_typar_sigil() {
    let source = "(x : ^T & #A)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let typar = intersection(&parse).typar().expect("head typar present");
    assert!(typar.is_head_type(), "`^T` is the head-typar form");
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : #Foo<int> & #B)`: a hash head whose inner carries a
/// prefix-app. The `<int>` stays *inside* the `#…`, so the head is still a
/// bare `HASH_CONSTRAINT_TYPE` and the intersection fires; `typar` is `None`.
#[test]
fn intersection_hash_prefix_app_head() {
    use crate::syntax::Type;
    let source = "(x : #Foo<int> & #B)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    assert!(i.typar().is_none());
    let types: Vec<Type> = i.types().collect();
    assert_eq!(types.len(), 2);
    assert!(
        matches!(types[0], Type::Hash(_)),
        "head is a hash constraint"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : #A & #B -> int)`: the intersection binds tighter than
/// the arrow (FCS layers `intersectionType` at `appTypeWithoutNull`, below
/// `typ: tupleType RARROW typ`), so it is the function *argument* and sits
/// directly under the `FUN_TYPE`.
#[test]
fn intersection_arrow_operand() {
    use crate::syntax::AstNode;
    let source = "(x : #A & #B -> int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    assert_eq!(
        i.syntax().parent().map(|p| p.kind()),
        Some(SyntaxKind::FUN_TYPE),
        "INTERSECTION_TYPE must be the arrow's argument; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : #A & #B * int)`: the intersection binds tighter than
/// the tuple `*`, so it is the first tuple segment (a child of `TUPLE_TYPE`).
#[test]
fn intersection_tuple_segment() {
    use crate::syntax::AstNode;
    let source = "(x : #A & #B * int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    assert_eq!(
        i.syntax().parent().map(|p| p.kind()),
        Some(SyntaxKind::TUPLE_TYPE),
        "INTERSECTION_TYPE must sit under the TUPLE_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : int & string)`: a *non*-typar / non-hash head must not
/// open an intersection (FCS requires a `typar` / `hashConstraint` head; the
/// bare `int` makes the `&` a parse error there, `Typed(_, int)`). We pin only
/// the structural property — no `INTERSECTION_TYPE` is produced.
#[test]
fn intersection_not_triggered_plain_head() {
    let source = "(x : int & string)\n";
    let parse = parse(source);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::INTERSECTION_TYPE),
        "`int & string` must not produce INTERSECTION_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 10.10 — `(x : 'T<int> & #A)`: a *prefix-applied* typar is not an
/// intersection head (FCS errors on `'T<int> & …`). The `<int>` is consumed as
/// an HPA prefix-app, so `at_intersection_head` declines and no
/// `INTERSECTION_TYPE` is produced. Pins the bare-typar disambiguation.
#[test]
fn intersection_not_triggered_typar_prefix_app() {
    let source = "(x : 'T<int> & #A)\n";
    let parse = parse(source);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::INTERSECTION_TYPE),
        "`'T<int> & #A` must not produce INTERSECTION_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 10.10 — `(x : 'T^2 & #A)`: a typar with a *measure-power* tail is not
/// an intersection head — FCS parses `'T^2` as a `MeasurePower` and errors on
/// the trailing `& #A` (no intersection). `intersectionType` is `typar AMP`,
/// so the token immediately after the typar ident must be `&`; the `^` here is
/// not, so `at_intersection_head` declines.
#[test]
fn intersection_not_triggered_typar_measure_power() {
    let source = "(x : 'T^2 & #A)\n";
    let parse = parse(source);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::INTERSECTION_TYPE),
        "`'T^2 & #A` must not produce INTERSECTION_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 10.10 — `(x : 'T list & #B)`: a typar with a *postfix-app* tail is not
/// an intersection head either — the token after the typar ident is `list`,
/// not `&`. Companion to the measure-power / prefix-app guards.
#[test]
fn intersection_not_triggered_typar_postfix_app() {
    let source = "(x : 'T list & #B)\n";
    let parse = parse(source);
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::INTERSECTION_TYPE),
        "`'T list & #B` must not produce INTERSECTION_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 10.10 — `(x : #A & #B list)`: an `appTypeWithoutNull` postfix-app
/// continuation applies *after* the reduced intersection, so the
/// `INTERSECTION_TYPE` is the argument of the `list` `APP_TYPE` (its parent).
#[test]
fn intersection_postfix_app_suffix() {
    use crate::syntax::AstNode;
    let source = "(x : #A & #B list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    assert_eq!(
        i.syntax().parent().map(|p| p.kind()),
        Some(SyntaxKind::APP_TYPE),
        "INTERSECTION_TYPE must be the postfix-app argument; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.10 — `(x : #A & #B[])`: the `arrayTypeSuffix` continuation wraps the
/// whole reduced intersection, so the `INTERSECTION_TYPE`'s parent is the
/// `ARRAY_TYPE`.
#[test]
fn intersection_array_suffix() {
    use crate::syntax::AstNode;
    let source = "(x : #A & #B[])\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let i = intersection(&parse);
    assert_eq!(
        i.syntax().parent().map(|p| p.kind()),
        Some(SyntaxKind::ARRAY_TYPE),
        "INTERSECTION_TYPE must sit under the ARRAY_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}
