//! Differential checks for FCS AST ranges.
//!
//! The ordinary `parser_diff_*` tests compare the normalised AST shape and
//! deliberately elide FCS ranges. These tests make the broad source-span layer a
//! separate oracle comparison: modules and declarations must point at the same
//! byte ranges on both sides.

use crate::common::{
    assert_ast_ranges_match, assert_sig_ast_ranges_match, ast_ranges_match, fcs_ast_batch,
    fcs_parse_had_errors,
};

/// Machine-enumerated sweep over implementation type-declaration tail forms:
/// every repr × augmentation-tail × follower combination is diffed against
/// FCS. FCS computes a type definition's range compositionally (header ∪ repr
/// ∪ each member's range), so member separators and an augmentation tail's
/// closing `end` never enter the range; hand-picked cases repeatedly missed
/// neighbouring forms (`; end`, empty `with end`), hence the enumeration.
///
/// Combinations that fail to parse on *both* sides are skipped (they are not
/// range questions); a combination that parses cleanly on exactly one side is
/// reported as a failure, so acceptance parity is checked for free.
///
/// Known divergence, excluded via [`is_known_semi_chain_divergence`]: for an
/// inline `;`-chained member tail with no closing `end` and a following
/// declaration (`type T with member _.M = 1; member _.N = 2\nlet x = 1`), our
/// lex-filter emits two `BlockEnd`s FCS does not (FCS pops the member-body
/// seq-block silently), so the parser keeps the member list open and swallows
/// the `let` into the type declaration. That is a lex-filter bug, not a range
/// rule; the exclusion asserts the divergence is still present so fixing the
/// lex-filter forces this list to be retired.
#[test]
fn diff_ast_ranges_generated_type_decl_tails() {
    let reprs: &[&str] = &[
        "",
        " = int",
        " = { A : int }",
        " = A | B",
        " = class member _.C = 3 end",
        " = interface end",
    ];
    let tails: &[&str] = &[
        "",
        " with end",
        " with member _.M = 1",
        " with member _.M = 1;",
        " with member _.M = 1 end",
        " with member _.M = 1 end;",
        " with member _.M = 1; end",
        " with member _.M = 1; member _.N = 2",
        " with member _.M = 1; member _.N = 2 end",
        "\n    with\n    end",
        "\n    with\n        member _.M = 1\n    end",
        "\n    with\n        member _.M = 1\n        member _.N = 2\n    end",
    ];
    let followers: &[&str] = &["", "let x = 1\n"];

    let is_known_semi_chain_divergence = |tail: &str, follower: &str| {
        tail == " with member _.M = 1; member _.N = 2" && !follower.is_empty()
    };

    let mut failures = Vec::new();
    let mut compared = 0usize;
    let mut skipped = 0usize;

    for repr in reprs {
        for tail in tails {
            for follower in followers {
                let source = format!("module M\ntype Token{repr}{tail}\n{follower}");

                let mut tmp = tempfile::NamedTempFile::with_suffix(".fs").expect("create tempfile");
                std::io::Write::write_all(&mut tmp, source.as_bytes()).expect("write source");
                let json = fcs_ast_batch(tmp.path());
                let fcs_errors = fcs_parse_had_errors(&json);

                let parse = borzoi_cst::parser::parse(&source);
                let rust_errors = !parse.errors.is_empty();

                match (rust_errors, fcs_errors) {
                    (true, true) => skipped += 1,
                    (true, false) => {
                        failures.push(format!("we reject, FCS accepts: {source:?}"));
                    }
                    (false, true) => {
                        failures.push(format!("we accept, FCS rejects: {source:?}"));
                    }
                    (false, false) => {
                        compared += 1;
                        let result = ast_ranges_match(&parse, &json, &source);
                        if is_known_semi_chain_divergence(tail, follower) {
                            if result.is_ok() {
                                failures.push(format!(
                                    "known lex-filter divergence now matches — \
                                     retire the exclusion: {source:?}"
                                ));
                            }
                        } else if let Err(message) = result {
                            failures.push(format!("range divergence for {source:?}\n{message}"));
                        }
                    }
                }
            }
        }
    }

    println!("compared {compared} clean combinations, skipped {skipped} both-reject");
    assert!(
        failures.is_empty(),
        "{} generated type-decl combinations diverge:\n\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
    assert!(compared > 0, "generator degenerated: nothing compared");
}

#[test]
fn diff_ast_ranges_for_impl_decls() {
    assert_ast_ranges_match(concat!(
        "module M\n\n",
        "open System\n",
        "let x =\n",
        "    1\n\n",
        "type T = { A : int }\n",
        "module N =\n",
        "    let y = x\n",
    ));
}

#[test]
fn diff_ast_ranges_trim_trivia_around_impl_decls() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "// leading comment for the declaration\n",
        "let x = 1\n\n",
        "(* block comment between declarations *)\n",
        "let y = x + 1\n",
    ));
}

