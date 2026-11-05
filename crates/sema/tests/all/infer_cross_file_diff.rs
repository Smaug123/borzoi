//! Differential test for **cross-file inference**: a use of an earlier
//! Compile-order file's top-level value, typed from what that file's own
//! inference published for it ([`borzoi_sema::infer_project`]), against FCS
//! checking the same files as one project (`fcs-dump file-batch`'s
//! `types-project` request: every file's `types` and `binder-types`).
//!
//! # What is compared
//!
//! - **Soundness**, certain-implies-exact, over every file of every project:
//!   each expression and binder type we commit, FCS reports identically at the
//!   same range. Deferring is always allowed.
//! - **Obligations**, from a relocation twin. Every earlier value the table
//!   says a later file may type ([`Expect::Typed`]) is also declared *in* a
//!   single-file twin, under a name of the same length, and every use cell is
//!   repeated there against the local copy. Whatever the in-file walk commits
//!   in the twin — the use's binder, and each node of a same-length spelling,
//!   by its offset in the binding — the cross-file cell must commit
//!   identically. So a cross-file use is held to the in-file rules, with no
//!   expectation written down by hand.
//! - **Declines**: a value the table says a later file may not type records
//!   its [`PrecedingGap`] in the census of every cell that uses it, so deleting
//!   the gate that declines it fails here even where FCS agrees with the
//!   wrong commit it would make.
//!
//! The earlier values are generic, monomorphic, loosened (`obj` taken),
//! constrained, inline, mutable, incomplete, recursive, attributed, operators,
//! in a nested and a `rec` module, in a namespace, a case and an exception
//! constructor, private, signature-screened, after a syntax error, and shadowed
//! by a later file's same-named value. The later file uses each one in every
//! modelled use form (value, application, partial application, pipe, reverse
//! pipe, composition, paren callee, qualified, member access, method call,
//! operator, a parameter argued through it) × position (synth, argument,
//! tuple, branch, function body). The ill-typed family applies each value at a
//! contradictory type, and relates one parameter through two earlier functions
//! of different domains, in both orders.
//!
//! # Vacuity
//!
//! Every soundness assertion passes on an implementation that commits nothing.
//! The obligation count and the count of committed cross-file cells are what
//! make a green run evidence; both are floors.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::common::{GradedDumps, full_bcl_env, invoke_fcs_dump_types_project, temp_fs_tree};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, LetDecl, SigFile, SyntaxKind};
use borzoi_sema::{
    Incomplete, InferredProject, PrecedingGap, ProjectFile, Resolution, ResolvedProject,
    SourceFile, SyntaxRecovery, infer_project, qualified_names, resolve_project_files,
    signature_partners,
};

// ---------------------------------------------------------------------------
// The earlier values
// ---------------------------------------------------------------------------

/// What a later file may make of an earlier value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Its type is published: a cross-file cell commits what its in-file twin
    /// commits, and records no [`Incomplete::PrecedingValue`].
    Typed,
    /// Its type is withheld, and every cell using it records this reason.
    Declined(PrecedingGap),
    /// Graded for soundness only: what the resolver makes of the name is its
    /// own business (a private value, an active pattern), or the twin is not
    /// the in-file equivalent (a `rec` module's value relocated outside it).
    Any,
}

/// How a later file can use an earlier value.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// A non-function value; `member` names a data member of its type, for the
    /// receiver forms; `contra` is an operand `+` rejects beside it.
    Value {
        member: Option<&'static str>,
        contra: &'static str,
    },
    /// A function taking `args` (one per curried group); `bad` is a first
    /// argument of a contradictory type, where there is one.
    Fun {
        args: &'static [&'static str],
        bad: Option<&'static str>,
        /// The first group's type, where it is a known non-generic one: the
        /// pair grid relates a parameter through two of these.
        domain: Option<&'static str>,
    },
    /// An infix operator: well-typed operands and a contradictory right one.
    Op {
        lhs: &'static str,
        rhs: &'static str,
        bad: Option<&'static str>,
    },
}

/// One earlier value. `name` carries the two-character prefix of the module
/// it is reached through (`a_` for `A`, `n_` for `A.N`, `r_` for `A.R`, `q_`
/// for `Lib.Q`), and the twin's local copy swaps that for `l_`; an operator
/// or a case name its own twin spelling, of the same length.
#[derive(Debug, Clone, Copy)]
struct Earlier {
    name: &'static str,
    twin: &'static str,
    /// The qualifier a qualified use writes.
    qualifier: &'static str,
    /// The declaration, as it appears in its module (one or more lines, not
    /// indented); its twin replaces `name` with `twin`.
    decl: &'static str,
    shape: Shape,
    expect: Expect,
}

const fn fun(
    args: &'static [&'static str],
    bad: Option<&'static str>,
    domain: Option<&'static str>,
) -> Shape {
    Shape::Fun { args, bad, domain }
}

const fn val(member: Option<&'static str>, contra: &'static str) -> Shape {
    Shape::Value { member, contra }
}

