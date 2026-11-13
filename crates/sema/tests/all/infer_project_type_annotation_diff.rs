//! Annotations naming a type the file itself declares, against FCS.
//!
//! FCS names a project type by its full path — namespace, then each
//! enclosing module's *compiled* name, then the type's — with its type
//! arguments (`N.Inner.G<System.Int32>`). Three things make that path
//! something other than the written one, and each is a row here: a type's own
//! `[<CompiledName>]`, a module that takes a `Module` suffix (an explicit
//! `ModuleSuffix`, or a sibling type of the same name), and a file with no
//! header, whose implicit module comes from its file name.
//!
//! The grid crosses type kinds (record, union, class, struct, interface,
//! enum, delegate, generic at arities 1 and 2, private, backticked, and the
//! shapes that must decline: an abbreviation, a measure, an exception, a
//! renamed type) with declaring positions (a namespace, a top-level module, a
//! nested module, a module in a namespace, a `[<RequireQualifiedAccess>]`
//! module, and the declining ones above), and annotates a parameter, a
//! value, a return, a typed pattern, a tuple element and a local in each.
//! Every type we commit is graded against FCS's `types` and `binder-types`:
//! certain-implies-exact. The nominal kinds in the plain positions must
//! commit the parameter, so the grid cannot pass by declining.
//!
//! The ill-typed family puts project-typed binders where FCS rejects the
//! program — a mismatched body, a value of one project type where another is
//! expected, an operator over one — and holds the same grader to the nodes
//! FCS keeps.

use std::collections::BTreeMap;
use std::path::Path;

use crate::common::{GradedDumps, full_bcl_env, invoke_fcs_dump_types_project, temp_fs_tree};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    ProjectItems, SourceFile, SyntaxRecovery, infer_named_file, qualified_names, resolve_file,
};

