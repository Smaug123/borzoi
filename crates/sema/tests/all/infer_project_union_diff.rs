//! The project's own union cases — as values (`A`, `B 1`, `U.B`) and in
//! `match` patterns (`| B n ->`, `| U.A ->`) — graded against FCS checking
//! the same files as one project.
//!
//! A case's type comes from its declaration: each field's written type, read
//! in the union's own type-parameter scope, and the union applied to its
//! parameters. The grid crosses union declarations (plain, generic,
//! recursive, named fields, `[<RequireQualifiedAccess>]`, struct, a field of
//! tuple type, a measured field, an error-obsolete case, a constrained
//! parameter, …) with where the union is declared (the using file, an earlier
//! file reached by an `open`, an earlier file reached by module-qualified
//! paths, an earlier file with a signature) and with the positions a case
//! sits in. Every node and binder inference types in any file must be FCS's;
//! each modelled cell FCS checks cleanly must commit, so the grid cannot pass
//! by declining. The ill-typed family puts cases where FCS rejects them.

use std::path::{Path, PathBuf};

use crate::common::{GradedDumps, full_bcl_env, invoke_fcs_dump_types_project, temp_fs_tree};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    AssemblyEnv, InferredProject, ProjectFile, ResolvedProject, SourceFile, SyntaxRecovery,
    infer_project, qualified_names, resolve_project_files,
};

/// A union declaration and what uses it.
///
/// In every template `{L}` is the qualifier a placement puts before a name
/// the union's file declares (empty, or `Lib.`), and `{x}` is an `int`
/// parameter in scope.
struct Union {
    id: &'static str,
    /// The declaration(s), at the top level of their file.
    decl: &'static str,
    /// A ground instance of the union, as an annotation.
    ty: &'static str,
    /// `(expression, modelled, generic result)`: a case as a value. A generic
    /// result keeps a variable of the union's (`GA : 'a G`), which FCS
    /// generalises in a value binding and inference does not.
    values: &'static [(&'static str, bool, bool)],
    /// `(clauses, modelled)`: a `match` over a parameter of type `ty`.
    matches: &'static [(&'static str, bool)],
}

const fn u(
    id: &'static str,
    decl: &'static str,
    ty: &'static str,
    values: &'static [(&'static str, bool, bool)],
    matches: &'static [(&'static str, bool)],
) -> Union {
    Union {
        id,
        decl,
        ty,
        values,
        matches,
    }
}

