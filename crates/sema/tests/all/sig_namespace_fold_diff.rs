//! The **project namespace half** of a fold, when an earlier signatured pair
//! declares types directly in that namespace — against FCS, over the real
//! FSharp.Core.
//!
//! A file in `namespace N` implicitly opens `N`, and an `open N` elsewhere
//! does the same; the fold brings in every value-space name the project's
//! earlier `N` fragments contribute — for a namespace, its union cases and
//! exception constructors. A signatured pair restricts that contribution to
//! what the `.fsi` exposes. The fold may only stay precise (no generation
//! barrier, no dotted-head blanket) when it can name exactly which bare names
//! the signature exposes there; whatever it cannot model must still shadow.
//!
//! So each row is one *signature shape* in `namespace N` — exposed, `[<
//! RequireQualifiedAccess>]`, `internal`, a private representation, opaque,
//! an exception, a nullary case spelled as an abbreviation, an enum, a record
//! — and each declares (or hides) a case spelled like an FSharp.Core name
//! (`Ok`, `Some`), so a fold that drops a shadow commits FSharp.Core where FCS
//! binds the project, and a fold that invents one commits the project where
//! FCS binds FSharp.Core. Every row is read from a later file in the same
//! namespace, from a file that writes `open N`, and from a file between the
//! signature and its implementation.
//!
//! Graded certain-implies-exact both ways: every use FCS resolves is either
//! uncommitted on our side or committed to FCS's own target (project decl
//! site, or assembly and leaf name), and every project commit we make has an
//! FCS use at the same range. Rows also carry **must-commit** obligations,
//! so the table cannot pass by deferring everything.

use std::path::{Path, PathBuf};

use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, ResolvedProject, SourceFile, SyntaxRecovery,
    qualified_names, resolve_project_files,
};
use rowan::TextRange;

use crate::common::{
    FileUses, ensure_fsharp_core_dll, ensure_system_runtime_dll, invoke_fcs_dump_project_with_refs,
    parse_fcs_uses_project, temp_fs_tree,
};
use crate::resolve_signatures::source_file;

/// FSharp.Core plus the ref-pack `System.Runtime`, built once per binary.
fn env() -> &'static AssemblyEnv {
    static ENV: std::sync::OnceLock<AssemblyEnv> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let dlls = [ensure_fsharp_core_dll(), ensure_system_runtime_dll()];
        let bytes: Vec<Vec<u8>> = dlls
            .iter()
            .map(|d| std::fs::read(d).unwrap_or_else(|e| panic!("read {d:?}: {e}")))
            .collect();
        let views: Vec<Ecma335Assembly> = bytes
            .iter()
            .map(|b| Ecma335Assembly::parse(b).expect("parse dll"))
            .collect();
        AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
    })
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// What the fold is expected to model for a row — the must-commit
/// obligations. Every row is graded certain-implies-exact regardless.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Model {
    /// The signature's bare names are exactly exported: the FSharp.Core
    /// names stay committed, and `Ok`/`Some`/`Case1` commit to the `.fsi`.
    Exposed,
    /// The signature exposes no bare value name (or none that is
    /// unmodelled): the FSharp.Core names stay committed.
    Precise,
    /// The signature exposes a bare name the fold does not model, so the
    /// fold must shadow everything before it: no obligation.
    Unmodelled,
}

/// One signature shape: the `.fsi` and `.fs` bodies (after `namespace N`).
/// An empty `sig` is the unsigned control.
struct Shape {
    label: &'static str,
    sig: &'static str,
    imp: &'static str,
    model: Model,
}

const U: &str = "type U =\n    | Case1 of int\n    | Ok\n    | Some of string\n";

