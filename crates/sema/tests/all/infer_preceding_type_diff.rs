//! Type-position names that an **earlier Compile-order file** declares,
//! against FCS checking the same files as one project.
//!
//! Two things are graded, each certain-implies-exact:
//!
//! - **the resolver**: every type-position use FCS reports in the using file
//!   that we resolve names FCS's declaration: an earlier file's type by its
//!   declaration site, this file's by its def, an assembly type by its
//!   assembly and full name (`uses-project`);
//! - **inference**: every node and binder type we commit in any file is FCS's
//!   (`types-project`).
//!
//! The kinds grid crosses the type kinds of the in-file grid
//! (`infer_project_type_annotation_diff`) with where the type is declared and
//! how a later file reaches it: the enclosing namespace (a `namespace`
//! header, and a `module N.M` header), an `open` of the namespace or of a
//! module, a qualified path, an `[<AutoOpen>]` module, a nested module, a
//! headerless file, and a file with a signature. The shadow grid crosses a
//! few kinds and the same positions with what can take the name — a later
//! `open` of another project namespace declaring it, an assembly `open`, a
//! type of the name in the using file, a project module of the name, an
//! `[<AutoOpen>]` redeclaration, another arity — and with names that
//! FSharp.Core or the BCL also supply (`Result`, `string`, `Random`).
//!
//! The uncontested cells of the positions the resolver models must commit,
//! in the resolver and in inference, so the grids cannot pass by declining.
//! The ill-typed family uses earlier files' types where FCS rejects the
//! program.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::common::{
    GradedDumps, full_bcl_env, invoke_fcs_dump_project, invoke_fcs_dump_types_project,
    parse_fcs_uses_project, temp_fs_tree,
};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::TextRange;
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    AssemblyEnv, InferredProject, ProjectFile, Resolution, ResolvedProject, SourceFile,
    SyntaxRecovery, infer_project, qualified_names, resolve_project_files,
};

/// A type declaration and how a use spells it: `{T}` is the type's name,
/// `{P}` the qualifier a position puts before it.
struct Kind {
    label: &'static str,
    decl: &'static str,
    spell: &'static str,
    /// Whether the use must commit in a position the resolver models.
    commits: bool,
}

const fn k(label: &'static str, decl: &'static str, spell: &'static str, commits: bool) -> Kind {
    Kind {
        label,
        decl,
        spell,
        commits,
    }
}

const KINDS: &[Kind] = &[
    k("record", "type {T} = { X : int }", "{P}{T}", true),
    k("union", "type {T} = A | B of int", "{P}{T}", true),
    k(
        "class",
        "type {T}(x : int) =\n    member _.X = x",
        "{P}{T}",
        true,
    ),
    k(
        "struct",
        "[<Struct>]\ntype {T} = { Y : int }",
        "{P}{T}",
        true,
    ),
    k(
        "interface",
        "type {T} =\n    abstract M : int -> int",
        "{P}{T}",
        true,
    ),
    k(
        "enum",
        "type {T} =\n    | Red = 0\n    | Green = 1",
        "{P}{T}",
        true,
    ),
    k(
        "delegate",
        "type {T} = delegate of int -> int",
        "{P}{T}",
        true,
    ),
    k("generic", "type {T}<'a> = { V : 'a }", "{P}{T}<int>", true),
    k(
        "generic-postfix",
        "type {T}<'a> = { V : 'a }",
        "int {P}{T}",
        true,
    ),
    k(
        "generic2",
        "type {T}<'a, 'b> = { P : 'a; Q : 'b }",
        "{P}{T}<int, string>",
        true,
    ),
    k(
        "rqa-union",
        "[<RequireQualifiedAccess>]\ntype {T} = A | B",
        "{P}{T}",
        true,
    ),
    k(
        "backticked",
        "type ``{T} Odd`` = { O : int }",
        "{P}``{T} Odd``",
        true,
    ),
    k(
        "internal",
        "type internal {T} = { Z : int }",
        "{P}{T}",
        true,
    ),
    k("private", "type private {T} = { Z : int }", "{P}{T}", true),
    k(
        "renamed",
        "[<CompiledName(\"Renamed\")>]\ntype {T} = { W : int }",
        "{P}{T}",
        false,
    ),
    k("abbreviation", "type {T} = int", "{P}{T}", false),
    k(
        "generic-abbreviation",
        "type {T}<'a> = 'a list",
        "{P}{T}<int>",
        false,
    ),
    k("measure", "[<Measure>]\ntype {T}", "float<{P}{T}>", false),
    k("exception", "exception {T} of int", "{P}{T}", false),
    k(
        "constrained",
        "type {T}<'a when 'a : struct> = { V : 'a }",
        "{P}{T}<int>",
        false,
    ),
    k(
        "warned",
        "[<System.Obsolete(\"old\")>]\ntype {T} = { X : int }",
        "{P}{T}",
        false,
    ),
];