/// The values of module `A` (file `A.fs`), its nested modules `N` and `rec
/// R`, and of `Lib.Q` (file `Lib.fs`, a namespace). The qualifier is the
/// whole path a file that opens nothing writes.
const EARLIER: &[Earlier] = &[
    // Generic.
    e(
        "a_gid",
        "l_gid",
        "A",
        "let a_gid x = x",
        fun(&["1"], None, None),
        Expect::Typed,
    ),
    e(
        "a_gk",
        "l_gk",
        "A",
        "let a_gk x y = x",
        fun(&["1", "\"s\""], None, None),
        Expect::Typed,
    ),
    e(
        "a_gp",
        "l_gp",
        "A",
        "let a_gp x y = (x, y)",
        fun(&["1", "true"], None, None),
        Expect::Typed,
    ),
    e(
        "a_gfst",
        "l_gfst",
        "A",
        "let a_gfst (a, b) = a",
        fun(&["(1, \"s\")"], Some("1"), None),
        Expect::Typed,
    ),
    // Monomorphic values.
    e(
        "a_mi",
        "l_mi",
        "A",
        "let a_mi = 42",
        val(None, "\"s\""),
        Expect::Typed,
    ),
    e(
        "a_ms",
        "l_ms",
        "A",
        "let a_ms = \"s\"",
        val(Some("Length"), "true"),
        Expect::Typed,
    ),
    e(
        "a_mt",
        "l_mt",
        "A",
        "let a_mt = (1, \"a\")",
        val(None, "1"),
        Expect::Typed,
    ),
    e(
        "a_mm",
        "l_mm",
        "A",
        "let mutable a_mm = 1",
        val(None, "\"s\""),
        Expect::Typed,
    ),
    // Monomorphic functions.
    e(
        "a_fb",
        "l_fb",
        "A",
        "let a_fb c = if c then 1 else 2",
        fun(&["true"], Some("1"), Some("bool")),
        Expect::Typed,
    ),
    e(
        "a_fi",
        "l_fi",
        "A",
        "let a_fi (x: int) = x",
        fun(&["1"], Some("\"s\""), Some("int")),
        Expect::Typed,
    ),
    e(
        "a_fs",
        "l_fs",
        "A",
        "let a_fs (s: string) = s.Length",
        fun(&["\"s\""], Some("1"), Some("string")),
        Expect::Typed,
    ),
    e(
        "a_fo",
        "l_fo",
        "A",
        "let a_fo (x: obj) = 1",
        fun(&["1"], None, None),
        Expect::Typed,
    ),
    e(
        "a_fu",
        "l_fu",
        "A",
        "let a_fu () = 1",
        fun(&["()"], Some("1"), None),
        Expect::Typed,
    ),
    e(
        "a_ft",
        "l_ft",
        "A",
        "let a_ft (a: int, b: string) = b",
        fun(&["(1, \"s\")"], Some("(\"s\", 1)"), None),
        Expect::Typed,
    ),
    e(
        "a_fc",
        "l_fc",
        "A",
        "let a_fc (a: int) (b: string) = b",
        fun(&["1", "\"s\""], Some("\"s\""), Some("int")),
        Expect::Typed,
    ),
    e(
        "a_fr",
        "l_fr",
        "A",
        "let a_fr (x: int) = \"r\"",
        fun(&["1"], Some("true"), Some("int")),
        Expect::Typed,
    ),
    // Operators.
    e(
        "+.",
        "-.",
        "A",
        "let (+.) (a: int) (b: int) = a",
        Shape::Op {
            lhs: "1",
            rhs: "2",
            bad: Some("\"s\""),
        },
        Expect::Typed,
    ),
    e(
        "<+>",
        "<->",
        "A",
        "let (<+>) x y = (x, y)",
        Shape::Op {
            lhs: "1",
            rhs: "\"s\"",
            bad: None,
        },
        Expect::Typed,
    ),
    // An attribute that is not `[<EntryPoint>]` or one that rejects a use,
    // but which the in-file walk does not prove so: declined in both files.
    e(
        "a_at",
        "l_at",
        "A",
        "[<System.CLSCompliant(true)>]\nlet a_at = 1",
        val(None, "\"s\""),
        Expect::Declined(PrecedingGap::RejectedUse),
    ),
    // Withheld.
    e(
        "a_geq",
        "l_geq",
        "A",
        "let a_geq x y = x = y",
        fun(&["1", "2"], Some("\"s\""), None),
        Expect::Declined(PrecedingGap::Untyped),
    ),
    e(
        "a_inl",
        "l_inl",
        "A",
        "let inline a_inl x = x",
        fun(&["1"], None, None),
        Expect::Declined(PrecedingGap::Inline),
    ),
    e(
        "a_inc",
        "l_inc",
        "A",
        "let a_inc x = match x with _ -> 1",
        fun(&["1"], None, None),
        Expect::Declined(PrecedingGap::Untyped),
    ),
    e(
        "a_lst",
        "l_lst",
        "A",
        "let a_lst = [1]",
        val(None, "1"),
        Expect::Declined(PrecedingGap::Untyped),
    ),
    e(
        "a_rec",
        "l_rec",
        "A",
        "let rec a_rec x = if x then 1 else a_rec x",
        fun(&["true"], Some("1"), None),
        Expect::Declined(PrecedingGap::NotWalked),
    ),
    e(
        "a_obw",
        "l_obw",
        "A",
        "[<System.Obsolete(\"w\")>]\nlet a_obw = 1",
        val(None, "\"s\""),
        Expect::Declined(PrecedingGap::RejectedUse),
    ),
    e(
        "a_obe",
        "l_obe",
        "A",
        "[<System.Obsolete(\"e\", true)>]\nlet a_obe = 1",
        val(None, "\"s\""),
        Expect::Declined(PrecedingGap::RejectedUse),
    ),
    e(
        "UA",
        "LA",
        "A",
        "type U = UA of int",
        fun(&["1"], Some("\"s\""), None),
        Expect::Declined(PrecedingGap::NotAValue),
    ),
    e(
        "XA",
        "XL",
        "A",
        "exception XA of int",
        fun(&["1"], Some("\"s\""), None),
        Expect::Declined(PrecedingGap::NotAValue),
    ),
    e(
        "a_pv",
        "l_pv",
        "A",
        "let private a_pv = 1",
        val(None, "\"s\""),
        Expect::Any,
    ),
    // Nested modules and a namespace.
    e(
        "n_nv",
        "l_nv",
        "A.N",
        "let n_nv = 7",
        val(None, "\"s\""),
        Expect::Typed,
    ),
    e(
        "n_nf",
        "l_nf",
        "A.N",
        "let n_nf (x: string) = x",
        fun(&["\"s\""], Some("1"), Some("string")),
        Expect::Typed,
    ),
    e(
        "r_ra",
        "l_ra",
        "A.R",
        "let r_ra x = x",
        fun(&["1"], None, None),
        Expect::Any,
    ),
    e(
        "r_rb",
        "l_rb",
        "A.R",
        "let r_rb () = r_ra 1",
        fun(&["()"], None, None),
        Expect::Any,
    ),
    // Values of a nested module carrying a rejecting attribute: their own
    // bindings are walked and typed, and a use F# rejects (error level) or
    // warns about is declined.
    e(
        "d_dv",
        "l_dv",
        "A.D",
        "let d_dv = 5",
        val(None, "\"s\""),
        Expect::Declined(PrecedingGap::RejectedUse),
    ),
    e(
        "d_df",
        "l_df",
        "A.D",
        "let d_df (x: int) = x",
        fun(&["1"], Some("\"s\""), Some("int")),
        Expect::Declined(PrecedingGap::RejectedUse),
    ),
    e(
        "w_wv",
        "l_wv",
        "A.W",
        "let w_wv = 6",
        val(None, "\"s\""),
        Expect::Declined(PrecedingGap::RejectedUse),
    ),
    e(
        "q_qv",
        "l_qv",
        "Lib.Q",
        "let q_qv = 3",
        val(None, "\"s\""),
        Expect::Typed,
    ),
    e(
        "q_qf",
        "l_qf",
        "Lib.Q",
        "let q_qf x = (x, x)",
        fun(&["1"], None, None),
        Expect::Typed,
    ),
];