const SHAPES: &[Shape] = &[
    // The control: no signature at all, so the namespace half is fully
    // enumerable.
    Shape {
        label: "unsigned",
        sig: "",
        imp: U,
        model: Model::Exposed,
    },
    Shape {
        label: "exposed",
        sig: U,
        imp: U,
        model: Model::Exposed,
    },
    Shape {
        label: "generic_exposed",
        sig: "type U<'a> =\n    | Case1 of 'a\n    | Ok\n    | Some of 'a * 'a\n",
        imp: "type U<'a> =\n    | Case1 of 'a\n    | Ok\n    | Some of 'a * 'a\n",
        model: Model::Exposed,
    },
    // `internal` restricts nothing within one project.
    Shape {
        label: "internal_type",
        sig: "type internal U =\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        imp: "type internal U =\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        model: Model::Exposed,
    },
    Shape {
        label: "internal_repr",
        sig: "type U =\n    internal\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        imp: "type U =\n    internal\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        model: Model::Exposed,
    },
    Shape {
        label: "rqa",
        sig: "[<RequireQualifiedAccess>]\ntype U =\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        imp: "[<RequireQualifiedAccess>]\ntype U =\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        model: Model::Precise,
    },
    Shape {
        label: "rqa_internal",
        sig: "[<RequireQualifiedAccess>]\ntype internal U =\n    | Case1 of int\n    | Ok\n",
        imp: "[<RequireQualifiedAccess>]\ntype internal U =\n    | Case1 of int\n    | Ok\n",
        model: Model::Precise,
    },
    // The signature hides the representation: the cases are not visible.
    Shape {
        label: "opaque",
        sig: "type U\n",
        imp: U,
        model: Model::Precise,
    },
    // The implementation's extra type is hidden outright.
    Shape {
        label: "hidden_extra_type",
        sig: "type V =\n    | Vcase\n",
        imp: "type V =\n    | Vcase\n\ntype U =\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        model: Model::Precise,
    },
    Shape {
        label: "record",
        sig: "type U = { Ok : int; Case1 : string }\n",
        imp: "type U = { Ok : int; Case1 : string }\n",
        model: Model::Precise,
    },
    Shape {
        label: "module_only",
        sig: "module M =\n    val x : int\n",
        imp: "module M =\n    let x = 1\n",
        model: Model::Precise,
    },
    Shape {
        label: "private_repr",
        sig: "type U =\n    private\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        imp: "type U =\n    private\n    | Case1 of int\n    | Ok\n    | Some of string\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "two_types_one_private",
        sig: "type V =\n    | Vcase\n\ntype private U =\n    | Case1 of int\n    | Ok\n",
        imp: "type V =\n    | Vcase\n\ntype private U =\n    | Case1 of int\n    | Ok\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "exception",
        sig: "exception Ok of string\n",
        imp: "exception Ok of string\n",
        model: Model::Unmodelled,
    },
    // A signatured exception spelled like an FSharp.Core function: FCS binds
    // the exception, so the FSharp.Core function must not stay committed.
    Shape {
        label: "exception_failure",
        sig: "exception Failure of string\n",
        imp: "exception Failure of string\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "unsigned_exception_failure",
        sig: "",
        imp: "exception Failure of string\n",
        model: Model::Unmodelled,
    },
    // FCS reads a single-ident abbreviation target that names no type as a
    // nullary union case.
    Shape {
        label: "abbrev_case",
        sig: "type U = Ok\n",
        imp: "type U = Ok\n",
        model: Model::Unmodelled,
    },
    // Two declarations put `Ok` in bare scope, one of them unmodelled (FCS
    // reads `type B = Ok` as a nullary case): neither may be the export.
    Shape {
        label: "duplicate_case_abbrev",
        sig: "type A =\n    | Ok of int\n\ntype B = Ok\n",
        imp: "type A =\n    | Ok of int\n\ntype B = Ok\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "duplicate_case_across_type_decls",
        sig: "type A =\n    | Ok\n    | X\n\ntype C =\n    | Ok\n    | Y\n",
        imp: "type A =\n    | Ok\n    | X\n\ntype C =\n    | Ok\n    | Y\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "duplicate_case_in_one_group",
        sig: "type A =\n    | Ok\n    | X\n\nand C =\n    | Ok\n    | Y\n",
        imp: "type A =\n    | Ok\n    | X\n\nand C =\n    | Ok\n    | Y\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "duplicate_case_and_exception",
        sig: "type A =\n    | Ok of int\n\nexception Ok of string\n",
        imp: "type A =\n    | Ok of int\n\nexception Ok of string\n",
        model: Model::Unmodelled,
    },
    // A class named like the case takes the constructor slot (Codex review).
    Shape {
        label: "case_then_class",
        sig: "type U =\n    | Ok of int\n\ntype Ok =\n    new : unit -> Ok\n",
        imp: "type U =\n    | Ok of int\n\ntype Ok() = class end\n",
        model: Model::Unmodelled,
    },
    // `[<RequireQualifiedAccess>]` under another name (Codex review): the
    // cases are not in bare scope, which the written name cannot say.
    Shape {
        label: "aliased_rqa",
        sig: "type Qual = Microsoft.FSharp.Core.RequireQualifiedAccessAttribute\n\n[<Qual>]\ntype U =\n    | Case1 of int\n    | Ok\n",
        imp: "type Qual = Microsoft.FSharp.Core.RequireQualifiedAccessAttribute\n\n[<Qual>]\ntype U =\n    | Case1 of int\n    | Ok\n",
        model: Model::Unmodelled,
    },
    // WoofWare.Incremental's `Cutoff.fsi` shape: well-known attributes and
    // an `internal` representation.
    Shape {
        label: "known_attributes_internal_repr",
        sig: "[<NoEquality ; NoComparison>]\ntype U<'a> =\n    internal\n    | Ok\n    | Case1 of ('a -> int)\n",
        imp: "[<NoEquality ; NoComparison>]\ntype U<'a> =\n    internal\n    | Ok\n    | Case1 of ('a -> int)\n",
        model: Model::Precise,
    },
    // The module-root analogues, read as `M.Ok`.
    Shape {
        label: "module_duplicate_case_abbrev",
        sig: "module M =\n    type A =\n        | Ok of int\n\n    type B = Ok\n",
        imp: "module M =\n    type A =\n        | Ok of int\n\n    type B = Ok\n",
        model: Model::Precise,
    },
    Shape {
        label: "module_case_and_val",
        sig: "module M =\n    type A =\n        | Ok of int\n\n    val Ok : int\n",
        imp: "module M =\n    type A =\n        | Ok of int\n\n    let Ok = 1\n",
        model: Model::Precise,
    },
    Shape {
        label: "enum",
        sig: "type U =\n    | Case1 = 0\n    | Ok = 1\n",
        imp: "type U =\n    | Case1 = 0\n    | Ok = 1\n",
        model: Model::Unmodelled,
    },
    Shape {
        label: "enum_lowercase",
        sig: "type U =\n    | ref = 0\n    | failwith = 1\n",
        imp: "type U =\n    | ref = 0\n    | failwith = 1\n",
        model: Model::Unmodelled,
    },
];

