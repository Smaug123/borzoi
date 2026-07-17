//! Differential test (`parser::parse` vs FCS): `begin … end` blocks — the
//! verbose-syntax delimiter that stands in for parentheses in an expression
//! (`begin e end` → `SynExpr.Paren e`, `begin end` → `SynConst.Unit`) and for
//! the `#light` `OBLOCKBEGIN … OBLOCKEND` around a module body
//! (`module X = begin … end` → `SynModuleDecl.NestedModule`, the `begin`/`end`
//! dropped from the AST). Grammar: `beginEndExpr` (`pars.fsy:5419`) and
//! `wrappedNamedModuleDefn` (`pars.fsy:1478`).

use crate::common::{assert_asts_match, assert_sig_asts_match};

// ---- Expression form `begin e end` → SynExpr.Paren -------------------------

/// `begin <atom> end` wraps the inner expression in `SynExpr.Paren`, exactly as
/// `( <atom> )` does.
#[test]
fn diff_ast_begin_end_atom() {
    assert_asts_match("let x = begin 1 end\n");
}

/// `begin <app> end` — the inner body is a full `typedSequentialExpr`, so an
/// application sits under the `Paren`.
#[test]
fn diff_ast_begin_end_app() {
    assert_asts_match("let x = begin id 1 end\n");
}

/// `begin a; b end` — a sequential body, `Paren(Sequential(a, b))`.
#[test]
fn diff_ast_begin_end_sequential() {
    assert_asts_match("let z = begin a; b end\n");
}

/// `begin end` (empty) is `SynConst.Unit`, *not* an empty `Paren`.
#[test]
fn diff_ast_begin_end_empty_is_unit() {
    assert_asts_match("let u = begin end\n");
}

/// A `begin … end` atom in argument position — `f begin x end` is
/// `App(f, Paren(x))`, like `f (x)`.
#[test]
fn diff_ast_begin_end_arg_position() {
    assert_asts_match("let w = f begin x end\n");
}

/// A `begin … end` block spanning several lines (offside-suppressed inner body,
/// like a paren).
#[test]
fn diff_ast_begin_end_multiline() {
    assert_asts_match("let x =\n    begin\n        printfn \"hi\"\n    end\n");
}

/// A bare `begin … end` at module level is a module-level expression decl,
/// `SynModuleDecl.Expr(Paren(App …))`.
#[test]
fn diff_ast_begin_end_top_level_expr() {
    assert_asts_match("begin\n    printfn \"hi\"\nend\n");
}

// ---- Module-body form `module X = begin … end` -----------------------------

/// `module X = begin … end` — the verbose module body. The `begin`/`end` are
/// dropped from the AST; the body decls hang directly under the
/// `SynModuleDecl.NestedModule`.
#[test]
fn diff_ast_module_begin_end_single_decl() {
    assert_asts_match("module X = begin\n    let y = 1\nend\n");
}

/// Several decls inside a verbose module body.
#[test]
fn diff_ast_module_begin_end_multi_decl() {
    assert_asts_match("module X = begin\n    let a = 1\n    let b = 2\nend\n");
}

/// An empty verbose module body `module X = begin end` — still a
/// `NestedModule`, with no body decls.
#[test]
fn diff_ast_module_begin_end_empty() {
    assert_asts_match("module X = begin end\n");
}

/// A nested module inside a verbose module body.
#[test]
fn diff_ast_module_begin_end_nested_module() {
    assert_asts_match("module X = begin\n    module Y =\n        let a = 1\nend\n");
}

/// A `class … end` type definition *inside* a verbose module body: the inner
/// `end` closes the class (consumed by the type-defn repr), not the module
/// body, so the trailing `let z` stays a sibling at module level.
#[test]
fn diff_ast_module_begin_end_inner_class_end() {
    assert_asts_match(
        "module X = begin\n    type T = class\n        member _.M = 1\n    end\n    let z = 2\nend\n",
    );
}

/// A comment is the only thing between `begin` and `end`: still the empty-body
/// unit (`begin (* c *) end` → `SynConst.Unit`), the comment being trivia.
#[test]
fn diff_ast_begin_end_comment_only_is_unit() {
    assert_asts_match("let u = begin (* c *) end\n");
}

// ---- Signature-file module-body form ---------------------------------------

/// `.fsi` verbose module body `module X = begin … end` (grammar
/// `wrappedNamedModuleDefn` reached through `namedModuleDefnBlock`'s sig path,
/// `pars.fsy:786`).
#[test]
fn diff_sig_module_begin_end() {
    assert_sig_asts_match("module X = begin\n    val a : int\nend\n");
}

// ---- Signature-file type-body form `type T = begin … end` -------------------
//
// FCS's `classSpfn` admits an OCaml-verbose `begin … end` object-model body
// (`SynTypeDefnSigRepr.ObjectModel(SynTypeDefnKind.Unspecified, memberSigs, _)`,
// the same *unspecified* kind a bare `type T = member …` body carries — the
// `begin`/`end` are pure delimiters, dropped from the AST). Seen in
// `tests/fsharp/core/members/basics/test.fsi`. `begin` opens no inner offside
// block (like `struct`), so the members sit directly in the `=`-body block.

/// `type T = begin member … end` — a single member sig in a verbose body.
#[test]
fn diff_sig_type_begin_end_member() {
    assert_sig_asts_match("module M\ntype T =\n    begin\n        member A : int\n    end\n");
}

/// A verbose body mixing `val` / `member` / `abstract` / `new` sigs, as
/// `test.fsi`'s `AbstractType` does.
#[test]
fn diff_sig_type_begin_end_mixed_members() {
    assert_sig_asts_match(
        "module M\ntype T =\n    begin\n        val f : string\n        \
         member A : int\n        abstract B : string -> int\n        new : string -> T\n    end\n",
    );
}

/// A `member … with get,set` property sig inside a verbose body — the indexer /
/// get-set shapes `test.fsi` exercises.
#[test]
fn diff_sig_type_begin_end_get_set() {
    assert_sig_asts_match(
        "module M\ntype T =\n    begin\n        member P : string with get,set\n        \
         member I : int -> string with get\n    end\n",
    );
}
