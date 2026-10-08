//! Named cases of the attachment rules, each pinning FCS's own answer as well
//! as our agreement with it: a differential alone would pass a fixture whose
//! expectation was backwards. Several are the shrunk counterexamples the
//! property found against deliberately broken models.

use borzoi::xml_doc::source::SourceDocDecline;

use super::harness::{Graded, OracleFile, Verdict, check_paths, run_fixture};

/// [`run_fixture`] for a fixture FCS rejects: graded all the same.
fn run_fixture_allowing_errors(files: &[(&str, &str)]) -> (Vec<Graded>, Vec<OracleFile>) {
    let dir = tempfile::TempDir::new().unwrap();
    let paths: Vec<_> = files
        .iter()
        .map(|(name, text)| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p
        })
        .collect();
    check_paths(
        &paths,
        files.iter().map(|(_, t)| t.to_string()).collect(),
        &[],
    )
}

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
fn the_implicit_compiled_symbol_is_defined() {
    let src = "module M\n/// a\n#if COMPILED\n/// b\n#endif\nlet v = 1\n";
    assert_attached(src, "v", &[" a", " b"]);
}

#[test]
fn an_attribute_after_the_doc_keeps_it_and_one_before_loses_it() {
    let before = "module M\n/// a\n[<System.Obsolete(\"o\")>]\nlet v = 1\n";
    assert_attached(before, "v", &[" a"]);
    let after = "module M\n[<System.Obsolete(\"o\")>]\n/// a\nlet v = 1\n";
    assert_attached(after, "v", &[]);
}

#[test]
fn a_use_whose_type_argument_count_differs_from_its_resolution_declines() {
    // Resolution reads `CT<int>` as the non-generic `CT` (#323); FCS binds the
    // generic one. The bare `CT` agrees with both, so it keeps its doc.
    let src = "module M\n/// generic\ntype CT<'T> = int -> 'T\n/// plain\ntype CT = int -> unit\nlet f (x: CT<int>) = x\nlet g (x: CT) = x\n";
    let sites = occurrences(src, "CT");
    let (verdict, fcs) = site(&sites, src.find("CT<int>").unwrap());
    assert_eq!(fcs, [" generic"]);
    assert_eq!(verdict, Verdict::Declined(SourceDocDecline::ArityMismatch));
    let (verdict, fcs) = site(&sites, src.find("CT) =").unwrap());
    assert_eq!(fcs, [" plain"]);
    assert_eq!(verdict, Verdict::Agree { attached: true });
}

#[test]
fn an_assembly_namesake_of_another_arity_is_not_shown_the_source_doc() {
    // `Action<int>` is `System.Action<'T>` to FCS, not the project's `Action`.
    let src = "module M\nopen System\n/// mine\ntype Action = int\nlet f (x: Action<int>) = x\n";
    let sites = occurrences(src, "Action");
    let (verdict, fcs) = site(&sites, src.find("Action<int>").unwrap());
    assert_eq!(fcs, Vec::<String>::new());
    assert_eq!(verdict, Verdict::Declined(SourceDocDecline::ArityMismatch));
}

#[test]
fn a_named_argument_name_declines() {
    // The left `areSimilar` is the constructor's parameter to FCS; resolution
    // finds the local (#324).
    let src = "module M\ntype A(areSimilar: int) =\n    member _.X = areSimilar\nlet h () =\n    /// local\n    let areSimilar = 1\n    A(areSimilar = areSimilar)\n";
    let sites = occurrences(src, "areSimilar");
    let lhs = src.rfind("A(areSimilar").unwrap() + 2;
    let rhs = src.rfind("areSimilar").unwrap();
    let at = |offset: usize| {
        sites
            .iter()
            .find(|(at, _, _)| *at == offset)
            .unwrap_or_else(|| panic!("no graded site at {offset}: {sites:?}"))
    };
    assert_eq!(at(lhs).2, Vec::<String>::new());
    assert_eq!(
        at(lhs).1,
        Verdict::Declined(SourceDocDecline::NamedArgumentCandidate)
    );
    assert_eq!(at(rhs).2, [" local"]);
    assert_eq!(at(rhs).1, Verdict::Agree { attached: true });
}

