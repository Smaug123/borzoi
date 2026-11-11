//! A type-qualified case reached through a same-file qualifier of one to
//! three segments, against FCS (#69, #70).
//!
//! `Alpha.Beta.Color.Red` resolves its head `Alpha` through the lexical
//! container chain and the source-ordered `open`s, then walks each further
//! segment through the container the previous one named, and finally reads
//! `Color.Red` there as a type's case. The qualifier may be rooted at a
//! lexical `module` or at a same-file `namespace`, and every segment of it
//! may be contended by another declaration of the same name.
//!
//! The grid crosses:
//!
//! * the qualifier's depth: one, two or three segments before `Color`;
//! * its root: nested modules under `module Top`; one `namespace` naming the
//!   whole qualifier; one `namespace` block per prefix of it; a namespace
//!   holding nested modules; and the `module rec` / `namespace rec` forms,
//!   where a declaration later than the use is in scope at it;
//! * a shadow: none, or a declaration named like the head, the type's
//!   container or the type itself — a value, a class, a module lacking the
//!   residual, a module owning a residual of its own, a union case, an
//!   exception, an active pattern, or an `open` of a module whose namesake
//!   owns (or lacks) the residual — placed before or after the declaration
//!   it shadows, between the declarations and the use, or beside the use;
//! * where the use sits: outside the qualifier's head, or inside it (where
//!   FCS does not see the head's own name);
//! * the use: an expression and a pattern.
//!
//! Every commitment we make, anywhere in each program, must be FCS's
//! (certain-implies-exact). Where nothing shadows the qualifier, the root is
//! not recursive, the use sits outside the head and FCS checks the program
//! cleanly, the case must commit, so the grid cannot pass by declining.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_resolve_uses, full_bcl_env, invoke_fcs_dump_census, parse_census_jsonl,
    temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::TextRange;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, Resolution, SyntaxRecovery, resolve_file};

/// The qualifier's segments, outermost first; a depth-`d` qualifier is the
/// first `d`.
const SEGS: [&str; 3] = ["Alpha", "Beta", "Gamma"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Root {
    /// `module Top` holding `module Alpha = module Beta = …`; the use sits in
    /// `module Use`, a sibling of `Alpha`.
    Module,
    /// As [`Self::Module`], under `module rec Top`.
    ModuleRec,
    /// As [`Self::Module`] in a headerless file, whose anonymous root module
    /// holds `Alpha` and `Use`.
    Headerless,
    /// `namespace Alpha.Beta` holding `Color`; the use sits in a later
    /// `namespace Client` block.
    Namespace,
    /// One `namespace` block per prefix of the qualifier (`namespace Alpha`,
    /// then `namespace Alpha.Beta`), the last holding `Color`.
    NamespaceBlocks,
    /// `namespace Alpha` holding `module Beta = …`.
    NamespaceMixed,
    /// As [`Self::Namespace`], with the use in a `namespace rec Client`.
    NamespaceRec,
}

const ROOTS: [Root; 7] = [
    Root::Module,
    Root::ModuleRec,
    Root::Headerless,
    Root::Namespace,
    Root::NamespaceBlocks,
    Root::NamespaceMixed,
    Root::NamespaceRec,
];

impl Root {
    fn is_rec(self) -> bool {
        matches!(self, Root::ModuleRec | Root::NamespaceRec)
    }

    /// Whether qualifier segment `i` is a namespace (otherwise a module).
    fn seg_is_namespace(self, i: usize) -> bool {
        match self {
            Root::Module | Root::ModuleRec | Root::Headerless => false,
            Root::Namespace | Root::NamespaceBlocks | Root::NamespaceRec => true,
            Root::NamespaceMixed => i == 0,
        }
    }
}

/// Which declaration a shadow is named after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The qualifier's first segment.
    Head,
    /// The qualifier's last segment, `Color`'s container (depth ≥ 2).
    Mid,
    /// `Color`.
    Type,
}