/// The uses every reader file makes. Each is a function so a nullary case,
/// a case-with-fields and an FSharp.Core generic all type-check in the same
/// spelling; a name a row hides is an FCS error, which grades as "FCS
/// resolved nothing" and so forbids any commit.
const PROBES: &str = concat!(
    "    let useRef () = ref 0\n",
    "    let useFailwith () : int = failwith \"x\"\n",
    "    let useOk () = Ok\n",
    "    let useSome () = Some\n",
    "    let useCase1 () = Case1\n",
    "    let useQualified () = U.Case1\n",
    "    let useListMap () = List.map id [ 1 ]\n",
    "    let useInt (x : int) = x\n",
    "    let useFailure () = Failure \"x\"\n",
    "    let useModuleOk () = M.Ok\n",
);

/// Where the reader file sits relative to the pair, and how it reaches `N`.
#[derive(Clone, Copy, Debug)]
enum Reader {
    /// A later `namespace N` file: the implicit enclosing-namespace open.
    SameNamespaceAfter,
    /// A later file outside `N` that writes `open N`.
    OpenAfter,
    /// A `namespace N` file between the `.fsi` and the `.fs`.
    SameNamespaceBetween,
}

const READERS: &[Reader] = &[
    Reader::SameNamespaceAfter,
    Reader::OpenAfter,
    Reader::SameNamespaceBetween,
];