const UNIONS: &[Union] = &[
    u(
        "plain",
        "type U = A | B of int | C of string * int\n",
        "{L}U",
        &[
            ("{L}A", true, false),
            ("{L}B 1", true, false),
            ("{L}B {x}", true, false),
            ("{L}B (id 1)", true, false),
            ("{L}C (\"s\", 1)", true, false),
            ("{L}C (\"s\", {x})", true, false),
            ("{L}B", true, false),
            ("{L}C", true, false),
            ("{L}U.A", true, false),
            ("{L}U.B 2", true, false),
        ],
        &[
            ("| {L}A -> 0 | {L}B n -> n | {L}C (_, n) -> n", true),
            ("| {L}B n -> n | _ -> 0", true),
            ("| {L}A -> 0 | {L}B _ -> 1 | {L}C _ -> 2", true),
            ("| {L}B 1 -> 1 | {L}B n -> n | _ -> 0", true),
            ("| {L}C (s, _) -> s | _ -> \"\"", true),
            ("| {L}U.A -> 0 | {L}U.B n -> n | {L}U.C (_, n) -> n", true),
            ("| {L}A -> 0 | {L}U.B n -> n | _ -> 1", true),
            // Unreachable: `B _` after `B n`.
            ("| {L}B n -> n | {L}B _ -> 0 | _ -> 1", false),
            // Every case covered, then a wildcard FCS keeps no node of.
            ("| {L}A -> 0 | {L}B _ -> 1 | {L}C _ -> 2 | _ -> 3", false),
            // An OR pattern.
            ("| {L}A | {L}B _ -> 0 | {L}C _ -> 1", false),
        ],
    ),
    u(
        "generic",
        "type G<'a> = GA | GB of 'a | GC of 'a * int\n",
        "{L}G<int>",
        &[
            ("{L}GA", true, true),
            ("{L}GB 1", true, false),
            ("{L}GB \"s\"", true, false),
            ("{L}GB {x}", true, false),
            ("{L}GC (1, 2)", true, false),
            ("{L}GB", true, true),
            ("{L}GB (Some 1)", true, false),
            ("{L}GB {L}GA", true, true),
        ],
        &[
            ("| {L}GA -> 0 | {L}GB n -> n | {L}GC (n, _) -> n", true),
            ("| {L}GB n -> n | _ -> 0", true),
            ("| {L}GC (_, m) -> m | _ -> 0", true),
        ],
    ),
    u(
        "postfixGeneric",
        "type 'a P = PA | PB of 'a list\n",
        "{L}P<int>",
        &[("{L}PA", true, true), ("{L}PB", true, true)],
        &[("| {L}PA -> 0 | {L}PB _ -> 1", true)],
    ),
    u(
        "twoParams",
        "type Pair<'a, 'b> = Pair of 'a * 'b | Nope\n",
        "{L}Pair<int, string>",
        &[
            ("{L}Pair (1, \"s\")", true, false),
            ("{L}Pair ({x}, {x})", true, false),
            ("{L}Nope", true, true),
        ],
        &[
            ("| {L}Pair (n, _) -> n | {L}Nope -> 0", true),
            ("| {L}Pair (_, s) -> s | _ -> \"\"", true),
        ],
    ),
    u(
        "recursive",
        "type Tree = Leaf | Node of Tree * int * Tree\n",
        "{L}Tree",
        &[
            ("{L}Leaf", true, false),
            ("{L}Node ({L}Leaf, 1, {L}Leaf)", true, false),
            (
                "{L}Node ({L}Node ({L}Leaf, {x}, {L}Leaf), 2, {L}Leaf)",
                true,
                false,
            ),
        ],
        &[
            ("| {L}Leaf -> 0 | {L}Node (_, n, _) -> n", true),
            ("| {L}Node ({L}Leaf, n, {L}Leaf) -> n | _ -> 0", true),
            ("| {L}Node (l, _, _) -> l | {L}Leaf -> {L}Leaf", true),
        ],
    ),
    u(
        "mutual",
        "type Expr = Num of int | Let of Bind * Expr\nand Bind = Bind of string * Expr\n",
        "{L}Expr",
        &[
            ("{L}Num 1", true, false),
            ("{L}Let ({L}Bind (\"a\", {L}Num 1), {L}Num 2)", true, false),
        ],
        &[("| {L}Num n -> n | {L}Let ({L}Bind (_, _), _) -> 0", true)],
    ),
    u(
        "named",
        "type N = NA of x: int | NB of name: string * age: int\n",
        "{L}N",
        &[("{L}NA 1", true, false), ("{L}NB (\"a\", 2)", true, false)],
        &[
            ("| {L}NA v -> v | {L}NB (_, a) -> a", true),
            // A named-field pattern.
            ("| {L}NA (x = v) -> v | _ -> 0", false),
        ],
    ),
    u(
        "rqa",
        "[<RequireQualifiedAccess>]\ntype Q = QA | QB of int\n",
        "{L}Q",
        &[
            ("{L}Q.QA", true, false),
            ("{L}Q.QB 1", true, false),
            ("{L}Q.QB", true, false),
        ],
        &[
            ("| {L}Q.QA -> 0 | {L}Q.QB n -> n", true),
            ("| {L}Q.QB 3 -> 3 | {L}Q.QB n -> n | {L}Q.QA -> 0", true),
        ],
    ),
    u(
        "struct",
        "[<Struct>]\ntype S = SA of a: int | SB of b: string\n",
        "{L}S",
        &[("{L}SA 1", true, false), ("{L}SB \"s\"", true, false)],
        &[("| {L}SA a -> a | {L}SB _ -> 0", true)],
    ),
    u(
        "tupleField",
        "type T2 = TT of (int * string) | TN\n",
        "{L}T2",
        &[
            ("{L}TT (1, \"s\")", true, false),
            ("{L}TT", true, false),
            ("{L}TT (id (1, \"s\"))", true, false),
        ],
        &[
            ("| {L}TT (n, _) -> n | {L}TN -> 0", true),
            ("| {L}TT p -> fst p | {L}TN -> 0", true),
        ],
    ),
    u(
        "unitField",
        "type Un = UU of unit | UV\n",
        "{L}Un",
        &[("{L}UU ()", true, false), ("{L}UV", true, false)],
        &[("| {L}UU () -> 1 | {L}UV -> 2", true)],
    ),
    u(
        "fnField",
        "type F = FF of (int -> int) | FN\n",
        "{L}F",
        &[("{L}FF id", true, false), ("{L}FN", true, false)],
        &[("| {L}FF f -> f 1 | {L}FN -> 0", true)],
    ),
    u(
        "objField",
        "type Ob = OO of obj | OS of string\n",
        "{L}Ob",
        &[
            ("{L}OO (box 1)", true, false),
            ("{L}OO \"s\"", true, false),
            ("{L}OO", true, false),
        ],
        &[("| {L}OO _ -> 1 | {L}OS _ -> 2", true)],
    ),
    u(
        "assemblyFields",
        "type Ab = AM of Map<string, int> | AL of int list | AO of int option\n",
        "{L}Ab",
        &[("{L}AO (Some 1)", true, false), ("{L}AO None", true, false)],
        &[
            // `Map<string, int>` is not bridged, so `AM` is not read.
            ("| {L}AL _ -> 1 | {L}AM _ -> 2 | {L}AO _ -> 3", false),
            ("| {L}AL _ -> 1 | _ -> 2", true),
            ("| {L}AO (Some n) -> n | _ -> 0", true),
        ],
    ),
    u(
        "projectField",
        "type Rec = { r: int }\ntype UR = UR1 of Rec | UR2\n",
        "{L}UR",
        &[("{L}UR2", true, false)],
        &[
            ("| {L}UR1 rr -> rr.r | {L}UR2 -> 0", false),
            ("| {L}UR1 _ -> 1 | {L}UR2 -> 2", true),
        ],
    ),
    u(
        "obsoleteCase",
        "type O =\n    | [<System.Obsolete(\"no\", true)>] OA\n    | OB of int\n",
        "{L}O",
        &[("{L}OA", false, false), ("{L}OB 1", false, false)],
        &[("| {L}OB n -> n | _ -> 0", false)],
    ),
    u(
        "obsoleteUnion",
        "[<System.Obsolete(\"no\", true)>]\ntype AU = AA | AB2\n",
        "{L}AU",
        &[("{L}AA", false, false)],
        &[],
    ),
    u(
        "measured",
        "[<Measure>] type m\ntype Mu = MA of float<m> | MB\n",
        "{L}Mu",
        &[("{L}MA 1.0<m>", false, false), ("{L}MB", false, false)],
        &[("| {L}MA _ -> 1 | {L}MB -> 2", false)],
    ),
    u(
        "constrained",
        "type K<'a when 'a : equality> = KA of 'a | KB\n",
        "{L}K<int>",
        &[("{L}KA 1", false, false), ("{L}KB", false, false)],
        &[("| {L}KA n -> n | {L}KB -> 0", false)],
    ),
    u(
        "caseNamedLikeType",
        "type Wrap = Wrap of int | Empty\n",
        "{L}Wrap",
        &[("{L}Wrap 1", true, false), ("{L}Wrap", true, false)],
        &[("| {L}Wrap 0 -> 0 | {L}Wrap n -> n | {L}Empty -> 1", true)],
    ),
    u(
        "laterShadow",
        "type U1 = X | Y of int\ntype U2 = X | Z\n",
        "{L}U2",
        // `X` is declared twice in its module, which inference declines.
        &[
            ("{L}X", false, false),
            ("{L}Y 1", true, false),
            ("{L}U1.X", false, false),
        ],
        &[
            ("| {L}X -> 0 | {L}Z -> 1", false),
            ("| {L}Y n -> n | _ -> 0", true),
        ],
    ),
    u(
        "withMembers",
        "type WM =\n    | WA of int\n    | WB\n    member this.Tag = 0\n",
        "{L}WM",
        &[("{L}WA 1", true, false), ("{L}WB", true, false)],
        &[("| {L}WA n -> n | {L}WB -> 0", true)],
    ),
    u(
        "valueThenCase",
        "let X = 5\ntype UV = X | W\n",
        "{L}UV",
        // `X` is declared twice in its module, which inference declines.
        &[
            ("{L}X", false, false),
            ("{L}UV.X", false, false),
            ("{L}W", true, false),
        ],
        &[
            ("| {L}X -> 0 | {L}W -> 1", false),
            ("| {L}W -> 0 | _ -> 1", true),
        ],
    ),
    u(
        "nestedModule",
        "module Inner =\n    type I = IA | IB of int\n",
        "{L}Inner.I",
        &[
            ("{L}Inner.I.IA", true, false),
            ("{L}Inner.I.IB 1", true, false),
            ("{L}Inner.IA", true, false),
        ],
        &[
            ("| {L}Inner.I.IA -> 0 | {L}Inner.I.IB n -> n", true),
            // A case pattern qualified by a module alone, which the resolver
            // does not resolve.
            ("| {L}Inner.IA -> 0 | {L}Inner.IB n -> n", false),
        ],
    ),
    u(
        "fullType",
        "type FT =\n    | FA : int -> FT\n    | FB : FT\n",
        "{L}FT",
        &[
            ("{L}FA 1", false, false),
            ("{L}FA", false, false),
            ("{L}FB", false, false),
        ],
        &[("| {L}FA n -> n | {L}FB -> 0", false)],
    ),
    u(
        "duplicateParameter",
        "type DU<'a, 'a> = DC of 'a | DN\n",
        "{L}DU<int, int>",
        &[
            ("{L}DC 1", false, false),
            ("{L}DC {x}", false, false),
            ("{L}DN", false, false),
        ],
        &[("| {L}DC n -> n | {L}DN -> 0", false)],
    ),
    u(
        "privateRepr",
        "type PR = private PRA | PRB of int\n",
        "{L}PR",
        &[("{L}PRA", true, false), ("{L}PRB 1", true, false)],
        &[("| {L}PRA -> 0 | {L}PRB n -> n", true)],
    ),
];