#[test]
fn diff_ast_ranges_for_signature_decls() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n\n",
        "open System\n",
        "val x : int\n",
        "type T = int\n",
        "module N =\n",
        "    val y : string\n",
    ));
}

#[test]
fn diff_ast_ranges_include_xml_doc_prefixes() {
    assert_ast_ranges_match(concat!(
        "/// Module documentation\n",
        "module M\n",
        "/// Value documentation\n",
        "let x = 1",
    ));
}

#[test]
fn diff_sig_ast_ranges_include_xml_doc_prefixes() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Value documentation\n",
        "val x : int",
    ));
}

#[test]
fn diff_ast_ranges_include_xml_doc_prefixes_across_blank_lines() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "/// Value documentation\n",
        "\n",
        "let x = 1",
    ));
}

#[test]
fn diff_ast_ranges_include_xml_doc_prefixes_across_intervening_trivia() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "/// Value documentation\n",
        "// ordinary comment between doc and declaration\n",
        "#nowarn \"57\"\n",
        "#if UNUSED_SYMBOL\n",
        "#endif\n",
        "let x = 1",
    ));
}

#[test]
fn diff_ast_ranges_do_not_merge_xml_doc_blocks_separated_by_comment() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "/// Earlier value documentation\n",
        "// ordinary comment separates doc blocks\n",
        "/// Nearest value documentation\n",
        "let x = 1",
    ));
}

#[test]
fn diff_sig_ast_ranges_include_xml_doc_prefixes_across_blank_lines() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Value documentation\n",
        "\n",
        "val x : int",
    ));
}

#[test]
fn diff_sig_ast_ranges_include_xml_doc_prefixes_across_intervening_trivia() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Value documentation\n",
        "// ordinary comment between doc and declaration\n",
        "#nowarn \"57\"\n",
        "#if UNUSED_SYMBOL\n",
        "#endif\n",
        "val x : int",
    ));
}

#[test]
fn diff_ast_ranges_do_not_attach_xml_docs_to_namespaces() {
    assert_sig_ast_ranges_match(concat!(
        "/// Namespace-looking documentation\n",
        "namespace A\n",
        "val x : int",
    ));
}

#[test]
fn diff_ast_ranges_do_not_attach_xml_docs_to_non_doc_impl_decls() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "/// Open docs are ignored\n",
        "open System\n",
        "/// Module abbreviation docs are ignored\n",
        "module IO = System.IO\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_do_not_attach_xml_docs_to_non_doc_sig_decls() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Open docs are ignored\n",
        "open System\n",
        "/// Module abbreviation docs are ignored\n",
        "module IO = System.IO\n",
    ));
}

#[test]
fn diff_ast_ranges_do_not_treat_four_slash_comments_as_xml_docs() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "//// Ordinary line comment\n",
        "let x = 1",
    ));
}

#[test]
fn diff_sig_ast_ranges_do_not_treat_four_slash_comments_as_xml_docs() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "//// Ordinary line comment\n",
        "val x : int",
    ));
}