#[allow(clippy::too_many_arguments)]
const fn e(
    name: &'static str,
    twin: &'static str,
    qualifier: &'static str,
    decl: &'static str,
    shape: Shape,
    expect: Expect,
) -> Earlier {
    Earlier {
        name,
        twin,
        qualifier,
        decl,
        shape,
        expect,
    }
}

impl Earlier {
    fn is_operator(&self) -> bool {
        matches!(self.shape, Shape::Op { .. })
    }

    /// The value spelled as an expression.
    fn bare(&self, twin: bool) -> String {
        let n = if twin { self.twin } else { self.name };
        if self.is_operator() {
            format!("({n})")
        } else {
            n.to_string()
        }
    }

    /// The declaration, renamed for the twin: every earlier name in it (a
    /// `rec` module's value refers to its sibling) takes its twin spelling.
    fn decl_for(&self, twin: bool) -> String {
        if !twin {
            return self.decl.to_string();
        }
        EARLIER
            .iter()
            .fold(self.decl.to_string(), |d, v| d.replace(v.name, v.twin))
    }
}

// ---------------------------------------------------------------------------
// The use cells
// ---------------------------------------------------------------------------

/// One use of one earlier value in a later file: the binding's text with the
/// binder name left as `{b}`, and the value's spelling substituted.
#[derive(Debug, Clone)]
struct Cell {
    value: usize,
    /// The cross-file text and the twin's.
    cross: String,
    twin: String,
    /// Whether the cell spells the value qualified. It then sits in the later
    /// file that opens nothing, where the resolver keys a qualified path over
    /// the whole path; a bare cell sits in the one that opens every module.
    qualified: bool,
}

/// The use forms of `v`, each a pair `(expression, uses_parameter)`; the
/// expression spells the value as `{v}` (bare), `{q}` (qualified) or, for an
/// operator, `{o}` (infix).
fn use_forms(v: &Earlier, ill: bool) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    match v.shape {
        Shape::Value { member, contra } => {
            if ill {
                out.push(("{v} 1".into(), false));
                out.push((format!("{{v}} + {contra}"), false));
                out.push((format!("{contra} + {{v}}"), false));
                out.push((format!("{{q}} + {contra}"), false));
            } else {
                out.push(("{v}".into(), false));
                out.push(("{q}".into(), false));
                out.push(("({v}, {v})".into(), false));
                out.push(("{v}.ToString()".into(), false));
                if let Some(m) = member {
                    out.push((format!("{{v}}.{m}"), false));
                    out.push((format!("{{q}}.{m}"), false));
                }
            }
        }
        Shape::Fun { args, bad, .. } => {
            let all = args.join(" ");
            let (first, rest) = args.split_first().expect("a function takes an argument");
            let rest = rest.join(" ");
            let sp = if rest.is_empty() { "" } else { " " };
            if ill {
                if let Some(bad) = bad {
                    out.push((format!("{{v}} {bad}{sp}{rest}"), false));
                    out.push((format!("{bad} |> {{v}}"), false));
                    out.push((format!("({{v}} {bad}{sp}{rest}, {{v}} {all})"), false));
                    out.push((format!("{{q}} {bad}{sp}{rest}"), false));
                }
                // A value applied to more arguments than it takes.
                out.push((format!("{{v}} {all} 1"), false));
            } else {
                out.push((format!("{{v}} {all}"), false));
                out.push((format!("{{q}} {all}"), false));
                out.push((format!("({{v}}) {all}"), false));
                out.push(("{v}".into(), false));
                out.push(("{q}".into(), false));
                out.push((format!("{{v}} {first}"), false));
                if args.len() == 1 {
                    out.push((format!("{first} |> {{v}}"), false));
                    out.push((format!("{{v}} <| {first}"), false));
                    out.push(("{v} >> id".into(), false));
                    out.push(("p |> {v}".into(), true));
                    out.push(("p |> {q}".into(), true));
                } else {
                    let (last, init) = args.split_last().expect("two or more");
                    out.push((format!("{last} |> {{v}} {}", init.join(" ")), false));
                }
                out.push((format!("{{v}} p{sp}{rest}"), true));
            }
        }
        Shape::Op { lhs, rhs, bad } => {
            if ill {
                if let Some(bad) = bad {
                    out.push((format!("{lhs} {{o}} {bad}"), false));
                    out.push((format!("{{v}} {bad} {rhs}"), false));
                    out.push((format!("{{q}} {bad} {rhs}"), false));
                }
            } else {
                out.push((format!("{lhs} {{o}} {rhs}"), false));
                out.push((format!("{{v}} {lhs} {rhs}"), false));
                out.push((format!("{{q}} {lhs} {rhs}"), false));
                out.push((format!("{{v}} {lhs}"), false));
                out.push(("{v}".into(), false));
                out.push((format!("p {{o}} {rhs}"), true));
            }
        }
    }
    out
}

