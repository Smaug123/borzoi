//! Flexible-type hash constraints (`#int`).
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.8 — `(x : #int)`: basic hash-constraint shape.
/// `HASH_CONSTRAINT_TYPE` wraps a `HASH_TOK` followed by an
/// atomic inner type, mirroring FCS's
/// `hashConstraint: HASH atomType` (`pars.fsy:2609-2611`).
#[test]
fn hash_constraint_type_basic_shape() {
    use crate::syntax::{AstNode, HashConstraintType, Type};
    let source = "(x : #int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let hash_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::HASH_CONSTRAINT_TYPE)
        .expect("HASH_CONSTRAINT_TYPE present for `#int`");
    let hash = HashConstraintType::cast(hash_node).expect("HASH_CONSTRAINT_TYPE casts");
    match hash.inner().expect("inner type present") {
        Type::LongIdent(_) => {}
        other => panic!("inner must be LongIdent(int), got {other:?}"),
    }
    let toks: Vec<_> = hash
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect();
    assert_eq!(
        toks,
        vec![SyntaxKind::HASH_TOK],
        "HASH_CONSTRAINT_TYPE direct token children must be exactly one HASH_TOK; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.8 — `(x : ##int)`: nested hash. Inner `parse_atomic_type`
/// recurses into a second `HASH_CONSTRAINT_TYPE`, so the green-tree
/// shape is `Hash > [#, Hash > [#, int]]`.
#[test]
fn hash_constraint_nested() {
    use crate::syntax::{AstNode, HashConstraintType, Type};
    let source = "(x : ##int)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let hashes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::HASH_CONSTRAINT_TYPE)
        .collect();
    assert_eq!(
        hashes.len(),
        2,
        "expected two nested HASH_CONSTRAINT_TYPE nodes; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let outer = HashConstraintType::cast(hashes[0].clone()).expect("outer casts");
    let Type::Hash(inner) = outer.inner().expect("outer inner") else {
        panic!(
            "outer inner must be Hash(_); got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    match inner.inner().expect("inner inner") {
        Type::LongIdent(_) => {}
        other => panic!("inner Hash inner must be LongIdent(int), got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.8 — `(x : #'T)`: hash over a type variable. The inner
/// atomic type is a `VAR_TYPE`, so `Hash.inner()` projects to
/// `Type::Var`.
#[test]
fn hash_constraint_typar() {
    use crate::syntax::{AstNode, HashConstraintType, Type};
    let source = "(x : #'T)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let hash_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::HASH_CONSTRAINT_TYPE)
        .expect("HASH_CONSTRAINT_TYPE present for `#'T`");
    let hash = HashConstraintType::cast(hash_node).expect("HASH_CONSTRAINT_TYPE casts");
    match hash.inner().expect("inner type present") {
        Type::Var(_) => {}
        other => panic!("inner must be Var('T), got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.8 — `(x : #Foo<int>)`: hash over a prefix-app. Because
/// the HPA wrap lives in `parse_atomic_type` (above the hash
/// branch's recursive `parse_atomic_type` call), the prefix-app
/// must sit *under* the hash — green shape
/// `Hash > [#, App(Foo, [int])]`, not `App(Hash(Foo), [int])`.
/// Pins the FCS layering: `atomType: hashConstraint |
/// appTypeConPower` (`pars.fsy:6534-6549`).
#[test]
fn hash_constraint_over_prefix_app() {
    use crate::syntax::{AppType, AstNode, HashConstraintType, Type};
    let source = "(x : #Foo<int>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let hash_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::HASH_CONSTRAINT_TYPE)
        .expect("HASH_CONSTRAINT_TYPE present for `#Foo<int>`");
    let hash = HashConstraintType::cast(hash_node).expect("HASH_CONSTRAINT_TYPE casts");
    let Type::App(app) = hash.inner().expect("inner type present") else {
        panic!(
            "inner must be App(_) for `#Foo<int>`; got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    assert!(!app.is_postfix(), "inner App must be prefix `Foo<int>`");
    let args = app.type_args();
    assert_eq!(args.len(), 1);
    assert!(matches!(args[0], Type::LongIdent(_)));
    let _ = AppType::cast(app.syntax().clone());
    // The APP_TYPE must be a descendant of HASH_CONSTRAINT_TYPE,
    // pinning that the prefix-app wraps the head *inside* the
    // hash, not the other way around.
    assert!(
        hash.syntax()
            .descendants()
            .any(|n| n.kind() == SyntaxKind::APP_TYPE),
        "APP_TYPE must sit under HASH_CONSTRAINT_TYPE; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.8 — `(x : #(int -> int))`: hash over a parenthesised
/// function type. FCS rule is `hashConstraint: HASH atomType`,
/// and `LPAREN typ rparen` is an atomType, so the inner can be a
/// PAREN_TYPE wrapping a FUN_TYPE. Pins that `parse_atomic_type`
/// (rather than `parse_type`) is the recursive call inside the
/// hash branch.
#[test]
fn hash_constraint_paren_arrow_inner() {
    use crate::syntax::{AstNode, HashConstraintType, Type};
    let source = "(x : #(int -> int))\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let hash_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::HASH_CONSTRAINT_TYPE)
        .expect("HASH_CONSTRAINT_TYPE present for `#(int -> int)`");
    let hash = HashConstraintType::cast(hash_node).expect("HASH_CONSTRAINT_TYPE casts");
    let Type::Paren(paren) = hash.inner().expect("inner type present") else {
        panic!(
            "inner must be Paren(_); got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    match paren.inner().expect("paren inner") {
        Type::Fun(_) => {}
        other => panic!("paren inner must be Fun(_), got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.8 — `(x : #int list)`: postfix-app outside hash. The
/// hash branch in `parse_atomic_type` returns after parsing
/// `#int`; control resumes in `parse_app_type`, which then sees
/// `list` and wraps `HASH_CONSTRAINT_TYPE` into `APP_TYPE` from
/// its checkpoint. Green shape: `App(list, [Hash(int)], postfix)`.
#[test]
fn hash_constraint_postfix_app_outside() {
    use crate::syntax::{AppType, AstNode, HashConstraintType, Type};
    let source = "(x : #int list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for `#int list`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts");
    assert!(app.is_postfix(), "outer App must be postfix `T list`");
    match app.type_name().expect("App head") {
        Type::LongIdent(_) => {}
        other => panic!("App head must be LongIdent(list), got {other:?}"),
    }
    let args = app.type_args();
    assert_eq!(args.len(), 1);
    let Type::Hash(inner_hash) = &args[0] else {
        panic!(
            "App's sole arg must be Hash(_); got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    let _ = HashConstraintType::cast(inner_hash.syntax().clone());
    match inner_hash.inner().expect("hash inner") {
        Type::LongIdent(_) => {}
        other => panic!("hash inner must be LongIdent(int), got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.8 recovery — `(x : #)` and friends. An incomplete
/// flexible-type constraint where the inner atomic-type is missing
/// must not panic the parser. The hash branch in
/// `parse_atomic_type` recurses; the recursion has to gate on a
/// raw atomic-type starter, otherwise it falls through to the
/// `unreachable!` arm. Mirrors the recovery gate the LPAREN /
/// paren-type arm uses. The typed-paren form below routes through
/// `parse_atomic_type` and must surface a `HASH_CONSTRAINT_TYPE`
/// node with no inner type child; the parser surfaces a
/// `ParseError` rather than crashing.
#[test]
fn hash_constraint_incomplete_no_panic() {
    // Inputs that reach the hash branch in `parse_atomic_type` via
    // a typed-paren expression. We assert (a) no panic, (b) a
    // HASH_CONSTRAINT_TYPE node is emitted, and (c) `inner()` is
    // `None` because the recovery left no inner-type child.
    use crate::syntax::{AstNode, HashConstraintType};
    for source in ["(x : #)\n", "(x : # )\n"] {
        let parse = parse(source);
        let hash_node = parse
                .root
                .descendants()
                .find(|n| n.kind() == SyntaxKind::HASH_CONSTRAINT_TYPE)
                .unwrap_or_else(|| {
                    panic!(
                        "{source:?}: expected a HASH_CONSTRAINT_TYPE node even with missing inner; got tree:\n{}",
                        debug_tree(&parse.root),
                    )
                });
        let hash = HashConstraintType::cast(hash_node).expect("HASH_CONSTRAINT_TYPE casts");
        assert!(
            hash.inner().is_none(),
            "{source:?}: hash with missing inner must have inner() = None; got tree:\n{}",
            debug_tree(&parse.root),
        );
        assert!(
            parse
                .errors
                .iter()
                .any(|e| e.message.contains("atomic type after `#`")),
            "{source:?}: expected a ParseError about the missing atomic type after `#`; got errors: {:?}",
            parse.errors,
        );
    }
    // Smoke test — must not panic, regardless of which surface
    // production the hash lives under.
    for source in ["let f (x : #) = x\n", "let f (x : # ) = x\n"] {
        let _ = parse(source);
    }
}

/// Phase 7.8 regression — FCS's postfix-app rule
/// `appTypeWithoutNull appTypeConPower` (`pars.fsy:6378`) restricts
/// the right-hand head to `appTypeConPower` = `path | typar |
/// T^n` (`pars.fsy:6344-6355`). Critically, the prefix-app form
/// (`appTypeCon HPA LESS … GREATER`) lives at the `atomType` level
/// (`pars.fsy:6594-6602`), NOT at `appTypeConPower`. So
/// `int Foo<string>` must NOT parse: the HPA virtual after `Foo`
/// cannot be consumed at the postfix-app right-hand layer.
///
/// Before this fix, `parse_app_type`'s postfix loop called
/// `parse_atomic_type` for the right-hand head, which would
/// happily apply the prefix-app HPA wrap and produce
/// `App(App(Foo, [string]), [int], postfix)` — a shape FCS rejects.
/// After the fix the loop calls a stricter helper that only
/// accepts a path/typar.
#[test]
fn postfix_app_head_rejects_prefix_app_form() {
    let source = "(x : int Foo<string>)\n";
    let parse = parse(source);
    let app_nodes: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::APP_TYPE)
        .collect();
    // The outer postfix `int Foo` parses fine; the trailing
    // `<string>` is what's not allowed at the postfix layer. The
    // postfix-app right-hand head must NOT itself be wrapped in
    // APP_TYPE, so we expect at most one APP_TYPE node (the
    // postfix wrap) — not two (which would mean a prefix-app
    // nested inside).
    assert!(
        app_nodes.len() <= 1,
        "expected at most one APP_TYPE (the postfix `int Foo`); a nested prefix-APP_TYPE for `Foo<string>` would mean the postfix head accepted a shape FCS rejects. got tree:\n{}",
        debug_tree(&parse.root),
    );
}