#[test]
fn diff_sig_ast_ranges_exclude_end_from_type_body() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Token docs\n",
        "type Token =\n",
        "    interface\n",
        "        inherit System.IDisposable\n",
        "    end\n",
        "\n",
        "/// Next docs\n",
        "type Next = int\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_exclude_end_from_final_type_body_module() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "type Token =\n",
        "    interface\n",
        "        inherit System.IDisposable\n",
        "    end\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_preserve_anon_module_tail_for_final_type_body() {
    assert_sig_ast_ranges_match(concat!(
        "type Token =\n",
        "    interface\n",
        "        inherit System.IDisposable\n",
        "    end\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_include_inline_end_in_type_body() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Token docs\n",
        "type Token = interface end\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_include_end_for_empty_multiline_type_body() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "type Token =\n",
        "    interface\n",
        "    end\n",
    ));
}

#[test]
fn diff_ast_ranges_exclude_end_from_type_body() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "type Token = { A : int }\n",
        "    with\n",
        "        member _.M = 1\n",
        "    end\n",
        "\n",
        "let x = 1\n",
    ));
}

#[test]
fn diff_ast_ranges_exclude_final_named_module_type_augmentation_end() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "type Token with\n",
        "    member _.M = 1 // trailing\n",
        "    end\n",
    ));
}

#[test]
fn diff_ast_ranges_exclude_inline_type_augmentation_end() {
    assert_ast_ranges_match("module M\ntype Token with member _.M = 1 end\n");
}

#[test]
fn diff_ast_ranges_include_member_body_end_in_type_augmentation() {
    assert_ast_ranges_match("module M\ntype Token with member _.M = begin 1 end\n");
}

#[test]
fn diff_ast_ranges_exclude_type_augmentation_trailing_semicolon() {
    assert_ast_ranges_match("module M\ntype Token with member _.M = 1;\n");
}

#[test]
fn diff_ast_ranges_include_inline_end_in_type_body() {
    assert_ast_ranges_match("module M\ntype Token = interface end\n");
}

#[test]
fn diff_ast_ranges_include_end_for_empty_multiline_type_body() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "type Token =\n",
        "    interface\n",
        "    end\n",
    ));
}

#[test]
fn diff_ast_ranges_include_end_for_explicit_object_model_type_body() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "type Token =\n",
        "    class\n",
        "        member _.M = 1\n",
        "    end\n",
        "\n",
        "let x = 1\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_exclude_typars_from_bodyless_type() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "/// Box docs\n",
        "type Box<'T>\n",
        "\n",
        "val x : int\n",
    ));
}

#[test]
fn diff_sig_ast_ranges_keep_typars_for_type_abbrev() {
    assert_sig_ast_ranges_match(concat!(
        "namespace A\n",
        "type Box<'T> = 'T\n",
        "\n",
        "val x : int\n",
    ));
}

#[test]
fn diff_ast_ranges_ignore_utf8_bom_for_anon_module() {
    assert_ast_ranges_match(concat!("\u{feff}", "open System\n", "let x = 1"));
}

#[test]
fn diff_ast_ranges_ignore_utf8_bom_for_named_module() {
    assert_ast_ranges_match(concat!("\u{feff}", "module M\n", "let x = 1"));
}