/// Wrap `expr` in each position. A form that uses the parameter `p` is only
/// placed inside a function of `p`.
fn positions(expr: &str, uses_param: bool) -> Vec<String> {
    if uses_param {
        return vec![
            format!("let {{b}} p = {expr}"),
            format!("let {{b}} p = ({expr}, p)"),
        ];
    }
    vec![
        format!("let {{b}} = {expr}"),
        format!("let {{b}} = id ({expr})"),
        format!("let {{b}} = ({expr}, 0)"),
        format!("let {{b}} = if true then {expr} else {expr}"),
        format!("let {{b}} p = ({expr}, p)"),
    ]
}

/// Substitute the value's spellings into a form.
fn spell(form: &str, v: &Earlier, twin: bool) -> String {
    let bare = v.bare(twin);
    let qualified = if twin {
        // The twin's copy is in-file and unqualified: a qualified cell is
        // compared with it by binder only.
        bare.clone()
    } else if v.is_operator() {
        format!("{}.({})", v.qualifier, v.name)
    } else {
        format!("{}.{}", v.qualifier, v.name)
    };
    let op = if twin { v.twin } else { v.name };
    form.replace("{v}", &bare)
        .replace("{q}", &qualified)
        .replace("{o}", op)
}

fn cells(ill: bool) -> Vec<Cell> {
    let mut out = Vec::new();
    for (i, v) in EARLIER.iter().enumerate() {
        for (form, uses_param) in use_forms(v, ill) {
            for pos in positions(&form, uses_param) {
                out.push(Cell {
                    value: i,
                    cross: spell(&pos, v, false),
                    twin: spell(&pos, v, true),
                    qualified: form.contains("{q}"),
                });
            }
        }
    }
    out.extend(pair_cells(ill));
    out
}

