//! `type T = X` with a single-identifier target, against FCS: an
//! abbreviation when `X` names a type in scope, and otherwise a union whose
//! nullary case `X` shadows a same-named module value
//! (`TyconCoreAbbrevThatIsReallyAUnion`). The grid crosses what `X` is (the
//! type's own name, a later type of the same recursive group, an earlier
//! type, a BCL type, nothing) with `[<RequireQualifiedAccess>]` and a module
//! value `X` declared before the type, and grades a later bare `X`: never
//! committed to anything but FCS's declaration, and committed to the module
//! value wherever the target certainly names a type.

use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_resolve_uses, invoke_fcs_dump_census, parse_census_jsonl, temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use rowan::TextRange;

/// The declarations, with the target name `X` they use and whether `X`
/// certainly names a type (so the later bare `X` must commit).
const SHAPES: &[(&str, &str, bool)] = &[
    ("self", "type X = X\n", false),
    ("later-in-group", "type A = X\nand X = | C\n", true),
    ("earlier-type", "type X = | C\ntype A = X\n", true),
    ("unknown", "type A = X\n", false),
];

#[test]
fn abbreviation_targets_shadow_values_as_fcs_does() {
    let mut programs = Vec::new();
    for (label, decl, names_type) in SHAPES {
        for rqa in [false, true] {
            let attr = if rqa {
                "[<RequireQualifiedAccess>]\n"
            } else {
                ""
            };
            // The attribute applies to the group's first definition.
            let src = format!("module M\n\nlet X = 42\n{attr}{decl}let answer = X\n");
            programs.push((*label, rqa, *names_type, src));
        }
    }
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|(_, _, _, src)| temp_fs_file("abbreviation_case", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let mut failures = Vec::new();
    let mut graded = 0usize;
    for ((label, rqa, names_type, src), file) in programs.iter().zip(&census) {
        if !file.ok || file.has_check_errors {
            // `type A = X` with nothing named `X` is a union FCS accepts; a
            // rejected program would be recovery, which grades nothing.
            failures.push(format!("{label} rqa={rqa}: FCS rejected\n{src}"));
            continue;
        }
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{src}");
        let rf = resolve_file(
            &ImplFile::cast(parsed.root.clone()).unwrap(),
            &ProjectItems::default(),
            &AssemblyEnv::default(),
            &SyntaxRecovery::of(&parsed),
        );
        let at = src.rfind("= X").unwrap() + 2;
        let Some(u) = census_resolve_uses(file, src)
            .into_iter()
            .find(|u| u.start == at && !u.is_from_definition)
        else {
            failures.push(format!("{label} rqa={rqa}: FCS reports no use\n{src}"));
            continue;
        };
        let ours = rf
            .resolution_at(TextRange::new(
                u32::try_from(at).unwrap().into(),
                u32::try_from(at + 1).unwrap().into(),
            ))
            .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
            .and_then(|r| rf.resolved_def(r))
            .map(|d| usize::from(d.range.start()));
        graded += 1;
        let value_at = src.find("let X").unwrap() + 4;
        match (u.decl, ours) {
            (CensusDecl::InFile(s, _), Some(ours)) if s == ours => {}
            (CensusDecl::InFile(s, _), None) if s != value_at || !(names_type | rqa) => {}
            (decl, ours) => failures.push(format!(
                "{label} rqa={rqa}: we gave {ours:?}, FCS {decl:?}\n{src}"
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(graded, programs.len());
}
