//! `when`-constrained types (`'T when 'T : struct`) — FCS's
//! `SynType.WithGlobalConstraints` from the `typeWithTypeConstraints` grammar —
//! and the individual `SynTypeConstraint` forms they carry.
//!
//! Reached through a binding's return-type annotation
//! (`let f x : 'T when 'T : struct = …`), a parenthesised parameter pattern
//! (`let f (x: 'a when 'a : enum<'b>) = …`), and a type-definition header.

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// `let f (x: 'T) : 'T when 'T : struct = x` — the return type carries one
/// `when` constraint. Green shape: `CONSTRAINED_TYPE > [VAR_TYPE, TYPAR_CONSTRAINTS]`,
/// where `base()` is the `'T` and `constraints()` is the `when` group.
#[test]
fn constrained_return_type_single() {
    use crate::syntax::{AstNode, ConstrainedType, Type};
    let source = "let f (x: 'T) : 'T when 'T : struct = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::CONSTRAINED_TYPE)
        .expect("CONSTRAINED_TYPE present for a `when`-constrained return type");
    let ct = ConstrainedType::cast(node).expect("CONSTRAINED_TYPE casts");
    match ct.base().expect("base type present") {
        Type::Var(_) => {}
        other => panic!("base must be VAR_TYPE ('T); got {other:?}"),
    }
    let constraints = ct.constraints().expect("TYPAR_CONSTRAINTS child present");
    assert_eq!(
        constraints.constraints().count(),
        1,
        "one `when` constraint",
    );
    // The node's range must start at the first type token (`'T`), not the
    // whitespace after the `:` — the same invariant unconstrained types hold.
    let r = ct.syntax().text_range();
    assert_eq!(
        &source[usize::from(r.start())..usize::from(r.start()) + 2],
        "'T",
        "CONSTRAINED_TYPE must start at the base type token, not leading trivia",
    );
    assert_lossless(source, &parse);
}

/// Two `and`-joined constraints (`not null and not struct`) sit in the single
/// `TYPAR_CONSTRAINTS` group.
#[test]
fn constrained_return_type_two_constraints() {
    use crate::syntax::{AstNode, ConstrainedType};
    let source = "let f (x: 'T) : 'T when 'T : not null and 'T : not struct = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::CONSTRAINED_TYPE)
        .expect("CONSTRAINED_TYPE present");
    let ct = ConstrainedType::cast(node).expect("casts");
    assert_eq!(
        ct.constraints()
            .expect("constraints present")
            .constraints()
            .count(),
        2,
        "both `and`-joined constraints are in one group",
    );
    assert_lossless(source, &parse);
}

/// Without a `when`, a plain return type does *not* gain a `CONSTRAINED_TYPE`
/// wrapper (guards against the trailing-`when` hook firing spuriously).
#[test]
fn plain_return_type_is_not_wrapped() {
    let source = "let f (x: int) : int = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    assert!(
        parse
            .root
            .descendants()
            .all(|n| n.kind() != SyntaxKind::CONSTRAINED_TYPE),
        "a `when`-less return type must not be wrapped in CONSTRAINED_TYPE",
    );
}

/// `'a : enum<int>` — the `enum` constraint (`WhereTyparIsEnum`). Green shape:
/// the `TYPAR_CONSTRAINT` carries an `IDENT_TOK("enum")`, a `LESS_TOK`/`GREATER_TOK`
/// type-argument list, and the single `int` type arg. `kind()` reports `Enum`.
#[test]
fn enum_constraint_classifies_and_carries_type_args() {
    use crate::syntax::{AstNode, TyparConstraint, TyparConstraintKind};
    let source = "let f (x: 'a) : 'a when 'a : enum<int> = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPAR_CONSTRAINT)
        .expect("TYPAR_CONSTRAINT present");
    let c = TyparConstraint::cast(node).expect("casts");
    assert_eq!(
        c.kind(),
        Some(TyparConstraintKind::Enum),
        "classified as Enum"
    );
    assert_eq!(c.type_args().count(), 1, "one type arg (`int`)");
    assert_lossless(source, &parse);
}

