//! Differential test: our cross-file resolution against FCS, via the
//! project-aware `uses-project` oracle (Stage A).
//!
//! The property (design-doc D7): for every symbol use FCS resolves to a
//! declaration *in some project file*, our resolution at that use's range either
//! **agrees** — a `Local` / `Item` whose `Def` has the same range and declaring
//! file as FCS's — or is honestly `Deferred` (or unrecorded). We never return
//! `Unresolved` where FCS resolved, and never point at the wrong file/binder.
//!
//! To keep the property from passing vacuously (all-`Deferred` proves nothing),
//! each corpus entry declares how many *cross-file* qualified references it
//! contains, and we assert we resolve exactly that many. Module-qualifier uses
//! (`Shared` in `Shared.foo`) and the implicit module symbol resolve to a
//! module/namespace, which we do not model as a def yet, so they fall in the
//! allowed `Deferred` / unrecorded bucket.

use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// One project: `(label, source)` per file in Compile order, and the number of
/// cross-file qualified references the project should resolve.
struct Project {
    files: Vec<(&'static str, &'static str)>,
    expected_cross_file: usize,
}

/// Resolve `project`, run FCS over it, and assert the differential property.
fn assert_matches_fcs(project: &Project) {
    // Materialise the files (FCS reads them from disk).
    let written: Vec<(std::path::PathBuf, String)> = project
        .files
        .iter()
        .map(|(label, src)| (temp_fs_file(label, src), (*src).to_string()))
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();

    let json = invoke_fcs_dump_project(&paths);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    // Our resolution over the same Compile-ordered sources.
    let asts: Vec<ImplFile> = written.iter().map(|(_, src)| impl_file(src)).collect();
    let proj = resolve_project(&asts, &AssemblyEnv::default());

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    let mut cross_file_agreed = 0usize;
    for (i, (path, _src)) in written.iter().enumerate() {
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("FCS reported no uses for {path:?}"));
        let rf = proj.file(i);

        for u in &fu.uses {
            // Skip the implicit module symbol (zero-width range).
            if u.start == u.end {
                continue;
            }
            // Only uses FCS declares within a project file are in scope here;
            // out-of-project decls (FSharp.Core, operators) project to `None`.
            let Some(decl) = &u.decl else {
                continue;
            };
            let use_range = span(u.start, u.end);

            match rf.resolution_at(use_range) {
                // Honest "say nothing" — allowed (D5).
                None | Some(Resolution::Deferred(_)) => {}
                Some(Resolution::Unresolved) => {
                    panic!(
                        "Unresolved where FCS resolved: {:?} at {use_range:?} in {path:?}",
                        u.name
                    );
                }
                Some(res) => {
                    // A Local lives in this file; an Item may be cross-file.
                    let (def_idx, def_range) = match res {
                        Resolution::Item(_) => {
                            let (idx, def) = proj.item_def(res).expect("item def for Item");
                            (idx, def.range)
                        }
                        Resolution::Local(_) => (i, rf.resolved_def(res).expect("local def").range),
                        // This corpus has no referenced assemblies (empty
                        // AssemblyEnv), so no Entity/Member can arise; the outer
                        // arm already handled Deferred/Unresolved.
                        Resolution::Entity(_)
                        | Resolution::Member { .. }
                        | Resolution::Deferred(_)
                        | Resolution::Unresolved => unreachable!(),
                    };
                    let def_path = &written[def_idx].0;
                    assert_eq!(
                        def_path.file_name(),
                        decl.file.file_name(),
                        "use {:?} at {use_range:?}: we point into {def_path:?}, FCS declares in {:?}",
                        u.name,
                        decl.file,
                    );
                    assert_eq!(
                        def_range,
                        span(decl.start, decl.end),
                        "use {:?} at {use_range:?}: we point at {def_range:?}, FCS declares at {}..{}",
                        u.name,
                        decl.start,
                        decl.end,
                    );
                    if decl.file.file_name() != path.file_name() {
                        cross_file_agreed += 1;
                    }
                }
            }
        }
    }

    assert_eq!(
        cross_file_agreed, project.expected_cross_file,
        "cross-file references resolved for {:?}",
        project.files,
    );
}