fn reader_src(reader: Reader) -> String {
    match reader {
        Reader::SameNamespaceAfter | Reader::SameNamespaceBetween => {
            format!("namespace N\n\nmodule UseB =\n{PROBES}")
        }
        Reader::OpenAfter => format!("module UseC\n\nopen N\n\nmodule Inner =\n{PROBES}"),
    }
}

/// The leaf name our assembly commit names (source spelling).
fn our_assembly_leaf(env: &AssemblyEnv, res: Resolution) -> (String, String) {
    match res {
        Resolution::Entity(h) => {
            let e = env.entity(h);
            (
                e.assembly.name.clone(),
                e.source_name.clone().unwrap_or_else(|| e.name.clone()),
            )
        }
        Resolution::Member { parent, idx } => {
            let e = env.entity(parent);
            let leaf = match env.member_at(parent, idx) {
                Member::Method(x) => x.source_name.clone().unwrap_or_else(|| x.name.clone()),
                Member::Field(x) => x.name.clone(),
                Member::Property(x) => x.name.clone(),
                Member::Event(x) => x.name.clone(),
            };
            (e.assembly.name.clone(), leaf)
        }
        _ => unreachable!("only assembly resolutions reach here"),
    }
}

/// Resolve the files with the signature-aware fold over [`env`].
fn resolve(files: &[(PathBuf, String)], rels: &[String]) -> ResolvedProject {
    let srcs: Vec<SourceFile> = rels
        .iter()
        .zip(files)
        .map(|(rel, (_, src))| source_file(rel, src))
        .collect();
    let paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
    let qnofs = qualified_names(&srcs, &paths);
    let input: Vec<ProjectFile> = srcs
        .into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof, SyntaxRecovery::Unretained))
        .collect();
    resolve_project_files(&input, env())
}

/// What one cell committed, for the must-commit obligations.
#[derive(Default)]
struct CellCommits {
    /// Probe needles whose use we committed to the project.
    project: Vec<&'static str>,
    /// Probe needles whose use we committed to an assembly.
    assembly: Vec<&'static str>,
    /// Certain-implies-exact violations: a commit FCS disagrees with.
    wrong: Vec<String>,
}

const NEEDLES: &[&str] = &[
    "ref", "failwith", "Ok", "Some", "Case1", "U.Case1", "List.map", "int", "Failure",
];