/// A type declaration and how a use spells it. `{T}` in `decl` and `spell`
/// stands for the type's name, and `{P}` in `spell` for the qualifier a
/// position puts before it.
struct Kind {
    label: &'static str,
    decl: &'static str,
    spell: &'static str,
    /// Whether the annotation must commit in a plain position.
    commits: bool,
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
    k("private", "type private {T} = { Z : int }", "{P}{T}", true),
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
    // A constrained type parameter, satisfied: valid F#, but nothing here
    // checks an argument against a constraint, so it declines.
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

const fn k(label: &'static str, decl: &'static str, spell: &'static str, commits: bool) -> Kind {
    Kind {
        label,
        decl,
        spell,
        commits,
    }
}

/// Where the type is declared. `{DECL}` is the declaration, `{USES}` the uses
/// module, both indented for the place they go; `prefix` qualifies the type at
/// the uses.
struct Position {
    label: &'static str,
    file: &'static str,
    /// Spaces of indent for the declaration and for the uses module.
    decl_indent: usize,
    uses_indent: usize,
    prefix: &'static str,
    commits: bool,
}

const POSITIONS: &[Position] = &[
    p(
        "namespace",
        "namespace N.Inner\n\n{DECL}\n\n{USES}",
        0,
        0,
        "",
        true,
    ),
    p("module", "module Top\n\n{DECL}\n\n{USES}", 0, 0, "", true),
    // Graded, but not bound to commit: the resolver records an in-file type
    // only at a single-segment name, so a qualified `M.T` declines (the
    // resolver stage's work).
    p(
        "nested-module",
        "module Top\n\nmodule M =\n{DECL}\n\n{USES}",
        4,
        0,
        "M.",
        false,
    ),
    p(
        "namespace-module",
        "namespace N\n\nmodule M =\n{DECL}\n\n{USES}",
        4,
        0,
        "M.",
        false,
    ),
    p(
        "rqa-module",
        "namespace N\n\n[<RequireQualifiedAccess>]\nmodule M =\n{DECL}\n\n{USES}",
        4,
        0,
        "M.",
        false,
    ),
    p(
        "inside-rqa-module",
        "namespace N\n\n[<RequireQualifiedAccess>]\nmodule M =\n{DECL}\n\n{USES}",
        4,
        4,
        "",
        true,
    ),
    p(
        "inside-nested-module",
        "module Top\n\nmodule M =\n{DECL}\n\n{USES}",
        4,
        4,
        "",
        true,
    ),
    p(
        "inside-module",
        "namespace N\n\nmodule M =\n{DECL}\n\n{USES}",
        4,
        4,
        "",
        true,
    ),
    // `module T` beside `type T` compiles as `TModule`.
    p(
        "suffix-sibling",
        "namespace N\n\ntype M = { S : int }\n\nmodule M =\n{DECL}\n\n{USES}",
        4,
        4,
        "",
        false,
    ),
    p(
        "suffix-attribute",
        "namespace N\n\n[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]\nmodule M =\n{DECL}\n\n{USES}",
        4,
        4,
        "",
        false,
    ),
    p("headerless", "{DECL}\n\n{USES}", 0, 0, "", true),
    p(
        "top-module-suffix",
        "[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]\nmodule Top\n\n{DECL}\n\n{USES}",
        0,
        0,
        "",
        false,
    ),
];

const fn p(
    label: &'static str,
    file: &'static str,
    decl_indent: usize,
    uses_indent: usize,
    prefix: &'static str,
    commits: bool,
) -> Position {
    Position {
        label,
        file,
        decl_indent,
        uses_indent,
        prefix,
        commits,
    }
}

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

/// The uses, for a type spelled `t`, in a module as private as the type
/// (FS0410 otherwise). `p` is the binder whose parameter must commit.
fn uses(t: &str, private: bool) -> String {
    let access = if private { "private " } else { "" };
    format!(
        "module {access}Uses =\n    \
         let p (x : {t}) = x\n    \
         let v : {t} = Unchecked.defaultof<_>\n    \
         let r () : {t} = Unchecked.defaultof<_>\n    \
         let (tp : {t}) = Unchecked.defaultof<_>\n    \
         let q (a : {t}, b : int) = b\n    \
         let w = v\n    \
         let h = p\n    \
         let l (x : {t}) =\n        let y : {t} = x\n        y\n"
    )
}

fn program(kind: &Kind, pos: &Position) -> String {
    let name = "T";
    let decl = kind.decl.replace("{T}", name);
    let spell = kind.spell.replace("{P}", pos.prefix).replace("{T}", name);
    pos.file
        .replace("{DECL}", &indent(&decl, pos.decl_indent))
        .replace(
            "{USES}",
            &indent(
                &uses(&spell, kind.decl.contains("private")),
                pos.uses_indent,
            ),
        )
}

#[derive(Default)]
struct Graded {
    /// `label -> committed binder types`, for the non-vacuity check.
    committed: BTreeMap<String, Vec<String>>,
    wrong: Vec<String>,
    rejected: Vec<String>,
    graded: usize,
}

/// Infer each `(label, source)` as its own one-file project and grade every
/// commit against FCS. `expect_errors` says the programs are meant to be
/// ill-typed.
fn grade(programs: &[(String, String)], expect_errors: bool) -> Graded {
    let env = full_bcl_env();
    let mut g = Graded::default();
    for (label, src) in programs {
        // The file name is the headerless file's implicit module.
        let (root, written) = temp_fs_tree("project_type_annotation", &[("Headerless.fs", src)]);
        let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let dumps = invoke_fcs_dump_types_project(&paths);
        let _ = std::fs::remove_dir_all(&root);
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "{label}: {:?}\n{src}",
            parsed.errors
        );
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let (types_json, binders_json) = &dumps[0];
        let dump = GradedDumps::read(types_json, binders_json, src, Some(&file))
            .unwrap_or_else(|| panic!("{label}: an unconverted declaration could not be placed"));
        if dump.errors.is_empty() == expect_errors {
            g.rejected
                .push(format!("{label}: errors {:?}\n{src}", dump.errors));
            continue;
        }
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let name = qualified_names(&[SourceFile::Impl(file.clone())], &[written[0].0.clone()])
            .pop()
            .expect("one name per file");
        let inferred = infer_named_file(
            &file,
            &resolved,
            env,
            &borzoi_sema::PrecedingTypes::default(),
            &name,
        );
        let line_of = |at: usize| src[..at].matches('\n').count() + 1;
        for (range, ty) in inferred.types() {
            let key = (
                u32::from(range.start()) as usize,
                u32::from(range.end()) as usize,
            );
            if dump.is_ungraded(key) {
                continue;
            }
            let ours = ty.render(env);
            match dump.types.get(&key) {
                Some(theirs) if *theirs == ours => g.graded += 1,
                theirs => g.wrong.push(format!(
                    "{label}: line {} `{}`: ours `{ours}`, FCS {theirs:?}\n{src}",
                    line_of(key.0),
                    &src[key.0..key.1]
                )),
            }
        }
        for (def, ty) in inferred.def_types() {
            let r = resolved.def(*def).range;
            let key = (u32::from(r.start()) as usize, u32::from(r.end()) as usize);
            if dump.is_ungraded(key) {
                continue;
            }
            let ours = ty.render(env);
            match dump.binders.get(&key) {
                Some(theirs) if *theirs == ours && !dump.hides_constraint(key, &ours) => {
                    g.graded += 1;
                    g.committed
                        .entry(label.clone())
                        .or_default()
                        .push(format!("{}: {ours}", &src[key.0..key.1]));
                }
                theirs => g.wrong.push(format!(
                    "{label}: binder `{}` line {}: ours `{ours}`, FCS {theirs:?}\n{src}",
                    &src[key.0..key.1],
                    line_of(key.0)
                )),
            }
        }
    }
    g
}