/// Where the type is declared, and how the using file reaches it.
///
/// `earlier` are the files before the using one, each `(name, template)`:
/// `{DECL}` is the declaration, indented by `decl_indent`. `using` is the
/// using file: `{OPENS}` takes the shadow's opens, `{DECLS}` its in-file
/// declarations, `{USES}` the uses module.
struct Position {
    label: &'static str,
    earlier: &'static [(&'static str, &'static str)],
    using: &'static str,
    decl_indent: usize,
    prefix: &'static str,
    /// Whether the resolver models this position, so an uncontested use must
    /// commit.
    commits: bool,
}

const POSITIONS: &[Position] = &[
    Position {
        label: "enclosing-namespace",
        earlier: &[("A.fs", "namespace Lib\n\n{DECL}\n")],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: true,
    },
    Position {
        label: "enclosing-namespace-of-module",
        earlier: &[("A.fs", "namespace Lib\n\n{DECL}\n")],
        using: "module Lib.Top\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: true,
    },
    // A namespace a referenced assembly declares too: with the name
    // `Random`, the BCL's `System.Random` sits beside the project's.
    Position {
        label: "assembly-namespace",
        earlier: &[("A.fs", "namespace System\n\n{DECL}\n")],
        using: "namespace System\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: false,
    },
    Position {
        label: "open-namespace",
        earlier: &[("A.fs", "namespace Lib\n\n{DECL}\n")],
        using: "namespace Other\n\nopen Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: false,
    },
    Position {
        label: "qualified-namespace",
        earlier: &[("A.fs", "namespace Lib\n\n{DECL}\n")],
        using: "namespace Other\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "Lib.",
        commits: false,
    },
    Position {
        label: "open-module",
        earlier: &[("A.fs", "module Lib\n\n{DECL}\n")],
        using: "namespace Other\n\nopen Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: false,
    },
    Position {
        label: "qualified-module",
        earlier: &[("A.fs", "module Lib\n\n{DECL}\n")],
        using: "namespace Other\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "Lib.",
        commits: false,
    },
    Position {
        label: "auto-open-module",
        earlier: &[(
            "A.fs",
            "namespace Lib\n\n[<AutoOpen>]\nmodule Auto =\n{DECL}\n",
        )],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 4,
        prefix: "",
        commits: false,
    },
    Position {
        label: "nested-module",
        earlier: &[("A.fs", "namespace Lib\n\nmodule Inner =\n{DECL}\n")],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 4,
        prefix: "Inner.",
        commits: false,
    },
    Position {
        label: "module-in-namespace",
        earlier: &[("A.fs", "module Lib.Mod\n\n{DECL}\n")],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "Mod.",
        commits: false,
    },
    Position {
        label: "headerless",
        earlier: &[("A.fs", "{DECL}\n")],
        using: "module B\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "A.",
        commits: false,
    },
    Position {
        label: "top-level-auto-open-module",
        earlier: &[("A.fs", "[<AutoOpen>]\nmodule Auto\n\n{DECL}\n")],
        using: "module B\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: false,
    },
    // Out of scope, named bare: FCS binds a same-named FSharp.Core type, or
    // rejects the program.
    Position {
        label: "unrelated-namespace",
        earlier: &[("A.fs", "namespace Lib\n\n{DECL}\n")],
        using: "namespace Other\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: false,
    },
    Position {
        label: "nested-module-bare",
        earlier: &[("A.fs", "namespace Lib\n\nmodule Inner =\n{DECL}\n")],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 4,
        prefix: "",
        commits: false,
    },
    // A `private` module: in scope from its namespace, not from an `open` of
    // it elsewhere.
    Position {
        label: "private-auto-open-module",
        earlier: &[(
            "A.fs",
            "namespace Lib\n\n[<AutoOpen>]\nmodule private Auto =\n{DECL}\n",
        )],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 4,
        prefix: "",
        commits: false,
    },
    Position {
        label: "private-auto-open-module-opened",
        earlier: &[(
            "A.fs",
            "namespace Lib\n\n[<AutoOpen>]\nmodule private Auto =\n{DECL}\n",
        )],
        using: "module B\n\nopen Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 4,
        prefix: "",
        commits: false,
    },
    // A signature is what a later file sees, and it may declare the type
    // differently from the implementation.
    Position {
        label: "signatured",
        earlier: &[
            ("A.fsi", "namespace Lib\n\n{DECL}\n"),
            ("A.fs", "namespace Lib\n\n{DECL}\n"),
        ],
        using: "namespace Lib\n\n{OPENS}{DECLS}{USES}",
        decl_indent: 0,
        prefix: "",
        commits: false,
    },
];