/// Grade one `(shape, reader)` cell; returns what the reader file committed.
fn grade_cell(shape: &Shape, reader: Reader) -> CellCommits {
    let sig = format!("namespace N\n\n{}", shape.sig);
    let imp = format!("namespace N\n\n{}", shape.imp);
    let use_src = reader_src(reader);
    let mut rows: Vec<(&str, &str)> = match reader {
        Reader::SameNamespaceBetween => vec![
            ("A.fsi", sig.as_str()),
            ("Use.fs", use_src.as_str()),
            ("A.fs", imp.as_str()),
        ],
        _ => vec![
            ("A.fsi", sig.as_str()),
            ("A.fs", imp.as_str()),
            ("Use.fs", use_src.as_str()),
        ],
    };
    if shape.sig.is_empty() {
        rows.retain(|(p, _)| *p != "A.fsi");
    }
    let use_idx = rows.iter().position(|(p, _)| *p == "Use.fs").unwrap();
    let label = format!("sig_ns_fold_{}_{reader:?}", shape.label);
    let (root, written) = temp_fs_tree(&label, &rows);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[]);
    let fcs: Vec<FileUses> = parse_fcs_uses_project(&json, &written);
    let rels: Vec<String> = rows.iter().map(|(p, _)| (*p).to_string()).collect();
    let proj = resolve(&written, &rels);
    let _ = std::fs::remove_dir_all(&root);

    let ctx = format!("[{} / {reader:?}]", shape.label);
    let mut commits = CellCommits::default();
    for (i, (path, src)) in written.iter().enumerate() {
        let rf = proj.file(i);
        let fu = fcs.iter().find(|f| f.path.file_name() == path.file_name());
        let uses = fu.map(|f| f.uses.as_slice()).unwrap_or_default();
        // Forward: every FCS use is uncommitted or committed to FCS's target.
        for u in uses {
            if u.start == u.end {
                continue;
            }
            let range = span(u.start, u.end);
            let Some(res) = rf.resolution_at(range) else {
                continue;
            };
            let text = &src[u.start..u.end];
            let file = path.file_name().unwrap_or_default();
            match (res, &u.decl) {
                (Resolution::Deferred(_), _) => {}
                (Resolution::Unresolved, _) => commits.wrong.push(format!(
                    "{ctx}: Unresolved at {text:?} in {file:?} where FCS resolved"
                )),
                (Resolution::Item(_) | Resolution::Local(_), Some(decl)) => {
                    let (def_idx, def_range) = match res {
                        Resolution::Item(_) => {
                            let (idx, def) = proj.item_def(res).expect("item def");
                            (idx, def.range)
                        }
                        _ => (i, rf.resolved_def(res).expect("local def").range),
                    };
                    if written[def_idx].0.file_name() != decl.file.file_name()
                        || def_range != span(decl.start, decl.end)
                    {
                        commits.wrong.push(format!(
                            "{ctx}: {text:?} in {file:?}: we point at {:?} {def_range:?}, FCS \
                             at {:?} {}..{}",
                            written[def_idx].0.file_name(),
                            decl.file.file_name(),
                            decl.start,
                            decl.end
                        ));
                    } else if i == use_idx
                        && def_idx != i
                        && let Some(n) = NEEDLES.iter().find(|n| **n == text)
                    {
                        commits.project.push(n);
                    }
                }
                (Resolution::Item(_) | Resolution::Local(_), None) => commits.wrong.push(format!(
                    "{ctx}: {text:?} in {file:?}: we commit in-project {res:?}, FCS binds \
                     {:?} in {:?}",
                    u.full_name, u.assembly
                )),
                (Resolution::Entity(_) | Resolution::Member { .. }, Some(decl)) => {
                    commits.wrong.push(format!(
                        "{ctx}: {text:?} in {file:?}: we commit assembly {:?}, FCS declares \
                         in-project at {:?}",
                        our_assembly_leaf(env(), res),
                        decl.file.file_name()
                    ));
                }
                (Resolution::Entity(_) | Resolution::Member { .. }, None) => {
                    let (asm, leaf) = our_assembly_leaf(env(), res);
                    let fcs_leaf = u
                        .full_name
                        .as_deref()
                        .and_then(|f| f.rsplit('.').next())
                        .unwrap_or_default();
                    if (Some(asm.as_str()), leaf.as_str()) != (u.assembly.as_deref(), fcs_leaf) {
                        commits.wrong.push(format!(
                            "{ctx}: {text:?} in {file:?}: we commit {asm}::{leaf}, FCS binds \
                             {:?} in {:?}",
                            u.full_name, u.assembly
                        ));
                    } else if i == use_idx
                        && let Some(n) = NEEDLES.iter().find(|n| **n == text)
                    {
                        commits.assembly.push(n);
                    }
                }
            }
        }
        // Reverse: every cross-file project commit has an FCS use there.
        for (&range, &res) in rf.resolutions() {
            if !matches!(res, Resolution::Item(_)) {
                continue;
            }
            let Some((def_idx, _)) = proj.item_def(res) else {
                continue;
            };
            if def_idx == i {
                continue;
            }
            let start = u32::from(range.start()) as usize;
            let end = u32::from(range.end()) as usize;
            if !uses
                .iter()
                .any(|u| u.start == start && u.end == end && u.decl.is_some())
            {
                commits.wrong.push(format!(
                    "{ctx}: we commit {:?} in {:?} to a project item; FCS resolves no \
                     project use there",
                    &src[start..end],
                    path.file_name()
                ));
            }
        }
    }
    commits
}