/// `(id, template, modelled)`: where a case value `{e}` sits. `{i}` is the
/// cell's index and `{T}` the union's ground instance.
const VALUE_CONTEXTS: &[(&str, &str, bool)] = &[
    ("value", "let v{i} = {e}\n", true),
    ("unitFn", "let f{i} () = {e}\n", true),
    ("param", "let f{i} (x: int) = {e}\n", true),
    ("tuple", "let f{i} (x: int) = ({e}, 0)\n", true),
    (
        "branch",
        "let f{i} (x: int) (b: bool) = if b then {e} else {e}\n",
        true,
    ),
];

/// `(id, template, modelled)`: where a match `{m}` over `u : {T}` sits.
const MATCH_CONTEXTS: &[(&str, &str, bool)] = &[
    ("body", "let f{i} (u: {T}) = match u with {m}\n", true),
    ("unannotated", "let f{i} u = match u with {m}\n", true),
    (
        "ignored",
        "let f{i} (u: {T}) = ignore (match u with {m})\n",
        true,
    ),
];

/// Where the union is declared, and how the using file reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// Above its uses, in the using file.
    InFile,
    /// In an earlier file, `open`ed.
    EarlierOpened,
    /// In an earlier file, every name qualified by its module.
    EarlierQualified,
    /// In an earlier file that has a signature: inference reads nothing of
    /// a screened file, so nothing is obligated.
    EarlierSignatured,
}

