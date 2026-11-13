//! Annotations applying one of FSharp.Core's generic abbreviations — `int
//! option`, `string list`, `seq<int>`, `int array` — against FCS.
//!
//! FCS names such a type by the abbreviation's target
//! (`Microsoft.FSharp.Core.FSharpOption<System.Int32>`, `System.Int32[]`), so
//! the annotation is read through the abbreviation only when the abbreviation
//! is FSharp.Core's own — its assembly's name *and* public-key token, as the
//! signature bridge checks it — never because a name is spelled `option`. The
//! grid crosses the abbreviations with argument shapes (primitives, a tuple, a
//! nested application, a project type) and with what can take the name: a
//! project type, an in-file abbreviation an opened module supplies, and an
//! assembly's own `array` (the structural-abbreviation fixture). Every type we
//! commit is graded against FCS's `binder-types` and `types`; the uncontested
//! cells over primitive arguments must commit.

use std::collections::BTreeMap;
use std::path::Path;

use crate::common::{GradedDumps, invoke_fcs_dump_types_project_with_refs, temp_fs_tree};
use crate::resolve_terminal_abbreviation_diff::{env, fixture_dll};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

/// `(label, spelling)`: `{A}` is the argument.
const HEADS: &[(&str, &str)] = &[
    ("option", "{A} option"),
    ("option-prefix", "option<{A}>"),
    ("voption", "{A} voption"),
    ("list", "{A} list"),
    ("list-prefix", "list<{A}>"),
    ("ref", "{A} ref"),
    ("seq", "seq<{A}>"),
    ("ResizeArray", "ResizeArray<{A}>"),
    ("array", "{A} array"),
    ("qualified-option", "{A} Microsoft.FSharp.Core.option"),
];

/// `(label, spelling, primitive)`.
const ARGS: &[(&str, &str, bool)] = &[
    ("int", "int", true),
    ("string", "string", true),
    ("unit", "unit", true),
    ("tuple", "(int * string)", false),
    ("nested", "int list", false),
    ("project", "T", false),
];

/// What may take the abbreviation's name, declared before the uses.
const CONTEXTS: &[(&str, &str)] = &[
    ("none", ""),
    ("project-option", "type option<'a> = { O : 'a }\n"),
    (
        "opened-in-file-abbreviation",
        "module X =\n    type list<'a> = 'a option\nopen X\n",
    ),
    ("opened-assembly-array", "open AbbrevNominal\n"),
];

fn program(context: &str, ann: &str) -> String {
    format!(
        "module M\n\ntype T = {{ X : int }}\n{context}\n\
         let p (x : {ann}) = x\n\
         let v : {ann} = Unchecked.defaultof<_>\n\
         let r () : {ann} = Unchecked.defaultof<_>\n"
    )
}

#[test]
fn fsharp_core_abbreviations_bridge_as_fcs_names_them() {
    let mut programs = Vec::new();
    let mut must_commit = Vec::new();
    for (ctx, decl) in CONTEXTS {
        for (head, spelling) in HEADS {
            for (arg, a, primitive) in ARGS {
                let label = format!("{ctx}/{head}/{arg}");
                // `ResizeArray`'s target is in `System.Collections`, which this
                // env does not load, so it is graded but cannot commit.
                if *ctx == "none" && *primitive && *head != "ResizeArray" {
                    must_commit.push(label.clone());
                }
                programs.push((label, program(decl, &spelling.replace("{A}", a))));
            }
        }
    }
    let mut wrong = Vec::new();
    let mut rejected = Vec::new();
    let mut committed: BTreeMap<String, usize> = BTreeMap::new();
    for (label, src) in &programs {
        let (root, written) = temp_fs_tree("fsharp_core_abbrev", &[("M.fs", src)]);
        let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let dumps = invoke_fcs_dump_types_project_with_refs(&paths, &[fixture_dll()]);
        let _ = std::fs::remove_dir_all(&root);
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{label}: {:?}", parsed.errors);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let (types_json, binders_json) = &dumps[0];
        let dump = GradedDumps::read(types_json, binders_json, src, Some(&file))
            .unwrap_or_else(|| panic!("{label}: unplaceable unconverted declaration"));
        if !dump.errors.is_empty() {
            rejected.push(format!("{label}: {:?}\n{src}", dump.errors));
            continue;
        }
        let resolved = resolve_file(&file, &ProjectItems::default(), env(), &recovery);
        let inferred = infer_file(
            &file,
            &resolved,
            env(),
            &borzoi_sema::PrecedingTypes::default(),
        );
        for (range, ty) in inferred.types() {
            let key = (
                u32::from(range.start()) as usize,
                u32::from(range.end()) as usize,
            );
            let ours = ty.render(env());
            if dump.types.get(&key) != Some(&ours) {
                wrong.push(format!(
                    "{label}: `{}`: ours `{ours}`, FCS {:?}\n{src}",
                    &src[key.0..key.1],
                    dump.types.get(&key)
                ));
            }
        }
        for (def, ty) in inferred.def_types() {
            let r = resolved.def(*def).range;
            let key = (u32::from(r.start()) as usize, u32::from(r.end()) as usize);
            let ours = ty.render(env());
            match dump.binders.get(&key) {
                Some(theirs) if *theirs == ours && !dump.hides_constraint(key, &ours) => {
                    if &src[key.0..key.1] == "x" {
                        *committed.entry(label.clone()).or_default() += 1;
                    }
                }
                theirs => wrong.push(format!(
                    "{label}: binder `{}`: ours `{ours}`, FCS {theirs:?}\n{src}",
                    &src[key.0..key.1]
                )),
            }
        }
    }
    assert!(wrong.is_empty(), "wrong commits:\n{}", wrong.join("\n"));
    assert!(
        rejected.is_empty(),
        "every cell is valid F#; FCS rejected:\n{}",
        rejected.join("\n")
    );
    let missing: Vec<&String> = must_commit
        .iter()
        .filter(|l| !committed.contains_key(*l))
        .collect();
    assert!(
        missing.is_empty(),
        "cells whose annotated parameter must commit did not:\n{missing:#?}"
    );
}

/// An abbreviation from another assembly that permutes its parameters
/// (`swap<'a, 'b> = Choice<'b, 'a>`): read as written it would name
/// `FSharpChoice<int, string>`, where FCS names `FSharpChoice<string, int>`.
/// It is not FSharp.Core's, so it declines.
#[test]
fn a_parameter_permuting_abbreviation_from_another_assembly_declines() {
    let src = "module M\n\nopen AbbrevNominal\n\nlet p (x : swap<int, string>) = x\n";
    let (root, written) = temp_fs_tree("swap_abbrev", &[("M.fs", src)]);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let dumps = invoke_fcs_dump_types_project_with_refs(&paths, &[fixture_dll()]);
    let _ = std::fs::remove_dir_all(&root);
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let (types_json, binders_json) = &dumps[0];
    let dump = GradedDumps::read(types_json, binders_json, src, Some(&file)).expect("placeable");
    assert!(dump.errors.is_empty(), "{:?}", dump.errors);
    let resolved = resolve_file(&file, &ProjectItems::default(), env(), &recovery);
    let inferred = infer_file(
        &file,
        &resolved,
        env(),
        &borzoi_sema::PrecedingTypes::default(),
    );
    for (def, ty) in inferred.def_types() {
        let r = resolved.def(*def).range;
        let key = (u32::from(r.start()) as usize, u32::from(r.end()) as usize);
        assert_eq!(
            dump.binders.get(&key),
            Some(&ty.render(env())),
            "binder `{}`",
            &src[key.0..key.1]
        );
    }
}
