//! FCS-graded differential for **union-case names a project module declares
//! more than once**, and for type-qualified cases reached through an `open`.
//!
//! A module can declare one simple name several times: two unions sharing a
//! case name, a value beside a case, an exception beside a case. Which one a
//! reference binds depends on how it reaches the module, and FCS uses a
//! different order for each route (`NameResolution.fs`):
//!
//! - a module-qualified expression (`Lib.X`,
//!   `ResolveExprLongIdentInModuleOrNamespace`): values, then exceptions, then
//!   the case of the *first* type in the module declaring it
//!   (`TryFindTypeWithUnionCase`, `AllEntities` order);
//! - a module-qualified pattern (`ResolvePatternLongIdentInModuleOrNamespace`):
//!   the first type's case, then exceptions, active patterns and values;
//! - an `open` adds exceptions, then types with their cases, then values
//!   (`AddModuleOrNamespaceContentsToNameEnv`), and a later addition wins.
//!
//! Every cell is graded **certain-implies-exact** over every use FCS declares in
//! a project file: a committed resolution must name FCS's declaration, a decline
//! makes no claim. The pins say, per cell, what FCS binds at the uses the cell
//! is about (so a backwards fixture fails here rather than passing the
//! differential by agreement), and which of them we must commit to, so the
//! grid cannot pass by declining everything.

use crate::common::{
    ensure_case_pattern_autoopen_fixture_built, fsharp_core_bytes, fsharp_core_env,
    invoke_fcs_dump_project, invoke_fcs_dump_project_with_refs, parse_fcs_uses_project,
    temp_fs_file, temp_fs_tree,
};
use crate::resolve_signatures::project;
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::TextRange;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project, resolve_project_files};

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

/// The byte range of the `nth` (zero-based) occurrence of `needle` in `hay`.
fn nth_range(hay: &str, needle: &str, nth: usize) -> (usize, usize) {
    let mut from = 0;
    for _ in 0..nth {
        let i = hay[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("occurrence of {needle:?} in {hay:?}"))
            + from;
        from = i + needle.len();
    }
    let i = hay[from..]
        .find(needle)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} in {hay:?}"))
        + from;
    (i, i + needle.len())
}

/// A text position in one of a cell's files: the `nth` occurrence of `needle`
/// in file `file`.
#[derive(Clone, Copy, Debug)]
struct At {
    file: usize,
    needle: &'static str,
    nth: usize,
}

const fn at(file: usize, needle: &'static str, nth: usize) -> At {
    At { file, needle, nth }
}

/// What a pinned use must do.
#[derive(Clone, Copy, Debug)]
enum Want {
    /// FCS binds the declaration at `decl`. With `must_commit`, we must
    /// resolve the use (to that declaration — the differential checks which);
    /// without, a decline is acceptable.
    Decl { decl: At, must_commit: bool },
    /// FCS does not bind the use cleanly (the file errors); we must not
    /// commit any target there.
    NoCommit,
}

struct Pin {
    usage: At,
    want: Want,
}

const fn commits(usage: At, decl: At) -> Pin {
    Pin {
        usage,
        want: Want::Decl {
            decl,
            must_commit: true,
        },
    }
}

const fn binds(usage: At, decl: At) -> Pin {
    Pin {
        usage,
        want: Want::Decl {
            decl,
            must_commit: false,
        },
    }
}

const fn no_commit(usage: At) -> Pin {
    Pin {
        usage,
        want: Want::NoCommit,
    }
}

struct Cell<'a> {
    name: &'static str,
    files: &'a [&'static str],
    /// Whether FCS is expected to report errors (the cell is then graded only
    /// through its pins, which must all be [`Want::NoCommit`] in erroring files).
    errors_expected: bool,
    pins: &'a [Pin],
}

/// Our resolution at `range` in file `i`, as `(declaring file, def range)` when
/// we commit to a project declaration; `None` when we decline.
fn our_target(
    proj: &borzoi_sema::ResolvedProject,
    i: usize,
    range: TextRange,
    cell: &str,
) -> Option<(usize, TextRange)> {
    let rf = proj.file(i);
    match rf.resolution_at(range)? {
        Resolution::Deferred(_) => None,
        Resolution::Unresolved => {
            panic!("{cell}: Unresolved at {range:?} in file {i}, where FCS resolved")
        }
        res @ (Resolution::Item(_) | Resolution::EarlierFileType { .. }) => {
            let (idx, def) = proj.item_def(res).expect("item def for Item");
            Some((idx, def.range))
        }
        res @ Resolution::Local(_) => Some((i, rf.resolved_def(res).expect("local def").range)),
        other => panic!("{cell}: no assembly in this grid, yet {other:?} at {range:?}"),
    }
}

