//! Same-named types of different generic arity, against FCS, over every way
//! a use can reach them (#323).
//!
//! FCS keys a type name by name *and* arity. In type position the written
//! argument count selects the declaration (`CT<int>` binds `CT<'T>` even
//! beside a later `type CT`); a qualified expression (`CT.M`, `CT()`) looks
//! the name up among every same-named type, fewest type parameters first,
//! and binds the first that has the member; with no candidate at the written
//! arity it falls back to a namesake of another arity, with an error.
//!
//! The grid crosses:
//!
//! * the arities declared, in declaration order: every ordering of every
//!   non-empty subset of {0, 1, 2};
//! * the kind of type declared: an abbreviation, a class with static members,
//!   a union;
//! * the name: a fresh one, or `Tuple` under `open System`, whose
//!   `System.Tuple` (arity 0, with a `Create`) and `Tuple<…>` namesakes come
//!   from a referenced assembly;
//! * the route from the declarations to the use: all in the using module; in
//!   a nested module that is opened; in a nested module named by a
//!   qualifier; alternating between the using module and opened nested
//!   modules (in either phase); the last declaration in a nested module that
//!   holds the use;
//! * the use: bare, applied to one, two or a wildcard argument, postfix,
//!   inside `typeof`, a static member through the bare and the applied name,
//!   a static member or union case only one arity declares, a constructor
//!   call with and without type arguments, a union case in an expression and
//!   a pattern.
//!
//! Every commitment we make must be FCS's (certain-implies-exact). Where the
//! using module declares the only types of the name, the use's arity among
//! them, and FCS checks the program cleanly, a type-position use must commit,
//! so the grid cannot pass by declining.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_resolve_uses, full_bcl_env, invoke_fcs_dump_census, invoke_fcs_dump_project,
    parse_census_jsonl, parse_fcs_uses_project, temp_fs_file, temp_fs_tree,
};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::TextRange;
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    ProjectFile, ProjectItems, Resolution, ResolvedProject, SourceFile, SyntaxRecovery,
    qualified_names, resolve_file, resolve_project_files,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Abbrev,
    Class,
    Union,
}

const KINDS: [Kind; 3] = [Kind::Abbrev, Kind::Class, Kind::Union];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// Every declaration in the using module.
    Same,
    /// Every declaration in `module N`, opened before the use.
    Opened,
    /// Every declaration in `module N`; the use writes `N.CT`.
    Qualified,
    /// Declaration `i` in the using module when `i` is even, otherwise in its
    /// own nested module opened straight after it.
    Alternating,
    /// As [`Self::Alternating`], with the phase swapped.
    AlternatingSwapped,
    /// The last declaration in `module U`, which also holds the use; the
    /// others in the using module.
    InnerHoldsUse,
}

const ROUTES: [Route; 6] = [
    Route::Same,
    Route::Opened,
    Route::Qualified,
    Route::Alternating,
    Route::AlternatingSwapped,
    Route::InnerHoldsUse,
];

/// The name the types are declared under, and what the using module opens
/// first. `open System` brings `System.Tuple`, a static class of arity 0
/// with a `Create`, and `Tuple<'T1>` up to `Tuple<'T1, …, 'TRest>`.
struct Name {
    name: &'static str,
    prelude: &'static str,
}

const NAMES: [Name; 2] = [
    Name {
        name: "CT",
        prelude: "",
    },
    Name {
        name: "Tuple",
        prelude: "open System\n",
    },
];

/// One use of the name: the text before and after the name, the arity it writes
/// (`None` where the lookup has no arity: a qualified expression), and
/// whether it sits in type position.
struct Use {
    label: &'static str,
    before: &'static str,
    after: &'static str,
    arity: Option<usize>,
    type_position: bool,
}