#[test]
fn cross_file_resolution_agrees_with_fcs() {
    let corpus = [
        // A later file references an earlier file's value, qualified.
        Project {
            files: vec![
                ("proj_value_1", "module Shared\nlet foo = 1\n"),
                ("proj_value_2", "module Other\nlet bar = Shared.foo\n"),
            ],
            expected_cross_file: 1,
        },
        // …a function, applied across files.
        Project {
            files: vec![
                ("proj_fn_1", "module M\nlet add a b = a\n"),
                ("proj_fn_2", "module N\nlet z = M.add 1 2\n"),
            ],
            expected_cross_file: 1,
        },
        // A third file references two distinct earlier files.
        Project {
            files: vec![
                ("proj_three_1", "module A\nlet x = 1\n"),
                ("proj_three_2", "module B\nlet y = 2\n"),
                ("proj_three_3", "module C\nlet z = A.x\nlet w = B.y\n"),
            ],
            expected_cross_file: 2,
        },
        // A nested module's body: a sibling reference inside the nested module
        // resolves (the `FSharp.Core/string.fs` shape). The module-name symbols
        // (`Topm`, `Innerm`) are unmodeled and fall in the allowed Deferred/
        // unrecorded bucket; the `vv` use is the checked intra-file resolution.
        Project {
            files: vec![(
                "nested_intra",
                "module Topm\nmodule Innerm =\n    let vv = 1\n    let ww = vv\n",
            )],
            expected_cross_file: 0,
        },
        // A cross-file reference into a *nested* module's exported value, fully
        // qualified by namespace + module (`Demo.Calc.answer`): nested-module
        // values now carry their full qualified export path.
        Project {
            files: vec![
                (
                    "nested_x1",
                    "namespace Demo\nmodule Calc =\n    let answer = 1\n",
                ),
                ("nested_x2", "module Otherm\nlet useit = Demo.Calc.answer\n"),
            ],
            expected_cross_file: 1,
        },
        // A nested module under `namespace global` is a real global module, so
        // its value is bare-cross-file referenceable (`Calc.answer`) — unlike an
        // anonymous file's nested module. FCS agrees the reference resolves.
        Project {
            files: vec![
                (
                    "global_x1",
                    "namespace global\nmodule Calc =\n    let answer = 1\n",
                ),
                ("global_x2", "module Otherm\nlet useit = Calc.answer\n"),
            ],
            expected_cross_file: 1,
        },
        // Substep 3: a plain `open M` of an earlier file's module brings its
        // direct values into *unqualified* scope, so a bare `foo` resolves to the
        // earlier `module Shared`'s value. (The `Shared` module-name use in the
        // `open` clause is unmodeled and falls in the allowed Deferred bucket.)
        Project {
            files: vec![
                ("open_mod_1", "module Shared\nlet foo = 1\n"),
                ("open_mod_2", "module Other\nopen Shared\nlet y = foo\n"),
            ],
            expected_cross_file: 1,
        },
        // Substep 3, *same-file*: `open Shared` shortened by the enclosing
        // `namespace Demo` brings sibling `module Shared`'s `foo` into scope, so
        // the bare `foo` in `module N` resolves to it (an in-file Item — not
        // counted as cross-file, but checked for correctness against FCS). Guards
        // that the enumeration agrees with FCS on the resolved binder.
        Project {
            files: vec![(
                "open_mod_same",
                "namespace Demo\nmodule Shared =\n    let foo = 1\nmodule N =\n    open Shared\n    let y = foo\n",
            )],
            expected_cross_file: 0,
        },
        // Substep 3, *chained* open: `open Shared; open Sub` resolves `Sub` as
        // `Shared.Sub` (chained), and the later open wins — so the bare `bar`,
        // present in both `Shared` and `Shared.Sub`, resolves to the submodule's
        // `Demo.Shared.Sub.bar`. Correctness-guarded against FCS (must point at
        // `Sub`'s binder, not `Shared`'s).
        Project {
            files: vec![(
                "open_mod_chain",
                "namespace Demo\nmodule Shared =\n    let bar = 1\n    module Sub =\n        let bar = 7\nmodule N =\n    open Shared\n    open Sub\n    let y = bar\n",
            )],
            expected_cross_file: 0,
        },
        // A *qualified* value reference through a same-file module: `Target.foo`
        // (relative to `namespace Demo`) resolves to `Demo.Target.foo`.
        // Correctness-guarded against FCS (same-file Item, not counted).
        Project {
            files: vec![(
                "qual_value_same_file",
                "namespace Demo\nmodule Target =\n    let foo = 1\nmodule N =\n    let x = Target.foo\n",
            )],
            expected_cross_file: 0,
        },
        // …and a multi-segment `A.B.foo` through a nested module.
        Project {
            files: vec![(
                "qual_value_nested",
                "namespace Demo\nmodule A =\n    module B =\n        let foo = 1\nmodule N =\n    let x = A.B.foo\n",
            )],
            expected_cross_file: 0,
        },
        // …and a qualified value through a module abbreviation: `Alias.foo` →
        // `Demo.Target.foo`.
        Project {
            files: vec![(
                "qual_value_alias",
                "namespace Demo\nmodule Target =\n    let foo = 1\nmodule N =\n    module Alias = Target\n    let x = Alias.foo\n",
            )],
            expected_cross_file: 0,
        },
        // A cross-file **union case**, opened: `open Lib` brings the earlier file's
        // DU case `Red` into scope (`Lib.Color.Red`, declared in file0).
        Project {
            files: vec![
                ("xcase_open_1", "module Lib\ntype Color = Red | Green\n"),
                ("xcase_open_2", "module Client\nopen Lib\nlet x = Red\n"),
            ],
            expected_cross_file: 1,
        },
        // …and via the module shortcut path `Lib.Red` (type elided).
        Project {
            files: vec![
                ("xcase_shortcut_1", "module Lib\ntype Color = Red | Green\n"),
                ("xcase_shortcut_2", "module Client\nlet x = Lib.Red\n"),
            ],
            expected_cross_file: 1,
        },
        // …and a cross-file **exception** constructor, opened.
        Project {
            files: vec![
                ("xexn_1", "module Lib\nexception MyErr of string\n"),
                ("xexn_2", "module Client\nopen Lib\nlet e = MyErr \"x\"\n"),
            ],
            expected_cross_file: 1,
        },
        // A cross-file case in **pattern** position: `match c with Red | Green`
        // resolves both case heads to file0 (two cross-file references).
        Project {
            files: vec![
                ("xpat_1", "module Lib\ntype Color = Red | Green\n"),
                (
                    "xpat_2",
                    "module Client\nopen Lib\nlet f c = match c with Red -> 1 | Green -> 2\n",
                ),
            ],
            expected_cross_file: 2,
        },
        // A **value-shadowed** cross-file case: `type T = Red` then `let Red = 0`.
        // The constructor index splits the namespaces — expression `Red` resolves
        // to the value, pattern `Red` to the case (two cross-file references, one
        // each).
        Project {
            files: vec![
                (
                    "xshadow_1",
                    "module Lib\ntype T = Red | Blue\nlet Red = 0\n",
                ),
                (
                    "xshadow_2",
                    "module Client\nopen Lib\nlet v = Red\nlet f x = match x with Red -> 1 | _ -> 2\n",
                ),
            ],
            expected_cross_file: 2,
        },
        // A case declared **directly under a namespace**, brought in by a bare
        // `open <namespace>` — resolved via the project-namespace index.
        Project {
            files: vec![
                ("xns_1", "namespace Lib\ntype Color = Red | Green\n"),
                ("xns_2", "module Client\nopen Lib\nlet x = Red\n"),
            ],
            expected_cross_file: 1,
        },
        // …and the *relative* form: a case in `namespace Outer.Inner`, opened as
        // `open Inner` from inside `namespace Outer`.
        Project {
            files: vec![
                (
                    "xns_rel_1",
                    "namespace Outer.Inner\ntype Color = Red | Green\n",
                ),
                (
                    "xns_rel_2",
                    "namespace Outer\nmodule Client =\n    open Inner\n    let x = Red\n",
                ),
            ],
            expected_cross_file: 1,
        },
        // …and the *chained* relative form: `open Inner; open Deep` reaches
        // `Outer.Inner.Deep` (the second open shortens against the namespace the
        // first resolved to).
        Project {
            files: vec![
                (
                    "xns_chain_1",
                    "namespace Outer.Inner.Deep\ntype Color = Red | Green\n",
                ),
                (
                    "xns_chain_2",
                    "namespace Outer\nmodule C =\n    open Inner\n    open Deep\n    let x = Red\n",
                ),
            ],
            expected_cross_file: 1,
        },
        // A later `open A` whose cross-file `A.Pal` owns nothing named `Color`
        // is transparent for the head of `Pal.Color.Red`: FCS backtracks to the
        // lexical `Client.Pal` and binds its case (probe CF2expr). The emitted
        // resolution is same-file, so it must agree with FCS's same-file decl.
        Project {
            files: vec![
                (
                    "xopen_res_1",
                    "namespace A\nmodule Pal =\n    let unrelated = 1\n",
                ),
                (
                    "xopen_res_2",
                    "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // …the pattern-position variant (probe CF2pat).
        Project {
            files: vec![
                (
                    "xopen_pat_1",
                    "namespace A\nmodule Pal =\n    let unrelated = 1\n",
                ),
                (
                    "xopen_pat_2",
                    "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 0 | _ -> 1\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // …a cross-file `type Color` without the case commits nothing in
        // pattern position — FCS backtracks to the lexical candidate (CF3pat).
        Project {
            files: vec![
                (
                    "xopen_ty_pat_1",
                    "namespace A\nmodule Pal =\n    type Color = Green | Indigo\n",
                ),
                (
                    "xopen_ty_pat_2",
                    "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 0 | _ -> 1\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // …a cross-file ABBREVIATION at the segment commits through its target
        // (FCS binds `A.Hue.Red` — probe CF8); sema defers, which the
        // agree-or-defer property allows, and emitting the lexical case here
        // would fail it.
        Project {
            files: vec![
                (
                    "xopen_abbrev_1",
                    "namespace A\ntype Hue = Red | Blue\nmodule Pal =\n    type Color = Hue\n",
                ),
                (
                    "xopen_abbrev_2",
                    "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // Project-type STATIC MEMBERS (docs/project-type-member-plan.md): the
        // member index emits `Color2.Red` / `Pal.Color.Red` where the segment is
        // an emit-eligible static (probes M1/M2a/M2d/M4b), including past a
        // companion module value and a same-file augmentation. All emitted
        // resolutions are same-file — the harness checks agreement with FCS's
        // decl (the member's name range) at every resolved use.
        Project {
            files: vec![
                ("xmember_1", "module Lib\nlet unrelated = 1\n"),
                (
                    "xmember_2",
                    "module Demo\nmodule Pal =\n    type Color() =\n        static member Red = 1\n    module Color =\n        let Red = 2\nlet a = Pal.Color.Red\ntype Color2() =\n    static member Red = 2\nlet b = Color2.Red\nlet Color2 = {| Red = 3 |}\ntype Color3() =\n    static member val Green = 1 with get, set\ntype Color3 with\n    static member Red = 9\nlet c = Color3.Red\nlet d = Color3.Green\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // A cross-file `module Color` at the head OUTRANKS the same-file type
        // for the qualifier (probes M13/M14 — the r13 module-namespace rule,
        // cross-file): `Color.Red` resolves to the earlier file's value, not
        // the same-file static member / enum case.
        Project {
            files: vec![
                ("xmember_modhead_1", "module Color\nlet Red = 99\n"),
                (
                    "xmember_modhead_2",
                    "module Client\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n",
                ),
            ],
            expected_cross_file: 1,
        },
        Project {
            files: vec![
                ("xenum_modhead_1", "module Color\nlet Red = 99\n"),
                (
                    "xenum_modhead_2",
                    "module Client\ntype Color =\n    | Red = 0\n    | Blue = 1\nlet x = Color.Red\n",
                ),
            ],
            expected_cross_file: 1,
        },
        // The definite-value head gate (probes M20a–M20i): FCS's unqualified
        // slot is ONE latest-wins list across the value and type namespaces —
        // a compound head is member access on a value only while the value is
        // the slot's latest entry; a later type EVICTS it and modules are then
        // searched first. The evicted shapes (M20a/M20b/M20h) resolve to the
        // cross-file module's value; the held shapes (M20c/M20d/M20f/M20g/
        // M20i) stay member access on the local. M20e (a residual-less
        // contesting module — FCS backtracks to the type's member) defers.
        Project {
            files: vec![
                ("xslot_evict_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_evict_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color() =\n    static member Blue = 0\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 1,
        },
        Project {
            files: vec![
                ("xslot_evict_enum_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_evict_enum_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color =\n    | Blue = 0\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 1,
        },
        Project {
            files: vec![
                ("xslot_held_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_held_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_retake_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_retake_use",
                    "module Client\n\ntype Color() =\n    static member Blue = 0\n\nlet Color = {| Red = 3 |}\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_backtrack_lib", "module Color\nlet Blue = 99\n"),
                (
                    "xslot_backtrack_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color() =\n    static member Red = 7\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // (Probe M20f also pinned the PARAMETER variant — `let g (Color: {|
        // Red: int |}) = Color.Red` binds the param — but sema's capitalized
        // pattern-binder conservatism leaves such a head unbound and the
        // qualified-value branch then resolves the cross-file module: a
        // pre-existing hole of the pattern-binder family, distinct from this
        // gate, so it is not in this corpus. See the §5 note in
        // `docs/project-type-member-plan.md`.)
        Project {
            files: vec![
                ("xslot_local_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_local_use",
                    "module Client\n\ntype Color() =\n    static member Blue = 0\n\nlet f () =\n    let Color = {| Red = 3 |}\n    Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_late_type_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_late_type_use",
                    "module Client\n\nlet f () =\n    let Color = {| Red = 3 |}\n    Color.Red + 0\n\ntype Color() =\n    static member Blue = 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_open_evict_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_open_evict_ns",
                    "namespace LibNs\n\ntype Color() =\n    static member Blue = 0\n",
                ),
                (
                    "xslot_open_evict_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\nopen LibNs\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 1,
        },
        Project {
            files: vec![
                ("xslot_open_held_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_open_held_ns",
                    "namespace LibNs\n\ntype Color() =\n    static member Blue = 0\n",
                ),
                (
                    "xslot_open_held_use",
                    "module Client\n\nopen LibNs\n\nlet Color = {| Red = 3 |}\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // M20j: an AUGMENTATION after the value does not re-enter the type in
        // the slot — the value keeps member access (definition positions only).
        Project {
            files: vec![
                ("xslot_augment_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_augment_use",
                    "module Client\n\ntype Color() =\n    static member Blue = 0\n\nlet Color = {| Red = 3 |}\n\ntype Color with\n    static member Green = 1\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // M20k/M20l/M20o (codex round 1): a plain UNION / RECORD / INTERFACE
        // never enters the slot — the value keeps member access even with the
        // type later. M20m: a `[<Struct>]` union IS a struct type and evicts
        // in FCS, but the textual marker is spoofable by a custom `Struct`
        // attribute (codex round 7), so sema defers it. M20n: an abbreviation
        // chases its target (`= int` is a struct and evicts in FCS); sema
        // cannot decide statically, so it defers — the harness's allowed
        // bucket, with 0 cross-file agreements.
        Project {
            files: vec![
                ("xslot_union_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_union_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color =\n    | Blue\n    | Green\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_record_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_record_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color = { Blue: int }\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_struct_union_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_struct_union_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\n[<Struct>]\ntype Color =\n    | Blue\n    | Green\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_abbrev_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_abbrev_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color = int\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_interface_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_interface_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype Color =\n    interface\n        abstract Member: int\n    end\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // M20p/M20q (codex round 3): an OPENED value enters the slot at the
        // OPEN's position — an `open M` after the type re-takes the slot for
        // `M.Color` (member access on the opened value), one before the type
        // loses it (FCS binds the cross-file module; sema defers behind the
        // project-module open's opaque flag — the allowed bucket).
        Project {
            files: vec![
                ("xslot_openval_late_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_openval_late_use",
                    "module Client\n\nmodule M =\n    let Color = {| Red = 3 |}\n\ntype Color() =\n    static member Blue = 0\n\nopen M\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_openval_early_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_openval_early_use",
                    "module Client\n\nmodule M =\n    let Color = {| Red = 3 |}\n\nopen M\n\ntype Color() =\n    static member Blue = 0\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // M20r/M20s (codex round 4): a `type private Color` is not imported
        // by an `open` from outside its declaration group — the local value
        // keeps member access — while within its own container the private
        // type is fully visible and evicts normally.
        Project {
            files: vec![
                ("xslot_private_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_private_ns",
                    "namespace LibNs\n\ntype private Color() =\n    static member Blue = 0\n",
                ),
                (
                    "xslot_private_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\nopen LibNs\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_private_local_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_private_local_use",
                    "module Client\n\nlet Color = {| Red = 3 |}\n\ntype private Color() =\n    static member Blue = 0\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 1,
        },
        // M20t/M20u (codex round 5): an evicted head must not take the
        // TYPE-SIDE fallbacks — the evicting opened type is a nearer tycon
        // candidate FCS tries first. When it owns `Red`, FCS binds ITS
        // member (M20t); when it lacks it, FCS backtracks to the earlier
        // file's root case (M20u). Sema defers both (the allowed bucket) —
        // it must never record the root case where FCS binds the member.
        Project {
            files: vec![
                (
                    "xslot_typeside_own_lib",
                    "namespace global\n\ntype Color =\n    | Red\n    | Blue\n",
                ),
                (
                    "xslot_typeside_own_use",
                    "namespace LibNs\n\ntype Color() =\n    static member Red = 1\n\nnamespace global\n\nmodule Client =\n    let Color = {| Red = 3 |}\n    open LibNs\n    let user = Color.Red\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                (
                    "xslot_typeside_lack_lib",
                    "namespace global\n\ntype Color =\n    | Red\n    | Blue\n",
                ),
                (
                    "xslot_typeside_lack_use",
                    "namespace LibNs\n\ntype Color() =\n    static member Blue2 = 1\n\nnamespace global\n\nmodule Client =\n    let Color = {| Red = 3 |}\n    open LibNs\n    let user = Color.Red\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // M20v/M20w (codex round 8): a project MODULE open imports types into
        // the slot too. The opened `M.Color` class evicts the earlier value
        // (FCS binds the cross-file module's Red; sema defers behind the
        // module open's opaque flag — the allowed bucket); a union in the
        // opened module keeps the value (member access on the local).
        Project {
            files: vec![
                ("xslot_modopen_class_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_modopen_class_use",
                    "module Client\n\nmodule M =\n    type Color() =\n        static member Red = 1\n\nlet Color = {| Red = 3 |}\n\nopen M\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        Project {
            files: vec![
                ("xslot_modopen_union_lib", "module Color\nlet Red = 99\n"),
                (
                    "xslot_modopen_union_use",
                    "module Client\n\nmodule M =\n    type Color =\n        | Blue\n        | Green\n\nlet Color = {| Red = 3 |}\n\nopen M\n\nlet user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // M20x (codex round 9): ONE `open Lib` supplies both a root module's
        // value and a relative namespace's type — FCS breaks the tie by
        // reading priority (the type wins; `Color.Red` = N.Lib.Color.Red).
        // Sema defers the equal-position contest (the allowed bucket); the
        // head must never record the opened value.
        Project {
            files: vec![
                (
                    "xslot_opente_lib0",
                    "module Lib\nlet Color = {| Red = 3 |}\n",
                ),
                (
                    "xslot_opente_lib1",
                    "namespace N.Lib\n\ntype Color() =\n    static member Red = 1\n",
                ),
                (
                    "xslot_opente_use",
                    "namespace N\n\nmodule Client =\n    open Lib\n    let user = Color.Red + 0\n",
                ),
            ],
            expected_cross_file: 0,
        },
        // Cross-tier namespace-straddle **S1** (Stage 5): a namespace's own direct
        // `exception Clash`@file0 and a later `[<AutoOpen>]` submodule value
        // `A.Clash`@file1. `open Probe; Clash` binds the later file's submodule
        // value — the natural push order, now that the fold knows the submodule
        // genuinely folds at file1. FCS-pinned against `main`'s conservative defer.
        Project {
            files: vec![
                (
                    "straddle_s1_a",
                    "namespace Probe\n\nexception Clash of int\n",
                ),
                (
                    "straddle_s1_b",
                    "namespace Probe\n\n[<AutoOpen>]\nmodule A =\n    let Clash = 1\n",
                ),
                (
                    "straddle_s1_use",
                    "namespace Other\n\nmodule Client =\n    open Probe\n    let y = Clash\n",
                ),
            ],
            expected_cross_file: 1,
        },
        // A **plain** (`module A`, no attribute) augmentation adding `X`@file2 to
        // an auto-open `A`@file0 is NOT auto-opened, so `open N; X` binds the
        // namespace's own direct `exception X`@file1 — the plain fragment never
        // enters the fold (per-fragment gate).
        Project {
            files: vec![
                (
                    "straddle_plain_a",
                    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Dummy = 0\n",
                ),
                ("straddle_plain_b", "namespace N\n\nexception X of int\n"),
                (
                    "straddle_plain_c",
                    "namespace N\n\nmodule A =\n    let X = 2\n",
                ),
                (
                    "straddle_plain_use",
                    "namespace Z\n\nmodule O =\n    open N\n    let y = X\n",
                ),
            ],
            expected_cross_file: 1,
        },
        // The dual: a second **`[<AutoOpen>]`** `module A` fragment adding `X`@file1
        // folds at its own file, so `open N; X` binds `N.A.X`@file1 (no direct `X`).
        Project {
            files: vec![
                (
                    "straddle_autoopen_a",
                    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Dummy = 0\n",
                ),
                (
                    "straddle_autoopen_b",
                    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let X = 2\n",
                ),
                (
                    "straddle_autoopen_use",
                    "namespace Z\n\nmodule O =\n    open N\n    let y = X\n",
                ),
            ],
            expected_cross_file: 1,
        },
    ];

    for project in &corpus {
        assert_matches_fcs(project);
    }
}

/// One `open` may be BOTH a project namespace (through a prior open's shortening
/// prefix — tier 1) and a project module (through the enclosing namespace —
/// tier 2). The tier-1 namespace reading out-ranks the tier-2 module for
/// everything the open registers — shortening prefixes and auto-opens included —
/// so the chained `open Y` must read through `X.Sub` first: bare `marker` is
/// `X.Sub.Y.marker` (file 0), not `Demo.Sub.Y.marker` (file 1). FCS-pinned.
///
/// Regression (PR #667 review): the namespace readings' prefixes were pushed
/// when the reading group was built, before the project-opens loop pushed the
/// module interpretation, so the lower-tier module won latest-wins.
#[test]
fn chained_open_prefers_prior_open_namespace_over_enclosing_module() {
    assert_matches_fcs(&Project {
        files: vec![
            (
                "chain_ns_lib",
                "namespace X.Sub\n\nmodule Y =\n    let marker = 1\n",
            ),
            (
                "chain_mod_lib",
                "namespace Demo\n\nmodule Sub =\n    module Y =\n        let marker = 2\n",
            ),
            (
                "chain_use",
                "namespace Demo\n\nmodule M =\n    open X\n    open Sub\n    open Y\n    let z = marker\n",
            ),
        ],
        expected_cross_file: 1,
    });
}