#[test]
fn an_optional_named_argument_name_declines() {
    let src = "module M\ntype A(?other: int) =\n    member _.X = other\nlet h () =\n    /// local\n    let other = 1\n    A(?other = Some other)\n";
    let sites = occurrences(src, "other");
    let lhs = src.find("?other =").unwrap() + 1;
    let (_, verdict, fcs) = sites
        .iter()
        .find(|(at, _, _)| *at == lhs)
        .unwrap_or_else(|| panic!("no graded site at {lhs}: {sites:?}"));
    assert_eq!(fcs, &Vec::<String>::new());
    assert_eq!(
        verdict,
        &Verdict::Declined(SourceDocDecline::NamedArgumentCandidate)
    );
}

#[test]
fn a_named_argument_name_in_a_new_expression_declines() {
    let src = "module M\ntype A(arg: int) =\n    member _.X = arg\nlet h () =\n    /// local\n    let arg = 1\n    new A(arg = arg)\n";
    let sites = occurrences(src, "arg");
    let (verdict, fcs) = site(&sites, src.find("A(arg =").unwrap() + 2);
    assert_eq!(fcs, Vec::<String>::new());
    assert_eq!(
        verdict,
        Verdict::Declined(SourceDocDecline::NamedArgumentCandidate)
    );
}

#[test]
fn arity_mismatch_declines_across_reopened_namespaces_and_files() {
    let src = "namespace N\n/// generic\ntype T<'a> = int -> 'a\nnamespace N\n/// plain\ntype T = int -> unit\nmodule M =\n    let f (x: T<int>) = x\n";
    let sites = occurrences(src, "T");
    let (verdict, fcs) = site(&sites, src.find("T<int>").unwrap());
    assert_eq!(fcs, [" generic"]);
    assert_eq!(verdict, Verdict::Declined(SourceDocDecline::ArityMismatch));

    let a = "namespace N\n/// generic\ntype T<'a> = int -> 'a\n";
    let b = "namespace N\n/// plain\ntype T = int -> unit\nmodule M =\n    let f (x: T<int>) = x\n";
    let files = [("A.fs", a), ("B.fs", b)];
    let (graded, _) = run_fixture(&files, &[]);
    assert!(super::harness::failures(&graded).is_empty(), "{graded:?}");
}

#[test]
fn duplicate_union_cases_decline() {
    let src =
        "module M\ntype C =\n    /// one\n    | Dup of int\n    /// two\n    | Dup of string\n";
    let files = [("M.fs", src)];
    let (graded, _) = run_fixture_allowing_errors(&files);
    for g in graded.iter().filter(|g| g.name == "Dup") {
        assert_eq!(
            g.verdict,
            Verdict::Declined(SourceDocDecline::SameNameDeclarations)
        );
    }
}

#[test]
fn a_leading_tab_is_not_trimmed_by_the_implicit_summary_rule() {
    // FCS trims only spaces: a tab-only line is not empty, so the block is
    // wrapped in an implicit `<summary>` and the `<summary>` text escaped.
    let src = "module M\n///\t\n///<summary>x</summary>\nlet v = 1\n";
    assert_attached(src, "v", &["\t", "<summary>x</summary>"]);
}

