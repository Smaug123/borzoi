//! Recovered-tree diffs: inputs both parsers reject, compared whole
//! (`assert_asts_match_allow_errors`), recovery placeholders included.
//!
//! The `foo.` group is the shape an editor buffer is in while a member access
//! is being typed, and so the one completion is driven by. FCS recovers it as
//! `SynExpr.DiscardAfterMissingQualificationAfterDot(receiver)` and keeps
//! parsing; every case here also requires the declaration after it to survive.

use std::collections::BTreeMap;

use crate::common::assert_asts_match_allow_errors;
use crate::common::recovery::{Relation, layout_grid, our_units, recovered_verdict};

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

/// A single `;` inside a bracket the debris opened (here, the parenthesis of
/// a `fun` after a stray `|>`) belongs to the abandoned construct, so it does
/// not end the debris: `b <- y` is not a declaration of its own.
#[test]
fn recover_semicolon_inside_abandoned_parens_stays_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "let a = 0\nlet f = g (fun (x, y)  x)\n|> h (fun (x, y) -> a <- x; b <- y)\nlet z = 3\n",
        2,
    );
}

/// `;;` closes every offside context, so after a stray token with no bracket
/// left open it ends the debris: `sentinel 42` is a declaration, as in FCS.
#[test]
fn recover_double_semicolon_after_balanced_debris_ends_it() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\n->;; sentinel 42\nlet z = 3\n",
        2,
    );
}

/// A `;;` inside a bracket the debris itself opened does not end it: `b <- y`
/// is not a declaration, and `let z` after the line still is.
#[test]
fn recover_double_semicolon_inside_debris_parens_stays_debris() {
    let verdict = crate::common::recovery::assert_recovered_trees_agree(
        "let a = 0\nlet f = g (fun (x, y)  x)\n|> h (fun (x, y) -> a <- x;; b <- y)\nlet z = 3\n",
        1,
    );
    assert!(
        verdict.beyond_fcs >= 1,
        "`let z` must survive past FCS's reach: {}",
        verdict.token()
    );
}

/// The same after a stray keyword, the bracket open straight after it.
#[test]
fn recover_double_semicolon_inside_stray_keyword_parens_stays_debris() {
    let verdict = crate::common::recovery::assert_recovered_trees_agree(
        "let a = 0\nmember (x;; b <- y)\nlet z = 3\n",
        1,
    );
    assert!(
        verdict.beyond_fcs >= 1,
        "`let z` must survive past FCS's reach: {}",
        verdict.token()
    );
}

/// A top-level `;` after a stray token with no bracket left open ends the
/// debris too: `sentinel 42` is a declaration, as in FCS.
#[test]
fn recover_semicolon_after_balanced_debris_ends_it() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\n->; sentinel 42\nlet z = 3\n",
        2,
    );
}

/// The module-level declarations of `src`, by text.
fn top_level_decls(src: &str) -> Vec<String> {
    use borzoi_cst::syntax::SyntaxKind;
    let parse = borzoi_cst::parser::parse(src);
    let module = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::MODULE_OR_NAMESPACE)
        .expect("a module");
    module
        .children()
        .map(|n| n.text().to_string().trim().to_string())
        .collect()
}

