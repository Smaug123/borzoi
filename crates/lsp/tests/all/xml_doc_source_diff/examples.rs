//! Named cases of the attachment rules, each pinning FCS's own answer as well
//! as our agreement with it: a differential alone would pass a fixture whose
//! expectation was backwards. Several are the shrunk counterexamples the
//! property found against deliberately broken models.

use super::harness::{Verdict, run_fixture};

/// FCS's lines and our verdict at the definition of the unique binder named
/// `name` in a one-file project.
fn at_definition(src: &str, name: &str) -> (Vec<String>, Verdict) {
    let files = [("M.fs", src)];
    let (graded, fcs) = run_fixture(&files, &["DEFINED_X"]);
    for f in &fcs {
        let errors: Vec<_> = f
            .diagnostics
            .iter()
            .filter(|d| d.severity == "Error")
            .map(|d| d.message.clone())
            .collect();
        assert!(
            errors.is_empty(),
            "fixture does not compile: {errors:?}\n{src}"
        );
    }
    let hits: Vec<_> = graded
        .into_iter()
        .filter(|g| g.is_definition && g.name == name)
        .collect();
    assert_eq!(hits.len(), 1, "one definition of {name}: {hits:?}");
    let g = hits.into_iter().next().unwrap();
    (g.fcs_lines.expect("paired with FCS"), g.verdict)
}

#[track_caller]
fn assert_attached(src: &str, name: &str, expected: &[&str]) {
    let (fcs, verdict) = at_definition(src, name);
    assert_eq!(fcs, expected, "FCS's doc for {name} in\n{src}");
    assert_eq!(
        verdict,
        Verdict::Agree {
            attached: !expected.is_empty()
        },
        "our doc for {name} in\n{src}"
    );
}

#[test]
fn and_binding_takes_the_doc_before_and_over_the_one_after_it() {
    let src = "module M\nlet rec f x = g x\n/// outer\nand\n    /// inner\n    g x = f x\n";
    assert_attached(src, "g", &[" outer"]);
}

#[test]
fn and_binding_falls_back_to_the_doc_after_and() {
    let src = "module M\nlet rec f x = g x\nand\n    /// inner\n    g x = f x\n";
    assert_attached(src, "g", &[" inner"]);
}

#[test]
fn a_blank_doc_before_and_still_hides_the_one_after_it() {
    let src = "module M\nlet rec f x = g x\n///\nand\n    /// inner\n    g x = f x\n";
    assert_attached(src, "g", &[]);
}

#[test]
fn and_type_falls_back_to_the_doc_after_and() {
    let src = "module M\ntype T = int\nand\n    /// inner\n    Q = string\n";
    assert_attached(src, "Q", &[" inner"]);
}

#[test]
fn an_ordinary_comment_then_a_doc_line_restarts_the_block() {
    let src = "module M\nlet a = 1 /// trailing\n// ordinary\n/// real\nlet b = 2\n";
    assert_attached(src, "b", &[" real"]);
}

#[test]
fn an_ordinary_comment_alone_does_not_break_the_block() {
    let src = "module M\n/// kept\n// ordinary\n(* block *)\nlet b = 2\n";
    assert_attached(src, "b", &[" kept"]);
}

#[test]
fn a_doc_trailing_code_attaches_to_the_next_declaration() {
    let src = "module M\nlet a = 1 /// trailing\nlet b = 2\n";
    assert_attached(src, "b", &[" trailing"]);
    assert_attached(src, "a", &[]);
}

#[test]
fn a_union_case_grabs_at_its_bar() {
    let src = "module M\ntype U =\n    | A\n    /// b\n    | B of int\n";
    assert_attached(src, "B", &[" b"]);
    assert_attached(src, "A", &[]);
}

#[test]
fn a_bar_less_first_case_grabs_at_its_name() {
    let src = "module M\ntype U =\n    /// a\n    A of int\n    | B\n";
    assert_attached(src, "A", &[" a"]);
}

#[test]
fn a_dead_region_between_doc_and_declaration_is_invisible() {
    let src = "module M\n/// a\n#if UNDEFINED_X\n/// dead\n#endif\nlet v = 1\n";
    assert_attached(src, "v", &[" a"]);
}

#[test]
fn a_live_region_contributes_its_doc_lines() {
    let src = "module M\n/// a\n#if DEFINED_X\n/// live\n#endif\nlet v = 1\n";
    assert_attached(src, "v", &[" a", " live"]);
}

#[test]
fn an_attribute_after_the_doc_keeps_it_and_one_before_loses_it() {
    let before = "module M\n/// a\n[<System.Obsolete(\"o\")>]\nlet v = 1\n";
    assert_attached(before, "v", &[" a"]);
    let after = "module M\n[<System.Obsolete(\"o\")>]\n/// a\nlet v = 1\n";
    assert_attached(after, "v", &[]);
}

#[test]
fn constructor_calls_are_graded_against_fcs() {
    let src =
        "module M\n/// The type.\ntype K() =\n    member _.P = 1\nlet a = K()\nlet b = new K()\n";
    let files = [("M.fs", src)];
    let (graded, _) = run_fixture(&files, &[]);
    let calls: Vec<_> = graded
        .iter()
        .filter(|g| g.name == "K" && !g.is_definition)
        .map(|g| {
            (
                usize::from(g.range.start()),
                g.verdict.clone(),
                g.fcs_lines.clone(),
            )
        })
        .collect();
    eprintln!("constructor-call occurrences: {calls:?}");
    assert!(super::harness::failures(&graded).is_empty());
}

#[test]
fn a_leading_tab_is_not_trimmed_by_the_implicit_summary_rule() {
    // FCS trims only spaces: a tab-only line is not empty, so the block is
    // wrapped in an implicit `<summary>` and the `<summary>` text escaped.
    let src = "module M\n///\t\n///<summary>x</summary>\nlet v = 1\n";
    assert_attached(src, "v", &["\t", "<summary>x</summary>"]);
}