/// FCS's lines and our verdict at every graded occurrence of `name`, in source
/// order, for a one-file fixture FCS checks cleanly.
fn every(src: &str, name: &str) -> Vec<(usize, bool, Verdict, Vec<String>)> {
    let files = [("M.fs", src)];
    let (graded, fcs) = run_fixture(&files, &[]);
    assert!(!fcs[0].has_errors(), "fixture does not compile:\n{src}");
    assert!(super::harness::failures(&graded).is_empty(), "{graded:#?}");
    graded
        .into_iter()
        .filter(|g| g.name == name)
        .map(|g| {
            (
                usize::from(g.range.start()),
                g.is_definition,
                g.verdict,
                g.fcs_lines.unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn a_constructor_call_shows_the_primary_constructors_own_doc() {
    // At `new K()` / `K()` FCS binds the constructor, whose doc is the block
    // between the type's name and its parameters — not the type's.
    let src = "module M\n/// The type.\ntype K\n    /// The constructor.\n    (x: int) =\n    member _.P = x\nlet a = new K(1)\nlet b : K = a\n";
    let sites = every(src, "K");
    let call = src.find("K(1)").unwrap();
    let annotation = src.find("K = a").unwrap();
    let definition = src.find("K\n").unwrap();
    for (at, is_def, verdict, fcs) in &sites {
        let expected: &[&str] = if *at == call {
            &[" The constructor."]
        } else {
            &[" The type."]
        };
        assert_eq!(fcs, expected, "FCS at {at} (definition: {is_def})");
        assert_eq!(verdict, &Verdict::Agree { attached: true }, "at {at}");
    }
    let at: Vec<_> = sites.iter().map(|s| s.0).collect();
    assert!(
        at.contains(&call) && at.contains(&annotation) && at.contains(&definition),
        "{sites:?}"
    );
}

#[test]
fn a_call_of_a_type_with_explicit_constructors_declines() {
    let src = "module M\n/// T\ntype K(x: int) =\n    /// ctor\n    new() = K(1)\n    member _.P = x\nlet a = new K()\nlet b = new K(2)\n";
    let sites = every(src, "K");
    let calls: Vec<_> = sites.iter().filter(|(_, def, _, _)| !def).collect();
    assert!(!calls.is_empty());
    for (at, _, verdict, _) in calls {
        assert_eq!(
            verdict,
            &Verdict::Declined(SourceDocDecline::ConstructorOverloads),
            "at {at}"
        );
    }
}

#[test]
fn an_attribute_name_shows_the_attribute_constructors_doc() {
    let src = "module M\n/// attr\ntype DocAttribute\n    /// pctor\n    () =\n    inherit System.Attribute()\n[<Doc>]\nlet a = 1\n[<DocAttribute>]\nlet b = 2\n";
    let sites = every(src, "DocAttribute");
    let uses: Vec<_> = sites.iter().filter(|(_, def, _, _)| !def).collect();
    assert_eq!(uses.len(), 2, "{sites:?}");
    for (at, _, verdict, fcs) in uses {
        assert_eq!(fcs, &[" pctor"], "at {at}");
        assert_eq!(verdict, &Verdict::Agree { attached: true }, "at {at}");
    }
}

#[test]
fn a_type_in_type_argument_positions_keeps_its_doc() {
    let src = "module M\n/// rec\ntype R = { V: int }\nlet a : R list = []\nlet b = System.Collections.Generic.Stack<R>()\nlet c = Unchecked.defaultof<R>\nlet d : System.Collections.Generic.Dictionary<string, R> = null\nlet e : (R * int) list = []\n/// ms\n[<Measure>]\ntype ms\nlet f (x: float<ms>) = x\n";
    for name in ["R", "ms"] {
        let sites = every(src, name);
        assert!(sites.len() >= 2, "{name}: {sites:?}");
        for (at, _, verdict, _) in &sites {
            assert_eq!(
                verdict,
                &Verdict::Agree { attached: true },
                "{name} at {at}"
            );
        }
    }
}

#[test]
fn a_qualifier_whose_member_only_a_generic_namesake_declares_shows_no_doc() {
    // `CN.M` binds `CN<'a>`, the one declaring `M` (#323).
    let src = "module M\n/// plain\ntype CN =\n    static member Other = 0\n/// generic\ntype CN<'a> =\n    static member M = 1\nlet x = CN.M\n";
    let sites = every(src, "CN");
    let use_site = src.find("CN.M").unwrap();
    let (_, _, verdict, fcs) = sites.iter().find(|s| s.0 == use_site).expect("graded");
    assert_eq!(fcs, &[" generic"]);
    assert!(matches!(verdict, Verdict::Declined(_)), "{verdict:?}");
}

#[test]
fn an_accessors_trailing_doc_and_a_doc_after_type_are_not_attached() {
    // Measured against FCS 43.12: `with /// d` before `get` adds nothing to
    // the property's doc, and a doc between `type` and the type's attributes
    // is dropped (the `type` keyword's grab wins, even when empty).
    let accessor = "module M\ntype PA() =\n    /// outer\n    static member Acc\n        with /// inner\n             get () = 1\nlet x = PA.Acc\n";
    let sites = every(accessor, "Acc");
    assert!(!sites.is_empty());
    for (_, _, verdict, fcs) in &sites {
        assert_eq!(fcs, &[" outer"]);
        assert_eq!(verdict, &Verdict::Agree { attached: true });
    }
    let inner = "module M\ntype\n    /// inner\n    [<System.Obsolete(\"o\")>] TI = int\nlet x = (1 : TI)\n";
    for (_, _, verdict, fcs) in every(inner, "TI") {
        assert_eq!(fcs, Vec::<String>::new());
        assert_eq!(verdict, Verdict::Agree { attached: false });
    }
}

#[test]
fn an_augmentation_head_shows_the_augmented_types_doc() {
    let src = "module M\n/// g\ntype G() =\n    member _.P = 1\n/// aug\ntype G with\n    member _.Q = 2\n";
    let sites = every(src, "G");
    let head = src.rfind("G with").unwrap();
    let (_, _, verdict, fcs) = sites.iter().find(|s| s.0 == head).expect("graded");
    assert_eq!(fcs, &[" g"]);
    assert_eq!(verdict, &Verdict::Agree { attached: true });
}

/// Every graded occurrence of `name`, as (byte offset, verdict, FCS lines).
fn occurrences(src: &str, name: &str) -> Vec<(usize, Verdict, Vec<String>)> {
    let files = [("M.fs", src)];
    let (graded, _) = run_fixture(&files, &[]);
    graded
        .into_iter()
        .filter(|g| g.name == name)
        .map(|g| {
            (
                usize::from(g.range.start()),
                g.verdict,
                g.fcs_lines.unwrap_or_default(),
            )
        })
        .collect()
}

/// The verdict at the occurrence of a type name starting at byte `at`, with
/// FCS's lines there, from graded `sites`.
fn site(sites: &[(usize, Verdict, Vec<String>)], at: usize) -> (Verdict, Vec<String>) {
    let (_, verdict, fcs) = sites
        .iter()
        .find(|(offset, _, _)| *offset == at)
        .unwrap_or_else(|| panic!("no graded site at {at}: {sites:?}"));
    (verdict.clone(), fcs.clone())
}

#[test]
fn a_struct_call_may_bind_the_generated_parameterless_constructor() {
    // `new S()` binds the struct's generated parameterless constructor, which
    // has no doc; `new S(1)` the documented primary one. Which one is overload
    // resolution, so both decline.
    let src = "module M\n/// S\n[<Struct>]\ntype S\n    /// pctor\n    (x: int) =\n    member _.X = x\nlet a = new S()\nlet b = new S(1)\n";
    let sites = every(src, "S");
    let calls: Vec<_> = sites.iter().filter(|(_, def, _, _)| !def).collect();
    assert_eq!(calls.len(), 2, "{sites:?}");
    for (at, _, verdict, _) in calls {
        assert_eq!(
            verdict,
            &Verdict::Declined(SourceDocDecline::ConstructorOverloads),
            "at {at}"
        );
    }
}

#[test]
fn a_member_reached_through_a_namesake_of_another_arity_shows_no_other_doc() {
    // FCS binds `T.M` to the non-generic `T`'s `M`; resolution may pick the
    // generic one's. `every` fails on any doc FCS does not attach.
    let src = "module M\ntype T =\n    /// plain M\n    static member M = 1\ntype T<'a> =\n    /// generic M\n    static member M = 2\nlet x = T.M\n";
    let sites = every(src, "M");
    let use_site = src.find("T.M").unwrap();
    let graded: Vec<_> = sites.iter().filter(|s| s.0 == use_site).collect();
    assert!(!graded.is_empty(), "{sites:?}");
    for (_, _, _, fcs) in graded {
        assert_eq!(fcs, &[" plain M"]);
    }
}
