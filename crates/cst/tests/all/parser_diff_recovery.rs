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

// A construct the parser abandons part-way must not spill its remains into the
// enclosing module as declarations of their own. The remains sit deeper than
// the module's offside line, so they belong to the abandoned construct, and
// FCS never makes module-level declarations of them.

/// A member whose accessor list does not parse: the rest of the type body is
/// debris, not module-level expressions, and the `let` after the type is a
/// declaration of its own.
#[test]
fn recover_type_body_debris_stays_inside_the_module_offside_line() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\ntype T() =\n    member this.test1 with private get private () = 0\n    member private this.test2 with private get () = 0\nlet y = 2\n",
        1,
    );
}

/// The same, with the debris in a nested module.
#[test]
fn recover_type_body_debris_in_a_nested_module() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nmodule N =\n    type T() =\n        member this.test1 with private get private () = 0\n        member private this.test2 with private get () = 0\n    let y = 2\nlet z = 3\n",
        1,
    );
}

/// A token that cannot begin a module-level declaration (`member` outside a
/// type) makes the rest of its line debris, not a fresh expression decl; the
/// next line's declaration still parses.
#[test]
fn recover_stray_keyword_line_is_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet x = 1\nmember this.P = 1\nlet y = 2\n",
        1,
    );
}

/// An augmentation whose member sits offside of its `with` (FCS drops the
/// rest of the module).
#[test]
fn recover_offside_augmentation_member_is_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module Module\n\ntype T with\nmember this.P = 1\n\n2\n",
        1,
    );
}

/// A `module` abbreviation inside a function body ends the binding at the
/// error; the rest of the body sits deeper than the module's offside line and
/// is debris, not a module-level expression (FCS keeps the binding and nothing
/// after it), and the following `let` still parses.
#[test]
fn recover_function_body_after_a_local_module_is_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "let someFunc x y =\n    module ListMod = Microsoft.FSharp.Collections.List\n\n    ListMod.sum [0; x; y]\n",
        0,
    );
}
