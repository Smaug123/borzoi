//! Recovered-tree diffs: inputs both parsers reject, compared whole
//! (`assert_asts_match_allow_errors`), recovery placeholders included.
//!
//! The `foo.` group is the shape an editor buffer is in while a member access
//! is being typed, and so the one completion is driven by. FCS recovers it as
//! `SynExpr.DiscardAfterMissingQualificationAfterDot(receiver)` and keeps
//! parsing; every case here also requires the declaration after it to survive.

use crate::common::assert_asts_match_allow_errors;

/// `foo.` — a one-segment receiver. FCS keeps it as `Ident`, not a one-segment
/// `LongIdent`.
#[test]
fn diff_ast_dot_missing_after_ident() {
    assert_asts_match_allow_errors("let x = foo.\nlet y = 2\n");
}

/// `A.B.` — a dotted receiver.
#[test]
fn diff_ast_dot_missing_after_long_ident() {
    assert_asts_match_allow_errors("let x = A.B.\nlet y = 2\n");
}

/// `(f x).` — a parenthesised receiver.
#[test]
fn diff_ast_dot_missing_after_paren() {
    assert_asts_match_allow_errors("let x = (f x).\nlet y = 2\n");
}

/// `foo.Bar(1).` — a method-call receiver.
#[test]
fn diff_ast_dot_missing_after_method_call() {
    assert_asts_match_allow_errors("let x = foo.Bar(1).\nlet y = 2\n");
}

/// `"hi".Length.` — a member-access receiver off a literal.
#[test]
fn diff_ast_dot_missing_after_dot_get() {
    assert_asts_match_allow_errors("let x = \"hi\".Length.\nlet y = 2\n");
}

/// `xs.[0].` — an indexer receiver.
#[test]
fn diff_ast_dot_missing_after_indexer() {
    assert_asts_match_allow_errors("let x = xs.[0].\nlet y = 2\n");
}

/// A `foo.` statement in a sequential block, the rest of the block intact.
#[test]
fn diff_ast_dot_missing_in_sequential_block() {
    assert_asts_match_allow_errors("let f () =\n    foo.\n    1\nlet y = 2\n");
}

/// A `foo.` as an application argument.
#[test]
fn diff_ast_dot_missing_as_argument() {
    assert_asts_match_allow_errors("let x = f foo.\nlet y = 2\n");
}

/// A `foo.` as the module-level expression itself.
#[test]
fn diff_ast_dot_missing_as_module_expr() {
    assert_asts_match_allow_errors("foo.\nlet y = 2\n");
}

/// The beyond-FCS exemption anchors a module's offside column on a declaration
/// both parsers agree on, never on one of ours that FCS does not have. Here FCS
/// keeps only `N`'s header, and our only unit inside `N` is the spill of a stray
/// `member` (`this.Foo`, off the body's column); anchoring on it would align it
/// with itself and pass it as agreement.
#[test]
fn recovery_oracle_does_not_anchor_the_offside_column_on_an_unconfirmed_unit() {
    let verdict =
        crate::common::recovery::recovered_verdict("module M\nmodule N =\n    member this.Foo\n");
    assert_eq!(
        verdict.relation,
        crate::common::recovery::Relation::Divergent,
        "{}",
        verdict.token()
    );
}