const PLACEMENTS: &[Placement] = &[
    Placement::InFile,
    Placement::EarlierOpened,
    Placement::EarlierQualified,
    Placement::EarlierSignatured,
];

impl Placement {
    fn qualifier(self) -> &'static str {
        match self {
            Placement::EarlierQualified => "Lib.",
            _ => "",
        }
    }

    /// Whether a cell whose context is `template` and whose filled text is
    /// `text` must commit here, for `union`.
    ///
    /// Inference reads nothing of a signatured file. An annotation is not
    /// bridged under an `open` of a project module or through a qualified
    /// path to a project type (`docs/sema-phase3-impl-plan.md`, *Earlier
    /// files' types*), and the resolver reaches no dotted path to a case
    /// (`U.A`) through an `open` of a project module. A match over an
    /// unannotated parameter of a generic union commits no binder, for
    /// FSharp.Core's unions too.
    fn obligates(self, union: &Union, template: &str, text: &str) -> bool {
        let annotated = template.contains(": ");
        let ty = fill(union.ty, self);
        let unannotated_generic = template.contains(" u = ") && ty.contains('<');
        if unannotated_generic || (annotated && ty.contains('.')) {
            return false;
        }
        match self {
            Placement::InFile => true,
            Placement::EarlierSignatured => false,
            Placement::EarlierOpened => !annotated && !text.contains('.'),
            Placement::EarlierQualified => !annotated,
        }
    }
}