/// What may take the name from the earlier file's type. `{N}` is the name.
struct Shadow {
    label: &'static str,
    /// A file between the declaring files and the using one.
    file: Option<&'static str>,
    /// Opens after the using file's header.
    opens: &'static str,
    /// Declarations in the using file, before the uses.
    decls: &'static str,
}

const SHADOWS: &[Shadow] = &[
    Shadow {
        label: "none",
        file: None,
        opens: "",
        decls: "",
    },
    Shadow {
        label: "later-project-open",
        file: Some("namespace Shadow\n\ntype {N} = { S : string }\n"),
        opens: "open Shadow\n\n",
        decls: "",
    },
    Shadow {
        label: "assembly-open",
        file: None,
        opens: "open System\n\n",
        decls: "",
    },
    Shadow {
        label: "in-file-type",
        file: None,
        opens: "",
        decls: "type {N} = { Local : int }\n\n",
    },
    Shadow {
        label: "same-named-module",
        file: Some("namespace Lib\n\nmodule {N} =\n    let v = 1\n"),
        opens: "",
        decls: "",
    },
    Shadow {
        label: "auto-open-redeclared",
        file: Some(
            "namespace Lib\n\n[<AutoOpen>]\nmodule Redeclared =\n    type {N} = { R : string }\n",
        ),
        opens: "",
        decls: "",
    },
    Shadow {
        label: "auto-open-exception",
        file: Some("namespace Lib\n\n[<AutoOpen>]\nmodule Exn =\n    exception {N} of int\n"),
        opens: "",
        decls: "",
    },
    Shadow {
        label: "other-arity",
        file: Some("namespace Lib\n\ntype {N}<'a, 'b, 'c> = { G : 'a }\n"),
        opens: "",
        decls: "",
    },
];

/// The type's name: fresh, or one FSharp.Core or the BCL also supplies.
const NAMES: &[&str] = &["T", "Result", "string", "Random"];

fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The uses, for a type spelled `t`, in a module as accessible as the type
/// (`access`; FS0410 otherwise). `p`'s parameter is the binder that must
/// commit.
fn uses(t: &str, access: &str) -> String {
    format!(
        "module {access}Uses =\n    \
         let p (x : {t}) = x\n    \
         let v : {t} = Unchecked.defaultof<_>\n    \
         let r () : {t} = Unchecked.defaultof<_>\n    \
         let (tp : {t}) = Unchecked.defaultof<_>\n    \
         let q (a : {t}, b : int) = b\n    \
         let h = p\n    \
         let l (x : {t}) =\n        let y : {t} = x\n        y\n"
    )
}