const USES: &[Use] = &[
    Use {
        label: "bare",
        before: "let f (x: ",
        after: ") = ()",
        arity: Some(0),
        type_position: true,
    },
    Use {
        label: "one-arg",
        before: "let f (x: ",
        after: "<int>) = ()",
        arity: Some(1),
        type_position: true,
    },
    Use {
        label: "wildcard-arg",
        before: "let f (x: ",
        after: "<_>) = ()",
        arity: Some(1),
        type_position: true,
    },
    Use {
        label: "two-args",
        before: "let f (x: ",
        after: "<int, string>) = ()",
        arity: Some(2),
        type_position: true,
    },
    Use {
        label: "postfix",
        before: "let f (x: int ",
        after: ") = ()",
        arity: Some(1),
        type_position: true,
    },
    Use {
        label: "typeof",
        before: "let t = typeof<",
        after: "<int>>",
        arity: Some(1),
        type_position: true,
    },
    Use {
        label: "static-bare",
        before: "let v = ",
        after: ".Create 1",
        arity: None,
        type_position: false,
    },
    Use {
        label: "static-applied",
        before: "let v = ",
        after: "<int>.Create 1",
        arity: Some(1),
        type_position: false,
    },
    Use {
        label: "static-only-plain",
        before: "let v = ",
        after: ".M0 1",
        arity: None,
        type_position: false,
    },
    Use {
        label: "static-only-generic",
        before: "let v = ",
        after: ".M1 1",
        arity: None,
        type_position: false,
    },
    Use {
        label: "case-only-plain",
        before: "let v = ",
        after: ".C0",
        arity: None,
        type_position: false,
    },
    Use {
        label: "case-only-generic",
        before: "let v = ",
        after: ".C1",
        arity: None,
        type_position: false,
    },
    Use {
        label: "ctor-bare",
        before: "let v = ",
        after: "()",
        arity: None,
        type_position: false,
    },
    Use {
        label: "ctor-applied",
        before: "let v = ",
        after: "<int>()",
        arity: Some(1),
        type_position: false,
    },
    Use {
        label: "case-expr",
        before: "let v = ",
        after: ".K",
        arity: None,
        type_position: false,
    },
    Use {
        label: "case-pattern",
        before: "let g x =\n    match x with\n    | ",
        after: ".K -> 1\n    | _ -> 2",
        arity: None,
        type_position: false,
    },
];

fn type_params(arity: usize) -> &'static str {
    ["", "<'a>", "<'a, 'b>"][arity]
}

/// The declaration of `name` at `arity`, indented by `indent`.
fn declaration(name: &str, kind: Kind, arity: usize, indent: &str) -> String {
    let tp = type_params(arity);
    match kind {
        Kind::Abbrev => {
            let rhs = ["int -> unit", "int -> 'a", "'a -> 'b"][arity];
            format!("{indent}type {name}{tp} = {rhs}\n")
        }
        // `Create` is every arity's, `M{arity}` only this one's.
        Kind::Class => format!(
            "{indent}type {name}{tp}() =\n{indent}    static member Create (x: int) = x + {arity}\n\
             {indent}    static member M{arity} (x: int) = x\n"
        ),
        // `K` is every arity's, `C{arity}` only this one's.
        Kind::Union => {
            format!("{indent}type {name}{tp} =\n{indent}    | K\n{indent}    | C{arity}\n")
        }
    }
}

struct Program {
    label: String,
    src: String,
    /// The `CT` the use writes.
    use_range: TextRange,
    must_commit: bool,
}