/// One parameter related through two earlier functions, in both orders.
/// Ill-typed where their domains differ — FCS fixes the parameter at the first
/// and rejects the second, so an unordered solve that let the second win would
/// type it wrong — and well-typed where they agree.
fn pair_cells(ill: bool) -> Vec<Cell> {
    let typed: Vec<(usize, &str)> = EARLIER
        .iter()
        .enumerate()
        .filter_map(|(i, v)| match v.shape {
            Shape::Fun {
                args,
                domain: Some(d),
                ..
            } if args.len() == 1 && v.expect == Expect::Typed => Some((i, d)),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for &(i, di) in &typed {
        for &(j, dj) in &typed {
            if i == j || (di != dj) != ill {
                continue;
            }
            let (f, g) = (&EARLIER[i], &EARLIER[j]);
            for shape in [
                "let {b} p = ({f} p, {g} p)",
                "let {b} p = ({f} p, p |> {g})",
                "let {b} p = (p |> {f}, {g} p, p)",
            ] {
                let text = |twin: bool| {
                    shape
                        .replace("{f}", &f.bare(twin))
                        .replace("{g}", &g.bare(twin))
                };
                out.push(Cell {
                    value: i,
                    cross: text(false),
                    twin: text(true),
                    qualified: false,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The projects
// ---------------------------------------------------------------------------

/// `decl` indented by `n` spaces, line by line.
fn indent(decl: &str, n: usize) -> String {
    decl.lines()
        .map(|l| format!("{}{l}\n", " ".repeat(n)))
        .collect()
}

/// The nested modules of `A`, by qualifier, with the lines that open them.
const NESTED: &[(&str, &str)] = &[
    ("A.N", "module N ="),
    ("A.R", "module rec R ="),
    ("A.D", "[<System.Obsolete(\"e\", true)>]\nmodule D ="),
    ("A.W", "[<System.Obsolete(\"w\")>]\nmodule W ="),
];

/// `A.fs`: module `A` with its nested modules ([`NESTED`]).
fn file_a() -> String {
    let mut s = String::from("module A\n\n");
    for v in EARLIER.iter().filter(|v| v.qualifier == "A") {
        s.push_str(v.decl);
        s.push('\n');
    }
    for (qualifier, header) in NESTED {
        s.push('\n');
        s.push_str(header);
        s.push('\n');
        for v in EARLIER.iter().filter(|v| v.qualifier == *qualifier) {
            s.push_str(&indent(v.decl, 4));
        }
    }
    s
}

/// `Lib.fs`: namespace `Lib`, module `Q`.
fn file_lib() -> String {
    let mut s = String::from("namespace Lib\n\nmodule Q =\n");
    for v in EARLIER.iter().filter(|v| v.qualifier == "Lib.Q") {
        s.push_str(&indent(v.decl, 4));
    }
    s
}

/// The binder name of cell `k`: `u` in the cross-file project, `t` in the
/// twin, the same length so the two bindings line up character for character.
fn binder(k: usize, twin: bool) -> String {
    format!("{}{k:04}", if twin { 't' } else { 'u' })
}

/// A later file of the cross-file project: the qualified cells in `Q.fs`,
/// which opens nothing, and the bare ones in `B.fs`, which opens every module.
fn file_later(cells: &[Cell], qualified: bool) -> String {
    let mut s = if qualified {
        String::from("module QualifiedUses\n\n")
    } else {
        String::from(
            "module B\n\nopen A\nopen A.N\nopen A.R\nopen A.D\nopen A.W\nopen Lib\nopen Lib.Q\n\n",
        )
    };
    for (k, c) in cells.iter().enumerate() {
        if c.qualified == qualified {
            s.push_str(&c.cross.replace("{b}", &binder(k, false)));
            s.push('\n');
        }
    }
    s
}

/// The twin: every earlier value declared in one file under its twin name,
/// then every cell against the local copy.
fn file_twin(cells: &[Cell]) -> String {
    let mut s = String::from("module T\n\n");
    for v in EARLIER {
        s.push_str(&v.decl_for(true));
        s.push('\n');
    }
    s.push('\n');
    for (k, c) in cells.iter().enumerate() {
        s.push_str(&c.twin.replace("{b}", &binder(k, true)));
        s.push('\n');
    }
    s
}

/// A project's files, parsed and folded as the LSP folds them.
struct Folded {
    sources: Vec<(String, String)>,
    files: Vec<ProjectFile>,
    resolved: ResolvedProject,
    inferred: InferredProject,
}

fn fold(sources: &[(&str, &str)]) -> Folded {
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = sources
        .iter()
        .map(|(path, src)| {
            if path.ends_with(".fsi") {
                let p = parse_sig(src);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Sig(SigFile::cast(p.root).expect("sig file")),
                    recovery,
                )
            } else {
                let p = parse(src);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
                    recovery,
                )
            }
        })
        .collect();
    let paths: Vec<std::path::PathBuf> = sources.iter().map(|(p, _)| p.into()).collect();
    let srcs: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let qnofs = qualified_names(&srcs, &paths);
    let files: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qnofs)
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    let env = full_bcl_env();
    let resolved = resolve_project_files(&files, env);
    let inferred = infer_project(&files, &resolved, env);
    Folded {
        sources: sources
            .iter()
            .map(|(p, s)| ((*p).to_string(), (*s).to_string()))
            .collect(),
        files,
        resolved,
        inferred,
    }
}

/// What grading one project found, per file.
#[derive(Debug, Default)]
struct Graded {
    /// The node commits FCS confirmed: range → rendered type.
    nodes: Vec<HashMap<(usize, usize), String>>,
    /// The binder commits FCS confirmed: binder name → rendered type.
    binders: Vec<HashMap<String, String>>,
    /// Commits inside a declaration FCS could not convert, or in a
    /// signature-paired file, whose nodes and binders FCS keeps no record of.
    ungraded: usize,
    /// FCS's error lines (1-based).
    error_lines: Vec<Vec<u32>>,
}

/// Check `folded` against FCS: every commit in every file must be FCS's.
fn grade(label: &str, folded: &Folded) -> Graded {
    let rows: Vec<(&str, &str)> = folded
        .sources
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (root, written) = temp_fs_tree(label, &rows);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let dumps = invoke_fcs_dump_types_project(&paths);
    let _ = std::fs::remove_dir_all(&root);
    let partners = signature_partners(&folded.files);
    let mut g = Graded::default();
    for (idx, (types_json, binders_json)) in dumps.iter().enumerate() {
        let (path, source) = &folded.sources[idx];
        let mut nodes = HashMap::new();
        let mut binders = HashMap::new();
        let impl_file = folded.files[idx].file.as_impl();
        let Some(dump) = GradedDumps::read(types_json, binders_json, source, impl_file) else {
            panic!("{label}: an unconverted declaration in {path} could not be placed");
        };
        g.error_lines
            .push(dump.errors.iter().map(|e| e.line).collect());
        // FCS declares a signature-paired implementation's values at their
        // signature, and keeps no record of them here.
        let screened = impl_file.is_some() && partners[idx].is_some();
        let Some(inferred) = folded.inferred.file(idx) else {
            g.nodes.push(nodes);
            g.binders.push(binders);
            continue;
        };
        let rf = folded.resolved.file(idx);
        let ctx = |range: (usize, usize)| -> String {
            let line_start = source[..range.0].rfind('\n').map_or(0, |i| i + 1);
            let line_end = source[range.1..]
                .find('\n')
                .map_or(source.len(), |i| range.1 + i);
            format!("{path}: `{}`", &source[line_start..line_end])
        };
        for (range, ty) in inferred.types() {
            let key = (
                u32::from(range.start()) as usize,
                u32::from(range.end()) as usize,
            );
            if dump.is_ungraded(key) {
                g.ungraded += 1;
                continue;
            }
            let ours = ty.render();
            let Some(theirs) = dump.types.get(&key) else {
                if screened {
                    g.ungraded += 1;
                    continue;
                }
                panic!(
                    "{label}: we inferred `{ours}` at {key:?} (`{}`) but FCS reports no node \
                     there\n{}",
                    &source[key.0..key.1],
                    ctx(key)
                );
            };
            assert_eq!(
                &ours,
                theirs,
                "{label}: expression type mismatch at {key:?} (`{}`)\n{}",
                &source[key.0..key.1],
                ctx(key)
            );
            nodes.insert(key, ours);
        }
        for (def_id, ty) in inferred.def_types() {
            let def = rf.def(*def_id);
            let key = (
                u32::from(def.range.start()) as usize,
                u32::from(def.range.end()) as usize,
            );
            if dump.is_ungraded(key) {
                g.ungraded += 1;
                continue;
            }
            let ours = ty.render();
            let Some(theirs) = dump.binders.get(&key) else {
                if screened {
                    g.ungraded += 1;
                    continue;
                }
                panic!(
                    "{label}: we inferred `{ours}` for binder `{}` at {key:?} but FCS reports \
                     no binder there\n{}",
                    def.name,
                    ctx(key)
                );
            };
            assert_eq!(
                &ours,
                theirs,
                "{label}: binder type mismatch for `{}`\n{}",
                def.name,
                ctx(key)
            );
            assert!(
                !dump.hides_constraint(key, &ours),
                "{label}: binder `{}` typed `{ours}`, which FCS constrains\n{}",
                def.name,
                ctx(key)
            );
            binders.insert(def.name.clone(), ours);
        }
        g.nodes.push(nodes);
        g.binders.push(binders);
    }
    g
}

/// The census of each module-level binding of the impl file at `idx`, keyed by
/// its first binder's name. Every binding of a later file here is
/// unattributed, non-recursive and unannotated, so each begins exactly one
/// census entry, in document order.
fn census_by_binding(folded: &Folded, idx: usize) -> HashMap<String, Vec<Incomplete>> {
    let file = folded.files[idx].file.as_impl().expect("impl file");
    let names: Vec<String> = file
        .syntax()
        .descendants()
        .filter_map(LetDecl::cast)
        .filter_map(|d| d.bindings().next())
        .filter_map(|b| b.pat())
        .map(|p| {
            p.syntax()
                .descendants_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
                .map(|t| t.text().to_string())
                .unwrap_or_default()
        })
        .collect();
    let census = folded
        .inferred
        .file(idx)
        .expect("inferred")
        .incompleteness();
    assert_eq!(
        census.len(),
        names.len(),
        "every binding of the later file begins one census entry"
    );
    names.into_iter().zip(census.iter().cloned()).collect()
}

/// Each binding line `let <prefix>NNNN …` of `source`: cell `NNNN` → the
/// line's byte range and its 1-based line number.
fn cell_lines(source: &str, prefix: char) -> HashMap<usize, (usize, usize, u32)> {
    let mut out = HashMap::new();
    let mut offset = 0;
    for (n, line) in source.split_inclusive('\n').enumerate() {
        if let Some(rest) = line.strip_prefix("let ")
            && rest.starts_with(prefix)
            && let Some(k) = rest.get(1..5).and_then(|d| d.parse::<usize>().ok())
        {
            let end = offset + line.trim_end_matches('\n').len();
            out.insert(k, (offset, end, n as u32 + 1));
        }
        offset += line.len();
    }
    out
}

/// Each node committed on a cell's line, by its offset from the line start.
fn nodes_by_cell(
    nodes: &HashMap<(usize, usize), String>,
    lines: &HashMap<usize, (usize, usize, u32)>,
) -> HashMap<usize, BTreeMap<(usize, usize), String>> {
    let mut out: HashMap<usize, BTreeMap<(usize, usize), String>> = HashMap::new();
    for (&k, &(start, end, _)) in lines {
        for (&(s, e), ty) in nodes {
            if start <= s && e <= end {
                out.entry(k)
                    .or_default()
                    .insert((s - start, e - start), ty.clone());
            }
        }
    }
    out
}

/// Whether the resolver resolved some name on the line `[start, end)` of the
/// impl file at `idx` to a preceding file's top-level value. Which binder a
/// name means is the resolver's answer: where it declines, a cell owes
/// nothing.
fn resolves_cross_file(folded: &Folded, idx: usize, start: usize, end: usize) -> bool {
    let rf = folded.resolved.file(idx);
    rf.resolutions().iter().any(|(range, res)| {
        let (s, e) = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        start <= s && e <= end && matches!(res, Resolution::Item(id) if !rf.owns_item(*id))
    })
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// The cross-file project of a family: `A.fs`, `Lib.fs`, then the two later
/// files — `Q.fs` (qualified cells, no opens) and `B.fs` (bare cells).
fn cross_project(cells: &[Cell]) -> Folded {
    let (a, lib) = (file_a(), file_lib());
    let (q, b) = (file_later(cells, true), file_later(cells, false));
    fold(&[("A.fs", &a), ("Lib.fs", &lib), ("Q.fs", &q), ("B.fs", &b)])
}

/// The later files' Compile indices in [`cross_project`].
const LATER: [usize; 2] = [2, 3];

/// The well-typed family: every earlier value in every use form and position,
/// held to its in-file twin and to the table's declines.
#[test]
fn cross_file_uses_commit_what_their_in_file_twin_commits() {
    let cells = cells(false);
    let cross = cross_project(&cells);
    let twin_src = file_twin(&cells);
    let twin = fold(&[("T.fs", &twin_src)]);

    let cg = grade("cross_file_well_typed", &cross);
    let tg = grade("cross_file_twin", &twin);

    let twin_lines = cell_lines(&twin_src, 't');
    let twin_nodes = nodes_by_cell(&tg.nodes[0], &twin_lines);
    // Per later file: the census, each cell's line, and the nodes on it.
    let later: Vec<_> = LATER
        .iter()
        .map(|&idx| {
            let lines = cell_lines(&cross.sources[idx].1, 'u');
            let nodes = nodes_by_cell(&cg.nodes[idx], &lines);
            (idx, census_by_binding(&cross, idx), lines, nodes)
        })
        .collect();

    let mut obligations = 0usize;
    let mut committed_cells = 0usize;
    let mut resolver_declined = 0usize;
    let mut failures = Vec::new();
    for (k, cell) in cells.iter().enumerate() {
        let v = &EARLIER[cell.value];
        let name = binder(k, false);
        let (idx, census, lines, cross_nodes) = &later[usize::from(!cell.qualified)];
        let &(start, end, line) = &lines[&k];
        let reasons = &census[&name];
        let preceding: Vec<PrecedingGap> = reasons
            .iter()
            .filter_map(|r| match r {
                Incomplete::PrecedingValue(g) => Some(*g),
                _ => None,
            })
            .collect();
        if !resolves_cross_file(&cross, *idx, start, end) {
            resolver_declined += 1;
            assert!(
                preceding.is_empty(),
                "`{}`: no name resolved cross-file, yet the census says {preceding:?}",
                cell.cross
            );
            continue;
        }
        match v.expect {
            Expect::Typed => {
                if cg.error_lines[*idx].contains(&line) {
                    failures.push(format!(
                        "generator: FCS rejects a well-typed cell `{}`",
                        cell.cross
                    ));
                    continue;
                }
                if !preceding.is_empty() {
                    failures.push(format!(
                        "`{}`: the earlier value is published, but the cell declined with \
                         {preceding:?}",
                        cell.cross
                    ));
                }
                if let Some(ty) = tg.binders[0].get(&binder(k, true)) {
                    obligations += 1;
                    match cg.binders[*idx].get(&name) {
                        Some(ours) if ours == ty => committed_cells += 1,
                        other => failures.push(format!(
                            "`{}`: the twin commits `{ty}` for the binder, cross-file \
                             {other:?}; census {reasons:?}",
                            cell.cross
                        )),
                    }
                }
                if cell.cross.len() == cell.twin.len()
                    && let Some(twin_nodes) = twin_nodes.get(&k)
                {
                    let ours = cross_nodes.get(&k);
                    for (span, ty) in twin_nodes {
                        obligations += 1;
                        if ours.and_then(|o| o.get(span)) != Some(ty) {
                            failures.push(format!(
                                "`{}`: the twin commits `{ty}` at {span:?} (`{}`), cross-file \
                                 does not; census {reasons:?}",
                                cell.cross,
                                &cell.twin.replace("{b}", &binder(k, true))[span.0..span.1]
                            ));
                        }
                    }
                }
            }
            Expect::Declined(gap) => {
                if !preceding.contains(&gap) {
                    failures.push(format!(
                        "`{}`: expected the decline {gap:?}, census {reasons:?}",
                        cell.cross
                    ));
                }
            }
            Expect::Any => {}
        }
    }
    eprintln!(
        "cross-file well-typed: {} cells ({resolver_declined} the resolver declined), \
         {obligations} obligations, {committed_cells} cells committed at their twin's type, {} \
         commits ungraded",
        cells.len(),
        cg.ungraded + tg.ungraded
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        obligations >= 1500 && committed_cells >= 600,
        "the sweep stopped measuring: {obligations} obligations, {committed_cells} cells"
    );
    assert!(
        resolver_declined * 10 < cells.len(),
        "the resolver declined {resolver_declined} of {} cells, so the sweep grades little",
        cells.len()
    );
}

/// The ill-typed family: each value applied at a contradictory type, and one
/// parameter related through two earlier functions of different domains, in
/// both orders. Commits must be only what FCS kept.
#[test]
fn ill_typed_cross_file_uses_commit_only_what_fcs_kept() {
    let cells = cells(true);
    let cross = cross_project(&cells);
    let g = grade("cross_file_ill_typed", &cross);
    let committed: usize = LATER
        .iter()
        .map(|&i| g.binders[i].len() + g.nodes[i].len())
        .sum();
    let rejected: usize = LATER.iter().map(|&i| g.error_lines[i].len()).sum();
    eprintln!(
        "cross-file ill-typed: {} cells, {committed} commits in the later files, {rejected} FCS \
         errors, {} ungraded",
        cells.len(),
        g.ungraded
    );
    assert!(
        rejected >= cells.len() / 2,
        "the ill-typed family is mostly well-typed: {rejected} errors over {} cells",
        cells.len()
    );
    assert!(
        committed >= 300,
        "the ill-typed family committed {committed}"
    );
}

/// A signature is what a later file sees: an attribute only it carries, or an
/// abstract type it declares over the implementation's abbreviation (which F#
/// rejects, and still uses), changes a use's type; a value the signature
/// leaves alone is screened all the same.
#[test]
fn a_signature_screens_every_value_of_its_implementation() {
    let sig = "module S\n\ntype T\n\n[<System.Obsolete(\"e\", true)>]\nval s_ob : int\nval s_ab : T\n\
               val s_id : x: 'a -> 'a\nval s_fi : x: int -> int\n";
    let imp = "module S\n\ntype T = int\n\nlet s_ob = 1\nlet s_ab = 1\nlet s_id x = x\n\
               let s_fi (x: int) = x\n";
    let later = "module B\n\nopen S\n\nlet u1 = s_ob\nlet u2 = s_ab\nlet u3 = s_id 1\n\
                 let u4 = s_fi 1\nlet u6 = (s_ab, 0)\n";
    let qualified = "module C\n\nlet u5 = S.s_fi 1\nlet u7 = S.s_ob\n";
    let folded = fold(&[
        ("S.fsi", sig),
        ("S.fs", imp),
        ("B.fs", later),
        ("C.fs", qualified),
    ]);
    let g = grade("cross_file_signature", &folded);
    for idx in [2, 3] {
        let census = census_by_binding(&folded, idx);
        for (name, reasons) in &census {
            assert!(
                reasons.contains(&Incomplete::PrecedingValue(PrecedingGap::Signature)),
                "{name}: a signature-screened value must decline, census {reasons:?}"
            );
        }
        assert!(
            g.binders[idx].is_empty(),
            "nothing in a later file is typed: {:?}",
            g.binders[idx]
        );
    }
}

/// A declaration after a syntax error is not walked, so a later file's use of
/// it declines as such; one before it is typed.
#[test]
fn a_value_after_a_syntax_error_is_not_published() {
    let a = "module A\n\nlet a_before = 1\nlet a_broken = (1 +\nlet a_after = 2\n";
    let b = "module B\n\nopen A\n\nlet u1 = a_before\nlet u2 = a_after\n";
    let c = "module C\n\nlet u3 = A.a_after\nlet u4 = A.a_before\n";
    let folded = fold(&[("A.fs", a), ("B.fs", b), ("C.fs", c)]);
    let g = grade("cross_file_syntax_error", &folded);
    let (cb, cc) = (census_by_binding(&folded, 1), census_by_binding(&folded, 2));
    assert_eq!(
        g.binders[1].get("u1").map(String::as_str),
        Some("System.Int32"),
        "a value before the error is typed; census {:?}",
        cb["u1"]
    );
    assert_eq!(
        g.binders[2].get("u4").map(String::as_str),
        Some("System.Int32"),
        "a value before the error is typed; census {:?}",
        cc["u4"]
    );
    for (idx, census, name) in [(1, &cb, "u2"), (2, &cc, "u3")] {
        assert!(
            !g.binders[idx].contains_key(name),
            "{name}: a value after a syntax error must not type"
        );
        // The resolver may not resolve the name at all; where it does, the
        // binding was not walked.
        let source = &folded.sources[idx].1;
        let start = source.find(&format!("let {name} ")).expect("binding");
        let end = start + source[start..].find('\n').expect("line end");
        assert!(
            !resolves_cross_file(&folded, idx, start, end)
                || census[name].contains(&Incomplete::PrecedingValue(PrecedingGap::NotWalked)),
            "{name}: census {:?}",
            census[name]
        );
    }
}

/// Which binder a name means is the resolver's answer: a later file's
/// same-named value, or the second of two opened modules, shadows the first,
/// and a qualified use reaches past the shadow. Pinned to FCS per binding:
/// every binder FCS types is committed at its type.
#[test]
fn a_shadowing_value_is_the_one_a_use_instantiates() {
    let a = "module A\n\nlet sh = 1\nlet shf (x: int) = x\nlet shg x = x\n";
    let m = "module M\n\nlet sh = \"s\"\nlet shf (x: string) = x\n";
    let c = "module C\n\nopen A\nopen M\n\nlet u1 = sh\nlet u4 = shf \"t\"\nlet u6 = shg true\n\
             let u7 = sh.Length\nlet sh = true\nlet u8 = sh\n";
    let d = "module D\n\nlet u2 = A.sh\nlet u3 = M.sh\nlet u5 = A.shf 1\nlet u9 = M.shf \"u\"\n";
    let folded = fold(&[("A.fs", a), ("M.fs", m), ("C.fs", c), ("D.fs", d)]);
    let g = grade("cross_file_shadowing", &folded);
    let want = [
        (2, "u1", "System.String"),
        (2, "u4", "System.String"),
        (2, "u6", "System.Boolean"),
        (2, "u7", "System.Int32"),
        (2, "u8", "System.Boolean"),
        (3, "u2", "System.Int32"),
        (3, "u3", "System.String"),
        (3, "u5", "System.Int32"),
        (3, "u9", "System.String"),
    ];
    for (idx, name, ty) in want {
        assert_eq!(
            g.binders[idx].get(name).map(String::as_str),
            Some(ty),
            "{name}: the shadowing value's type"
        );
    }
}

/// Reuse is invisible: after an edit to the last file, the fold that reuses
/// the unchanged files infers exactly what a cold fold does, file for file —
/// and reuses them, so the reuse path is the one under test.
#[test]
fn a_reusing_fold_infers_what_a_cold_fold_does() {
    let cells = cells(false);
    let (a, lib) = (file_a(), file_lib());
    let (q, b) = (file_later(&cells, true), file_later(&cells, false));
    let before = fold(&[("A.fs", &a), ("Lib.fs", &lib), ("Q.fs", &q), ("B.fs", &b)]);
    let edited = format!("{b}let u_edit = a_gid 1\n");
    // The edited file is parsed afresh; the others keep their trees, which is
    // how the resolver's incremental fold knows them unchanged.
    let edited_tree = fold(&[("B.fs", &edited)]).files.remove(0);
    let mut after_files = before.files.clone();
    after_files[3] = ProjectFile::new(
        edited_tree.file,
        after_files[3].qnof.clone(),
        edited_tree.recovery,
    );
    let env = full_bcl_env();
    let (incremental, reused) = borzoi_sema::resolve_project_files_incremental(
        &before.files,
        &before.resolved,
        &after_files,
        env,
    );
    let reused = reused.iter().filter(|r| **r).count();
    assert!(reused >= 3, "the resolver reused {reused} files");
    assert!(before.inferred.shared_prefix(&incremental) >= 3);
    let reusing =
        borzoi_sema::infer_project_reusing(&after_files, &incremental, env, &before.inferred);
    let cold = infer_project(&after_files, &incremental, env);
    for idx in 0..after_files.len() {
        let (r, c) = (reusing.file(idx), cold.file(idx));
        assert_eq!(r.is_some(), c.is_some());
        if let (Some(r), Some(c)) = (r, c) {
            assert_eq!(r.types(), c.types(), "file {idx}: node types");
            assert_eq!(r.def_types(), c.def_types(), "file {idx}: binder types");
            assert_eq!(r.incompleteness(), c.incompleteness(), "file {idx}: census");
        }
    }
    let rf = incremental.file(3);
    let Some(Resolution::Item(id)) = rf
        .resolutions()
        .iter()
        .find(|(range, _)| &edited[**range] == "a_gid" && usize::from(range.start()) > b.len())
        .map(|(_, res)| *res)
    else {
        panic!("the edit's use resolves to `a_gid`");
    };
    assert_eq!(
        reusing.item_type(id).map(|t| t.render()),
        Ok("'a -> 'a".to_string()),
        "the reused file's publication carries over"
    );
}

/// A rejecting attribute on a file's own top-level module header reaches
/// every use of its values from another file — a case no in-file use meets —
/// so each such value is declined, at error and warning level alike.
#[test]
fn a_rejecting_header_module_declines_every_value() {
    let err = "[<System.Obsolete(\"e\", true)>]\nmodule O\n\nlet o_v = 1\nlet o_f (x: int) = x\n";
    let warn = "[<System.Obsolete(\"w\")>]\nmodule OW\n\nlet ow_v = 1\n";
    let qualified =
        "module B\n\nlet u1 = O.o_v\nlet u2 = O.o_f 1\nlet u3 = OW.ow_v\nlet u4 = (O.o_v, 0)\n";
    let opened = "module C\n\nopen OW\n\nlet u5 = ow_v\nlet u6 = ow_v + 1\n";
    let folded = fold(&[
        ("O.fs", err),
        ("OW.fs", warn),
        ("B.fs", qualified),
        ("C.fs", opened),
    ]);
    let g = grade("cross_file_rejecting_header", &folded);
    for idx in [2, 3] {
        let census = census_by_binding(&folded, idx);
        let source = &folded.sources[idx].1;
        for (name, reasons) in &census {
            let start = source.find(&format!("let {name} ")).expect("binding");
            let end = start + source[start..].find('\n').expect("line end");
            if resolves_cross_file(&folded, idx, start, end) {
                assert!(
                    reasons.contains(&Incomplete::PrecedingValue(PrecedingGap::RejectedUse)),
                    "{name}: a value of a rejecting module must decline, census {reasons:?}"
                );
            }
        }
    }
    assert!(
        g.binders[2].is_empty() && g.binders[3].is_empty(),
        "nothing in a later file is typed: {:?} {:?}",
        g.binders[2],
        g.binders[3]
    );
}