/// Whether `clauses` hold a case pattern qualified by the earlier file's
/// module alone (`| Lib.A ->`, not `| Lib.U.A ->`), which the resolver does
/// not resolve across files.
fn module_qualified_pattern(clauses: &str) -> bool {
    clauses.match_indices("Lib.").any(|(i, _)| {
        let rest = &clauses[i + "Lib.".len()..];
        let name_len = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        !rest[name_len..].starts_with('.')
    })
}

/// One cell: its line in the using file, binder name, obligation and label.
type Cell = (u32, String, bool, String);

/// The files of one program: the union's, and the using file's body after
/// its header, by placement.
fn files(union: &Union, placement: Placement, body: &str) -> Vec<(String, String)> {
    match placement {
        Placement::InFile => vec![("Use.fs".into(), format!("module M\n{}{body}", union.decl))],
        Placement::EarlierOpened | Placement::EarlierQualified => {
            let open = if placement == Placement::EarlierOpened {
                "open Lib\n"
            } else {
                ""
            };
            vec![
                ("Lib.fs".into(), format!("module Lib\n{}", union.decl)),
                ("Use.fs".into(), format!("module M\n{open}{body}")),
            ]
        }
        Placement::EarlierSignatured => vec![
            (
                "Lib.fsi".into(),
                // A member's and a value's signature forms; every other
                // declaration is its own.
                format!(
                    "module Lib\n{}",
                    union
                        .decl
                        .replace("member this.Tag = 0", "member Tag : int")
                        .replace("let X = 5", "val X : int")
                ),
            ),
            ("Lib.fs".into(), format!("module Lib\n{}", union.decl)),
            ("Use.fs".into(), format!("module M\nopen Lib\n{body}")),
        ],
    }
}

/// The using file's header lines before `body`, for line numbering.
fn header_lines(union: &Union, placement: Placement) -> u32 {
    match placement {
        Placement::InFile => 1 + union.decl.matches('\n').count() as u32,
        Placement::EarlierOpened | Placement::EarlierSignatured => 2,
        Placement::EarlierQualified => 1,
    }
}

fn fill(template: &str, placement: Placement) -> String {
    template
        .replace("{L}", placement.qualifier())
        .replace("{x}", "x")
}

/// Every value and match cell of `union` at `placement`, as one program.
fn program(union: &Union, placement: Placement) -> (Vec<(String, String)>, Vec<Cell>) {
    let mut body = String::new();
    let mut cells = Vec::new();
    let mut i = 0usize;
    let first = header_lines(union, placement) + 1;
    let ty = fill(union.ty, placement);
    for (expr, modelled, generic) in union.values {
        for (cid, template, ctx_modelled) in VALUE_CONTEXTS {
            let line = first + body.matches('\n').count() as u32;
            let e = fill(expr, placement);
            body.push_str(&template.replace("{e}", &e).replace("{i}", &i.to_string()));
            let name = if template.starts_with("let v") {
                format!("v{i}")
            } else {
                format!("f{i}")
            };
            let obligated = *modelled
                && *ctx_modelled
                && placement.obligates(union, template, &e)
                && !(*generic && matches!(*cid, "value" | "branch"));
            cells.push((
                line,
                name,
                obligated,
                format!("{}/{placement:?}/{cid}/{e}", union.id),
            ));
            i += 1;
        }
    }
    for (clauses, modelled) in union.matches {
        for (cid, template, ctx_modelled) in MATCH_CONTEXTS {
            let line = first + body.matches('\n').count() as u32;
            let m = fill(clauses, placement);
            body.push_str(
                &template
                    .replace("{m}", &m)
                    .replace("{T}", &ty)
                    .replace("{i}", &i.to_string()),
            );
            cells.push((
                line,
                format!("f{i}"),
                *modelled
                    && *ctx_modelled
                    && placement.obligates(union, template, &m)
                    && !module_qualified_pattern(&m),
                format!("{}/{placement:?}/{cid}/{m}", union.id),
            ));
            i += 1;
        }
    }
    (files(union, placement, &body), cells)
}

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