fn render(name: &Name, kind: Kind, arities: &[usize], route: Route, u: &Use) -> Program {
    let Name { name, prelude } = *name;
    let mut src = format!("module M\n{prelude}\n");
    let mut use_indent = "";
    let mut qualifier = "";
    match route {
        Route::Same => {
            for &a in arities {
                src.push_str(&declaration(name, kind, a, ""));
            }
        }
        Route::Opened | Route::Qualified => {
            src.push_str("module N =\n");
            for &a in arities {
                src.push_str(&declaration(name, kind, a, "    "));
            }
            if route == Route::Opened {
                src.push_str("open N\n");
            } else {
                qualifier = "N.";
            }
        }
        Route::Alternating | Route::AlternatingSwapped => {
            let nested_parity = usize::from(route == Route::Alternating);
            for (i, &a) in arities.iter().enumerate() {
                if i % 2 == nested_parity {
                    writeln!(src, "module N{i} =").unwrap();
                    src.push_str(&declaration(name, kind, a, "    "));
                    writeln!(src, "open N{i}").unwrap();
                } else {
                    src.push_str(&declaration(name, kind, a, ""));
                }
            }
        }
        Route::InnerHoldsUse => {
            let (last, rest) = arities.split_last().expect("non-empty");
            for &a in rest {
                src.push_str(&declaration(name, kind, a, ""));
            }
            src.push_str("module U =\n");
            src.push_str(&declaration(name, kind, *last, "    "));
            use_indent = "    ";
        }
    }
    src.push('\n');
    // Indent every line of the use's text.
    let before = u.before.replace('\n', &format!("\n{use_indent}"));
    let after = u.after.replace('\n', &format!("\n{use_indent}"));
    write!(src, "{use_indent}{before}{qualifier}").unwrap();
    let start = src.len();
    src.push_str(name);
    let use_range = range_of(start, src.len());
    writeln!(src, "{after}").unwrap();
    let must_commit =
        route == Route::Same && u.type_position && u.arity.is_some_and(|a| arities.contains(&a));
    Program {
        label: format!("{name} {kind:?} {arities:?} {route:?} {}", u.label),
        src,
        use_range,
        must_commit,
    }
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Every ordering of every non-empty subset of {0, 1, 2}.
fn arity_orders() -> Vec<Vec<usize>> {
    fn extend(prefix: &[usize], out: &mut Vec<Vec<usize>>) {
        for a in 0..3 {
            if !prefix.contains(&a) {
                let mut next = prefix.to_vec();
                next.push(a);
                out.push(next.clone());
                extend(&next, out);
            }
        }
    }
    let mut out = Vec::new();
    extend(&[], &mut out);
    out
}

fn programs() -> Vec<Program> {
    let orders = arity_orders();
    assert_eq!(orders.len(), 15);
    let mut out = Vec::new();
    for name in &NAMES {
        for kind in KINDS {
            for arities in &orders {
                for route in ROUTES {
                    // The assembly's namesake is in scope wherever the use
                    // is; the routes are the in-file grid's.
                    if name.prelude.is_empty() || route == Route::Same {
                        for u in USES {
                            out.push(render(name, kind, arities, route, u));
                        }
                    }
                }
            }
        }
    }
    out
}

#[test]
fn same_named_types_of_every_arity_agree_with_fcs_over_every_route() {
    let programs = programs();
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|p| temp_fs_file("type_arity_route", &p.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let mut wrong = Vec::new();
    let mut undercommitted = Vec::new();
    let mut must_commit_points = 0usize;
    let mut clean_programs = 0usize;
    let mut committed_uses = 0usize;
    for (program, file) in programs.iter().zip(&census) {
        let src = &program.src;
        let clean = file.ok && !file.has_check_errors;
        clean_programs += usize::from(clean);
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{src}\n{:?}", parsed.errors);
        let recovery = SyntaxRecovery::of(&parsed);
        let impl_file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(
            &impl_file,
            &ProjectItems::default(),
            full_bcl_env(),
            &recovery,
        );
        let uses = census_resolve_uses(file, src);
        for (range, res) in rf.resolutions() {
            let range = *range;
            // FCS spans a dotted path from its first segment; the two share
            // an end.
            let at: Vec<_> = uses
                .iter()
                .filter(|u| u.end == usize::from(range.end()) && u.start <= range.start().into())
                .collect();
            let ours = match res {
                Resolution::Deferred(_) => continue,
                Resolution::Unresolved => {
                    wrong.push(format!("{}: Unresolved at {range:?}\n{src}", program.label));
                    continue;
                }
                Resolution::EarlierFileType { .. } => {
                    wrong.push(format!(
                        "{}: an earlier file's type at {range:?}\n{src}",
                        program.label
                    ));
                    continue;
                }
                Resolution::Local(_) | Resolution::Item(_) => {
                    rf.resolved_def(*res).map(|d| d.range)
                }
                Resolution::Entity(_)
                | Resolution::Member { .. }
                | Resolution::UnionCase { .. } => None,
            };
            // A definition's own record, which FCS reports as a definition.
            if at.is_empty() && (ours == Some(range) || !clean) {
                continue;
            }
            if at.iter().any(|u| u.is_from_definition) && ours == Some(range) {
                continue;
            }
            // FCS can record several symbols at one range (a type and its
            // constructor); ours must be one of them.
            let agrees = at.iter().any(|u| match (ours, u.decl) {
                (_, CensusDecl::Absent) => true,
                (Some(def), CensusDecl::InFile(s, e)) => def == range_of(s, e),
                (None, CensusDecl::OtherFile) => true,
                _ => false,
            });
            if !agrees {
                wrong.push(format!(
                    "{}: {:?} at {range:?}: we gave {res:?} ({ours:?}), FCS {:?}\n{src}",
                    program.label,
                    &src[range],
                    at.iter().map(|u| u.decl).collect::<Vec<_>>()
                ));
            } else if range == program.use_range {
                committed_uses += 1;
            }
        }
        if clean && program.must_commit {
            must_commit_points += 1;
            let committed = rf
                .resolution_at(program.use_range)
                .is_some_and(|r| matches!(r, Resolution::Local(_)));
            if !committed {
                undercommitted.push(format!("{}\n{src}", program.label));
            }
        }
    }
    eprintln!(
        "type-arity route grid: {} programs, {clean_programs} clean, \
         {must_commit_points} must-commit, {committed_uses} uses committed",
        programs.len()
    );
    assert!(
        wrong.is_empty(),
        "{} commitments FCS does not make:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert!(
        undercommitted.is_empty(),
        "{} must-commit uses were declined:\n{}",
        undercommitted.len(),
        undercommitted.join("\n")
    );
    // Every must-commit point is checked above; this pins how many FCS
    // accepts, so the obligation cannot quietly empty.
    assert!(
        must_commit_points >= 396,
        "the must-commit set shrank to {must_commit_points}"
    );
}

/// A route from an earlier file's declarations to a later file's use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossRoute {
    /// `module A` declares them; the using module opens `A`.
    Opened,
    /// `module A` declares them; the use writes `A.CT`.
    Qualified,
    /// `namespace P` declares them; the use sits in a module of a later
    /// `namespace P` block.
    Namespace,
    /// `module A` declares the first; the using module opens `A` and
    /// declares the rest.
    Split,
    /// The earlier file's `namespace P{i}` declares the first; the using
    /// file's `namespace P{i}` block declares the rest, then holds the use
    /// in a module (one namespace per using file, so the files' types do
    /// not collide).
    NamespaceSplit,
    /// As [`Self::Qualified`], with `A.fsi` declaring every type before
    /// `A.fs` does: a later file sees the signature's declarations.
    Signature,
    /// `module A` declares all but the first; the using module declares the
    /// first and writes `A.CT`, which weighs only `A`'s types.
    QualifiedOverLocal,
    /// `namespace NA` and then `namespace NB` each declare a `module Pal`,
    /// `NA`'s with the first type only and `NB`'s with all of them; the
    /// using module opens `NA` then `NB` and writes `Pal.CT`, which FCS
    /// reads through `NB`, the later open, alone.
    ShadowedModule,
}

const CROSS_ROUTES: [CrossRoute; 8] = [
    CrossRoute::Opened,
    CrossRoute::Qualified,
    CrossRoute::Namespace,
    CrossRoute::Split,
    CrossRoute::NamespaceSplit,
    CrossRoute::Signature,
    CrossRoute::QualifiedOverLocal,
    CrossRoute::ShadowedModule,
];

/// The signature of [`declaration`]'s type.
fn signature_declaration(kind: Kind, arity: usize) -> String {
    let tp = type_params(arity);
    match kind {
        Kind::Abbrev | Kind::Union => declaration("CT", kind, arity, ""),
        Kind::Class => format!(
            "type CT{tp} =\n    new : unit -> CT{tp}\n    static member Create : x: int -> int\n\
             \x20   static member M{arity} : x: int -> int\n"
        ),
    }
}

/// One project: the declaring files, then one using file per [`USES`]
/// entry, so FCS's per-file diagnostics say which uses it accepts.
struct CrossProject {
    label: String,
    files: Vec<(String, String)>,
    /// How many of `files` declare rather than use.
    declaring: usize,
}

fn render_cross(kind: Kind, arities: &[usize], route: CrossRoute) -> CrossProject {
    let indent_all = |text: &str, indent: &str| -> String {
        text.lines().map(|l| format!("{indent}{l}\n")).collect()
    };
    let (declaring, using_head, qualifier, indent, local) = match route {
        CrossRoute::Opened
        | CrossRoute::Qualified
        | CrossRoute::Split
        | CrossRoute::Signature
        | CrossRoute::QualifiedOverLocal => {
            let over_local = route == CrossRoute::QualifiedOverLocal && arities.len() > 1;
            let declared = if route == CrossRoute::Split {
                &arities[..1]
            } else if over_local {
                &arities[1..]
            } else {
                arities
            };
            let mut a = String::from("module A\n\n");
            for &ar in declared {
                a.push_str(&declaration("CT", kind, ar, ""));
            }
            let (head, qualifier) = match route {
                CrossRoute::Qualified | CrossRoute::Signature | CrossRoute::QualifiedOverLocal => {
                    ("", "A.")
                }
                _ => ("open A\n", ""),
            };
            let local: String = if route == CrossRoute::Split {
                arities[1..]
                    .iter()
                    .map(|&ar| declaration("CT", kind, ar, ""))
                    .collect()
            } else if over_local {
                declaration("CT", kind, arities[0], "")
            } else {
                String::new()
            };
            (a, head, qualifier, "", local)
        }
        CrossRoute::Namespace => {
            let mut a = String::from("namespace P\n\n");
            for &ar in arities {
                a.push_str(&declaration("CT", kind, ar, ""));
            }
            (a, "", "", "    ", String::new())
        }
        CrossRoute::ShadowedModule => {
            let module = |declared: &[usize]| -> String {
                declared
                    .iter()
                    .map(|&ar| declaration("CT", kind, ar, "    "))
                    .collect()
            };
            let a = format!(
                "namespace NA\n\nmodule Pal =\n{}\nnamespace NB\n\nmodule Pal =\n{}",
                module(&arities[..1]),
                module(arities)
            );
            (a, "open NA\nopen NB\n", "Pal.", "", String::new())
        }
        CrossRoute::NamespaceSplit => {
            let mut a = String::new();
            for i in 0..USES.len() {
                writeln!(a, "namespace P{i}\n").unwrap();
                a.push_str(&declaration("CT", kind, arities[0], ""));
            }
            let local: String = arities[1..]
                .iter()
                .map(|&ar| declaration("CT", kind, ar, ""))
                .collect();
            (a, "", "", "    ", local)
        }
    };
    let mut files = Vec::new();
    if route == CrossRoute::Signature {
        let mut sig = String::from("module A\n\n");
        for &ar in arities {
            sig.push_str(&signature_declaration(kind, ar));
        }
        files.push(("A.fsi".to_string(), sig));
    }
    files.push(("A.fs".to_string(), declaring));
    let declaring = files.len();
    for (i, u) in USES.iter().enumerate() {
        let body = format!("{}{qualifier}CT{}\n", u.before, u.after);
        let src = if route == CrossRoute::Namespace {
            format!(
                "namespace P\n\nmodule U{i} =\n{}",
                indent_all(&body, indent)
            )
        } else if route == CrossRoute::NamespaceSplit {
            format!(
                "namespace P{i}\n\n{local}module U{i} =\n{}",
                indent_all(&body, indent)
            )
        } else {
            format!("module U{i}\n{using_head}{local}\n{body}")
        };
        files.push((format!("U{i}.fs"), src));
    }
    CrossProject {
        label: format!("{kind:?} {arities:?} {route:?}"),
        files,
        declaring,
    }
}

/// Resolve `files` (Compile order, `.fsi` among them) as one project rooted
/// at `dir`.
fn resolve_cross(dir: &std::path::Path, files: &[(String, String)]) -> ResolvedProject {
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = files
        .iter()
        .map(|(name, src)| {
            if name.ends_with(".fsi") {
                let p = parse_sig(src);
                assert!(p.errors.is_empty(), "{name}\n{src}\n{:?}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Sig(SigFile::cast(p.root).expect("sig file")),
                    recovery,
                )
            } else {
                let p = parse(src);
                assert!(p.errors.is_empty(), "{name}\n{src}\n{:?}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
                    recovery,
                )
            }
        })
        .collect();
    let paths: Vec<PathBuf> = files.iter().map(|(n, _)| dir.join(n)).collect();
    let sources: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let qnofs = qualified_names(&sources, &paths);
    let project: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qnofs)
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    resolve_project_files(&project, full_bcl_env())
}

