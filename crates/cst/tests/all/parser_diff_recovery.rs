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

/// Balanced constructs a debris bracket may hold: every bracket kind, the
/// keyword blocks an `end` closes, and plain atoms. A bare `struct … end` or
/// `interface … end` is not one: FCS's LexFilter opens those blocks only
/// after a type definition's `=`, so elsewhere their `end` closes an
/// enclosing `begin`.
const BALANCED: &[&str] = &[
    "",
    "x",
    "( )",
    "[ ]",
    "[| |]",
    "{ }",
    "{| |}",
    "<@ x @>",
    "<@@ x @@>",
    "[<A>]",
    "$\"{x}\"",
    "begin end",
    "class end",
    "sig end",
    "type T = struct end",
    "type T = interface end",
    "type T = class end",
];

/// Over pairs of [`BALANCED`] constructs inside each kind of debris bracket:
/// the bracket stays open until its own closer, so the `let` on the line
/// after the constructs is never a declaration of the enclosing module. A
/// closer of another kind (`end` closing a `struct` the debris never counted)
/// must not end the bracket early.
#[test]
fn recover_a_debris_bracket_closes_only_at_its_own_closer() {
    let mut failures = Vec::new();
    for (open, close) in [
        ("(", ")"),
        ("[", "]"),
        ("[|", "|]"),
        ("{", "}"),
        ("begin", "end"),
    ] {
        for a in BALANCED {
            for b in BALANCED {
                let src = format!(
                    "let x = 0\nmember foo {open} {a} {b}\n            let inner = 1\n            {close}\nlet after = x\n"
                );
                let decls = top_level_decls(&src);
                if decls != ["let x = 0", "let after = x"] {
                    failures.push(format!("{src:?}: {decls:?}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A stray token's debris ends at its line, closers LexFilter swallows
/// included, so the nested module after it keeps its header and its body:
/// for every stray, a debris tail ending in each kind of [`BALANCED`]
/// construct. (An attribute list at the end of the line belongs to the
/// module after it, so it is not a tail.)
#[test]
fn recover_debris_leaves_the_next_module_its_header() {
    let mut failures = Vec::new();
    for stray in ["private", "internal", "member", "override"] {
        for tail in BALANCED.iter().filter(|t| **t != "[<A>]") {
            let src = format!("{stray} foo {tail}\nmodule A =\n    let x = 1\n");
            let decls = top_level_decls(&src);
            if decls.last().map(String::as_str) != Some("module A =\n    let x = 1") {
                failures.push(format!("{src:?}: {decls:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// What a stray token's debris may hold: atoms, every bracket kind open,
/// closed or balanced, the keyword blocks `end` closes and the `struct` tuple
/// it does not, comments and strings on one line or across lines,
/// separators, and a declaration keyword.
const DEBRIS: &[&str] = &[
    "",
    "1",
    "x",
    "( )",
    "(",
    ")",
    "[ ]",
    "{ }",
    "begin end",
    "begin",
    "end",
    "struct (1, 2)",
    "type T = struct end",
    "(* c *)",
    "(*\n c\n*)",
    "\"s\"",
    "\"\"\"\nmulti\n\"\"\"",
    "$\"{x}\"",
    "<@ x @>",
    "[<A>]",
    ";",
    ";;",
    "let y = 1",
    "->",
    "=",
];

/// The debris grid, graded against FCS: a stray token (`_`, an access
/// modifier, a member keyword, an arrow) and two [`DEBRIS`] items after it,
/// then optionally a line indented past the stray (a continuation of what it
/// began), followed by two declarations, or (after one debris item) by a
/// nested module. The trees must agree outside the damage: the debris neither
/// swallows a declaration FCS keeps nor leaves one of its own that FCS does
/// not have. And every unit FCS keeps after the stray's lines must be in our
/// tree too, however wide the damage: an error span of ours that covers a
/// declaration would otherwise hide that we lost it. The cells where either
/// fails are pinned one per line in `tests/manifests/debris_grid.txt`.
#[test]
fn recover_debris_grid_matches_fcs() {
    use crate::common::recovery::{fcs_units, grade};
    use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV};
    const FOLLOWING: [&str; 2] = ["let after = a\nlet z = 2\n", "module N =\n    let x = 1\n"];
    // Distinct cells, keyed by the stray's lines and what follows them: `(`
    // then `)` is `( )`.
    let mut cells = std::collections::BTreeSet::new();
    for stray in ["_", "private", "member", "override", "->"] {
        for a in DEBRIS {
            for b in DEBRIS {
                let line = format!("{stray} foo {a} {b}").trim_end().to_string();
                let following: &[&str] = if b.is_empty() {
                    &FOLLOWING
                } else {
                    &FOLLOWING[..1]
                };
                for more in ["", "\n    let inner = 1", "\n    x"] {
                    for f in following {
                        cells.insert((format!("{line}{more}"), *f));
                    }
                }
            }
        }
    }
    let mut divergent = Vec::new();
    for (line, following) in &cells {
        let head = format!("module M\nlet a = 0\n{line}\n");
        let src = format!("{head}{following}");
        let json = {
            let mut tmp = tempfile::NamedTempFile::with_suffix(".fs").expect("create tempfile");
            std::io::Write::write_all(&mut tmp, src.as_bytes()).expect("write source");
            crate::common::fcs_ast_batch(tmp.path())
        };
        let parse = borzoi_cst::parser::parse(&src);
        let cell = match following.starts_with("module") {
            true => format!("{} ⏎module N", line.replace('\n', "⏎")),
            false => line.replace('\n', "⏎"),
        };
        let verdict =
            grade(&parse, &Default::default(), &json, &src).expect("FCS record is readable");
        if verdict.relation == Relation::Divergent {
            divergent.push(format!("{cell} | divergent"));
        }
        let fcs = fcs_units(&json, &src).expect("FCS record is readable");
        let ours = our_units(&parse);
        for unit in fcs.iter().filter(|u| u.key.start >= head.len()) {
            if !ours.iter().any(|o| o.key == unit.key) {
                let name = src[unit.key.start..].lines().next().unwrap_or("");
                divergent.push(format!("{cell} | loses `{name}`"));
            }
        }
    }
    let actual = Manifest::from_entries(divergent).expect("one line per cell");
    crate::common::corpus_manifest::check_manifest(
        "debris_grid",
        &actual,
        &format!(
            "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-cst --test all parser_diff_recovery::recover_debris_grid"
        ),
    );
}

/// A line indented past the stray's continues what it began, so the `let` on
/// it is not a declaration of the module, as in FCS.
#[test]
fn recover_indented_continuation_of_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet inner = 0\n_ x =\n    let inner = 1\nlet after = inner\n",
        1,
    );
}

/// Columns are FCS's UTF-16 units: after `(*é*)` the `let` is at column 7, the
/// stray's own, so it begins a declaration rather than continuing the debris,
/// as in FCS.
#[test]
fn recover_continuation_columns_count_utf16_units() {
    use crate::common::recovery::{Unit, fcs_units};
    let src = "module M =\n       ->\n(*é*)  let after = 1\n";
    let at = src.find("let after").expect("the declaration");
    let has_after = |units: &[Unit]| {
        units
            .iter()
            .any(|u| u.key.start == at && u.key.kind == "Let")
    };
    let fcs = {
        let mut tmp = tempfile::NamedTempFile::with_suffix(".fs").expect("create tempfile");
        std::io::Write::write_all(&mut tmp, src.as_bytes()).expect("write source");
        fcs_units(&crate::common::fcs_ast_batch(tmp.path()), src).expect("FCS record is readable")
    };
    assert!(has_after(&fcs), "FCS keeps `after`");
    assert!(
        has_after(&our_units(&borzoi_cst::parser::parse(src))),
        "we drop `after`"
    );
}

/// After a stray access modifier, FCS discards tokens to the next separator,
/// brackets uncounted, so the declarations after `private foo (;;` are kept,
/// as in FCS.
#[test]
fn recover_separator_inside_debris_after_an_access_modifier() {
    let src = "module M\nprivate foo (;;\nlet after = 1\nlet final = after\n";
    crate::common::recovery::assert_recovered_trees_agree(src, 2);
    let decls = top_level_decls(src);
    assert!(
        decls.contains(&"let after = 1".to_string())
            && decls.contains(&"let final = after".to_string()),
        "{decls:?}"
    );
}

/// A stray arrow begins nothing, so FCS discards tokens to the next separator,
/// brackets uncounted, and keeps the declarations after `-> (;;`.
#[test]
fn recover_separator_inside_debris_after_a_stray_arrow() {
    let src = "module M\n-> (;;\nlet after = 1\nlet z = 2\n";
    crate::common::recovery::assert_recovered_trees_agree(src, 2);
    let decls = top_level_decls(src);
    assert!(
        decls.contains(&"let after = 1".to_string()) && decls.contains(&"let z = 2".to_string()),
        "{decls:?}"
    );
}

/// The next line's indentation is measured at its first raw token, a swallowed
/// `module` keyword included, so an unindented nested module after a stray is
/// not a continuation of it, as in FCS.
#[test]
fn recover_module_after_debris_is_not_a_continuation() {
    let src = "module M\n_ (*x*)\nmodule N =\n    let x = 1\n";
    crate::common::recovery::assert_recovered_trees_agree(src, 1);
    let decls = top_level_decls(src);
    assert!(
        decls.contains(&"module N =\n    let x = 1".to_string()),
        "{decls:?}"
    );
}

/// Only a type definition's `=` opens a `struct` block: after the equality in
/// `x = struct (1, 2)` the `end` closes the `begin`, and the declarations after
/// the line survive, as in FCS.
#[test]
fn recover_struct_tuple_after_an_expression_equals() {
    let src = "module M\n_ begin x = struct (1, 2) end\nlet after = 1\nlet z = 2\n";
    crate::common::recovery::assert_recovered_trees_agree(src, 1);
    let decls = top_level_decls(src);
    assert!(
        decls.contains(&"let after = 1".to_string()) && decls.contains(&"let z = 2".to_string()),
        "{decls:?}"
    );
}

/// A `struct (1, 2)` tuple opens no block, so the `end` after it closes the
/// `begin` and the declarations after the line survive, as in FCS.
#[test]
fn recover_struct_tuple_inside_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet a = 0\n_ foo begin struct (1, 2) end\nlet after = a\nlet z = 2\n",
        1,
    );
}

/// A comment crossing the stray's line end takes the debris with it: the `1`
/// after it is not a declaration, as in FCS.
#[test]
fn recover_multi_line_comment_inside_debris() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet a = 0\n_ foo (*\n comment\n*) 1\nlet after = a\n",
        1,
    );
}

/// The `)` LexFilter swallows at the end of a stray access modifier's line
/// is the debris's, so the nested module after it keeps its header, as in
/// FCS.
#[test]
fn recover_swallowed_closer_ending_debris_leaves_the_module_header() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nprivate foo ()\nmodule A =\n    let x = 1\n",
        2,
    );
}

/// The `end` of a `struct` block inside a debris bracket does not close the
/// bracket, so the `let` after it stays inside the abandoned member, as in
/// FCS.
#[test]
fn recover_keyword_block_end_inside_a_debris_bracket() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet inner = 0\nmember foo (type T = struct end\n            let inner = 1\n            )\nlet after = inner\n",
        1,
    );
}

/// Straight after a stray access modifier, a declaration keyword ends the
/// debris and begins a declaration, as in FCS (`private let x1 = 42`).
/// Anything else on the line is debris.
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
        let src = format!("let x = 0\nprivate {inner}\nlet after = x\n");
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
/// BOM), however wide the BOM is in bytes. FCS keeps nothing there, so the
/// trees diverge exactly when our tree keeps a unit at `this`.
#[test]
fn recovery_oracle_does_not_count_a_bom_as_indentation() {
    let source = "\u{FEFF}member\n   this.Foo\n";
    let at = source.find("this").expect("the spill");
    let kept = our_units(&borzoi_cst::parser::parse(source))
        .iter()
        .any(|u| u.key.start == at);
    let verdict = recovered_verdict(source);
    assert_eq!(
        verdict.relation == Relation::Divergent,
        kept,
        "{}",
        verdict.token()
    );
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