#[derive(Default, Debug)]
struct Tally {
    cells: usize,
    committed: usize,
    committed_erroring: usize,
    exprs: usize,
    binders: usize,
    wrong: Vec<String>,
    missed: Vec<String>,
}

fn dump(files: &[(String, String)]) -> String {
    files
        .iter()
        .map(|(n, s)| format!("--- {n}\n{s}"))
        .collect::<Vec<_>>()
        .join("")
}

/// Grade one program: every node and binder inference types in any file
/// against FCS's, then each cell's obligation in the using file (the last).
fn check(sources: &[(String, String)], cells: &[Cell], tally: &mut Tally) {
    let named: Vec<(&str, &str)> = sources
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    let (root, written) = temp_fs_tree("project_union", &named);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let types = invoke_fcs_dump_types_project(&paths);
    let folded = fold(&root, sources, full_bcl_env());
    let _ = std::fs::remove_dir_all(&root);
    let last = sources.len() - 1;
    let mut ours_by_name = std::collections::HashMap::new();
    let mut using_errors = Vec::new();
    for (idx, (name, src)) in sources.iter().enumerate() {
        if name.ends_with(".fsi") {
            continue;
        }
        let Some(inferred) = folded.inferred.file(idx) else {
            continue;
        };
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let (types_json, binders_json) = &types[idx];
        let dumps = GradedDumps::read(types_json, binders_json, src, Some(&file))
            .expect("every declaration is placeable");
        if idx == last {
            using_errors = dumps.errors.iter().map(|e| e.line).collect();
        }
        let line_of = |offset: usize| src[..offset].matches('\n').count() as u32 + 1;
        for (range, ty) in inferred.types() {
            let key = (usize::from(range.start()), usize::from(range.end()));
            if dumps.is_ungraded(key) {
                continue;
            }
            let ours = ty.render();
            match dumps.types.get(&key) {
                Some(theirs) if *theirs == ours => tally.exprs += 1,
                theirs => tally.wrong.push(format!(
                    "{name} `{}` (line {}): ours `{ours}`, FCS {theirs:?}\n{}",
                    &src[key.0..key.1],
                    line_of(key.0),
                    dump(sources)
                )),
            }
        }
        let rf = folded.resolved.file(idx);
        for (def, ty) in inferred.def_types() {
            let d = rf.def(*def);
            let key = (usize::from(d.range.start()), usize::from(d.range.end()));
            if dumps.is_ungraded(key) {
                continue;
            }
            let ours = ty.render();
            match dumps.binders.get(&key) {
                Some(theirs) if *theirs == ours && !dumps.hides_constraint(key, &ours) => {
                    tally.binders += 1;
                    if idx == last {
                        ours_by_name.insert((d.name.to_string(), line_of(key.0)), ours);
                    }
                }
                theirs => tally.wrong.push(format!(
                    "{name} binder `{}` (line {}): ours `{ours}`, FCS {theirs:?}\n{}",
                    d.name,
                    line_of(key.0),
                    dump(sources)
                )),
            }
        }
    }
    for (line, name, obligated, label) in cells {
        tally.cells += 1;
        let erroring = using_errors.contains(line);
        match ours_by_name.get(&(name.clone(), *line)) {
            Some(_) => {
                tally.committed += 1;
                tally.committed_erroring += usize::from(erroring);
            }
            None if *obligated && !erroring => tally.missed.push(label.clone()),
            None => {}
        }
    }
}