#[test]
fn same_named_types_of_every_arity_agree_with_fcs_across_files() {
    // Orderings of up to two arities; `BORZOI_TYPE_ARITY_CROSS_FULL=1` adds
    // the three-arity ones.
    let full = std::env::var_os("BORZOI_TYPE_ARITY_CROSS_FULL").is_some();
    let orders: Vec<Vec<usize>> = arity_orders()
        .into_iter()
        .filter(|o| full || o.len() <= 2)
        .collect();
    let mut wrong = Vec::new();
    let mut committed = 0usize;
    let mut clean_files = 0usize;
    let mut projects = 0usize;
    let mut must_commit_points = 0usize;
    let mut undercommitted = Vec::new();
    for kind in KINDS {
        for arities in &orders {
            for route in CROSS_ROUTES {
                let project = render_cross(kind, arities, route);
                projects += 1;
                let named: Vec<(&str, &str)> = project
                    .files
                    .iter()
                    .map(|(n, s)| (n.as_str(), s.as_str()))
                    .collect();
                let (root, written) = temp_fs_tree("type_arity_cross", &named);
                let paths: Vec<&std::path::Path> =
                    written.iter().map(|(p, _)| p.as_path()).collect();
                let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project(&paths), &written);
                let _ = std::fs::remove_dir_all(&root);
                let proj = resolve_cross(&root, &project.files);
                let first_use = project.declaring;
                let declaring_clean = written[..first_use].iter().all(|(p, _)| {
                    fcs.iter()
                        .find(|f| f.path.file_name() == p.file_name())
                        .is_some_and(|f| f.errors.is_empty())
                });
                for (idx, (path, src)) in written.iter().enumerate() {
                    let Some(fu) = fcs.iter().find(|f| f.path.file_name() == path.file_name())
                    else {
                        panic!("FCS reported nothing for {path:?}");
                    };
                    let clean = declaring_clean && fu.errors.is_empty();
                    clean_files += usize::from(clean && idx >= first_use);
                    let rf = proj.file(idx);
                    // The earlier file's only type of the name, at the use's
                    // arity, directly in the enclosing namespace: the use must
                    // commit to it.
                    if idx >= first_use
                        && clean
                        && route == CrossRoute::Namespace
                        && arities.len() == 1
                        && USES[idx - first_use].type_position
                        && USES[idx - first_use].arity == Some(arities[0])
                    {
                        must_commit_points += 1;
                        let at = src.rfind("CT").expect("the use");
                        if !matches!(
                            rf.resolution_at(range_of(at, at + 2)),
                            Some(Resolution::EarlierFileType { .. })
                        ) {
                            undercommitted.push(format!(
                                "{} {:?}\n{src}",
                                project.label,
                                path.file_name()
                            ));
                        }
                    }
                    // The earlier module's only union of the name, named by
                    // its module, its signature's or its own: a case qualified
                    // by it must commit, whatever the union's arity and
                    // whatever namesake the using module declares. (Through
                    // `open A`, `CT.K` commits at no arity, so `Opened` has no
                    // obligation.)
                    if idx >= first_use
                        && clean
                        && kind == Kind::Union
                        && (arities.len() == 1
                            && matches!(route, CrossRoute::Qualified | CrossRoute::Signature)
                            || arities.len() == 2 && route == CrossRoute::QualifiedOverLocal)
                        && matches!(USES[idx - first_use].label, "case-expr" | "case-pattern")
                    {
                        must_commit_points += 1;
                        let start = src.rfind("CT").expect("the use");
                        let start = if route != CrossRoute::Opened {
                            start - 2
                        } else {
                            start
                        };
                        let end = src[start..].find(".K").expect("the case") + start + 2;
                        if !matches!(
                            rf.resolution_at(range_of(start, end)),
                            Some(Resolution::Item(_))
                        ) {
                            undercommitted.push(format!(
                                "{} {:?}: {:?} gave {:?}\n{src}",
                                project.label,
                                path.file_name(),
                                &src[start..end],
                                rf.resolution_at(range_of(start, end))
                            ));
                        }
                    }
                    for (range, res) in rf.resolutions() {
                        let range = *range;
                        let ours: Option<(usize, TextRange)> = match res {
                            Resolution::Deferred(_) => continue,
                            Resolution::Unresolved => {
                                wrong.push(format!(
                                    "{} {:?}: Unresolved at {range:?}\n{src}",
                                    project.label,
                                    path.file_name()
                                ));
                                continue;
                            }
                            Resolution::Local(_) => rf.resolved_def(*res).map(|d| (idx, d.range)),
                            Resolution::Item(_) | Resolution::EarlierFileType { .. } => {
                                proj.item_def(*res).map(|(i, d)| (i, d.range))
                            }
                            Resolution::Entity(_)
                            | Resolution::Member { .. }
                            | Resolution::UnionCase { .. } => None,
                        };
                        let at: Vec<_> = fu
                            .uses
                            .iter()
                            .filter(|u| {
                                u.end == usize::from(range.end())
                                    && u.start <= usize::from(range.start())
                            })
                            .collect();
                        if at.is_empty() && (ours == Some((idx, range)) || !clean) {
                            continue;
                        }
                        if at.iter().any(|u| u.is_from_definition) && ours == Some((idx, range)) {
                            continue;
                        }
                        let agrees = at.iter().any(|u| match (&ours, &u.decl) {
                            (Some((file, def)), Some(d)) => {
                                written[*file].0.file_name() == d.file.file_name()
                                    && *def == range_of(d.start, d.end)
                            }
                            (None, None) => true,
                            _ => false,
                        });
                        if !agrees {
                            wrong.push(format!(
                                "{} {:?}: {:?} at {range:?}: we gave {res:?} ({ours:?}), FCS {:?}\n{src}",
                                project.label,
                                path.file_name(),
                                &src[range],
                                at.iter().map(|u| &u.decl).collect::<Vec<_>>()
                            ));
                        } else if idx >= first_use && &src[range] == "CT" {
                            committed += 1;
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "type-arity cross-file grid: {projects} projects, {clean_files} clean using files, \
         {must_commit_points} must-commit, {committed} `CT` uses committed"
    );
    assert!(
        wrong.is_empty(),
        "{} commitments FCS does not make:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert!(
        undercommitted.is_empty(),
        "{} must-commit uses were declined:\n{}",
        undercommitted.len(),
        undercommitted.join("\n")
    );
    assert!(
        must_commit_points >= 42,
        "the must-commit set shrank to {must_commit_points}"
    );
}