#[test]
fn project_type_annotations_commit_what_fcs_does() {
    let mut programs = Vec::new();
    let mut must_commit = Vec::new();
    for kind in KINDS {
        for pos in POSITIONS {
            // A private type is not reachable from a sibling module.
            if kind.decl.contains("private") && !pos.prefix.is_empty() {
                continue;
            }
            let label = format!("{}/{}", kind.label, pos.label);
            if kind.commits && pos.commits {
                must_commit.push(label.clone());
            }
            programs.push((label, program(kind, pos)));
        }
    }
    // An optional extension of the type, later in the file, in another module
    // that opens the type's: the type keeps the path it was declared at.
    let extended = "namespace N\n\nmodule M =\n    type T = { X : int }\n    \
                    let p (x : T) = x\n\nmodule Ext =\n    open M\n    type T with\n        \
                    member _.Z = 1\n";
    programs.push(("extension-elsewhere".to_string(), extended.to_string()));
    // An error-level attribute on the module the type is declared in, used
    // inside it: F# accepts it, and the type declines.
    programs.push((
        "obsolete-error-module".to_string(),
        "namespace N\n\n[<System.Obsolete(\"no\", true)>]\nmodule M =\n    \
         type T = { X : int }\n    let f (x : T) = x\n"
            .to_string(),
    ));
    must_commit.push("extension-elsewhere".to_string());
    let g = grade(&programs, false);
    assert!(g.wrong.is_empty(), "wrong commits:\n{}", g.wrong.join("\n"));
    assert!(
        g.rejected.is_empty(),
        "every cell is valid F#; FCS rejected:\n{}",
        g.rejected.join("\n")
    );
    let missing: Vec<&String> = must_commit
        .iter()
        .filter(|label| {
            !g.committed
                .get(*label)
                .is_some_and(|c| c.iter().any(|b| b.starts_with("x: ") && !b.contains("'")))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "cells whose annotated parameter must commit did not:\n{missing:#?}"
    );
}

/// Project-typed binders where FCS rejects the program.
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
    // `NoEq` has a function field, so F# derives no equality for it.
    ("no-equality", "let e (x : NoEq) (y : NoEq) = x = y\n"),
];

#[test]
fn ill_typed_project_type_annotations_commit_only_what_fcs_kept() {
    let programs: Vec<(String, String)> = ILL_TYPED
        .iter()
        .map(|(label, body)| {
            (
                (*label).to_string(),
                format!(
                    "namespace N\n\ntype T = {{ X : int }}\ntype U = A | B\n\
                     type NoEq = {{ F : int -> int }}\n\nmodule Uses =\n{}",
                    indent(body, 4)
                ),
            )
        })
        .collect();
    let g = grade(&programs, true);
    assert!(g.wrong.is_empty(), "wrong commits:\n{}", g.wrong.join("\n"));
    assert!(
        g.rejected.is_empty(),
        "every ill-typed cell must be rejected by FCS:\n{}",
        g.rejected.join("\n")
    );
    // The family is not vacuous: binders FCS types positionally are committed.
    let committed: usize = g.committed.values().map(Vec::len).sum();
    assert!(committed >= ILL_TYPED.len(), "committed only {committed}");
}

/// Annotations whose *head* F# rejects: an argument violating a type
/// parameter's constraint, a measure parameter given a type, and a type (or
/// its module) carrying an error-level use attribute. FCS errors and recovers
/// the binder to a type variable, so committing the written type is a wrong
/// answer, not a commit on an erroring line.
const REJECTED_HEADS: &[(&str, &str, &str)] = &[
    (
        "constraint-violated",
        "type T<'a when 'a : struct> = { V : 'a }",
        "T<string>",
    ),
    // The same constraint, written after the type's name.
    (
        "constraint-after-name",
        "type T<'a> when 'a : struct = { V : 'a }",
        "T<string>",
    ),
    (
        "measure-given-a-type",
        "type T<[<Measure>] 'u> = { V : float<'u> }",
        "T<int>",
    ),
    (
        "obsolete-error",
        "[<System.Obsolete(\"no\", true)>]\ntype T = { X : int }",
        "T",
    ),
    (
        "compiler-message-error",
        "[<CompilerMessage(\"no\", 42, IsError = true)>]\ntype T = { X : int }",
        "T",
    ),
];

#[test]
fn a_rejected_annotation_head_commits_only_what_fcs_kept() {
    let programs: Vec<(String, String)> = REJECTED_HEADS
        .iter()
        .map(|(label, decl, spell)| {
            (
                (*label).to_string(),
                format!(
                    "namespace N\n\n{decl}\n\nmodule Uses =\n    let f (x : {spell}) = x\n    \
                     let v : {spell} = Unchecked.defaultof<_>\n"
                ),
            )
        })
        .collect();
    let g = grade(&programs, true);
    assert!(g.wrong.is_empty(), "wrong commits:\n{}", g.wrong.join("\n"));
    assert!(
        g.rejected.is_empty(),
        "every cell must be rejected by FCS:\n{}",
        g.rejected.join("\n")
    );
}