/// Whether `kind`'s declaration can be written at `pos`: a signature cannot
/// hold a class's implementation.
fn fits(kind: &Kind, pos: &Position) -> bool {
    !(kind.label == "class" && pos.earlier.iter().any(|(f, _)| f.ends_with(".fsi")))
}

/// One project: its files in Compile order, the using file last.
struct Cell {
    label: String,
    files: Vec<(String, String)>,
    must_commit: bool,
}

fn cell(kind: &Kind, pos: &Position, shadow: &Shadow, name: &str) -> Cell {
    let decl = indent(&kind.decl.replace("{T}", name), pos.decl_indent);
    let mut files: Vec<(String, String)> = pos
        .earlier
        .iter()
        .map(|(file, template)| ((*file).to_string(), template.replace("{DECL}", &decl)))
        .collect();
    if let Some(shadow_file) = shadow.file {
        files.push(("S.fs".to_string(), shadow_file.replace("{N}", name)));
    }
    let spell = kind.spell.replace("{P}", pos.prefix).replace("{T}", name);
    let access = if kind.decl.contains("internal") {
        "internal "
    } else if kind.decl.contains("private") {
        "private "
    } else {
        ""
    };
    let using = pos
        .using
        .replace("{OPENS}", shadow.opens)
        .replace("{DECLS}", &shadow.decls.replace("{N}", name))
        .replace("{USES}", &uses(&spell, access));
    files.push(("B.fs".to_string(), using));
    Cell {
        label: format!("{}/{}/{}/{name}", kind.label, pos.label, shadow.label),
        files,
        must_commit: kind.commits && pos.commits && shadow.label == "none" && name == "T",
    }
}

/// A project's files, parsed and folded as the LSP folds them.
struct Folded {
    resolved: ResolvedProject,
    inferred: InferredProject,
}

fn fold(dir: &Path, sources: &[(String, String)], env: &AssemblyEnv) -> Folded {
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = sources
        .iter()
        .map(|(name, src)| {
            if name.ends_with(".fsi") {
                let p = parse_sig(src);
                assert!(p.errors.is_empty(), "{name}: {:?}\n{src}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Sig(SigFile::cast(p.root).expect("sig file")),
                    recovery,
                )
            } else {
                let p = parse(src);
                assert!(p.errors.is_empty(), "{name}: {:?}\n{src}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
                    recovery,
                )
            }
        })
        .collect();
    let paths: Vec<PathBuf> = sources.iter().map(|(n, _)| dir.join(n)).collect();
    let srcs: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let qnofs = qualified_names(&srcs, &paths);
    let files: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qnofs)
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    let resolved = resolve_project_files(&files, env);
    let inferred = infer_project(&files, &resolved, env);
    Folded { resolved, inferred }
}