#[test]
fn diff_ast_ranges_ignore_utf8_bom_before_ordinary_comment() {
    assert_ast_ranges_match(concat!(
        "\u{feff}",
        "// ordinary leading comment\n",
        "open System\n",
        "let x = 1",
    ));
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module() {
    assert_ast_ranges_match("open System\nlet x = 1\n");
}

#[test]
fn diff_ast_ranges_empty_anon_module_is_zero_width_at_eof() {
    assert_ast_ranges_match("\n");
}

#[test]
fn diff_ast_ranges_comment_only_anon_module_is_zero_width_at_eof() {
    assert_ast_ranges_match("// #Regression #NoMT #CompilerOptions \n\n\n");
}

#[test]
fn diff_ast_ranges_include_trailing_blank_lines_for_anon_module() {
    assert_ast_ranges_match("open System\nlet x = 1\n\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_final_open_over_trailing_blank_lines_for_anon_module() {
    assert_ast_ranges_match("open System\n\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_final_extern_over_trailing_blank_lines_for_anon_module() {
    assert_ast_ranges_match("extern void Meh()\n\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_final_open_over_trailing_comment_for_anon_module() {
    assert_ast_ranges_match("open System\n// trailing\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_final_extern_over_trailing_comment_for_anon_module() {
    assert_ast_ranges_match("extern void Meh()\n// trailing\n");
}

#[test]
fn diff_ast_ranges_include_trailing_literate_comment_for_anon_module_let() {
    assert_ast_ranges_match(concat!(
        "open System\n",
        "let x = 1\n",
        "(**\n",
        "Trailing prose.\n",
        "*)",
    ));
}

#[test]
fn diff_ast_ranges_include_trailing_literate_comment_for_anon_module_for_expr() {
    assert_ast_ranges_match(concat!(
        "for x in [1] do\n",
        "    printfn \"%d\" x\n",
        "(**\n",
        "Trailing prose.\n",
        "*)",
    ));
}

#[test]
fn diff_ast_ranges_do_not_extend_anon_module_hash_directive_to_trailing_literate_comment() {
    assert_ast_ranges_match(concat!(
        "(*** hide ***)\n",
        "#I \"lib\"\n",
        "(**\n",
        "Prose.\n",
        "*)",
    ));
}

#[test]
fn diff_ast_ranges_handle_hash_directive_trailing_semisemis() {
    assert_ast_ranges_match("#q;;\n");
    assert_ast_ranges_match("#time;;\n");
    assert_ast_ranges_match("#q foo;;\n");
    assert_ast_ranges_match("#I \"lib\";;\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_named_module_to_trailing_newline() {
    assert_ast_ranges_match("module M\nlet x = 1\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_anon_module_final_nested_module_to_trailing_whitespace() {
    assert_ast_ranges_match("module M = begin\n    let x = 1\nend\n           ");
}

#[test]
fn diff_ast_ranges_include_anon_module_final_indented_nested_module_trailing_whitespace() {
    assert_ast_ranges_match("module M =\n    let x = 1\n        ");
}

#[test]
fn diff_ast_ranges_include_anon_module_final_indented_nested_module_when_child_ends() {
    assert_ast_ranges_match("module M =\n    type T = interface end\n        ");
}

#[test]
fn diff_ast_ranges_static_optimization_end_at_main_expr() {
    assert_ast_ranges_match(concat!(
        "module M\n",
        "let inline f x =\n",
        "    g x\n",
        "    when 'T : int = h x\n",
    ));
}

#[test]
fn diff_ast_ranges_exclude_exception_augmentation_end() {
    assert_ast_ranges_match("exception E of int with\n  override this.Message = \"x\"\n  end\n");
}

#[test]
fn diff_ast_ranges_exclude_inline_exception_augmentation_end() {
    assert_ast_ranges_match("module M\nexception E with member this.M = 1 end\n");
}

#[test]
fn diff_ast_ranges_exclude_exception_augmentation_semicolon_before_end() {
    assert_ast_ranges_match("module M\nexception E with member this.M = 1; end\n");
}

#[test]
fn diff_ast_ranges_exclude_empty_exception_augmentation_end() {
    assert_ast_ranges_match("module M\nexception E with end\nlet x = 1\n");
    assert_ast_ranges_match("module M\nexception E of int with end\n");
}

#[test]
fn diff_ast_ranges_exclude_final_named_module_exception_augmentation_end() {
    assert_ast_ranges_match("module M\nexception E with\n  member this.M = 1\n  end\n");
}

#[test]
fn diff_ast_ranges_exclude_final_nested_module_exception_augmentation_end() {
    assert_ast_ranges_match(
        "module M =\n    exception E with\n      member this.M = 1\n      end\n",
    );
}

#[test]
fn diff_ast_ranges_preserve_anon_module_tail_for_static_optimization() {
    assert_ast_ranges_match(concat!(
        "let inline f x =\n",
        "    g x\n",
        "    when 'T : int = h x\n",
    ));
}

#[test]
fn diff_ast_ranges_exclude_top_level_semicolon_from_let_decl() {
    assert_ast_ranges_match("module M\nlet x = 1;\nlet y = 2\n");
}

#[test]
fn diff_ast_ranges_exclude_final_top_level_semicolon_from_let_decl() {
    assert_ast_ranges_match("module M\nlet x = 1;\n");
}

#[test]
fn diff_ast_ranges_include_final_anon_module_semisemi_without_trailing_trivia() {
    assert_ast_ranges_match("let x = 1;;\n");
}

#[test]
fn diff_ast_ranges_include_final_anon_module_expr_semisemi_without_trailing_trivia() {
    assert_ast_ranges_match("let x = 1\nx + 1;;\n");
}

#[test]
fn diff_ast_ranges_exclude_final_named_module_semisemi_from_let_tail() {
    assert_ast_ranges_match("module M\nlet x = 1;;\n");
}

#[test]
fn diff_ast_ranges_exclude_final_named_module_semisemi_from_expr_tail() {
    assert_ast_ranges_match("module M\nlet x = 1\nx + 1;;\n");
}

#[test]
fn diff_ast_ranges_exclude_final_named_module_semisemi_from_hash_directive_with_args() {
    assert_ast_ranges_match("module M\n#I \"lib\";;\n");
}

#[test]
fn diff_ast_ranges_keep_final_named_module_semisemi_for_argumentless_hash_directive() {
    assert_ast_ranges_match("module M\n#time;;\n");
}

#[test]
fn diff_ast_ranges_include_top_level_let_in_tail() {
    assert_ast_ranges_match("let x = 1 in\n\nlet y = 2\n");
}

#[test]
fn diff_ast_ranges_include_top_level_active_pattern_in_tail() {
    assert_ast_ranges_match(concat!(
        "let (|MulThree|_|) inp = if inp % 3 = 0 then Some (inp / 3) else None in\n",
        "\n",
        "let result = 9\n",
    ));
}

#[test]
fn diff_ast_ranges_do_not_extend_anon_module_expr_to_trailing_newline() {
    assert_ast_ranges_match("open System\nexit 0\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_anon_module_app_with_plain_piped_arg() {
    assert_ast_ranges_match("let result = 0\nexit <| result\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_app_with_if_arg() {
    assert_ast_ranges_match("exit <| if true then 0 else 1\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_for_each_expr() {
    assert_ast_ranges_match("for x in [1] do\n    printfn \"%d\" x\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_for_expr() {
    assert_ast_ranges_match("for i = 1 to 2 do\n    printfn \"%d\" i\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_while_expr() {
    assert_ast_ranges_match("while true do\n    printfn \"x\"\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_do_expr() {
    assert_ast_ranges_match("do printfn \"x\"\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_if_expr() {
    assert_ast_ranges_match("if true then\n    printfn \"x\"\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_match_expr() {
    assert_ast_ranges_match("match 1 with\n| 1 -> printfn \"one\"\n| _ -> printfn \"other\"\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_try_expr() {
    assert_ast_ranges_match("try\n    printfn \"x\"\nwith _ ->\n    printfn \"caught\"\n");
}

#[test]
fn diff_ast_ranges_include_trailing_newline_for_anon_module_function_expr() {
    assert_ast_ranges_match("1\n|> function\n    | 1 -> ()\n    | _ -> ()\n");
}

#[test]
fn diff_ast_ranges_do_not_extend_anon_module_app_with_nested_function_expr() {
    assert_ast_ranges_match("ignore (function | _ -> 1)\n");
}

#[test]
fn diff_sig_ast_ranges_include_trailing_newline_for_anon_module() {
    assert_sig_ast_ranges_match("val x : int\n");
}

#[test]
fn diff_sig_ast_ranges_empty_anon_module_is_zero_width_at_eof() {
    assert_sig_ast_ranges_match("\n");
}

#[test]
fn diff_sig_ast_ranges_exclude_exception_augmentation_end() {
    assert_sig_ast_ranges_match("module M\nexception E with member M : int end\n");
    assert_sig_ast_ranges_match("module M\nexception E with end\n");
    assert_sig_ast_ranges_match("module M\nmodule Inner =\n  exception E with end\n");
}