/// `'a : delegate<System.EventArgs, unit>` — the `delegate` constraint
/// (`WhereTyparIsDelegate`). The `DELEGATE_TOK` keyword drives `kind()`, and the
/// `< … >` carries the two type args.
#[test]
fn delegate_constraint_classifies_and_carries_type_args() {
    use crate::syntax::{AstNode, TyparConstraint, TyparConstraintKind};
    let source = "let f (x: 'a when 'a : delegate<System.EventArgs, unit>) = ()\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPAR_CONSTRAINT)
        .expect("TYPAR_CONSTRAINT present");
    let c = TyparConstraint::cast(node).expect("casts");
    assert_eq!(
        c.kind(),
        Some(TyparConstraintKind::Delegate),
        "classified as Delegate",
    );
    assert_eq!(c.type_args().count(), 2, "two type args (args, ret)");
    assert_lossless(source, &parse);
}

/// The `ty()` and `type_args()` accessors must not conflate forms: a subtype
/// constraint (`'a :> System.IComparable`) exposes its target via `ty()` and an
/// *empty* `type_args()`, while an `enum<int>` constraint exposes its arg via
/// `type_args()` and `ty() == None`. (The `CONSTRAINT_TYPE_ARGS` wrapper keeps
/// the two physically separate in the tree.)
#[test]
fn ty_and_type_args_do_not_conflate_across_forms() {
    use crate::syntax::{AstNode, TyparConstraint, TyparConstraintKind};
    let find = |source: &str| {
        let parse = parse(source);
        assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
        let node = parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::TYPAR_CONSTRAINT)
            .expect("TYPAR_CONSTRAINT present");
        TyparConstraint::cast(node).expect("casts")
    };

    let sub = find("let f (x: 'a) : 'a when 'a :> System.IComparable = x\n");
    assert_eq!(sub.kind(), Some(TyparConstraintKind::SubtypeOf));
    assert!(sub.ty().is_some(), "subtype target read via ty()");
    assert_eq!(
        sub.type_args().count(),
        0,
        "a subtype target is NOT a constraint type-arg",
    );

    let en = find("let f (x: 'a) : 'a when 'a : enum<int> = x\n");
    assert_eq!(en.kind(), Some(TyparConstraintKind::Enum));
    assert!(en.ty().is_none(), "an enum arg is NOT read via ty()");
    assert_eq!(en.type_args().count(), 1, "the enum arg is a type-arg");
}

/// Gate: an *unknown* identifier with a type-argument list (`'a : flange<int>`)
/// is not a constraint we accept — FCS raises `parsUnexpectedIdentifier` here, so
/// our parser must record an error rather than silently treating any `ident<…>`
/// as an enum-like constraint. Pins the `enum`-only gate in the ident arm.
#[test]
fn unknown_ident_type_arg_constraint_errors() {
    let source = "let f (x: 'a when 'a : flange<int>) = ()\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an unknown `ident<…>` constraint must error, not be accepted",
    );
    assert_lossless(source, &parse);
}

/// Recovery: an incomplete return annotation whose type position is empty before
/// a LexFilter-swallowed `)` must not let `parse_type_with_constraints` cross the
/// delimiter and steal the following token (`y`) as the return type. The `y`
/// stays a sibling expression, the round-trip is lossless, and no spurious
/// `CONSTRAINED_TYPE` is produced. (Mirrors `parse_type`'s
/// `in_paren_missing_type_does_not_eat_outer_rparen` guard for the new helper.)
#[test]
fn empty_return_annotation_does_not_eat_token_past_swallowed_rparen() {
    let source = "(let f : ) y\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse
            .root
            .descendants()
            .all(|n| n.kind() != SyntaxKind::CONSTRAINED_TYPE),
        "an empty return annotation must not produce a CONSTRAINED_TYPE",
    );
    // `y` survives as its own ident expression rather than being absorbed as the
    // return type.
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y"),
        "the post-`)` token `y` must survive in the tree",
    );
}