/// The table, graded cell by cell, with the must-commit obligations that
/// keep it from passing by deferring everything.
#[test]
fn signature_namespace_fold_agrees_with_fcs() {
    let mut missing = Vec::new();
    let mut wrong = Vec::new();
    for shape in SHAPES {
        for &reader in READERS {
            let mut c = grade_cell(shape, reader);
            wrong.append(&mut c.wrong);
            let cell = format!("[{} / {reader:?}]", shape.label);
            // Between the signature and its implementation nothing of the
            // pair is published, so every row reads like no row at all.
            let model = if matches!(reader, Reader::SameNamespaceBetween) {
                Model::Precise
            } else {
                shape.model
            };
            if matches!(model, Model::Exposed | Model::Precise) {
                for needle in ["ref", "failwith"] {
                    if !c.assembly.contains(&needle) {
                        missing.push(format!(
                            "{cell}: {needle:?} must commit to FSharp.Core (assembly {:?}, \
                             project {:?})",
                            c.assembly, c.project
                        ));
                    }
                }
            }
            if model == Model::Exposed {
                for needle in ["Ok", "Some", "Case1"] {
                    if !c.project.contains(&needle) {
                        missing.push(format!(
                            "{cell}: {needle:?} must commit to the signature's case (project \
                             {:?})",
                            c.project
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "wrong commits:\n{}", wrong.join("\n"));
    assert!(
        missing.is_empty(),
        "must-commit obligations unmet:\n{}",
        missing.join("\n")
    );
}

/// The resolution-explain trace records each block's implicit open of its
/// own enclosing namespace, so a namespace fold that shadows everything can be
/// read off the trace rather than hand-traced: an unmodelled row raises the
/// barrier and the dotted-head blanket there, an exactly-exported one neither.
#[test]
fn the_implicit_namespace_open_is_traced() {
    let cases = [
        ("exposed", false),
        ("opaque", false),
        ("private_repr", true),
    ];
    for (label, perturbs) in cases {
        let shape = SHAPES.iter().find(|s| s.label == label).expect("row");
        let sig = format!("namespace N\n\n{}", shape.sig);
        let imp = format!("namespace N\n\n{}", shape.imp);
        let use_src = reader_src(Reader::SameNamespaceAfter);
        let rows = [
            ("/p/A.fsi", sig.as_str()),
            ("/p/A.fs", imp.as_str()),
            ("/p/Use.fs", use_src.as_str()),
        ];
        let written: Vec<(PathBuf, String)> = rows
            .iter()
            .map(|(p, s)| (PathBuf::from(p), (*s).to_string()))
            .collect();
        let rels: Vec<String> = rows.iter().map(|(p, _)| (*p).to_string()).collect();
        let proj = resolve(&written, &rels);
        let opens = &proj.file(2).resolution_trace().opens;
        let implicit: Vec<_> = opens.iter().filter(|o| o.implicit).collect();
        assert_eq!(implicit.len(), 1, "{label}: one block, one implicit open");
        let o = implicit[0];
        assert_eq!(o.path, vec!["N".to_string()], "{label}");
        let header = use_src.find('N').expect("header");
        assert_eq!(o.range, span(header, header + 1), "{label}: header range");
        assert_eq!(
            o.opacity.staled_earlier && o.opacity.opaque_dotted,
            perturbs,
            "{label}: {:?}",
            o.opacity
        );
    }
}

/// One namespace-level declaration that puts `Ok` somewhere — a pairwise
/// grid's alphabet. `precise` says whether it leaves the fold enumerable on
/// its own.
struct Fragment {
    label: &'static str,
    sig: &'static str,
    imp: &'static str,
    precise: bool,
}

const FRAGMENTS: &[Fragment] = &[
    Fragment {
        label: "union",
        sig: "type A1 =\n    | Ok of int\n",
        imp: "type A1 =\n    | Ok of int\n",
        precise: true,
    },
    Fragment {
        label: "rqa",
        sig: "[<RequireQualifiedAccess>]\ntype A2 =\n    | Ok\n",
        imp: "[<RequireQualifiedAccess>]\ntype A2 =\n    | Ok\n",
        precise: true,
    },
    Fragment {
        label: "abbrev_case",
        sig: "type A3 = Ok\n",
        imp: "type A3 = Ok\n",
        precise: false,
    },
    Fragment {
        label: "exception",
        sig: "exception Ok of string\n",
        imp: "exception Ok of string\n",
        precise: false,
    },
    Fragment {
        label: "private_repr",
        sig: "type A5 =\n    private\n    | Ok\n",
        imp: "type A5 =\n    private\n    | Ok\n",
        precise: false,
    },
    Fragment {
        label: "enum",
        sig: "type A6 =\n    | Ok = 1\n",
        imp: "type A6 =\n    | Ok = 1\n",
        precise: false,
    },
    Fragment {
        label: "record",
        sig: "type A7 = { Ok : int }\n",
        imp: "type A7 = { Ok : int }\n",
        precise: true,
    },
    Fragment {
        label: "class",
        sig: "type Ok =\n    new : unit -> Ok\n",
        imp: "type Ok() = class end\n",
        precise: false,
    },
    Fragment {
        label: "aliased_rqa",
        sig: "type Qual9 = Microsoft.FSharp.Core.RequireQualifiedAccessAttribute\n\n[<Qual9>]\ntype A9 =\n    | Ok\n",
        imp: "type Qual9 = Microsoft.FSharp.Core.RequireQualifiedAccessAttribute\n\n[<Qual9>]\ntype A9 =\n    | Ok\n",
        precise: false,
    },
    Fragment {
        label: "opaque",
        sig: "type A8\n",
        imp: "type A8 =\n    | Ok\n",
        precise: true,
    },
];

/// Every ordered pair of distinct [`FRAGMENTS`] in one signatured namespace:
/// the collision space a single-declaration table cannot reach (two
/// declarations of one bare name, one of them unmodelled, is how a stale
/// export became a wrong target). Graded exactly as the table is; a pair of
/// precise fragments must keep the FSharp.Core names committed.
#[test]
fn pairwise_namespace_declarations_agree_with_fcs() {
    let mut wrong = Vec::new();
    let mut missing = Vec::new();
    for a in FRAGMENTS {
        for b in FRAGMENTS {
            if a.label == b.label {
                continue;
            }
            let label: &'static str = Box::leak(format!("{}_then_{}", a.label, b.label).into());
            let sig: &'static str = Box::leak(format!("{}\n{}", a.sig, b.sig).into());
            let imp: &'static str = Box::leak(format!("{}\n{}", a.imp, b.imp).into());
            let shape = Shape {
                label,
                sig,
                imp,
                model: Model::Unmodelled,
            };
            for reader in [Reader::SameNamespaceAfter, Reader::OpenAfter] {
                let mut c = grade_cell(&shape, reader);
                wrong.append(&mut c.wrong);
                if a.precise && b.precise {
                    for needle in ["ref", "failwith"] {
                        if !c.assembly.contains(&needle) {
                            missing.push(format!("[{label} / {reader:?}]: {needle:?}"));
                        }
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "wrong commits:\n{}", wrong.join("\n"));
    assert!(
        missing.is_empty(),
        "must-commit obligations unmet:\n{}",
        missing.join("\n")
    );
}