/// Where a shadow is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pos {
    /// In the container declaring the target, before the target.
    Before,
    /// In the container declaring the target, after the target (for the
    /// head: between the declarations and the using block).
    After,
    /// At the using block's top level, before `module Use` (head only).
    UseBlock,
    /// In `module Use`, before the use (head only).
    UseBefore,
    /// In `module Use`, after the use (head only; in scope under `rec`).
    UseAfter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shadow {
    Value,
    Class,
    /// A module of the name that lacks the residual.
    Module,
    /// A module of the name owning a residual of its own.
    ModuleOwning,
    UnionCase,
    Exception,
    ActivePattern,
    /// `open Opened`, where `Opened`'s namesake owns a residual of its own.
    Open,
    /// `open Opened`, where `Opened`'s namesake lacks the residual.
    OpenEmpty,
    /// An `[<AutoOpen>]` module whose namesake owns a residual of its own.
    AutoOpen,
    /// A DllImport `extern` function.
    Extern,
    /// A case of a `[<RequireQualifiedAccess>]` union.
    QualifiedCase,
}

const SHADOWS: [Shadow; 12] = [
    Shadow::Value,
    Shadow::Class,
    Shadow::Module,
    Shadow::ModuleOwning,
    Shadow::UnionCase,
    Shadow::Exception,
    Shadow::ActivePattern,
    Shadow::Open,
    Shadow::OpenEmpty,
    Shadow::AutoOpen,
    Shadow::Extern,
    Shadow::QualifiedCase,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    /// Outside the head (a sibling module, or another namespace).
    Outside,
    /// Inside the head (a module nested in it, or a namespace under it).
    InsideHead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UseKind {
    Expr,
    Pattern,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    root: Root,
    depth: usize,
    shadow: Option<(Shadow, Target, Pos)>,
    site: Site,
    use_kind: UseKind,
}

/// Indent every line of `text` by `indent`.
fn indented(text: &str, indent: &str) -> String {
    text.lines().map(|l| format!("{indent}{l}\n")).collect()
}

/// `names` as nested modules, the innermost holding `type Color = Red | Blue`
/// (`names` empty: the type itself).
fn module_chain(names: &[&str]) -> String {
    match names.split_first() {
        None => "type Color = Red | Blue\n".to_string(),
        Some((first, rest)) => {
            format!(
                "module {first} =\n{}",
                indented(&module_chain(rest), "    ")
            )
        }
    }
}

/// The shadow's declaration text (unindented), named `name`; `rest` is the
/// residual's module segments after the target (for an owning variant).
fn shadow_text(shadow: Shadow, target: Target, name: &str, rest: &[&str]) -> String {
    // An owning variant of a `Color`-named module owns `Red` as a value.
    let owning_body = || {
        if target == Target::Type {
            "let Red = 0\n".to_string()
        } else {
            module_chain(rest)
        }
    };
    match shadow {
        Shadow::Value => format!("let {name} = 0\n"),
        Shadow::Class => format!("type {name}() =\n    static member Z = 0\n"),
        Shadow::Module => format!("module {name} =\n    let z = 0\n"),
        Shadow::ModuleOwning => format!("module {name} =\n{}", indented(&owning_body(), "    ")),
        Shadow::UnionCase => format!("type {name}Union =\n    | {name}\n    | {name}Other\n"),
        Shadow::Exception => format!("exception {name}\n"),
        Shadow::ActivePattern => {
            format!("let (|{name}|_|) (x: int) = if x = 0 then Some () else None\n")
        }
        Shadow::Open => format!(
            "module Opened =\n{}open Opened\n",
            indented(
                &format!("module {name} =\n{}", indented(&owning_body(), "    ")),
                "    "
            )
        ),
        Shadow::OpenEmpty => {
            format!("module Opened =\n    module {name} =\n        let z = 0\nopen Opened\n")
        }
        Shadow::Extern => {
            format!("[<System.Runtime.InteropServices.DllImport(\"x\")>]\nextern int {name}()\n")
        }
        Shadow::QualifiedCase => format!(
            "[<RequireQualifiedAccess>]\ntype {name}Union =\n    | {name}\n    | {name}Other\n"
        ),
        Shadow::AutoOpen => format!(
            "[<AutoOpen>]\nmodule Helper =\n{}",
            indented(
                &format!("module {name} =\n{}", indented(&owning_body(), "    ")),
                "    "
            )
        ),
    }
}

struct Program {
    label: String,
    src: String,
    /// The whole written path `Alpha.….Color.Red`.
    path_range: TextRange,
    must_commit: bool,
}

/// The text a shadow contributes at `slot`, if the cell places one there.
fn slot(cell: &Cell, target: Target, pos: Pos) -> String {
    match cell.shadow {
        Some((shadow, t, p)) if t == target && p == pos => {
            let segs = &SEGS[..cell.depth];
            let (name, rest): (&str, &[&str]) = match target {
                Target::Head => (segs[0], &segs[1..]),
                Target::Mid => (segs[cell.depth - 1], &[]),
                Target::Type => ("Color", &[]),
            };
            shadow_text(shadow, target, name, rest)
        }
        _ => String::new(),
    }
}

/// The text of the container declaring qualifier segment `i` onwards when
/// segment `i` is a module: `module SEGS[i] = …`, shadows of the segments it
/// declares placed around them.
fn module_body_from(cell: &Cell, i: usize) -> String {
    let d = cell.depth;
    // What `SEGS[i]`'s body holds: the next segment's module, or `Color`.
    let mut body = String::new();
    if i + 1 < d {
        let target_next = (i + 1 == d - 1).then_some(Target::Mid);
        if let Some(t) = target_next {
            body.push_str(&slot(cell, t, Pos::Before));
        }
        body.push_str(&module_body_from(cell, i + 1));
        if let Some(t) = target_next {
            body.push_str(&slot(cell, t, Pos::After));
        }
    } else {
        body.push_str(&slot(cell, Target::Type, Pos::Before));
        body.push_str("type Color = Red | Blue\n");
        body.push_str(&slot(cell, Target::Type, Pos::After));
    }
    format!("module {} =\n{}", SEGS[i], indented(&body, "    "))
}

fn use_text(cell: &Cell, path: &str) -> String {
    match cell.use_kind {
        UseKind::Expr => format!("let x = {path}.Color.Red\n"),
        UseKind::Pattern => {
            format!("let f v =\n    match v with\n    | {path}.Color.Red -> 1\n    | _ -> 0\n")
        }
    }
}

fn render(cell: &Cell) -> Program {
    let d = cell.depth;
    let segs = &SEGS[..d];
    let path = segs.join(".");
    let rec = if cell.root.is_rec() { "rec " } else { "" };
    let use_module = format!(
        "module Use =\n{}",
        indented(
            &format!(
                "{}{}{}",
                slot(cell, Target::Head, Pos::UseBefore),
                use_text(cell, &path),
                slot(cell, Target::Head, Pos::UseAfter)
            ),
            "    "
        )
    );
    let mut src = String::new();
    match cell.root {
        Root::Module | Root::ModuleRec | Root::Headerless => {
            if cell.root != Root::Headerless {
                writeln!(src, "module {rec}Top\n").unwrap();
            }
            src.push_str(&slot(cell, Target::Head, Pos::Before));
            let mut head = module_body_from(cell, 0);
            if cell.site == Site::InsideHead {
                head.push_str(&indented(&use_module, "    "));
            }
            src.push_str(&head);
            src.push_str(&slot(cell, Target::Head, Pos::After));
            src.push_str(&slot(cell, Target::Head, Pos::UseBlock));
            if cell.site == Site::Outside {
                src.push_str(&use_module);
            }
        }
        Root::Namespace | Root::NamespaceBlocks | Root::NamespaceRec | Root::NamespaceMixed => {
            // The namespace segments, and the module segments under them.
            let ns_len = (0..d)
                .take_while(|&i| cell.root.seg_is_namespace(i))
                .count();
            let head_before = slot(cell, Target::Head, Pos::Before);
            if !head_before.is_empty() {
                writeln!(src, "namespace global\n\n{head_before}").unwrap();
            }
            // A shadow of a namespace segment `Mid` sits in a block of its
            // parent namespace, before or after the declaring blocks.
            let mid_is_ns = d >= 2 && cell.root.seg_is_namespace(d - 1);
            let parent_ns = segs[..d - 1].join(".");
            if mid_is_ns {
                let before = slot(cell, Target::Mid, Pos::Before);
                if !before.is_empty() {
                    writeln!(src, "namespace {parent_ns}\n\n{before}").unwrap();
                }
            }
            if cell.root == Root::NamespaceBlocks {
                for k in 1..ns_len {
                    let prefix = segs[..k].join(".");
                    writeln!(src, "namespace {prefix}\n\ntype Filler{k} = Filler{k}\n").unwrap();
                }
            }
            let ns_path = segs[..ns_len].join(".");
            writeln!(src, "namespace {ns_path}\n").unwrap();
            if ns_len == d {
                src.push_str(&slot(cell, Target::Type, Pos::Before));
                src.push_str("type Color = Red | Blue\n");
                src.push_str(&slot(cell, Target::Type, Pos::After));
            } else {
                if ns_len == d - 1 {
                    src.push_str(&slot(cell, Target::Mid, Pos::Before));
                }
                src.push_str(&module_body_from(cell, ns_len));
                if ns_len == d - 1 {
                    src.push_str(&slot(cell, Target::Mid, Pos::After));
                }
            }
            src.push('\n');
            if mid_is_ns {
                let after = slot(cell, Target::Mid, Pos::After);
                if !after.is_empty() {
                    writeln!(src, "namespace {parent_ns}\n\n{after}").unwrap();
                }
            }
            let head_after = slot(cell, Target::Head, Pos::After);
            if !head_after.is_empty() {
                writeln!(src, "namespace global\n\n{head_after}").unwrap();
            }
            let client = match cell.site {
                Site::Outside => "Client".to_string(),
                Site::InsideHead => format!("{}.Delta", segs[0]),
            };
            let rec_ns = if cell.root == Root::NamespaceRec {
                "rec "
            } else {
                ""
            };
            writeln!(src, "namespace {rec_ns}{client}\n").unwrap();
            src.push_str(&slot(cell, Target::Head, Pos::UseBlock));
            src.push_str(&use_module);
        }
    }
    let written = format!("{path}.Color.Red");
    let start = src
        .rfind(&written)
        .unwrap_or_else(|| panic!("{written} in\n{src}"));
    let path_range = range_of(start, start + written.len());
    let must_commit = !cell.root.is_rec() && cell.shadow.is_none() && cell.site == Site::Outside;
    let shadow = match cell.shadow {
        None => "none".to_string(),
        Some((s, t, p)) => format!("{s:?}@{t:?}/{p:?}"),
    };
    Program {
        label: format!(
            "{:?} depth={} {shadow} {:?} {:?}",
            cell.root, d, cell.site, cell.use_kind
        ),
        src,
        path_range,
        must_commit,
    }
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Whether the slot a shadow of `target` at `pos` occupies is directly in a
/// namespace, where a `let` is FS0201.
fn slot_in_namespace(root: Root, depth: usize, target: Target, pos: Pos) -> bool {
    let namespaced = !matches!(root, Root::Module | Root::ModuleRec | Root::Headerless);
    match target {
        Target::Head => namespaced && matches!(pos, Pos::Before | Pos::After | Pos::UseBlock),
        Target::Mid => depth >= 2 && root.seg_is_namespace(depth - 2),
        Target::Type => root.seg_is_namespace(depth - 1),
    }
}

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for root in ROOTS {
        for depth in 1..=3 {
            for use_kind in [UseKind::Expr, UseKind::Pattern] {
                for site in [Site::Outside, Site::InsideHead] {
                    out.push(Cell {
                        root,
                        depth,
                        shadow: None,
                        site,
                        use_kind,
                    });
                }
                let mut placements = vec![
                    (Target::Head, Pos::Before),
                    (Target::Head, Pos::After),
                    (Target::Head, Pos::UseBlock),
                    (Target::Head, Pos::UseBefore),
                    (Target::Type, Pos::Before),
                    (Target::Type, Pos::After),
                ];
                if root.is_rec() {
                    placements.push((Target::Head, Pos::UseAfter));
                }
                if depth >= 2 {
                    placements.push((Target::Mid, Pos::Before));
                    placements.push((Target::Mid, Pos::After));
                }
                for shadow in SHADOWS {
                    for &(target, pos) in &placements {
                        // A `let` directly in a namespace is FS0201, which FCS
                        // recovers from by dropping the value; sema keeps it
                        // (a separate defect), so those programs grade that
                        // recovery rather than the qualifier.
                        if matches!(
                            shadow,
                            Shadow::Value | Shadow::ActivePattern | Shadow::Extern
                        ) && slot_in_namespace(root, depth, target, pos)
                        {
                            continue;
                        }
                        out.push(Cell {
                            root,
                            depth,
                            shadow: Some((shadow, target, pos)),
                            site: Site::Outside,
                            use_kind,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Shapes outside the grid's dimensions, each guarding one rule of the walk:
/// `(label, written path, source)`, the source holding `USE` where the use
/// goes (rendered as an expression and as a pattern).
const SHAPES: &[(&str, &str, &str)] = &[
    // A `private` module segment is inaccessible from outside its container,
    // so FCS skips the nearer `Alpha` for the outer one.
    (
        "private-segment",
        "Alpha.Beta.Color.Red",
        "module Top\n\nmodule Alpha =\n    module Beta =\n        type Color = Red | Blue\n\
         module Mid =\n    module Alpha =\n        module private Beta =\n            \
         type Color = Red | Blue\n    module Use =\n        USE\n",
    ),
    // The use's own block extends the head namespace: its declarations join
    // the namespace only once the block closes.
    (
        "head-namespace-block",
        "Alpha.Beta.Color.Red",
        "namespace Alpha\n\ntype Filler = Filler\n\nnamespace Alpha\n\nmodule Beta =\n    \
         type Color = Red | Blue\nmodule Use =\n    USE\n",
    ),
    // The namespace is declared only by the use's own block.
    (
        "own-block-namespace",
        "Alpha.Beta.Color.Red",
        "namespace Alpha.Beta\n\ntype Color = Red | Blue\nmodule Use =\n    USE\n",
    ),
    // An `open` in a `namespace global` block, after a root module.
    (
        "global-block-open",
        "Alpha.Color.Red",
        "namespace global\n\nmodule Alpha =\n    type Color = Red | Blue\nmodule Opened =\n    \
         module Alpha =\n        type Color = Red | Blue\nopen Opened\n\nnamespace Client\n\n\
         module Use =\n    USE\n",
    ),
    // A type declared twice in the qualifier's last container (FS0037).
    (
        "redeclared-type",
        "Alpha.Beta.Color.Red",
        "module Top\n\nmodule Alpha =\n    module Beta =\n        type Color = Red | Blue\n        \
         type Color = Red | Blue\nmodule Use =\n    USE\n",
    ),
    (
        "redeclared-type-shallow",
        "Alpha.Color.Red",
        "module Top\n\nmodule Alpha =\n    type Color = Red | Blue\n    type Color = Red | Blue\n\
         module Use =\n    USE\n",
    ),
    // A raw `global` head is the root marker, not an escaped ``global`` module.
    (
        "global-marker",
        "global.System.String.Empty",
        "module Top\n\nmodule ``global`` =\n    module System =\n        \
         type String = Empty | Other\nmodule Use =\n    USE\n",
    ),
    // A pattern tries the head's types first: an abbreviation of a BCL type
    // that nests the residual takes it from the namespace or module.
    (
        "abbreviation-head-namespace",
        "Environment.SpecialFolder.Desktop",
        "namespace Environment\n\ntype SpecialFolder = Desktop | Other\n\nnamespace Client\n\n\
         type Environment = System.Environment\nmodule Use =\n    USE\n",
    ),
    (
        "abbreviation-head-module",
        "Environment.SpecialFolder.Desktop",
        "module Top\n\nmodule Environment =\n    type SpecialFolder = Desktop | Other\n\
         type Environment = System.Environment\nmodule Use =\n    USE\n",
    ),
    // An `open` supplying an assembly module of the head's name whose *value*
    // owns the next segment (FSharp.Core's `List.empty`).
    (
        "assembly-module-value",
        "List.empty.Length.CompareTo(1)",
        "module Top\n\nmodule List =\n    module empty =\n        type Length() =\n            \
         static member CompareTo(x: int) = x\nopen Microsoft.FSharp.Collections\nmodule Use =\n    USE\n",
    ),
    // Entering an assembly's namespace opens its `[<AutoOpen>]` modules, whose
    // nested `Unchecked` outranks a same-file root namespace.
    (
        "assembly-auto-open-rival",
        "Unchecked.hash.ToString()",
        "namespace Unchecked\n\ntype hash() =\n    static member ToString() = \"x\"\n\n\
         namespace Microsoft.FSharp.Core\n\nmodule Use =\n    USE\n",
    ),
    // A module segment declared twice in one container (FS0037).
    (
        "redeclared-segment",
        "Alpha.Beta.Color.Red",
        "module Top\n\nmodule Alpha =\n    module Beta =\n        type Color = Red | Blue\n    \
         module Beta =\n        type Color = Red | Blue\nmodule Use =\n    USE\n",
    ),
];

/// A [`SHAPES`] entry as an expression and as a pattern program.
fn shape(label: &str, written: &str, src: &str) -> Vec<Program> {
    [UseKind::Expr, UseKind::Pattern]
        .into_iter()
        .map(|use_kind| {
            let at = src.find("USE").expect("a USE slot");
            let indent = &src[src[..at].rfind('\n').map_or(0, |i| i + 1)..at];
            let use_src = match use_kind {
                UseKind::Expr => format!("let x = {written}"),
                UseKind::Pattern => format!(
                    "let f v =\n{indent}    match v with\n{indent}    | {written} -> 1\n\
                     {indent}    | _ -> 0"
                ),
            };
            let src = src.replace("USE", &use_src);
            let start = src.rfind(written).expect("the use");
            Program {
                label: format!("{label} {use_kind:?}"),
                path_range: range_of(start, start + written.len()),
                src,
                must_commit: false,
            }
        })
        .collect()
}

/// How one program's use of the case came out, for the census printed
/// beside the verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    /// FCS binds an in-file case there and so do we.
    Committed,
    /// FCS binds an in-file case there; we decline.
    Declined,
    /// FCS binds nothing in-file there (or the program does not check).
    FcsElsewhere,
}

#[test]
fn a_same_file_qualifier_of_any_depth_agrees_with_fcs() {
    let mut programs: Vec<Program> = cells().iter().map(render).collect();
    programs.extend(
        SHAPES
            .iter()
            .flat_map(|&(label, written, src)| shape(label, written, src)),
    );
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|p| temp_fs_file("type_qualified_prefix", &p.src))
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
    let mut outcomes: BTreeMap<(String, Outcome), usize> = BTreeMap::new();
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
            // FCS can record several symbols at one range; ours must be one
            // of them.
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
            }
        }
        // The use: does FCS bind an in-file case there, and do we?
        let fcs_case = uses
            .iter()
            .filter(|u| {
                u.end == usize::from(program.path_range.end())
                    && u.start >= usize::from(program.path_range.start())
                    && !u.is_from_definition
            })
            .find_map(|u| match u.decl {
                CensusDecl::InFile(s, e) if &src[s..e] == "Red" => Some(range_of(s, e)),
                _ => None,
            });
        let ours = rf
            .resolution_at(program.path_range)
            .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
            .and_then(|r| rf.resolved_def(r))
            .map(|d| d.range);
        let outcome = match (clean, fcs_case, ours) {
            (true, Some(f), Some(o)) if f == o => Outcome::Committed,
            (true, Some(_), _) => Outcome::Declined,
            _ => Outcome::FcsElsewhere,
        };
        let row = program
            .label
            .split(' ')
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        *outcomes.entry((row, outcome)).or_default() += 1;
        if clean && program.must_commit {
            must_commit_points += 1;
            if outcome != Outcome::Committed {
                undercommitted.push(format!(
                    "{} (FCS {fcs_case:?}, ours {ours:?})\n{src}",
                    program.label
                ));
            }
        }
    }
    let mut census_text = String::new();
    for ((row, outcome), n) in &outcomes {
        writeln!(census_text, "  {row}: {outcome:?} {n}").unwrap();
    }
    eprintln!(
        "type-qualified prefix grid: {} programs, {clean_programs} clean, \
         {must_commit_points} must-commit\n{census_text}",
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
        must_commit_points >= 30,
        "the must-commit set shrank to {must_commit_points}"
    );
}

/// Projects in which an earlier file bears on a same-file root head:
/// `(label, earlier file, qualifier)`. The later file declares the head as
/// `namespace Alpha`, or as a root `module Alpha` in a `namespace global`
/// block (holding `module Beta` when the qualifier has two segments), and
/// uses the qualifier from `namespace Client`, which FCS enters after the
/// root head was added — so what entering `Client` opens is tried first.
const CROSS_FILE: &[(&str, &str, &str)] = &[
    // The control: an earlier file that bears on nothing, where the use
    // must commit.
    ("unrelated", "namespace Other\n\ntype T = T\n", "Alpha.Beta"),
    (
        "rival-module-owning",
        "namespace Client\n\nmodule Alpha =\n    type Color = Red | Blue\n",
        "Alpha",
    ),
    (
        "rival-module-empty",
        "namespace Client\n\nmodule Alpha =\n    let z = 0\n",
        "Alpha",
    ),
    (
        "rival-module-owning-deep",
        "namespace Client\n\nmodule Alpha =\n    module Beta =\n        type Color = Red | Blue\n",
        "Alpha.Beta",
    ),
    (
        "rival-namespace",
        "namespace Client.Alpha\n\ntype Color = Red | Blue\n",
        "Alpha",
    ),
    (
        "rival-auto-open",
        "namespace Client\n\n[<AutoOpen>]\nmodule Helper =\n    module Alpha =\n        \
         type Color = Red | Blue\n",
        "Alpha",
    ),
    (
        "namespace-shared",
        "namespace Alpha\n\ntype Other = Other\n",
        "Alpha",
    ),
];

/// Every commitment at the use agrees with FCS's project check, where an
/// earlier file declares something a same-file root head may lose to.
#[test]
fn a_root_head_beside_an_earlier_file_agrees_with_fcs() {
    use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project};
    use borzoi_sema::resolve_project;
    use std::path::Path;

    let mut wrong = Vec::new();
    let mut control_commits = 0;
    for &(label, earlier, qualifier) in CROSS_FILE {
        for (root, use_kind) in [false, true]
            .into_iter()
            .flat_map(|g| [(g, UseKind::Expr), (g, UseKind::Pattern)])
        {
            let decls = if qualifier == "Alpha" {
                "type Color = Red | Blue\n".to_string()
            } else {
                "module Beta =\n    type Color = Red | Blue\n".to_string()
            };
            let use_src = match use_kind {
                UseKind::Expr => format!("    let x = {qualifier}.Color.Red\n"),
                UseKind::Pattern => format!(
                    "    let f v =\n        match v with\n        | {qualifier}.Color.Red -> 1\n        | _ -> 0\n"
                ),
            };
            let head = if root {
                format!(
                    "namespace global\n\nmodule Alpha =\n{}",
                    indented(&decls, "    ")
                )
            } else {
                format!("namespace Alpha\n\n{decls}")
            };
            let later = format!("{head}\nnamespace Client\n\nmodule Use =\n{use_src}");
            let written: Vec<(PathBuf, String)> = [earlier, later.as_str()]
                .iter()
                .enumerate()
                .map(|(i, src)| {
                    (
                        temp_fs_file(&format!("ns_head_{label}_{i}"), src),
                        (*src).to_string(),
                    )
                })
                .collect();
            let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
            let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project(&paths), &written);
            let asts: Vec<ImplFile> = written
                .iter()
                .map(|(_, s)| ImplFile::cast(parse(s).root).expect("impl file"))
                .collect();
            let proj = resolve_project(&asts, full_bcl_env());
            for (p, _) in &written {
                let _ = std::fs::remove_file(p);
            }
            let path = format!("{qualifier}.Color.Red");
            let start = later.rfind(&path).expect("the use");
            let range = range_of(start, start + path.len());
            let Some(ours) = proj.file(1).resolution_at(range) else {
                continue;
            };
            let ours = match ours {
                Resolution::Item(_) => proj.item_def(ours).map(|(file, d)| (file, d.range)),
                Resolution::Local(_) => proj.file(1).resolved_def(ours).map(|d| (1, d.range)),
                _ => None,
            };
            let Some((our_file, our_range)) = ours else {
                continue;
            };
            control_commits += usize::from(label == "unrelated");
            let root = if root { "root module" } else { "namespace" };
            let later_fcs = fcs
                .iter()
                .find(|f| f.path.file_name() == written[1].0.file_name())
                .expect("FCS checked the later file");
            let agrees = later_fcs.uses.iter().any(|u| {
                u.end == usize::from(range.end())
                    && !u.is_from_definition
                    && u.decl.as_ref().is_some_and(|d| {
                        d.file.file_name() == written[our_file].0.file_name()
                            && range_of(d.start, d.end) == our_range
                    })
            });
            if !agrees {
                wrong.push(format!(
                    "{label} {root} {use_kind:?}: we gave file {our_file} {our_range:?}; FCS errors {:?}\n{earlier}\n{later}",
                    later_fcs.errors
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} commitments FCS does not make:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert_eq!(control_commits, 4, "the control's uses must commit");
}
