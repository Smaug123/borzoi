//! In-file type names declared at more than one generic arity, against FCS.
//!
//! FCS looks a type name up by name *and* arity first: with both `E` and
//! `E<'T>` in scope, a bare `E` is the arity-0 one and `E<int>` the generic
//! one, whichever was declared later and whichever container declared it. A
//! name-keyed lookup that hands back the latest same-named declaration binds
//! the wrong one. This grid crosses ways of declaring the pair (one recursive
//! group in either order, separate groups in either order, union pairs, the
//! generic one in a nested module) with type-position uses (a `let`
//! parameter, a member parameter, a return annotation, an augmentation
//! member's parameter, explicit and postfix type arguments, and a union case
//! through its type in value position), and asserts
//! certain-implies-exact at every use FCS reports: we bind FCS's declaration
//! or decline.
//!
//! A control row declares only `E`, where every bare-`E` use must commit, so
//! the grid cannot pass by declining everything.

use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_resolve_uses, invoke_fcs_dump_census, parse_census_jsonl, temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use rowan::TextRange;

/// Ways to declare `E`, each at module level of `module M`. The first is the
/// single-arity control.
const DECLS: &[(&str, &str)] = &[
    ("only-E", "type E(x: int) =\n    member _.X = x\n"),
    (
        "rec-E-then-generic",
        "type E(x: int) =\n    member _.X = x\nand E<'T>(y: int) =\n    inherit E(y)\n",
    ),
    (
        "rec-generic-then-E",
        "type E<'T>(y: int) =\n    member _.Y = y\nand E(x: int) =\n    inherit E<int>(x)\n",
    ),
    (
        "E-then-generic",
        "type E(x: int) =\n    member _.X = x\ntype E<'T>(y: int) =\n    inherit E(y)\n",
    ),
    (
        "generic-then-E",
        "type E<'T>(y: int) =\n    member _.Y = y\ntype E(x: int) =\n    inherit E<int>(x)\n",
    ),
    (
        "unions",
        "type E =\n    | A\n    | B\ntype E<'T> =\n    | C of 'T\n",
    ),
    (
        "generic-in-nested-module",
        "type E(x: int) =\n    member _.X = x\nmodule N =\n    type E<'T>(y: int) =\n        member _.Y = y\nopen N\n",
    ),
];

/// Uses of `E` in type position, each a module-level declaration.
const USES: &[&str] = &[
    "let f (t: E) = ()\n",
    "let g (t: E<int>) = ()\n",
    "let h (t: int E) = ()\n",
    "let r () : E option = None\n",
    "type F() =\n    static member B(t: E) = ()\n",
    "type E with\n    static member Aug(t: E) = ()\n",
    // Value position: a case through its union (FCS rejects it for the
    // class rows).
    "let v = E.A\n",
];

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