fn check(cell: &Cell<'_>) {
    let names: Vec<String> = (0..cell.files.len())
        .map(|i| format!("File{i}.fs"))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    check_with(cell, &names, &[], fsharp_core_env());
}

/// [`check`], with each file under the given name (a headerless file's
/// implicit module is its name), FCS referencing `refs` and our side
/// resolving over `env`, which should hold the same assemblies.
fn check_with(cell: &Cell<'_>, names: &[&str], refs: &[&std::path::Path], env: &AssemblyEnv) {
    assert_eq!(
        names.len(),
        cell.files.len(),
        "{}: one name per file",
        cell.name
    );
    let rows: Vec<(&str, &str)> = names
        .iter()
        .copied()
        .zip(cell.files.iter().copied())
        .collect();
    let (root, written) = temp_fs_tree(&format!("dupcase_{}", cell.name), &rows);
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, refs);
    let fcs_files = parse_fcs_uses_project(&json, &written);
    let proj = resolve_project_files(&project(&rows), env);
    let _ = std::fs::remove_dir_all(&root);

    let file_index = |path: &std::path::Path| {
        written
            .iter()
            .position(|(p, _)| p.file_name() == path.file_name())
            .expect("a project file")
    };
    let fcs_of = |i: usize| {
        fcs_files
            .iter()
            .find(|f| f.path.file_name() == written[i].0.file_name())
            .unwrap_or_else(|| panic!("{}: FCS reported nothing for file {i}", cell.name))
    };

    let any_errors = (0..written.len()).any(|i| !fcs_of(i).errors.is_empty());
    assert_eq!(
        any_errors,
        cell.errors_expected,
        "{}: FCS errors: {:?}",
        cell.name,
        (0..written.len())
            .map(|i| &fcs_of(i).errors)
            .collect::<Vec<_>>()
    );

    // Certain-implies-exact over every use FCS declares in a project file, in
    // every file that checked cleanly.
    for (i, _) in written.iter().enumerate() {
        let fu = fcs_of(i);
        if !fu.errors.is_empty() {
            continue;
        }
        for u in &fu.uses {
            if u.start == u.end {
                continue;
            }
            let Some(decl) = &u.decl else { continue };
            let range = span(u.start, u.end);
            if let Some((idx, def)) = our_target(&proj, i, range, cell.name) {
                assert_eq!(
                    (idx, def),
                    (file_index(&decl.file), span(decl.start, decl.end)),
                    "{}: use {:?} at {range:?} in file {i}: ours (file, range) vs FCS's",
                    cell.name,
                    u.name,
                );
            }
        }
    }

    for pin in cell.pins {
        let (s, e) = nth_range(cell.files[pin.usage.file], pin.usage.needle, pin.usage.nth);
        let range = span(s, e);
        let ours = our_target(&proj, pin.usage.file, range, cell.name);
        match pin.want {
            Want::Decl { decl, must_commit } => {
                let fu = fcs_of(pin.usage.file);
                assert!(
                    fu.errors.is_empty(),
                    "{}: pin {:?} sits in a file FCS rejects",
                    cell.name,
                    pin.usage
                );
                let u = fu
                    .uses
                    .iter()
                    .find(|u| u.start == s && u.end == e && u.decl.is_some())
                    .unwrap_or_else(|| {
                        panic!(
                            "{}: FCS reports no project use at {:?}; uses: {:?}",
                            cell.name,
                            pin.usage,
                            fu.uses
                                .iter()
                                .map(|u| (&u.name, u.start, u.end))
                                .collect::<Vec<_>>()
                        )
                    });
                let fd = u.decl.as_ref().expect("filtered on decl");
                let (ds, de) = nth_range(cell.files[decl.file], decl.needle, decl.nth);
                assert_eq!(
                    (file_index(&fd.file), fd.start, fd.end),
                    (decl.file, ds, de),
                    "{}: FCS binds {:?} somewhere other than the pinned {:?}",
                    cell.name,
                    pin.usage,
                    decl
                );
                if must_commit {
                    assert_eq!(
                        ours,
                        Some((decl.file, span(ds, de))),
                        "{}: we must resolve {:?} to {:?}",
                        cell.name,
                        pin.usage,
                        decl
                    );
                }
            }
            Want::NoCommit => assert_eq!(
                ours, None,
                "{}: {:?} must not commit where FCS does not bind cleanly",
                cell.name, pin.usage
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Module-qualified names
// ---------------------------------------------------------------------------

const TWO_UNIONS: &str = "module Lib\ntype U1 = X | Y\ntype U2 = X | Z\n";
const VALUE_THEN_UNION: &str = "module Lib\nlet X = 5\ntype UV = X | W\n";
const UNION_THEN_VALUE: &str = "module Lib\ntype UV = X | W\nlet X = 5\n";

#[test]
fn qualified_case_binds_the_first_union_declaring_it() {
    check(&Cell {
        name: "qualified_two_unions",
        files: &[
            TWO_UNIONS,
            "module Use\nlet v = Lib.X\nlet w = Lib.Z\nlet f (u: Lib.U1) = match u with Lib.X -> 1 | _ -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "Lib.X", 0), at(0, "X", 0)),
            commits(at(1, "Lib.Z", 0), at(0, "Z", 0)),
            binds(at(1, "Lib.X", 1), at(0, "X", 0)),
        ],
    });
}

#[test]
fn qualified_expression_prefers_a_value_declared_before_the_case() {
    check(&Cell {
        name: "qualified_value_then_union",
        files: &[
            VALUE_THEN_UNION,
            "module Use\nlet v = Lib.X + 1\nlet f (u: Lib.UV) = match u with Lib.X -> 1 | Lib.W -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "Lib.X", 0), at(0, "X", 0)),
            binds(at(1, "Lib.X", 1), at(0, "X", 1)),
            binds(at(1, "Lib.W", 0), at(0, "W", 0)),
        ],
    });
}

#[test]
fn qualified_expression_prefers_a_value_declared_after_the_case() {
    check(&Cell {
        name: "qualified_union_then_value",
        files: &[
            UNION_THEN_VALUE,
            "module Use\nlet v = Lib.X + 1\nlet f (u: Lib.UV) = match u with Lib.X -> 1 | Lib.W -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "Lib.X", 0), at(0, "X", 1)),
            binds(at(1, "Lib.X", 1), at(0, "X", 0)),
            binds(at(1, "Lib.W", 0), at(0, "W", 0)),
        ],
    });
}

#[test]
fn qualified_case_in_a_same_file_nested_module() {
    check(&Cell {
        name: "nested_two_unions",
        files: &[
            "module Use\nmodule Inner =\n    type U1 = X | Y\n    type U2 = X | Z\nlet v = Inner.X\nlet w = Inner.Z\nlet f (u: Inner.U1) = match u with Inner.X -> 1 | _ -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(0, "Inner.X", 0), at(0, "X", 0)),
            commits(at(0, "Inner.Z", 0), at(0, "Z", 0)),
            binds(at(0, "Inner.X", 1), at(0, "X", 0)),
        ],
    });
}

#[test]
fn qualified_value_beside_a_case_in_a_same_file_nested_module() {
    check(&Cell {
        name: "nested_value_then_union",
        files: &[
            "module Use\nmodule Inner =\n    let X = 5\n    type UV = X | W\nlet v = Inner.X + 1\nlet f (u: Inner.UV) = match u with Inner.X -> 1 | Inner.W -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(0, "Inner.X", 0), at(0, "X", 0)),
            binds(at(0, "Inner.X", 1), at(0, "X", 1)),
            binds(at(0, "Inner.W", 0), at(0, "W", 0)),
        ],
    });
}

#[test]
fn qualified_case_across_namespace_fragments() {
    check(&Cell {
        name: "namespace_fragments",
        files: &[
            "namespace N\ntype U1 = X | Y\n",
            "namespace N\ntype U2 = X | Z\n",
            "module Use\nlet v = N.X\nlet w = N.Z\nlet f (u: N.U2) = match u with N.X -> 1 | _ -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(2, "N.X", 0), at(1, "X", 0)),
            commits(at(2, "N.Z", 0), at(1, "Z", 0)),
            binds(at(2, "N.X", 1), at(1, "X", 0)),
        ],
    });
}

#[test]
fn qualified_case_whose_first_union_requires_qualified_access() {
    // `TryFindTypeWithUnionCase` stops at the first type declaring the case,
    // RQA or not, so the later non-RQA `U2.X` is never the answer.
    check(&Cell {
        name: "qualified_rqa_first",
        files: &[
            "module Lib\n[<RequireQualifiedAccess>]\ntype U1 = X | Y\ntype U2 = X | Z\n",
            "module Use\nlet v = Lib.X\n",
        ],
        errors_expected: true,
        pins: &[no_commit(at(1, "Lib.X", 0))],
    });
}

#[test]
fn qualified_case_whose_first_union_has_a_private_representation() {
    check(&Cell {
        name: "qualified_private_repr_first",
        files: &[
            "module Lib\ntype U1 = private X | Y\ntype U2 = X | Z\n",
            "module Use\nlet v = Lib.X\n",
        ],
        errors_expected: true,
        pins: &[no_commit(at(1, "Lib.X", 0))],
    });
}

#[test]
fn qualified_case_beside_an_enum_field_of_its_name() {
    // An enum field is reached only as `Lib.E.A`, so it is no second
    // declaration of `Lib.A`: the union's case binds.
    check(&Cell {
        name: "qualified_case_beside_enum_field",
        files: &[
            "module Lib\ntype U = A | B\ntype E = A = 0 | C = 1\n",
            "module Use\nlet v = Lib.A\n",
        ],
        errors_expected: false,
        pins: &[commits(at(1, "Lib.A", 0), at(0, "A", 0))],
    });
}

// ---------------------------------------------------------------------------
// `open`
// ---------------------------------------------------------------------------

#[test]
fn open_prefers_a_value_declared_before_the_case() {
    check(&Cell {
        name: "open_value_then_union",
        files: &[
            VALUE_THEN_UNION,
            "module Use\nopen Lib\nlet v = X + 1\nlet f (u: UV) = match u with X -> 1 | W -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "X", 0), at(0, "X", 0)),
            binds(at(1, "X", 1), at(0, "X", 1)),
            commits(at(1, "W", 0), at(0, "W", 0)),
        ],
    });
}

#[test]
fn open_prefers_a_value_declared_after_the_case() {
    check(&Cell {
        name: "open_union_then_value",
        files: &[
            UNION_THEN_VALUE,
            "module Use\nopen Lib\nlet v = X + 1\nlet f (u: UV) = match u with X -> 1 | W -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "X", 0), at(0, "X", 1)),
            binds(at(1, "X", 1), at(0, "X", 0)),
            commits(at(1, "W", 0), at(0, "W", 0)),
        ],
    });
}

#[test]
fn open_binds_the_last_union_declaring_a_case() {
    check(&Cell {
        name: "open_two_unions",
        files: &[
            TWO_UNIONS,
            "module Use\nopen Lib\nlet v = X\nlet f (u: U2) = match u with X -> 1 | _ -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "X", 0), at(0, "X", 1)),
            binds(at(1, "X", 1), at(0, "X", 1)),
        ],
    });
}

// ---------------------------------------------------------------------------
// Type-qualified cases reached through an `open`
// ---------------------------------------------------------------------------

#[test]
fn type_qualified_case_named_like_an_implicitly_opened_module_member() {
    // FSharp.Core's `Option` module, opened by no source `open`, is searched
    // before the opened project type: `Option.map` is `Option.map`.
    check(&Cell {
        name: "implicit_module_namesake",
        files: &[
            "module Lib\n[<RequireQualifiedAccess>]\ntype Option = map | Other\n",
            "module Use\nopen Lib\nlet x = Option.map\n",
        ],
        errors_expected: false,
        pins: &[no_commit(at(1, "Option.map", 0))],
    });
}

#[test]
fn type_qualified_case_named_like_an_implicit_filename_module() {
    // A headerless earlier file is the implicit module `U`, searched before
    // the opened project type: `U.A` is that file's value.
    let cell = Cell {
        name: "implicit_filename_module_namesake",
        files: &[
            "module Lib\ntype U = A | B\n",
            "let A = 42\n",
            "module Use\nopen Lib\nlet x = U.A\n",
        ],
        errors_expected: false,
        pins: &[no_commit(at(2, "U.A", 0))],
    };
    check_with(
        &cell,
        &["Lib.fs", "U.fsx", "Use.fs"],
        &[],
        fsharp_core_env(),
    );
}

#[test]
fn type_qualified_case_named_like_a_retained_auto_open_module() {
    // A referenced assembly's manifest `AutoOpen` brings `module Shade` into
    // bare scope, searched before the opened project type.
    let fixture = ensure_case_pattern_autoopen_fixture_built();
    let view = Ecma335Assembly::parse(&std::fs::read(fixture).expect("read fixture"))
        .expect("parse fixture");
    let core = Ecma335Assembly::parse(fsharp_core_bytes()).expect("parse FSharp.Core");
    let env = AssemblyEnv::from_views(&[core, view]).expect("build AssemblyEnv");
    let cell = Cell {
        name: "retained_auto_open_namesake",
        files: &[
            "module Lib\ntype Shade = A | B\n",
            "module Use\nopen Lib\nlet x = Shade.A\n",
        ],
        errors_expected: false,
        pins: &[no_commit(at(1, "Shade.A", 0))],
    };
    check_with(&cell, &["Lib.fs", "Use.fs"], &[fixture], &env);
}

#[test]
fn type_qualified_case_named_like_a_same_file_namespace() {
    // An earlier namespace block of this file named like the type takes
    // the head: `U.A` is the namespace-qualified case `U.V.A`.
    check(&Cell {
        name: "same_file_namespace_namesake",
        files: &[
            "module Lib\ntype U = A | B\n",
            "namespace U\ntype V = A | C\n\nnamespace Client\nmodule M =\n    open Lib\n    let x = U.A\n",
        ],
        errors_expected: false,
        pins: &[binds(at(1, "U.A", 0), at(1, "A", 0))],
    });
}

#[test]
fn type_qualified_case_beside_a_generic_type_of_the_same_name() {
    // `U` and `U<'T>` share a path and a case name, and FCS splits them by
    // position: the expression reads the non-generic type's `A`, the
    // pattern the generic type's (`U.A` with no argument there is FCS's
    // "applied to 0 argument(s) but expects 1").
    check(&Cell {
        name: "generic_namesake",
        files: &[
            "module Lib\ntype U = A | B\ntype U<'T> = A of 'T | C\n",
            "module Use\nopen Lib\nlet a = U.A\nlet f x = match x with U.A _ -> 1 | _ -> 2\n",
        ],
        errors_expected: false,
        pins: &[
            binds(at(1, "U.A", 0), at(0, "A", 0)),
            binds(at(1, "U.A", 1), at(0, "A", 1)),
        ],
    });
}

/// The using file for the type-qualified cells.
const TYPE_QUALIFIED_USE: &str = "module Use\nopen Lib\n\
    let a = U.A\n\
    let b = U.B 1\n\
    let q = Q.QA\n\
    let r = Q.QB 2\n\
    let f u = match u with U.A -> 1 | U.B n -> n\n\
    let g q = match q with Q.QA -> 1 | Q.QB n -> n\n";

const TYPE_QUALIFIED_PINS: &[Pin] = &[
    commits(at(1, "U.A", 0), at(0, "A", 0)),
    commits(at(1, "U.B", 0), at(0, "B", 0)),
    commits(at(1, "Q.QA", 0), at(0, "QA", 0)),
    commits(at(1, "Q.QB", 0), at(0, "QB", 0)),
    commits(at(1, "U.A", 1), at(0, "A", 0)),
    commits(at(1, "U.B", 1), at(0, "B", 0)),
    commits(at(1, "Q.QA", 1), at(0, "QA", 0)),
    commits(at(1, "Q.QB", 1), at(0, "QB", 0)),
];

#[test]
fn type_qualified_case_through_an_opened_module() {
    check(&Cell {
        name: "open_module_type_qualified",
        files: &[
            "module Lib\ntype U = A | B of int\n[<RequireQualifiedAccess>]\ntype Q = QA | QB of int\n",
            TYPE_QUALIFIED_USE,
        ],
        errors_expected: false,
        pins: TYPE_QUALIFIED_PINS,
    });
}

#[test]
fn type_qualified_case_through_an_opened_namespace() {
    check(&Cell {
        name: "open_namespace_type_qualified",
        files: &[
            "namespace Lib\ntype U = A | B of int\n[<RequireQualifiedAccess>]\ntype Q = QA | QB of int\n",
            TYPE_QUALIFIED_USE,
        ],
        errors_expected: false,
        pins: TYPE_QUALIFIED_PINS,
    });
}

#[test]
fn type_qualified_case_through_an_opened_nested_module() {
    check(&Cell {
        name: "open_nested_module_type_qualified",
        files: &[
            "namespace N\nmodule Lib =\n    type U = A | B of int\n    [<RequireQualifiedAccess>]\n    type Q = QA | QB of int\n",
            "module Use\nopen N\nopen Lib\n\
             let a = U.A\n\
             let b = U.B 1\n\
             let q = Q.QA\n\
             let r = Q.QB 2\n\
             let f u = match u with U.A -> 1 | U.B n -> n\n\
             let g q = match q with Q.QA -> 1 | Q.QB n -> n\n",
        ],
        errors_expected: false,
        pins: TYPE_QUALIFIED_PINS,
    });
}

#[test]
fn type_qualified_case_through_the_enclosing_namespace() {
    // WoofWare.PawPrint's shape: a union in one file of a namespace, used
    // type-qualified from a module of the same namespace in a later file.
    check(&Cell {
        name: "enclosing_namespace_type_qualified",
        files: &[
            "namespace Lib\ntype U = A | B of int\n[<RequireQualifiedAccess>]\ntype Q = QA | QB of int\n",
            "namespace Lib\nmodule Use =\n\
             \x20   let a = U.A\n\
             \x20   let b = U.B 1\n\
             \x20   let q = Q.QA\n\
             \x20   let r = Q.QB 2\n\
             \x20   let f u = match u with U.A -> 1 | U.B n -> n\n\
             \x20   let g q = match q with Q.QA -> 1 | Q.QB n -> n\n",
        ],
        errors_expected: false,
        pins: TYPE_QUALIFIED_PINS,
    });
}

// ---------------------------------------------------------------------------
// Generated: what else an opened project module may put under a head
// ---------------------------------------------------------------------------

/// Resolve `files` with both sides and grade every use certain-implies-exact
/// in each file FCS checks cleanly. Returns whether every file was clean and
/// how many uses spelled `head_text` (by range) we committed in the last file.
fn grade_project(label: &str, files: &[String], head_text: &str) -> (bool, usize) {
    let written: Vec<(std::path::PathBuf, String)> = files
        .iter()
        .enumerate()
        .map(|(i, src)| {
            (
                temp_fs_file(&format!("dupgrid_{label}_{i}"), src),
                src.clone(),
            )
        })
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs_files = parse_fcs_uses_project(&json, &written);
    let asts: Vec<ImplFile> = written.iter().map(|(_, src)| impl_file(src)).collect();
    let proj = resolve_project(&asts, fsharp_core_env());
    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }
    let file_index = |path: &std::path::Path| {
        written
            .iter()
            .position(|(p, _)| p.file_name() == path.file_name())
            .expect("a project file")
    };
    let mut clean = true;
    for (i, (path, _)) in written.iter().enumerate() {
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("{label}: FCS reported nothing for file {i}"));
        if !fu.errors.is_empty() {
            clean = false;
            continue;
        }
        for u in &fu.uses {
            let Some(decl) = &u.decl else { continue };
            if u.start == u.end {
                continue;
            }
            let range = span(u.start, u.end);
            if let Some(ours) = our_target(&proj, i, range, label) {
                assert_eq!(
                    ours,
                    (file_index(&decl.file), span(decl.start, decl.end)),
                    "{label}: use {:?} at {range:?} in file {i}: ours vs FCS's\n{files:#?}",
                    u.name,
                );
            }
        }
    }
    let last = files.len() - 1;
    let src = &files[last];
    let commits = src
        .match_indices(head_text)
        .filter(|(at, _)| our_target(&proj, last, span(*at, at + head_text.len()), label).is_some())
        .count();
    (clean, commits)
}

#[test]
fn type_qualified_case_past_an_opened_module_never_mis_binds() {
    // The opened module `Lib` always declares `type U = A | B`; around it, the
    // ways something else can take the head `U` or the path `U.A`.
    const LIB_EXTRAS: &[(&str, &str)] = &[
        ("none", ""),
        ("companion", "module U =\n    let A = 5\n"),
        ("value", "let U = 3\n"),
        ("exception", "exception U of int\n"),
        (
            "auto_open",
            "[<AutoOpen>]\nmodule Inner =\n    type U = A | C\n",
        ),
        ("dup_case", "type U2 = A | D\n"),
        ("generic", "type U<'T> = A | G of 'T\n"),
    ];
    // A second earlier file, what the consumer opens of it, and the
    // consumer's header: the contenders an opened project module, an opened
    // namespace, the enclosing namespace or the root bring.
    struct Second {
        name: &'static str,
        file: &'static str,
        open: Option<&'static str>,
        header: &'static str,
    }
    const fn second(
        name: &'static str,
        file: &'static str,
        open: Option<&'static str>,
        header: &'static str,
    ) -> Second {
        Second {
            name,
            file,
            open,
            header,
        }
    }
    const SECOND: &[Second] = &[
        second("none", "", None, "module Use"),
        second(
            "type",
            "module L2\ntype U = A | E\n",
            Some("L2"),
            "module Use",
        ),
        second(
            "module",
            "module L2\nmodule U =\n    let A = 6\n",
            Some("L2"),
            "module Use",
        ),
        second(
            "unrelated",
            "module L2\nlet x = 1\n",
            Some("L2"),
            "module Use",
        ),
        second("root_module", "module U\nlet A = 99\n", None, "module Use"),
        second(
            "ns_module",
            "namespace NS\nmodule U =\n    let A = 8\n",
            Some("NS"),
            "module Use",
        ),
        second(
            "ns_type",
            "namespace NS\ntype U = A | H\n",
            Some("NS"),
            "module Use",
        ),
        second(
            "ns_exception",
            "namespace NS\nexception U of int\n",
            Some("NS"),
            "module Use",
        ),
        second(
            "enclosing_module",
            "namespace N\nmodule U =\n    let A = 9\n",
            None,
            "module N.Use",
        ),
    ];
    const SAME_FILE: &[(&str, &str)] = &[
        ("none", ""),
        ("type", "type U = A | F\n"),
        ("module", "module U =\n    let A = 7\n"),
    ];
    let mut cells = 0;
    let mut clean_cells = 0;
    let mut commits = 0;
    for (lib_name, lib_extra) in LIB_EXTRAS {
        for second in SECOND {
            // Both orders of the two `open`s, or of the two files when the
            // second is not opened.
            let orders: &[bool] = if second.file.is_empty() {
                &[false]
            } else {
                &[false, true]
            };
            for &second_first in orders {
                for (same_name, same) in SAME_FILE {
                    let label = format!("{lib_name}_{}_{second_first}_{same_name}", second.name);
                    let lib = format!("module Lib\ntype U = A | B\n{lib_extra}");
                    let opens = match (second.open, second_first) {
                        (None, _) => "open Lib\n".to_string(),
                        (Some(o), false) => format!("open Lib\nopen {o}\n"),
                        (Some(o), true) => format!("open {o}\nopen Lib\n"),
                    };
                    let user = format!(
                        "{}\n{opens}{same}let a = U.A\nlet f x = match x with U.A -> 1 | _ -> 2\n",
                        second.header
                    );
                    let mut files = vec![lib];
                    if !second.file.is_empty() {
                        let at = if second.open.is_none() && second_first {
                            0
                        } else {
                            1
                        };
                        files.insert(at, second.file.to_string());
                    }
                    files.push(user);
                    let (clean, n) = grade_project(&label, &files, "U.A");
                    cells += 1;
                    clean_cells += usize::from(clean);
                    commits += n;
                    if label == "none_none_false_none" {
                        assert!(clean, "the baseline cell checks cleanly");
                        assert_eq!(n, 2, "the baseline commits both uses");
                    }
                }
            }
        }
    }
    eprintln!("opened-module head grid: {cells} cells, {clean_cells} clean, {commits} commits");
    assert!(clean_cells * 2 > cells, "most cells must grade something");
}