/// Inside a bracket a stray token's debris leaves open, nothing becomes a
/// declaration of the enclosing module, keyword-led or not: that would put
/// the abandoned construct's bindings in module scope, and resolve a later
/// use to them.
#[test]
fn recover_nothing_inside_a_debris_bracket_is_a_declaration() {
    let inners = [
        "let inner = 1",
        "let rec inner = 1",
        "use inner = 1",
        "type Inner = int",
        "open System",
        "module Inner = List",
        "exception Inner",
        "do ()",
        "[<A>] let inner = 1",
        "[<A>] type Inner = int",
        "#nowarn \"40\"",
        "extern void f()",
        "inner",
    ];
    let mut failures = Vec::new();
    for inner in inners {
        for (shape, src) in [
            (
                "multi-line",
                format!("let x = 0\nmember foo ({inner}\n            1)\nlet after = x\n"),
            ),
            (
                "same line",
                format!("let x = 0\nmember foo ({inner}) 1\nlet after = x\n"),
            ),
            (
                "interpolation fill",
                format!(
                    "let x = 0\nmember foo $\"\"\"{{{inner}\n            1}}\"\"\"\nlet after = x\n"
                ),
            ),
        ] {
            let decls = top_level_decls(&src);
            if decls != ["let x = 0", "let after = x"] {
                failures.push(format!("{shape} {inner:?}: {decls:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Outside any bracket, a declaration keyword on the stray token's line ends
/// the debris and begins a declaration, as FCS does after a stray access
/// modifier (`private let x1 = 42`). Anything else on the line is debris.
#[test]
fn recover_a_declaration_keyword_ends_unbracketed_debris() {
    let mut failures = Vec::new();
    for (inner, decl) in [
        ("let inner = 1", Some("let inner = 1")),
        ("type Inner = int", Some("type Inner = int")),
        ("open System", Some("open System")),
        ("module Inner = List", Some("module Inner = List")),
        ("exception Inner", Some("exception Inner")),
        ("[<A>] let inner = 1", Some("[<A>] let inner = 1")),
        ("extern void f()", Some("extern void f()")),
        ("do ()", Some("do ()")),
        ("inner", None),
        ("#nowarn \"40\"", None),
    ] {
        let src = format!("let x = 0\nmember foo {inner}\nlet after = x\n");
        let want: Vec<&str> = ["let x = 0"]
            .into_iter()
            .chain(decl)
            .chain(["let after = x"])
            .collect();
        let decls = top_level_decls(&src);
        if decls != want {
            failures.push(format!("{inner:?}: {decls:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The access-modifier shape graded against FCS: each `let` after a stray
/// `private`/`public` is a declaration of the nested module.
#[test]
fn recover_stray_access_modifier_keeps_the_declaration_after_it() {
    let verdict = crate::common::recovery::assert_recovered_trees_agree(
        "module M =\n    private let x1 = 42\n    public let x2 = 42\n",
        1,
    );
    assert_eq!(
        verdict.relation,
        crate::common::recovery::Relation::Exact,
        "{}",
        verdict.token()
    );
}

/// The scope leak in full, graded against FCS: the abandoned member's local
/// `inner` stays out of the module.
#[test]
fn recover_stray_member_keeps_its_local_binding_local() {
    let src = "let inner = 0\nmember foo (let inner = 1\n            inner)\nlet after = inner\n";
    assert_eq!(top_level_decls(src), ["let inner = 0", "let after = inner"]);
    crate::common::recovery::assert_recovered_trees_agree(src, 1);
}

/// Debris ending at a declaration keyword on the line of the previous
/// declaration still lets that keyword begin one: the stray token, not the
/// missing separator, is the error (`type B` here, which we do not parse on
/// the same line as `type A`).
#[test]
fn recover_same_line_declaration_after_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module Module\n\ntype A = A type B = A module C = type CC = int\n",
        3,
    );
}

/// Debris whose offside block closes on the next declaration's line, with no
/// separator before it, still lets that line begin a declaration.
#[test]
fn recover_declaration_after_debris_block_end() {
    let decls = top_level_decls("let f class = 5\n\nexit 1\n");
    assert_eq!(
        decls.last().map(String::as_str),
        Some("exit 1"),
        "{decls:?}"
    );
}

/// Debris that absorbed no layout separator leaves the previous
/// declaration's need for one standing: the `let` after the stray
/// ``` ``b`` = ``` (a `module` head that lost its keyword) is not a
/// module-level declaration, as in FCS.
#[test]
fn recover_keyword_after_unseparated_debris_is_not_a_declaration() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nmodule ``a`` = let a = 1\n ``b`` = let a = 1\nmodule ``c`` = let a = 1\n",
        2,
    );
}

/// A `do` after a stray access modifier is a declaration too, as in FCS.
#[test]
fn recover_stray_access_modifier_keeps_the_do_after_it() {
    let verdict = crate::common::recovery::assert_recovered_trees_agree(
        "module M =\n    private do foo x\n    let y = 1\n",
        1,
    );
    assert_eq!(
        verdict.relation,
        crate::common::recovery::Relation::Exact,
        "{}",
        verdict.token()
    );
}

/// A stray `member` as the first thing in a nested module's body is debris,
/// not a declaration of the module: FCS keeps only `N`'s header, and so do we.
#[test]
fn recover_stray_member_opening_a_nested_module_is_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nmodule N =\n    member this.Foo\n",
        1,
    );
}

/// A file-start BOM is not indentation: `this.Foo`, three columns into line 2,
/// is off the anonymous module's offside line (column 0 of line 1, after the
/// BOM), however wide the BOM is in bytes.
#[test]
fn recovery_oracle_does_not_count_a_bom_as_indentation() {
    let verdict = recovered_verdict("\u{FEFF}member\n   this.Foo\n");
    assert_eq!(verdict.relation, Relation::Divergent, "{}", verdict.token());
}

/// A backward line directive maps the error before it to two lines, one of
/// them past the end of the file: only the one in the file is damage, so `let
/// good` after the directive is still compared.
#[test]
fn recovery_oracle_maps_diagnostics_back_only_into_the_file() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet bad = (1 + )\n# 0 \"gen.fs\"\nlet good = 2\n",
        2,
    );
}

/// Over the layout grid ([`layout_grid`]), graded against FCS. FCS keeps
/// nothing of a stray `member this.Foo`, and its `this.Foo` sits off the body's
/// offside line, so the trees diverge exactly when our tree has a unit starting
/// there, however the body is wrapped. And a doc comment or an attribute on the
/// declaration after it moves nothing in the verdict, on whatever column the
/// comment sits.
#[test]
fn recovery_verdicts_follow_the_generated_layout() {
    let mut failures = Vec::new();
    let mut families: BTreeMap<String, (String, String)> = BTreeMap::new();
    for case in layout_grid() {
        let verdict = recovered_verdict(&case.source);
        let token = verdict.token();
        let debris = case.spill.is_some_and(|at| {
            our_units(&borzoi_cst::parser::parse(&case.source))
                .iter()
                .any(|u| u.key.start == at)
        });
        if (verdict.relation == Relation::Divergent) != debris {
            failures.push(format!(
                "{:?}: {token} {}",
                case.source,
                verdict.first_divergence.as_deref().unwrap_or("")
            ));
        }
        let (first, first_token) = families
            .entry(case.family.clone())
            .or_insert_with(|| (case.source.clone(), token.clone()));
        if *first_token != token {
            failures.push(format!(
                "{:?}: {token}, but {first:?}: {first_token}",
                case.source
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