#[test]
fn same_named_types_of_different_arity_never_wrong_resolve() {
    let mut programs: Vec<(&str, String)> = Vec::new();
    for (label, decl) in DECLS {
        for u in USES {
            programs.push((label, format!("module M\n\n{decl}\n{u}")));
        }
    }
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|(_, src)| temp_fs_file("type_arity", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let mut wrong = Vec::new();
    let mut graded = 0usize;
    let mut control_declined = Vec::new();
    let mut control_committed = 0usize;
    let mut fcs_rejected = 0usize;
    for ((label, src), file) in programs.iter().zip(&census) {
        if !file.ok || file.has_check_errors {
            fcs_rejected += 1;
            continue;
        }
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{src}\n{:?}", parsed.errors);
        let recovery = SyntaxRecovery::of(&parsed);
        let rf = resolve_file(
            &ImplFile::cast(parsed.root).unwrap(),
            &ProjectItems::default(),
            &AssemblyEnv::default(),
            &recovery,
        );
        for u in census_resolve_uses(file, src) {
            if u.is_from_definition || u.start == u.end || &src[u.start..u.end] != "E" {
                continue;
            }
            let at = span(u.start, u.end);
            let res = rf.resolution_at(at);
            let ours = res
                .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
                .and_then(|r| rf.resolved_def(r))
                .map(|d| d.range);
            match (u.decl, ours) {
                (CensusDecl::InFile(s, e), Some(def)) => {
                    graded += 1;
                    if def != span(s, e) {
                        wrong.push(format!(
                            "{label}: `E` at {at:?} bound {def:?}, FCS declares {:?}\n{src}",
                            span(s, e)
                        ));
                    }
                    if *label == "only-E" {
                        control_committed += 1;
                    }
                }
                (CensusDecl::InFile(..), None) if *label == "only-E" => {
                    control_declined.push(format!("`E` at {at:?} gave {res:?}\n{src}"));
                }
                (CensusDecl::OtherFile | CensusDecl::Absent, Some(def)) => wrong.push(format!(
                    "{label}: `E` at {at:?} bound {def:?}, FCS declares outside the file\n{src}"
                )),
                _ => {}
            }
        }
    }
    eprintln!(
        "type-arity grid: {} programs, {fcs_rejected} rejected by FCS, {graded} commits graded",
        programs.len()
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        control_declined.is_empty(),
        "a single-arity `E` must commit:\n{}",
        control_declined.join("\n")
    );
    assert!(
        control_committed > 0 && fcs_rejected < programs.len() / 2,
        "the grid is vacuous: {control_committed} control commits, {fcs_rejected} of {} \
         programs rejected",
        programs.len()
    );
}

/// The other declaration need not be in the file: with `type List` in the
/// file and `System.Collections.Generic` open, FCS binds `List<int>` to the
/// BCL's generic list and only a bare `List` to the file's.
#[test]
fn an_in_file_type_does_not_capture_a_use_of_another_arity() {
    let src = "module M\nopen System.Collections.Generic\ntype List(x: int) =\n    member _.X = x\nlet f (t: List<int>) = t.Count\nlet g (t: List) = t.X\n";
    let path = temp_fs_file("type_arity_bcl", src);
    let census = parse_census_jsonl(&invoke_fcs_dump_census(std::slice::from_ref(&path)));
    let _ = std::fs::remove_file(&path);
    let file = &census[0];
    assert!(file.ok && !file.has_check_errors, "FCS rejected {src}");
    let parsed = parse(src);
    let rf = resolve_file(
        &ImplFile::cast(parsed.root.clone()).unwrap(),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &SyntaxRecovery::of(&parsed),
    );
    let ours = |at: usize| {
        rf.resolution_at(span(at, at + 4))
            .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
            .and_then(|r| rf.resolved_def(r))
            .map(|d| d.range)
    };
    let uses = census_resolve_uses(file, src);
    let decl_at = |at: usize| {
        uses.iter()
            .find(|u| u.start == at && !u.is_from_definition)
            .map(|u| u.decl)
    };
    let generic = src.find("List<int>").unwrap();
    let bare = src.find("List) =").unwrap();
    let defined = src.find("List(x").unwrap();
    assert!(matches!(decl_at(generic), Some(CensusDecl::OtherFile)));
    assert_eq!(ours(generic), None, "`List<int>` is the BCL's");
    assert!(matches!(decl_at(bare), Some(CensusDecl::InFile(s, _)) if s == defined));
    assert_eq!(ours(bare), Some(span(defined, defined + 4)));
}

/// An `open` is scoped to its module: one inside a nested module that has
/// closed does not contest the outer type a later annotation names.
#[test]
fn an_open_in_a_closed_nested_module_does_not_contest_a_type() {
    let src = "module M\ntype T =\n    | A\nmodule N =\n    open System\n    let n = 0\nlet f (t: T) = t\n";
    let path = temp_fs_file("type_open_scope", src);
    let census = parse_census_jsonl(&invoke_fcs_dump_census(std::slice::from_ref(&path)));
    let _ = std::fs::remove_file(&path);
    let file = &census[0];
    assert!(file.ok && !file.has_check_errors, "FCS rejected {src}");
    let parsed = parse(src);
    let rf = resolve_file(
        &ImplFile::cast(parsed.root.clone()).unwrap(),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &SyntaxRecovery::of(&parsed),
    );
    let at = src.find("T) = t").unwrap();
    let defined = src.find("T =").unwrap();
    let uses = census_resolve_uses(file, src);
    assert!(uses.iter().any(|u| u.start == at
        && !u.is_from_definition
        && matches!(u.decl, CensusDecl::InFile(s, _) if s == defined)));
    let ours = rf
        .resolution_at(span(at, at + 1))
        .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
        .and_then(|r| rf.resolved_def(r))
        .map(|d| d.range);
    assert_eq!(ours, Some(span(defined, defined + 1)));
}