#[derive(Default)]
struct Graded {
    wrong: Vec<String>,
    /// Cells FCS rejects: not graded.
    rejected: Vec<String>,
    /// Labels whose `p` parameter use resolved to FCS's declaration.
    resolved_use: BTreeSet<String>,
    /// Labels whose `p` parameter binder inference typed.
    typed_binder: BTreeSet<String>,
    /// Resolver commits FCS confirmed at an earlier file's type.
    preceding_commits: usize,
    graded: usize,
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Grade one cell. `expect_errors` says it is meant to be ill-typed.
fn grade_cell(c: &Cell, expect_errors: bool, g: &mut Graded) {
    let named: Vec<(&str, &str)> = c
        .files
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    let (root, written) = temp_fs_tree("preceding_type", &named);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let uses_json = invoke_fcs_dump_project(&paths);
    let fcs_uses = parse_fcs_uses_project(&uses_json, &written);
    let types = invoke_fcs_dump_types_project(&paths);
    let folded = fold(&root, &c.files, full_bcl_env());
    let _ = std::fs::remove_dir_all(&root);
    let label = &c.label;
    let errors: Vec<&String> = fcs_uses.iter().flat_map(|f| &f.errors).collect();
    if errors.is_empty() == expect_errors {
        g.rejected.push(format!("{label}: errors {errors:?}"));
        return;
    }
    let dump = |files: &[(String, String)]| {
        files
            .iter()
            .map(|(n, s)| format!("--- {n}\n{s}"))
            .collect::<Vec<_>>()
            .join("")
    };
    let last = c.files.len() - 1;
    let using_src = &c.files[last].1;
    // `p`'s annotation, in the cells that have one.
    let p_use = using_src.find("let p (x : ").map(|i| {
        let at = i + "let p (x : ".len();
        let end = using_src[at..].find(')').expect("p's annotation") + at;
        span(at, end)
    });

    // The resolver, at every use FCS reports in the using file.
    let rf = folded.resolved.file(last);
    let fcs_file = fcs_uses.last().expect("the using file");
    for u in &fcs_file.uses {
        if u.is_from_definition {
            continue;
        }
        let text = &using_src[u.start..u.end];
        let tail = text.rsplit('.').next().unwrap_or("");
        let tail = tail.split('<').next().unwrap_or("");
        let at = span(u.end - tail.len(), u.end);
        let Some(res) = rf.resolution_at(at) else {
            continue;
        };
        let verdict = match res {
            // Members and cases are other differentials' subject.
            Resolution::Deferred(_)
            | Resolution::Unresolved
            | Resolution::Member { .. }
            | Resolution::UnionCase { .. } => continue,
            Resolution::Local(def) => u.decl.as_ref().is_some_and(|d| {
                d.file == written[last].0 && span(d.start, d.end) == rf.def(def).range
            }),
            Resolution::Entity(h) => {
                let env = full_bcl_env();
                u.decl.is_none()
                    && u.full_name.as_deref() == Some(env.entity_full_name(h).as_str())
                    && u.assembly.as_deref() == Some(env.entity(h).assembly.name.as_str())
            }
            other => match folded.resolved.item_def(other) {
                Some((file, def)) => {
                    let ok = u.decl.as_ref().is_some_and(|d| {
                        d.file == written[file].0 && span(d.start, d.end) == def.range
                    });
                    if ok && file != last {
                        g.preceding_commits += 1;
                    }
                    ok
                }
                None => false,
            },
        };
        if verdict {
            g.graded += 1;
            if p_use.is_some_and(|p| p.contains_range(at)) {
                g.resolved_use.insert(label.clone());
            }
        } else {
            g.wrong.push(format!(
                "{label}: `{text}` resolved {res:?}; FCS {:?} {:?} {:?}\n{}",
                u.full_name,
                u.assembly,
                u.decl,
                dump(&c.files)
            ));
        }
    }

    // Inference, over every implementation file.
    for (idx, (name, src)) in c.files.iter().enumerate() {
        if name.ends_with(".fsi") {
            continue;
        }
        let Some(inferred) = folded.inferred.file(idx) else {
            continue;
        };
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let (types_json, binders_json) = &types[idx];
        let Some(dump_types) = GradedDumps::read(types_json, binders_json, src, Some(&file)) else {
            continue;
        };
        for (range, ty) in inferred.types() {
            let key = (usize::from(range.start()), usize::from(range.end()));
            if dump_types.is_ungraded(key) {
                continue;
            }
            let ours = ty.render();
            match dump_types.types.get(&key) {
                Some(theirs) if *theirs == ours => g.graded += 1,
                theirs => g.wrong.push(format!(
                    "{label}: {name} `{}`: ours `{ours}`, FCS {theirs:?}\n{}",
                    &src[key.0..key.1],
                    dump(&c.files)
                )),
            }
        }
        let rf = folded.resolved.file(idx);
        for (def, ty) in inferred.def_types() {
            let r = rf.def(*def).range;
            let key = (usize::from(r.start()), usize::from(r.end()));
            if dump_types.is_ungraded(key) {
                continue;
            }
            let ours = ty.render();
            match dump_types.binders.get(&key) {
                Some(theirs) if *theirs == ours && !dump_types.hides_constraint(key, &ours) => {
                    g.graded += 1;
                    if idx == last && &src[key.0..key.1] == "x" {
                        let first_x = src.find("let p (x").map(|i| i + "let p (".len());
                        if first_x == Some(key.0) {
                            g.typed_binder.insert(label.clone());
                        }
                    }
                }
                theirs => g.wrong.push(format!(
                    "{label}: {name} binder `{}`: ours `{ours}`, FCS {theirs:?}\n{}",
                    &src[key.0..key.1],
                    dump(&c.files)
                )),
            }
        }
    }
}

fn grade(cells: &[Cell], expect_errors: bool) -> Graded {
    let mut g = Graded::default();
    for c in cells {
        grade_cell(c, expect_errors, &mut g);
    }
    g
}

fn assert_must_commit(cells: &[Cell], g: &Graded) {
    let missing: Vec<String> = cells
        .iter()
        .filter(|c| c.must_commit)
        .filter_map(|c| {
            let resolved = g.resolved_use.contains(&c.label);
            let typed = g.typed_binder.contains(&c.label);
            (!resolved || !typed).then(|| {
                format!(
                    "{} (resolved {resolved}, typed {typed}){}",
                    c.label,
                    if g.rejected.iter().any(|r| r.starts_with(&c.label)) {
                        ": FCS rejected it"
                    } else {
                        ""
                    }
                )
            })
        })
        .collect();
    assert!(
        missing.is_empty(),
        "cells whose `p` annotation must resolve and type did not:\n{missing:#?}"
    );
}

#[test]
fn earlier_files_types_by_kind_and_position() {
    let shadow = &SHADOWS[0];
    let cells: Vec<Cell> = KINDS
        .iter()
        .flat_map(|kind| {
            POSITIONS
                .iter()
                .filter(|pos| fits(kind, pos))
                .map(move |pos| cell(kind, pos, shadow, "T"))
        })
        .collect();
    let g = grade(&cells, false);
    assert!(g.wrong.is_empty(), "wrong commits:\n{}", g.wrong.join("\n"));
    assert_must_commit(&cells, &g);
    assert!(g.preceding_commits > 0, "no earlier file's type committed");
    eprintln!(
        "kinds grid: {} cells, {} rejected by FCS, {} graded commits, {} at earlier files' types",
        cells.len(),
        g.rejected.len(),
        g.graded,
        g.preceding_commits
    );
}

/// The kinds the shadow grid crosses: one nominal, one generic, one union.
const SHADOW_KINDS: &[&str] = &["record", "generic", "union"];

#[test]
fn earlier_files_types_against_what_can_take_the_name() {
    let mut cells = Vec::new();
    for kind in KINDS.iter().filter(|k| SHADOW_KINDS.contains(&k.label)) {
        for pos in POSITIONS.iter().filter(|pos| fits(kind, pos)) {
            for shadow in SHADOWS {
                for name in NAMES {
                    cells.push(cell(kind, pos, shadow, name));
                }
            }
        }
    }
    let g = grade(&cells, false);
    assert!(g.wrong.is_empty(), "wrong commits:\n{}", g.wrong.join("\n"));
    assert_must_commit(&cells, &g);
    eprintln!(
        "shadow grid: {} cells, {} rejected by FCS, {} graded commits, {} at earlier files' types",
        cells.len(),
        g.rejected.len(),
        g.graded,
        g.preceding_commits
    );
}

/// Earlier files' types where FCS rejects the program.
const ILL_TYPED: &[(&str, &str)] = &[
    ("mismatched-body", "let f (x : T) : int = x\n"),
    ("mismatched-value", "let v : T = 1\n"),
    ("other-type", "let g (x : T) : U = x\n"),
    ("operator", "let h (x : T) = x + 1\n"),
    (
        "branches",
        "let k (x : T) (y : U) = if true then x else y\n",
    ),
    ("applied", "let m (f : T -> int) (u : U) = f u\n"),
    (
        "pattern",
        "let n (x : T) = match x with | 0 -> 1 | _ -> 2\n",
    ),
    ("no-equality", "let e (x : NoEq) (y : NoEq) = x = y\n"),
    ("generic-arity", "let a (x : G<int, int>) = x\n"),
];

#[test]
fn ill_typed_uses_of_earlier_files_types_commit_only_what_fcs_kept() {
    let cells: Vec<Cell> = ILL_TYPED
        .iter()
        .map(|(label, body)| Cell {
            label: (*label).to_string(),
            files: vec![
                (
                    "A.fs".to_string(),
                    "namespace Lib\n\ntype T = { X : int }\ntype U = A | B\n\
                     type NoEq = { F : int -> int }\ntype G<'a> = { V : 'a }\n"
                        .to_string(),
                ),
                (
                    "B.fs".to_string(),
                    format!("namespace Lib\n\nmodule Uses =\n{}", indent(body, 4)),
                ),
            ],
            must_commit: false,
        })
        .collect();
    let g = grade(&cells, true);
    assert!(g.wrong.is_empty(), "wrong commits:\n{}", g.wrong.join("\n"));
    assert!(
        g.rejected.is_empty(),
        "every ill-typed cell must be rejected by FCS:\n{}",
        g.rejected.join("\n")
    );
    assert!(
        g.preceding_commits >= ILL_TYPED.len() - 1,
        "{}",
        g.preceding_commits
    );
}

/// Earlier files' types against an assembly whose `[<AutoOpen>]` module
/// declares the name (`AbbrevAuto.Auto.Shadowed`, the structural-abbreviation
/// fixture): reached by an explicit `open`, a later write than the enclosing
/// namespace, and sharing the enclosing namespace itself. Graded like the
/// grids: a commit to an earlier file's type must be FCS's declaration.
#[test]
fn earlier_files_types_against_an_assembly_auto_open_module() {
    use crate::common::invoke_fcs_dump_project_with_refs;
    use crate::resolve_terminal_abbreviation_diff::{env, fixture_dll};
    let cases: &[(&str, &str, &str)] = &[
        (
            "explicit-open",
            "namespace Lib\n\ntype Shadowed = { X : int }\n",
            "namespace Lib\n\nopen AbbrevAuto\n\nmodule Uses =\n    let p (x : Shadowed) = x\n",
        ),
        (
            "shared-namespace",
            "namespace AbbrevAuto\n\ntype Shadowed = { X : int }\n",
            "namespace AbbrevAuto\n\nmodule Uses =\n    let p (x : Shadowed) = x\n",
        ),
    ];
    let mut wrong = Vec::new();
    for (label, a, b) in cases {
        let files = [
            ("A.fs".to_string(), (*a).to_string()),
            ("B.fs".to_string(), (*b).to_string()),
        ];
        let named: Vec<(&str, &str)> = files
            .iter()
            .map(|(n, s)| (n.as_str(), s.as_str()))
            .collect();
        let (root, written) = temp_fs_tree("assembly_auto_open", &named);
        let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture_dll()]);
        let fcs = parse_fcs_uses_project(&json, &written);
        let folded = fold(&root, &files, env());
        let _ = std::fs::remove_dir_all(&root);
        let src = &files[1].1;
        let at = src.find("Shadowed").expect("the use");
        let fcs_use = fcs[1]
            .uses
            .iter()
            .find(|u| u.start == at && !u.is_from_definition)
            .expect("FCS reports the use");
        let res = folded
            .resolved
            .file(1)
            .resolution_at(span(at, at + "Shadowed".len()));
        eprintln!(
            "{label}: FCS {:?} (errors {:?}); ours {res:?}",
            fcs_use.full_name, fcs[1].errors
        );
        if let Some(res @ Resolution::EarlierFileType { .. }) = res {
            let (file, def) = folded.resolved.item_def(res).expect("an earlier type");
            let agrees = fcs[1].errors.is_empty()
                && fcs_use.decl.as_ref().is_some_and(|d| {
                    d.file == written[file].0 && span(d.start, d.end) == def.range
                });
            if !agrees {
                wrong.push(format!(
                    "{label}: committed {res:?}; FCS {:?}",
                    fcs_use.full_name
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "wrong commits:\n{}", wrong.join("\n"));
}