fn assert_clean(tally: &Tally, floor: usize) {
    eprintln!(
        "cells {} committed {} (erroring {}) exprs {} binders {}",
        tally.cells, tally.committed, tally.committed_erroring, tally.exprs, tally.binders
    );
    assert!(
        tally.wrong.is_empty(),
        "{} disagreements with FCS:\n{}",
        tally.wrong.len(),
        tally.wrong.join("\n")
    );
    assert!(
        tally.missed.is_empty(),
        "{} obligated cells declined:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(tally.committed >= floor, "too little committed: {tally:?}");
}

#[test]
fn project_union_cases_agree_with_fcs_by_declaration_and_placement() {
    let mut tally = Tally::default();
    for union in UNIONS {
        for &placement in PLACEMENTS {
            let (sources, cells) = program(union, placement);
            check(&sources, &cells, &mut tally);
        }
    }
    assert_clean(&tally, 700);
}

/// `(union id, using-file body)`: cases where FCS rejects them — a payload
/// of the wrong type, a case of another union in a pattern, a later clause
/// contradicting an earlier one, a case applied to too many fields, an
/// inaccessible case — graded for agreement only.
const ILL_TYPED: &[(&str, &str)] = &[
    ("plain", "let f0 () = {L}B \"s\"\n"),
    ("plain", "let f1 (x: string) = {L}B x\n"),
    (
        "plain",
        "let f2 (u: {L}U) = match u with {L}A -> 0 | {L}B s -> s + \"x\" | _ -> 1\n",
    ),
    (
        "plain",
        "let f3 (u: {L}U) = match u with {L}A -> 0 | {L}GA -> 1 | _ -> 2\n",
    ),
    (
        "plain",
        "let f4 (x: int) = match x with {L}A -> 0 | _ -> 1\n",
    ),
    ("plain", "let f5 () = {L}C (1, \"s\")\n"),
    (
        "plain",
        "let f6 (u: {L}U) = match u with {L}B (a, b) -> a | _ -> 0\n",
    ),
    (
        "plain",
        "let f7 u = (match u with {L}A -> 0 | _ -> 1, match u with Some x -> x | None -> 0)\n",
    ),
    (
        "generic",
        "let f8 (g: {L}G<string>) = match g with {L}GB n -> n + 1 | _ -> 0\n",
    ),
    ("generic", "let f9 () = ({L}GB 1 : {L}G<string>)\n"),
    ("rqa", "let f10 () = QA\n"),
    (
        "rqa",
        "let f11 (q: {L}Q) = match q with QB n -> n | _ -> 0\n",
    ),
    ("tupleField", "let f12 () = {L}TT 1\n"),
    (
        "plain",
        "let f13 (u: {L}U) = match u with {L}A 1 -> 0 | _ -> 1\n",
    ),
];

#[test]
fn ill_typed_uses_of_project_union_cases_commit_only_what_fcs_kept() {
    let mut tally = Tally::default();
    for &placement in &[
        Placement::InFile,
        Placement::EarlierOpened,
        Placement::EarlierQualified,
    ] {
        for (id, body) in ILL_TYPED {
            let union = UNIONS.iter().find(|u| u.id == *id).expect("a union");
            // The union `f3` names a case of.
            let decl = if body.contains("GA") && *id == "plain" {
                format!("{}{}", union.decl, UNIONS[1].decl)
            } else {
                union.decl.to_string()
            };
            let combined = Union {
                decl: Box::leak(decl.into_boxed_str()),
                ..*union
            };
            let sources = files(&combined, placement, &fill(body, placement));
            check(&sources, &[], &mut tally);
        }
    }
    assert_clean(&tally, 0);
}

/// A namespace declared across two earlier files, each declaring a case
/// `X`: FCS resolves the namespace-qualified `N.X` by the namespace's own
/// order, which the resolver's latest-wins history need not follow.
#[test]
fn a_case_name_two_namespace_fragments_declare_agrees_with_fcs() {
    let mut tally = Tally::default();
    for use_body in [
        "let v0 = N.X\nlet v1 = N.U1.X\nlet v2 = N.U2.X\n",
        "open N\nlet v0 = X\nlet v1 = U1.X\nlet f2 u = match u with X -> 0 | Z -> 1\n",
        "let f0 (u: N.U1) = match u with N.X -> 0 | _ -> 1\n",
    ] {
        let sources = vec![
            (
                "A.fs".to_string(),
                "namespace N\ntype U1 = X | Y\n".to_string(),
            ),
            (
                "B.fs".to_string(),
                "namespace N\ntype U2 = X | Z\n".to_string(),
            ),
            ("Use.fs".to_string(), format!("module M\n{use_body}")),
        ];
        check(&sources, &[], &mut tally);
    }
    assert_clean(&tally, 0);
}
