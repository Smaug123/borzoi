//! Infix operators against FCS's typed tree: an application `a OP b` whose
//! operator resolves to FSharp.Core's own, typed as the curried application
//! `OP a b` of the operator's signature.
//!
//! # What is compared
//!
//! Soundness (D5), in the certain-implies-exact direction, against both FCS
//! typed-tree oracles: every expression type and every binder type we commit,
//! FCS reports identically at the same range. Deferring is always allowed.
//!
//! # The grid
//!
//! Generated rather than curated: every operator in [`OPS`], with each operand
//! drawn from every [`Kind`] (a literal, an annotated parameter, an unannotated
//! one, a lambda, a nested application, an ill-typed operand), in every
//! [`Position`] (the body of a function, under a return annotation, as an
//! argument, and applied as a callee). The well-typed rows must check clean in
//! FCS, which validates the generator; the ill-typed rows exercise FCS's
//! recovery, which unifies in source order and keeps no node inside what it
//! rejects.
//!
//! # Vacuity
//!
//! Every assertion above passes on an implementation that commits nothing, so
//! the grid also pins what the slice does: the operators it types leave their
//! bindings complete (no operator reason in the census), the ones it declines
//! say why, and an in-file operator — whose result its definition fixes —
//! commits at the application.
//!
//! # Pipes, composition, and the order of argument checks
//!
//! A pipe's result is fixed only through an argument whose domain is a
//! function type (`|>`'s `'T1 -> 'U`), so it commits only where such a check
//! discharges — and that is sound only where the value piped has the type FCS
//! gives its use (FCS loosens a parameter that is not sealed at every use of
//! a value) and where the order FCS checks in cannot matter. The pipe grid
//! ([`pipes_and_composition_over_in_file_functions_agree_with_fcs`]) grades the
//! first, with a per-row obligation that the well-typed rows commit; the
//! use-order grid and the argument-check generator grade the second, over
//! programs FCS mostly rejects.

use std::collections::HashMap;

use std::path::Path;

use crate::common::{
    full_bcl_env, invoke_fcs_dump, invoke_fcs_dump_with_refs, module_value_fixture_env,
    module_value_fixture_refs, parse_fcs_binder_types_with_errors, parse_fcs_types_with_errors,
    temp_fs_file, try_invoke_fcs_dump_with_refs,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind};
use borzoi_sema::{
    AssemblyEnv, Incomplete, OperatorGap, ProjectItems, SyntaxRecovery, infer_file, resolve_file,
};

/// The types an operand or a result can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Int,
    Bool,
    Str,
    /// `int -> int`.
    IntFn,
    /// `int list`.
    IntList,
    /// `int * int`.
    Pair,
    /// `int -> int -> int`.
    Curried,
    /// `int ref`.
    RefInt,
    Unit,
}

impl Sort {
    fn annotation(self) -> &'static str {
        match self {
            Sort::Int => "int",
            Sort::Bool => "bool",
            Sort::Str => "string",
            Sort::IntFn => "int -> int",
            Sort::IntList => "int list",
            Sort::Pair => "int * int",
            Sort::Curried => "int -> int -> int",
            Sort::RefInt => "int ref",
            Sort::Unit => "unit",
        }
    }

    /// The annotated parameter of this sort every generated function takes.
    fn param(self) -> &'static str {
        match self {
            Sort::Int => "ai",
            Sort::Bool => "bi",
            Sort::Str => "si",
            Sort::IntFn => "fi",
            Sort::IntList => "li",
            Sort::Pair => "pi",
            Sort::Curried => "ci",
            Sort::RefInt => "ri",
            Sort::Unit => "ui",
        }
    }
}

const SORTS: [Sort; 9] = [
    Sort::Int,
    Sort::Bool,
    Sort::Str,
    Sort::IntFn,
    Sort::IntList,
    Sort::Pair,
    Sort::Curried,
    Sort::RefInt,
    Sort::Unit,
];

/// How an operand is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Literal,
    Annotated,
    /// An unannotated parameter, whose type only the operator fixes.
    Open,
    Lambda,
    /// An application, itself sometimes an operator's.
    Nested,
    /// An operand of the wrong type.
    IllTyped,
}

const KINDS: [Kind; 6] = [
    Kind::Literal,
    Kind::Annotated,
    Kind::Open,
    Kind::Lambda,
    Kind::Nested,
    Kind::IllTyped,
];

/// The operand of `sort` spelled as `kind`; `open` names the unannotated
/// parameter to use.
fn operand(sort: Sort, kind: Kind, open: &str) -> String {
    let s = match (kind, sort) {
        (Kind::Literal, Sort::Int) => "1",
        (Kind::Literal, Sort::Bool) => "true",
        (Kind::Literal, Sort::Str) => "\"s\"",
        (Kind::Literal, Sort::IntFn) => "inc",
        (Kind::Literal, Sort::IntList) => "[1]",
        (Kind::Literal, Sort::Pair) => "(1, 2)",
        (Kind::Literal, Sort::Curried) => "add2",
        (Kind::Literal, Sort::RefInt) => "(ref 1)",
        (Kind::Literal, Sort::Unit) => "()",
        (Kind::Annotated, s) => s.param(),
        (Kind::Open, _) => open,
        (Kind::Lambda, Sort::IntFn) => "(fun (x: int) -> x)",
        (Kind::Lambda, Sort::Curried) => "(fun (x: int) (y: int) -> x)",
        (Kind::Lambda, s) => {
            return format!("((fun () -> {}) ())", operand(s, Kind::Literal, open));
        }
        (Kind::Nested, Sort::Int) => "(1 |> inc)",
        (Kind::Nested, Sort::Bool) => "(bi || true)",
        (Kind::Nested, Sort::Str) => "(idf \"s\")",
        (Kind::Nested, Sort::IntFn) => "(inc >> inc)",
        (Kind::Nested, Sort::IntList) => "(li @ [2])",
        (Kind::Nested, Sort::Pair) => "(idf (1, 2))",
        (Kind::Nested, Sort::Curried) => "(idf add2)",
        (Kind::Nested, Sort::RefInt) => "(idf ri)",
        (Kind::Nested, Sort::Unit) => "(idf ())",
        (Kind::IllTyped, Sort::Int) => "\"s\"",
        (Kind::IllTyped, Sort::Bool) => "1",
        (Kind::IllTyped, Sort::Str) => "1",
        (Kind::IllTyped, Sort::IntFn) => "incs",
        (Kind::IllTyped, Sort::IntList) => "[\"s\"]",
        (Kind::IllTyped, Sort::Pair) => "1",
        (Kind::IllTyped, Sort::Curried) => "inc",
        (Kind::IllTyped, Sort::RefInt) => "1",
        (Kind::IllTyped, Sort::Unit) => "1",
    };
    s.to_string()
}

/// What inference makes of an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Typed as an application of its signature: the binding stays complete.
    Typed,
    /// Declined, for this reason.
    Declined(OperatorGap),
}

/// An infix operator, its operand sorts and its result sort, as FSharp.Core
/// declares it.
struct Op {
    spelling: &'static str,
    lhs: Sort,
    rhs: Sort,
    result: Sort,
    expect: Expect,
    /// FCS rejects every use (`^` is obsolete as an error).
    always_ill_typed: bool,
}

const fn op(spelling: &'static str, lhs: Sort, rhs: Sort, result: Sort, expect: Expect) -> Op {
    Op {
        spelling,
        lhs,
        rhs,
        result,
        expect,
        always_ill_typed: false,
    }
}

const OPS: &[Op] = &[
    op("|>", Sort::Int, Sort::IntFn, Sort::Int, Expect::Typed),
    op("<|", Sort::IntFn, Sort::Int, Sort::Int, Expect::Typed),
    op(">>", Sort::IntFn, Sort::IntFn, Sort::IntFn, Expect::Typed),
    op("<<", Sort::IntFn, Sort::IntFn, Sort::IntFn, Expect::Typed),
    // `LanguagePrimitives.IntrinsicOperators`, an auto-open the resolver does
    // not fold, so the token resolves to nothing.
    op(
        "&&",
        Sort::Bool,
        Sort::Bool,
        Sort::Bool,
        Expect::Declined(OperatorGap::Unresolved),
    ),
    op(
        "||",
        Sort::Bool,
        Sort::Bool,
        Sort::Bool,
        Expect::Declined(OperatorGap::Unresolved),
    ),
    // In-file operators ([`HEADER`]): an application of their definition,
    // whose result their annotations fix.
    op("+.", Sort::Int, Sort::Int, Sort::Int, Expect::Typed),
    op("&&.", Sort::Bool, Sort::Bool, Sort::Bool, Expect::Typed),
    // Signatures over FSharp.Core's generic types, which F# names under a
    // compiled name (`'T list` is `FSharpList<'T>`, `'T ref` is `FSharpRef<'T>`).
    op(
        "@",
        Sort::IntList,
        Sort::IntList,
        Sort::IntList,
        Expect::Typed,
    ),
    op(":=", Sort::RefInt, Sort::Int, Sort::Unit, Expect::Typed),
    // A tupled argument group beside a curried one, split by the pickle's
    // group sizes.
    op("||>", Sort::Pair, Sort::Curried, Sort::Int, Expect::Typed),
    op("<||", Sort::Curried, Sort::Pair, Sort::Int, Expect::Typed),
    Op {
        always_ill_typed: true,
        ..op(
            "^",
            Sort::Str,
            Sort::Str,
            Sort::Str,
            Expect::Declined(OperatorGap::Signature),
        )
    },
    // Statically resolved, or `equality`-constrained.
    op(
        "+",
        Sort::Int,
        Sort::Int,
        Sort::Int,
        Expect::Declined(OperatorGap::Constrained),
    ),
    op(
        "=",
        Sort::Int,
        Sort::Int,
        Sort::Bool,
        Expect::Declined(OperatorGap::Constrained),
    ),
];

/// Where the application sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// The body of an unannotated function: a synth position.
    Synth,
    /// The body of a function with a return annotation: a check position.
    Annotated,
    /// The argument of an application.
    Argument,
    /// Applied to an argument.
    Callee,
}

const POSITIONS: [Position; 4] = [
    Position::Synth,
    Position::Annotated,
    Position::Argument,
    Position::Callee,
];

/// The functions every generated file applies, and the parameters every
/// generated function takes.
const HEADER: &str = "module Ops\n\
                      let idf x = x\n\
                      let inc (x: int) = 1\n\
                      let incs (t: string) = 2\n\
                      let add2 (x: int) (y: int) = 3\n\
                      let (+.) (a: int) (b: int) = 4\n\
                      let (&&.) (a: bool) (b: bool) = a\n";

/// The parameters of a function whose body is `body`: the annotated
/// parameter of each sort the body names, then the unannotated `p` and `q`.
///
/// Only the named ones, because an annotation outside the modelled set (a
/// function or list type) marks the whole binding incomplete, and a complete
/// binding is where argument checks fire — the case worth grading.
fn params(body: &str) -> String {
    let mut out = String::new();
    for s in SORTS {
        let named = body
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == s.param());
        if named {
            out.push_str(&format!("({}: {}) ", s.param(), s.annotation()));
        }
    }
    out.push_str("p q");
    out
}

/// One generated row: the application's source range, and whether it sits in
/// a synth position whose operands are all modelled.
pub(crate) struct Row {
    pub(crate) range: (usize, usize),
    pub(crate) synth: bool,
}

/// Where a grid's functions are defined: in the file, by the grid's header,
/// or in the module-value fixture assembly, as a module the file opens
/// ([`module_value_corpus::RELOCATED`](crate::common::module_value_corpus::RELOCATED)).
/// FCS checks the two programs alike, so a grid's rows grade both routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Defs {
    InFile,
    Assembly,
    /// In the file, which also opens a fixture module it uses nothing of:
    /// the in-file baseline a relocated grid is compared with, since opening
    /// an F# module is itself a perturbation (an opened module may declare
    /// extension members, so the method-call engine declines a static call
    /// under one).
    InFileBesideOpen,
}

impl Defs {
    /// The file's header: the grid's own, or an `open` of its relocated
    /// module in its place.
    pub(crate) fn header(self, in_file: &str) -> String {
        match self {
            Defs::InFile => in_file.to_string(),
            Defs::InFileBesideOpen => {
                let (module_line, rest) = in_file
                    .split_once('\n')
                    .expect("a grid header starts with its module line");
                format!(
                    "{module_line}\nopen {}.{}\n{rest}",
                    crate::common::module_value_corpus::NS,
                    crate::common::module_value_corpus::MODULE
                )
            }
            Defs::Assembly => {
                let module = in_file
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("module "))
                    .expect("a grid header starts with its module line");
                crate::common::module_value_corpus::relocated_probe_header(module)
            }
        }
    }

    /// The env inference reads, and the references the oracle checks with.
    fn references(self) -> (&'static AssemblyEnv, Vec<&'static Path>) {
        match self {
            Defs::InFile => (full_bcl_env(), Vec::new()),
            Defs::Assembly | Defs::InFileBesideOpen => {
                (module_value_fixture_env(), module_value_fixture_refs())
            }
        }
    }
}

/// Append a function applying `op` to operands of `lk` and `rk` in `pos`,
/// returning its row, or `None` when the combination is not a program of the
/// family asked for (`ill_typed`).
fn push_row(
    src: &mut String,
    n: usize,
    op: &Op,
    lk: Kind,
    rk: Kind,
    pos: Position,
    ill_typed: bool,
) -> Option<Row> {
    let callee_ok = op.result == Sort::IntFn;
    let row_ill = op.always_ill_typed
        || lk == Kind::IllTyped
        || rk == Kind::IllTyped
        || (pos == Position::Callee && !callee_ok);
    if row_ill != ill_typed {
        return None;
    }
    let lhs = operand(op.lhs, lk, "p");
    let rhs = operand(op.rhs, rk, "q");
    let app = format!("{lhs} {} {rhs}", op.spelling);
    let (head, before, after) = match pos {
        Position::Synth => (String::new(), "", ""),
        Position::Annotated => (format!(" : {}", op.result.annotation()), "", ""),
        Position::Argument => (String::new(), "idf (", ")"),
        Position::Callee => (String::new(), "(", ") 1"),
    };
    let prefix = format!("let r{n} {}{head} = {before}", params(&app));
    let start = src.len() + prefix.len();
    src.push_str(&prefix);
    src.push_str(&app);
    src.push_str(after);
    src.push('\n');
    Some(Row {
        range: (start, start + app.len()),
        synth: pos == Position::Synth,
    })
}

/// What one checked file committed.
#[derive(Debug, Default)]
pub(crate) struct Checked {
    pub(crate) exprs: usize,
    pub(crate) binders: usize,
    /// Committed expression types at a row's whole application, in synth
    /// position.
    pub(crate) at_application: usize,
    /// The synth rows committed at their whole application.
    pub(crate) committed_rows: std::collections::HashSet<(usize, usize)>,
    /// FCS errors on the file.
    pub(crate) errors: usize,
    /// Every incompleteness reason any binding recorded.
    pub(crate) reasons: Vec<Incomplete>,
    /// FCS's expression types, by range.
    pub(crate) fcs_types: HashMap<(usize, usize), String>,
    /// Every commit: an expression's or a binder's range, which of the two,
    /// and the type.
    pub(crate) commits: Vec<(usize, usize, &'static str, String)>,
}

/// Resolve, infer and diff `src` against both oracles. `None` when FCS cannot
/// produce a typed tree for the file (it throws materialising some rejected
/// operators), which the ill-typed family counts rather than grades.
fn check(label: &str, src: &str, rows: &[Row], expect_clean: bool) -> Option<Checked> {
    check_in(Defs::InFile, label, src, rows, expect_clean)
}

/// [`check`], with the grid's functions defined where `defs` says.
pub(crate) fn check_in(
    defs: Defs,
    label: &str,
    src: &str,
    rows: &[Row],
    expect_clean: bool,
) -> Option<Checked> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "generated source has parse errors: {:?}\n{src}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let (env, refs) = defs.references();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let path = temp_fs_file(label, src);
    let oracle = |sub: &str| {
        if expect_clean {
            Some(invoke_fcs_dump_with_refs(sub, &path, &refs))
        } else {
            try_invoke_fcs_dump_with_refs(sub, &path, &refs).ok()
        }
    };
    let jsons = oracle("types").zip(oracle("binder-types"));
    let _ = std::fs::remove_file(&path);
    let (types_json, binders_json) = jsons?;
    let (fcs_types, errors) = parse_fcs_types_with_errors(&types_json, src);
    let (fcs_binders, _) = parse_fcs_binder_types_with_errors(&binders_json, src);
    if expect_clean {
        assert!(
            errors.is_empty(),
            "the generator produced a program FCS rejects: {errors:?}\n{src}"
        );
    }

    let synth_rows: std::collections::HashSet<(usize, usize)> =
        rows.iter().filter(|r| r.synth).map(|r| r.range).collect();
    let mut c = Checked {
        errors: errors.len(),
        reasons: inferred
            .incompleteness()
            .iter()
            .flatten()
            .copied()
            .collect(),
        ..Checked::default()
    };
    // Every disagreement in the file, so one run names the whole class.
    let mut wrong: Vec<String> = Vec::new();
    // The enclosing line of a range, which names the generated row.
    let line_of = |at: usize| {
        let start = src[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = src[at..].find('\n').map_or(src.len(), |i| at + i);
        &src[start..end]
    };
    for (range, ty) in inferred.types() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let ours = ty.render();
        match fcs_types.get(&key) {
            None => wrong.push(format!(
                "`{}` at {key:?} typed `{ours}`, but FCS has no node there: {}",
                &src[key.0..key.1],
                line_of(key.0)
            )),
            Some(theirs) if *theirs != ours => wrong.push(format!(
                "`{}` at {key:?} typed `{ours}`, FCS `{theirs}`: {}",
                &src[key.0..key.1],
                line_of(key.0)
            )),
            Some(_) => {}
        }
        c.exprs += 1;
        c.commits.push((key.0, key.1, "expr", ours.clone()));
        if synth_rows.contains(&key) {
            c.at_application += 1;
            c.committed_rows.insert(key);
        }
    }
    for (def_id, ty) in inferred.def_types() {
        let def = resolved.def(*def_id);
        let key = (
            u32::from(def.range.start()) as usize,
            u32::from(def.range.end()) as usize,
        );
        let ours = ty.render();
        match fcs_binders.get(&key) {
            None => wrong.push(format!(
                "binder `{}` typed `{ours}`, but FCS has no binder there: {}",
                def.name,
                line_of(key.0)
            )),
            Some(theirs) if *theirs != ours => wrong.push(format!(
                "binder `{}` typed `{ours}`, FCS `{theirs}`: {}",
                def.name,
                line_of(key.0)
            )),
            Some(_) => {}
        }
        c.binders += 1;
        c.commits.push((key.0, key.1, "binder", ours));
    }
    assert!(
        wrong.is_empty(),
        "{} commits disagree with FCS in `{label}`:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    c.fcs_types = fcs_types;
    Some(c)
}

fn is_operator_reason(r: &Incomplete) -> bool {
    matches!(
        r,
        Incomplete::Operator(_) | Incomplete::CalleeShape(SyntaxKind::INFIX_APP_EXPR)
    )
}

/// The well-typed grid: every operator, every operand kind on each side, every
/// position. FCS must accept each file; every type we commit, it must agree
/// with; a typed operator leaves no operator reason in the census, and a
/// declined one records exactly its own.
#[test]
fn well_typed_operator_grid_agrees_with_fcs() {
    let mut at_application: HashMap<&str, usize> = HashMap::new();
    let mut committed = 0;
    for op in OPS.iter().filter(|o| !o.always_ill_typed) {
        let mut src = String::from(HEADER);
        let mut rows = Vec::new();
        let mut n = 0;
        for lk in KINDS {
            for rk in KINDS {
                for pos in POSITIONS {
                    if let Some(row) = push_row(&mut src, n, op, lk, rk, pos, false) {
                        rows.push(row);
                        n += 1;
                    }
                }
            }
        }
        let c = check("operators_well_typed", &src, &rows, true).expect("a clean file checks");
        committed += c.exprs + c.binders;
        *at_application.entry(op.spelling).or_default() += c.at_application;
        // Each row's operands nest the slice's own operators (`1 |> inc`,
        // `inc >> inc`, `bi || true`, `li @ [2]`), so the census of a file
        // mixes them; the reasons owed to *this* operator are what is graded.
        match op.expect {
            Expect::Typed => {
                let nested_declines = [OperatorGap::Signature, OperatorGap::Unresolved];
                for r in c.reasons.iter().filter(|r| is_operator_reason(r)) {
                    assert!(
                        matches!(r, Incomplete::Operator(g) if nested_declines.contains(g)),
                        "`{}` is typed, yet a binding records {r:?}\n{src}",
                        op.spelling
                    );
                }
            }
            Expect::Declined(gap) => assert!(
                c.reasons.contains(&Incomplete::Operator(gap)),
                "`{}` is declined as {gap:?}, which no binding records: {:?}",
                op.spelling,
                c.reasons
            ),
        }
    }
    eprintln!("well-typed grid: {committed} commits; at the application: {at_application:?}");
    // The in-file operators' annotations fix their result, so every synth row
    // commits it: one per well-typed operand-kind pair. So does `:=`'s
    // signature, whose result is `unit` whatever its operands.
    for spelling in ["+.", "&&.", ":="] {
        assert_eq!(
            at_application.get(spelling).copied().unwrap_or(0),
            25,
            "`{spelling}` should type every well-typed synth row at the application"
        );
    }
}

/// The ill-typed grid: an operand of the wrong type on either side, `^` (which
/// FCS rejects as obsolete), and an application of a result that is not a
/// function. FCS recovers by retyping what it rejected, in source order; every
/// type we commit must still be one it kept.
#[test]
fn ill_typed_operator_grid_commits_only_what_fcs_kept() {
    let mut graded = 0;
    let mut ungradable = Vec::new();
    let mut errors = 0;
    for op in OPS {
        let mut src = String::from(HEADER);
        let mut rows = Vec::new();
        let mut n = 0;
        for lk in KINDS {
            for rk in KINDS {
                for pos in POSITIONS {
                    if let Some(row) = push_row(&mut src, n, op, lk, rk, pos, true) {
                        rows.push(row);
                        n += 1;
                    }
                }
            }
        }
        match check("operators_ill_typed", &src, &rows, false) {
            Some(c) => {
                graded += 1;
                errors += c.errors;
            }
            None => ungradable.push(op.spelling),
        }
    }
    eprintln!(
        "ill-typed grid: {graded} files graded, {errors} FCS errors, ungradable: {ungradable:?}"
    );
    assert!(
        errors >= 100,
        "the ill-typed grid stopped producing ill-typed programs: {errors}"
    );
    // The constrained operators' ill-typed rows violate their constraints,
    // which the oracle cannot materialise; every other file must be graded.
    for spelling in &ungradable {
        assert!(
            OPS.iter().any(|o| o.spelling == *spelling
                && o.expect == Expect::Declined(OperatorGap::Constrained)),
            "`{spelling}`'s ill-typed file could not be graded"
        );
    }
}

/// A shadowed operator is the shadow's, never FSharp.Core's. Each shadow gives
/// `&&` (whose FSharp.Core result is `bool`) and `|>` an `int`/`string`
/// result, so a FSharp.Core reading would disagree with FCS at the
/// application; the certain-implies-exact check is the test, and the floor
/// says the shadow itself is typed.
#[test]
fn a_shadowed_operator_is_never_fsharp_cores() {
    let cases: &[(&str, &str)] = &[
        (
            "in-file, spelled",
            "let (&&) (a: bool) (b: bool) = 1\nlet (|>) (a: int) (b: int) = \"s\"\n",
        ),
        (
            "in-file, compiled name",
            "let op_BooleanAnd (a: bool) (b: bool) = 1\nlet op_PipeRight (a: int) (b: int) = \"s\"\n",
        ),
        (
            "opened module",
            "module Shadow =\n    let (&&) (a: bool) (b: bool) = 1\n    let (|>) (a: int) (b: int) = \"s\"\nopen Shadow\n",
        ),
    ];
    for (label, defs) in cases {
        let mut src = format!("module S\n{defs}");
        let mut rows = Vec::new();
        for (i, app) in ["true && false", "1 |> 2"].iter().enumerate() {
            let prefix = format!("let v{i} = ");
            let start = src.len() + prefix.len();
            src.push_str(&prefix);
            src.push_str(app);
            src.push('\n');
            rows.push(Row {
                range: (start, start + app.len()),
                synth: true,
            });
        }
        let c = check("operators_shadowed", &src, &rows, true).expect("a clean file checks");
        assert_eq!(
            c.at_application, 2,
            "{label}: the shadowing definition types both applications\n{src}"
        );
    }
    // A local shadow, inside the function that uses it.
    let src = "module S\nlet f () =\n    let (&&) (a: bool) (b: bool) = 1\n    true && false\n";
    check("operators_shadowed", src, &[], true).expect("a clean file checks");
}

/// The census names an operator the slice declines by why, and an in-file
/// operator is an ordinary application of its definition.
#[test]
fn an_in_file_operator_is_an_application_of_its_definition() {
    let src = "module S\nlet (+.) (a: bool) (b: int) = \"s\"\nlet v = true +. 1\n";
    let start = src.find("true +. 1").unwrap();
    let rows = [Row {
        range: (start, start + "true +. 1".len()),
        synth: true,
    }];
    let c = check("operators_in_file", src, &rows, true).expect("a clean file checks");
    assert_eq!(c.at_application, 1, "`true +. 1` types as `string`");
}

/// The signature reading itself, against FCS: every function of FSharp.Core's
/// operator modules that inference reads a type off, bound to a value, has
/// exactly the type FCS gives that value. This grades the reading where the
/// grid cannot — the grid commits an operator's result only where its operands fix it
/// — and over every function the rule admits, not only the operators.
#[test]
fn every_read_fsharp_core_signature_is_the_type_fcs_gives_it() {
    let env = full_bcl_env();
    let core = ["Microsoft", "FSharp", "Core"].map(String::from);
    let mut src = String::from("module Sigs\n");
    let mut ours: HashMap<(usize, usize), (String, String)> = HashMap::new();
    for module in ["Operators", "ExtraTopLevelOperators"] {
        let handle = env
            .lookup_type(&core, module, 0)
            .unwrap_or_else(|| panic!("FSharp.Core declares {module}"));
        for (name, ty) in borzoi_sema::test_support::module_function_types(env, handle) {
            let binder = format!("v{}", ours.len());
            let start = src.len() + "let ".len();
            src.push_str(&format!(
                "let {binder} = Microsoft.FSharp.Core.{module}.``{name}``\n"
            ));
            ours.insert(
                (start, start + binder.len()),
                (format!("{module}.{name}"), ty),
            );
        }
    }
    let path = temp_fs_file("operators_signatures", &src);
    let json = invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let (fcs, errors) = parse_fcs_binder_types_with_errors(&json, &src);
    // FS0417 rejects a first-class `reraise` after inference
    // (`PostInferenceChecks.CheckValRef`), which leaves its type standing.
    let errors: Vec<_> = errors.into_iter().filter(|e| e.code != 417).collect();
    assert!(
        errors.is_empty(),
        "FCS rejects a binding: {errors:?}\n{src}"
    );
    for (key, (name, ty)) in &ours {
        let theirs = fcs
            .get(key)
            .unwrap_or_else(|| panic!("FCS reports no binder for {name}\n{src}"));
        assert_eq!(ty, theirs, "{name}'s signature read as its F# type");
    }
    let mut names: Vec<&str> = ours.values().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    eprintln!("signatures read: {}: {names:?}", ours.len());
    // `|>`, `<|`, `>>`, `<<`, `id`, `ignore`, `not`, `box`, `unbox`, `failwith`, …
    assert!(
        ours.len() >= 12,
        "vacuous: only {} signatures read\n{src}",
        ours.len()
    );
}

/// A use of an in-file definition F# rejects for its attributes — an
/// error-level `[<CompilerMessage>]` or `[<Obsolete>]`, or
/// `[<RequiresExplicitTypeArguments>]` written without them — is typed by FCS
/// as its error type, not by the definition: each attribute × each use form
/// (infix, applied, bound as a value), so what we commit must still be what
/// FCS kept. The warning-level twins type normally, and are the control.
#[test]
fn a_use_f_sharp_rejects_for_its_attributes_commits_nothing_wrong() {
    let attributes = [
        "[<CompilerMessage(\"no\", 9001, IsError = true)>]",
        "[<System.Obsolete(\"no\", true)>]",
        "[<RequiresExplicitTypeArguments>]",
        "[<CompilerMessage(\"no\", 9001)>]",
        "[<System.Obsolete(\"no\")>]",
    ];
    let mut src = String::from("module S\n");
    for (i, attr) in attributes.iter().enumerate() {
        let op = format!("+{}", ".".repeat(i + 1));
        src.push_str(&format!(
            "{attr}\nlet ({op}) (a: int) (b: int) = \"s\"\n\
             {attr}\nlet f{i}<'T> (a: int) = \"s\"\n\
             let infix{i} = 1 {op} 2\n\
             let applied{i} = f{i} 1\n\
             let value{i} = f{i}\n\
             let local{i} () =\n    let {attr} g (a: int) = \"s\"\n    g 1\n"
        ));
    }
    let c = check("operators_rejected", &src, &[], false).expect("FCS checks the file");
    assert!(c.errors >= 3, "the error-level attributes reject: {c:?}");
}

/// A module F# will not open — one carrying an error-level `[<Obsolete>]` or
/// `[<CompilerMessage>]`, which FCS reports at the `open` and then leaves
/// unopened — contributes nothing to the names after it: `&&` stays
/// FSharp.Core's, typed `bool`, though the module defines a `&&` returning
/// `string`. Each attribute × each use form (infix, applied, bound as a
/// value, qualified); the warning-level twins open normally and are the
/// control.
#[test]
fn a_definition_in_a_module_f_sharp_will_not_open_commits_nothing_wrong() {
    let attributes = [
        "[<System.Obsolete(\"no\", true)>]",
        "[<CompilerMessage(\"no\", 9001, IsError = true)>]",
        "[<System.Obsolete(\"no\")>]",
        "[<CompilerMessage(\"no\", 9001)>]",
    ];
    for (i, attr) in attributes.iter().enumerate() {
        let src = format!(
            "module S\n\
             {attr}\n\
             module Deprecated =\n    \
                 let (&&) (a: bool) (b: bool) = \"s\"\n    \
                 let f (a: int) = \"s\"\n\
             open Deprecated\n\
             let infix = true && false\n\
             let applied = f 1\n\
             let value = f\n\
             let qualified = Deprecated.f 1\n"
        );
        check(&format!("operators_module_rejected_{i}"), &src, &[], false)
            .unwrap_or_else(|| panic!("FCS checks the file\n{src}"));
    }
}

/// An in-file operator grounds its operands as any applied function does:
/// `&&.` over `bool`s makes both parameters `bool`.
#[test]
fn an_in_file_operator_grounds_its_operands() {
    let src = "module S\nlet (&&.) (a: bool) (b: bool) = a\nlet both p q = p &&. q\n";
    let c = check("operators_ground", src, &[], true).expect("a clean file checks");
    assert_eq!(
        c.binders, 2,
        "`(&&.)` and `both` are `bool -> bool -> bool`\n{src}"
    );
}

// ---------------------------------------------------------------------------
// Pipes and composition over in-file functions.
// ---------------------------------------------------------------------------

/// The functions the pipe grid pipes into. `fo`, `fc`, `fv` and `addo` take a
/// parameter of a type that is not sealed, which FCS loosens at each use of
/// the function (`MakeApplicableExprWithFlex`): to a fresh type variable that
/// is a subtype of it. `opn` is generic.
pub(crate) const PIPE_HEADER: &str = "module Pipes\n\
                           let idf x = x\n\
                           let inc (x: int) = 1\n\
                           let incs (t: string) = 2\n\
                           let opn x = 1\n\
                           let fo (x: obj) = 1\n\
                           let fc (x: System.IComparable) = 1\n\
                           let fv (x: System.ValueType) = 1\n\
                           let add2 (x: int) (y: int) = 3\n\
                           let addo (x: int) (y: obj) = 3\n";

/// The type of an operand, as far as the grid needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpTy {
    Int,
    Str,
    /// An unannotated parameter, which the function it meets fixes.
    Open,
}

/// The function a pipe or composition is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeFn {
    /// `int -> int`.
    Inc,
    /// `string -> int`.
    Incs,
    /// `'a -> int`.
    Opn,
    /// `obj -> int`: loosened, to a subtype of `obj`, which every type is.
    Fo,
    /// `IComparable -> int`: loosened to a bounded subtype.
    Fc,
    /// `ValueType -> int`: loosened to a bounded subtype, of a class.
    Fv,
    /// `'a -> 'a`.
    Idf,
    /// A lambda, whose function type inference does not model.
    Lambda,
    /// A partial application, `int -> int`.
    Add2,
    /// A partial application whose remaining parameter is loosened.
    Addo,
    /// A composition, `int -> int`.
    Composed,
    /// A composition through a generic function, `int -> int`.
    IdfComposed,
}

const PIPE_FNS: [PipeFn; 12] = [
    PipeFn::Inc,
    PipeFn::Incs,
    PipeFn::Opn,
    PipeFn::Fo,
    PipeFn::Fc,
    PipeFn::Fv,
    PipeFn::Idf,
    PipeFn::Lambda,
    PipeFn::Add2,
    PipeFn::Addo,
    PipeFn::Composed,
    PipeFn::IdfComposed,
];

impl PipeFn {
    fn spelling(self) -> &'static str {
        match self {
            PipeFn::Inc => "inc",
            PipeFn::Incs => "incs",
            PipeFn::Opn => "opn",
            PipeFn::Fo => "fo",
            PipeFn::Fc => "fc",
            PipeFn::Fv => "fv",
            PipeFn::Idf => "idf",
            PipeFn::Lambda => "(fun (x: int) -> x)",
            PipeFn::Add2 => "(add2 1)",
            PipeFn::Addo => "(addo 1)",
            PipeFn::Composed => "(inc >> inc)",
            PipeFn::IdfComposed => "(idf >> inc)",
        }
    }

    /// Whether FCS accepts an argument of type `ty`.
    fn accepts(self, ty: OpTy) -> bool {
        match (self, ty) {
            (_, OpTy::Open) => true,
            (
                PipeFn::Inc
                | PipeFn::Lambda
                | PipeFn::Add2
                | PipeFn::Composed
                | PipeFn::IdfComposed
                | PipeFn::Fv,
                OpTy::Str,
            ) => false,
            (PipeFn::Incs, OpTy::Int) => false,
            _ => true,
        }
    }

    /// The type of the function applied to an argument of type `ty`.
    fn result(self, ty: OpTy) -> OpTy {
        match self {
            PipeFn::Idf => ty,
            _ => OpTy::Int,
        }
    }

    /// Whether inference must type an application of this function: every
    /// parameter is sealed, a type parameter, or loosened only to a subtype of
    /// `obj`, a bound every type meets. A loosening to any other bound is a
    /// subtype constraint inference does not model, and a lambda's type is not
    /// modelled at all.
    fn must_type(self) -> bool {
        !matches!(self, PipeFn::Fc | PipeFn::Fv | PipeFn::Lambda)
    }
}

/// An operand of the pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeOperand {
    Literal,
    /// The annotated `ai: int`.
    Annotated,
    /// The unannotated `p`.
    Open,
    Nested,
    /// A string, which some functions reject.
    Str,
}

const PIPE_OPERANDS: [PipeOperand; 5] = [
    PipeOperand::Literal,
    PipeOperand::Annotated,
    PipeOperand::Open,
    PipeOperand::Nested,
    PipeOperand::Str,
];

impl PipeOperand {
    fn spelling(self) -> &'static str {
        match self {
            PipeOperand::Literal => "1",
            PipeOperand::Annotated => "ai",
            PipeOperand::Open => "p",
            PipeOperand::Nested => "(1 |> inc)",
            PipeOperand::Str => "\"s\"",
        }
    }

    fn ty(self) -> OpTy {
        match self {
            PipeOperand::Open => OpTy::Open,
            PipeOperand::Str => OpTy::Str,
            _ => OpTy::Int,
        }
    }
}

/// How the function and the operand are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeShape {
    /// `x |> f`.
    Pipe,
    /// `f <| x`.
    BackPipe,
    /// `x |> f |> idf`.
    Chain,
    /// `(f >> idf) x`.
    Compose,
    /// `(idf << f) x`.
    ComposeBack,
    /// `f >> idf`, a function value: no operand.
    ComposeValue,
}

const PIPE_SHAPES: [PipeShape; 6] = [
    PipeShape::Pipe,
    PipeShape::BackPipe,
    PipeShape::Chain,
    PipeShape::Compose,
    PipeShape::ComposeBack,
    PipeShape::ComposeValue,
];

impl PipeShape {
    fn render(self, f: &str, x: &str) -> String {
        match self {
            PipeShape::Pipe => format!("{x} |> {f}"),
            PipeShape::BackPipe => format!("{f} <| {x}"),
            PipeShape::Chain => format!("{x} |> {f} |> idf"),
            PipeShape::Compose => format!("({f} >> idf) {x}"),
            PipeShape::ComposeBack => format!("(idf << {f}) {x}"),
            PipeShape::ComposeValue => format!("{f} >> idf"),
        }
    }
}

/// Where the application sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipePosition {
    Synth,
    /// Under a return annotation of the result's type.
    Annotated,
    /// The argument of `idf`.
    Argument,
    /// Beside its own operand in a tuple, which reads the operand's type back
    /// after the pipe has fixed it.
    Tupled,
}

const PIPE_POSITIONS: [PipePosition; 4] = [
    PipePosition::Synth,
    PipePosition::Annotated,
    PipePosition::Argument,
    PipePosition::Tupled,
];

/// One row of the pipe grid.
struct PipeRow {
    f: PipeFn,
    x: PipeOperand,
    shape: PipeShape,
    pos: PipePosition,
    range: (usize, usize),
}

impl PipeRow {
    fn well_typed(&self) -> bool {
        self.shape == PipeShape::ComposeValue || self.f.accepts(self.x.ty())
    }

    /// Whether inference must commit the whole application's type here: a
    /// well-typed synth row whose function it must type, and whose result is
    /// not left open by the operand.
    fn must_commit(&self) -> bool {
        self.well_typed()
            && self.pos == PipePosition::Synth
            && self.shape != PipeShape::ComposeValue
            && self.f.must_type()
            && self.f.result(self.x.ty()) != OpTy::Open
    }
}

/// The pipe grid's file for one function: every operand, shape and position.
fn pipe_file(defs: Defs, f: PipeFn) -> (String, Vec<PipeRow>) {
    let mut src = defs.header(PIPE_HEADER);
    let mut rows = Vec::new();
    for x in PIPE_OPERANDS {
        for shape in PIPE_SHAPES {
            // A function value has no operand to vary.
            if shape == PipeShape::ComposeValue && x != PipeOperand::Literal {
                continue;
            }
            for pos in PIPE_POSITIONS {
                let app = shape.render(f.spelling(), x.spelling());
                let result = match f.result(x.ty()) {
                    OpTy::Str => "string",
                    OpTy::Int | OpTy::Open => "int",
                };
                let (head, before, after) = match (pos, shape) {
                    (PipePosition::Synth, _) => (String::new(), "", String::new()),
                    (PipePosition::Annotated, PipeShape::ComposeValue) => {
                        (" : int -> int".to_string(), "", String::new())
                    }
                    (PipePosition::Annotated, _) => (format!(" : {result}"), "", String::new()),
                    (PipePosition::Argument, _) => (String::new(), "idf (", ")".to_string()),
                    (PipePosition::Tupled, PipeShape::ComposeValue) => {
                        (String::new(), "(", ", p)".to_string())
                    }
                    (PipePosition::Tupled, _) => {
                        (String::new(), "(", format!(", {})", x.spelling()))
                    }
                };
                let prefix = format!("let r{} (ai: int) p q{head} = {before}", rows.len());
                let start = src.len() + prefix.len();
                src.push_str(&prefix);
                src.push_str(&app);
                src.push_str(&after);
                src.push('\n');
                rows.push(PipeRow {
                    f,
                    x,
                    shape,
                    pos,
                    range: (start, start + app.len()),
                });
            }
        }
    }
    (src, rows)
}

/// Pipes and composition over in-file functions, against FCS: every function
/// kind × every operand × every shape × every position, in one file per
/// function. Every type we commit, FCS must report identically; and each
/// well-typed synth row over a function inference must type commits the
/// whole application — a per-row obligation, so a regression names its row.
///
/// The rows FCS rejects are graded by the same certain-implies-exact check:
/// FCS recovers an operand it rejects to `obj` in source order (`"s" |> inc`
/// is a `System.Object`), so committing the function's result there is wrong.
/// The loosened functions are the other trap: `p |> fo` leaves `p` generic in
/// FCS, so the tuple's `p` beside it must not read back `obj`.
#[test]
fn pipes_and_composition_over_in_file_functions_agree_with_fcs() {
    pipe_grid(Defs::InFile);
}

/// The pipe grid, with its functions defined where `defs` says.
pub(crate) fn pipe_grid(defs: Defs) {
    let mut committed = 0;
    let mut obligations = 0;
    let mut ill_typed_files_errors = 0;
    let mut missed = Vec::new();
    for f in PIPE_FNS {
        let (src, rows) = pipe_file(defs, f);
        let grid_rows: Vec<Row> = rows
            .iter()
            .map(|r| Row {
                range: r.range,
                synth: r.pos == PipePosition::Synth,
            })
            .collect();
        let expect_clean = rows.iter().all(PipeRow::well_typed);
        let c = check_in(defs, "pipes", &src, &grid_rows, expect_clean)
            .unwrap_or_else(|| panic!("FCS checks the `{}` file\n{src}", f.spelling()));
        committed += c.exprs + c.binders;
        ill_typed_files_errors += c.errors;
        for row in rows.iter().filter(|r| r.must_commit()) {
            obligations += 1;
            if !c.committed_rows.contains(&row.range) {
                missed.push(format!(
                    "{:?} {:?} {:?}: `{}`",
                    row.f,
                    row.x,
                    row.shape,
                    &src[row.range.0..row.range.1]
                ));
            }
        }
    }
    eprintln!(
        "pipe grid ({defs:?}): {committed} commits, {obligations} obligations, \
         {ill_typed_files_errors} FCS errors in the files with rejected rows"
    );
    assert!(
        missed.is_empty(),
        "{} well-typed rows did not commit their application:\n{}",
        missed.len(),
        missed.join("\n")
    );
    assert!(
        obligations >= 150,
        "vacuous: only {obligations} obligations"
    );
    assert!(
        ill_typed_files_errors >= 30,
        "the grid stopped producing rejected rows: {ill_typed_files_errors}"
    );
}

/// FCS loosens a value's parameter of a type that is not sealed at every use
/// of the value, not only where it is applied, so the use has a type the
/// definition does not: `let g = fo` is `'a -> int`, and so is the body of
/// `let a (h: obj -> int) = h`. A parameter whose type an earlier argument
/// check has made such a function is loosened at its later uses too, but not
/// at the one that fixed it.
#[test]
fn a_loosened_parameter_is_loosened_at_every_use() {
    let src = "module Flex\n\
               let fo (o: obj) = 1\n\
               let fd (o: System.IDisposable) = 1\n\
               let fs (o: string) = 1\n\
               let fi (o: int) = 1\n\
               let ap (f: obj -> int) = 1\n\
               let idf x = x\n\
               let g = fo\n\
               let gd = fd\n\
               let gs = fs\n\
               let a2 (h: obj -> int) = h\n\
               let a3 h = (ap h, h)\n\
               let a4 h = (h, ap h)\n\
               let b7 y = idf fo y\n\
               let a1 y = y |> fo\n\
               let a2d y = y |> fd\n\
               let a6 y = fo <| y\n\
               let a8 y = (fs >> fo) y\n\
               let b5 = (fo >> fi) 1\n\
               let apply h x = x |> h\n\
               let comp f g = f >> g\n";
    // The applications whose result the loosening leaves fixed: each commits.
    let rows: Vec<Row> = ["y |> fo", "fo <| y", "(fs >> fo) y"]
        .iter()
        .map(|app| {
            let start = src.find(app).expect("the application");
            Row {
                range: (start, start + app.len()),
                synth: true,
            }
        })
        .collect();
    let c = check("flex", src, &rows, true).expect("a clean file checks");
    assert_eq!(c.at_application, rows.len(), "{c:?}\n{src}");
}

/// A function in the use-order grid, and the type of a literal it accepts.
const ORDER_FNS: [(&str, &str); 4] = [("fi", "1"), ("fs", "\"s\""), ("fo", "1"), ("idf", "1")];

/// One use of the parameter `p` in the use-order grid, through `f`, whose
/// accepted literal is `lit`.
fn order_use(kind: usize, f: &str, lit: &str) -> String {
    match kind {
        0 => format!("p |> {f}"),
        1 => format!("{f} <| p"),
        2 => format!("{f} p"),
        3 => format!("({f} >> idf) p"),
        // A member access, which FCS resolves only on what it knows of `p`
        // at this point in the source.
        4 => "p.Length".to_string(),
        // A branch relation inference drops, which fixes `p` in FCS.
        5 => format!("(if true then {lit} else p)"),
        // A method argument, whose relation to the parameter inference does
        // not model.
        6 => "System.String.IsNullOrEmpty(p)".to_string(),
        // The parameter itself, which a combiner's own parameter may fix.
        _ => "p".to_string(),
    }
}

const ORDER_USES: usize = 8;

/// How the use-order grid combines its two uses of `p`, each followed by `p`
/// itself in a synth position.
const ORDER_COMBINERS: [&str; 4] = [
    // Elements of a tuple.
    "({u1}, {u2}, p)",
    // The two arguments of one curried application, of a generic function
    // and of an operator, which FCS checks left to right like a tuple's.
    "(two ({u1}) ({u2}), p)",
    "(({u1}) +? ({u2}), p)",
    // The same, of a function whose own parameters fix each argument too.
    "(gib ({u1}) ({u2}), p)",
];

/// FCS checks a function body in source order: the first use of a
/// parameter that fixes its type wins, a later conflicting one is reported
/// and recovered, and a member access on a parameter nothing has fixed yet is
/// rejected (FS0072) even if a later use would fix it. Inference solves a
/// binding's constraints together, so every ordered pair of uses of one
/// parameter — through a pipe, a back pipe, a direct application, a
/// composition, a member access, a dropped branch relation, a method
/// argument — over every function of the grid is a case, combined as a
/// tuple's elements or as the arguments of one application
/// ([`ORDER_COMBINERS`]), each followed by the parameter itself in a synth
/// position, which reads back what the pair made of it.
///
/// Every type committed must be FCS's. The pairs that both fix `p` to the
/// same sealed type must commit that final `p`, through any combiner whose
/// own parameters do not fix it too: a per-row obligation.
#[test]
fn every_ordered_pair_of_uses_of_a_parameter_agrees_with_fcs() {
    ordered_pair_grid(Defs::InFile);
}

/// The functions of the use-order grid.
pub(crate) const ORDER_HEADER: &str = "module Order\n\
                                       let idf x = x\n\
                                       let fi (o: int) = 1\n\
                                       let fs (o: string) = 1\n\
                                       let fo (o: obj) = 1\n\
                                       let two a b = 1\n\
                                       let (+?) a b = 1\n\
                                       let gib (a: int) (b: bool) = 1\n";

/// The use-order grid, with its functions defined where `defs` says.
pub(crate) fn ordered_pair_grid(defs: Defs) {
    let mut functions = 0;
    let mut errors = 0;
    let mut missed: Vec<String> = Vec::new();
    for combiner in ORDER_COMBINERS {
        let mut src = defs.header(ORDER_HEADER);
        let mut must = Vec::new();
        for k1 in 0..ORDER_USES {
            for (f1, l1) in ORDER_FNS {
                for k2 in 0..ORDER_USES {
                    for (f2, l2) in ORDER_FNS {
                        let u1 = order_use(k1, f1, l1);
                        let u2 = order_use(k2, f2, l2);
                        let body = combiner.replace("{u1}", &u1).replace("{u2}", &u2);
                        let prefix = format!("let o{functions} p = ");
                        // The final `p`, before the closing parenthesis.
                        let at = src.len() + prefix.len() + body.len() - 2;
                        src.push_str(&prefix);
                        src.push_str(&body);
                        src.push('\n');
                        functions += 1;
                        // Both uses applications of one sealed function.
                        if k1 < 4
                            && k2 < 4
                            && f1 == f2
                            && (f1 == "fi" || f1 == "fs")
                            && !combiner.starts_with("(gib")
                        {
                            must.push(((at, at + 1), body));
                        }
                    }
                }
            }
        }
        let rows: Vec<Row> = must
            .iter()
            .map(|(range, _)| Row {
                range: *range,
                synth: true,
            })
            .collect();
        let c = check_in(defs, "order", &src, &rows, false).expect("FCS checks the file");
        errors += c.errors;
        missed.extend(
            must.iter()
                .filter(|(range, _)| !c.committed_rows.contains(range))
                .map(|(_, what)| what.clone()),
        );
    }
    eprintln!("use-order grid ({defs:?}): {functions} functions, {errors} FCS errors");
    assert!(
        missed.is_empty(),
        "{} pairs fixing `p` to a sealed type did not commit it:\n{missed:?}",
        missed.len()
    );
    assert!(errors >= 400, "the grid stopped rejecting: {errors}");
}

/// An earlier binder inference did not type has the type FCS gave it at its
/// own binding, and applying it here gives its open variable a structure of
/// this binding's variables. An argument check that grounds one of those
/// retypes the earlier binder: `oldf` is `obj -> string` in FCS, so both
/// applications below are rejected, and neither may commit `int`.
#[test]
fn a_check_never_grounds_an_earlier_binders_application() {
    let src = "module Retro\n\
               let inc (x: int) = 1\n\
               let fi (o: int) = 1\n\
               let oldf = fst ((fun (o: obj) -> \"s\"), 1)\n\
               let a x = oldf x |> inc\n\
               let b x = fi (oldf x)\n\
               let c x = (oldf x |> inc, oldf x)\n";
    check("retro", src, &[], false).expect("FCS checks the file");
}

/// A check can wait on its domain and fire after checks that follow it in the
/// source: `h p`'s domain is `h`'s, which only the pipe into `apf` fixes, to
/// `int`. FCS has fixed `p` to that domain at `h p` already, so it rejects
/// `fs p`; firing `fs p` first makes `p` a `string`, and `h p` then fails. A
/// check that fails is a relation FCS rejects somewhere, in an order the
/// solver does not keep, so nothing the binding's checks grounded may stand.
#[test]
fn a_late_check_that_fails_undoes_the_binding_checks() {
    let src = "module Late\n\
               let fs (o: string) = 1\n\
               let apf (f: int -> int) = 1\n\
               let a h p = (h p, h |> apf, fs p, p)\n\
               let b h p = (h p, h |> apf, p)\n";
    let start = src.find("(h p, h |> apf, p)").expect("b's body") + "(h p, h |> apf, ".len();
    let rows = [Row {
        range: (start, start + 1),
        synth: true,
    }];
    let c = check("late", src, &rows, false).expect("FCS checks the file");
    assert_eq!(
        c.at_application, 1,
        "the well-typed twin fixes `p` to `int` through the late check\n{src}"
    );
}

/// A member access inference does not resolve — on a generic receiver, or an
/// array's intrinsic `Length` — has the member's type in FCS, a relation
/// inference does not hold. An argument check that fixes its result instead
/// fixes it to whatever the check says, which FCS rejects when the two differ.
#[test]
fn a_check_never_fixes_an_unresolved_members_result() {
    let src = "module Members\n\
               let incs (t: string) = 2\n\
               let inc (t: int) = 2\n\
               let a (xs: System.Collections.Generic.List<int>) = xs.Count |> incs\n\
               let b (arr: int[]) = arr.Length |> incs\n\
               let c (arr: int[]) = (arr.Length |> incs, arr.Length)\n\
               let d (xs: System.Collections.Generic.List<int>) = (xs.Count |> inc, xs.Count)\n";
    let c = check("members", src, &[], false).expect("FCS checks the file");
    eprintln!("members: {c:?}");
}

/// A branch relation fixes `p` in FCS before a later argument conflicts with
/// it: `(if true then p else 1) +. p` makes `p` an `int`, and FCS rejects the
/// second operand. Inference drops the branch relation and grounds `p` through
/// the second operand's check, which must not stand. Infix and prefix forms.
#[test]
fn a_branch_relation_fixes_a_parameter_before_a_later_argument() {
    let src = "module Branch\n\
               let (+.) (a: int) (b: bool) = a\n\
               let g (a: int) (b: bool) = a\n\
               let f p = (if true then p else 1) +. p\n\
               let h p = g (if true then p else 1) p\n";
    check("branch", src, &[], false).expect("FCS checks the file");
}

/// SplitMix64: a deterministic stream, so a failing seed reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'s>(&mut self, xs: &[&'s str]) -> &'s str {
        xs[self.below(xs.len())]
    }
}

/// The functions the argument-check generator applies: sealed, generic,
/// loosened to `obj` and to a bounded subtype, partially applied, composed,
/// one taking a function, an earlier binder inference cannot type (`oldf`,
/// `obj -> string` in FCS), and the generated function's own unannotated
/// parameter `h`.
pub(crate) const ARG_CHECK_HEADER: &str = "module Checks\n\
                                let idf x = x\n\
                                let inc (x: int) = 1\n\
                                let incs (t: string) = 2\n\
                                let opn x = 1\n\
                                let fo (x: obj) = 1\n\
                                let fc (x: System.IComparable) = 1\n\
                                let add2 (x: int) (y: int) = 3\n\
                                let addo (x: int) (y: obj) = 3\n\
                                let ap (f: obj -> int) = 1\n\
                                let oldf = fst ((fun (o: obj) -> \"s\"), 1)\n\
                                let two a b = 1\n\
                                let (+?) a b = 1\n\
                                let gib (a: int) (b: bool) = 1\n\
                                let (+.) (a: int) (b: bool) = a\n";

const ARG_CHECK_FNS: [&str; 14] = [
    "inc",
    "incs",
    "opn",
    "fo",
    "fc",
    "idf",
    "(add2 1)",
    "(addo 1)",
    "ap",
    "oldf",
    "h",
    "(inc >> idf)",
    "(fo >> inc)",
    "(idf << incs)",
];

/// An expression over the parameters `ai: int`, `ss: string`, `xs: List<int>`,
/// `arr: int[]`, `p`, `q` and `h`, built to `depth`, of no particular type: the program it lands in may
/// well be one FCS rejects, which is the point.
fn arg_check_expr(rng: &mut Rng, depth: usize) -> String {
    if depth == 0 || rng.below(5) == 0 {
        return rng
            .pick(&[
                "p",
                "q",
                "ai",
                "ss",
                "1",
                "\"s\"",
                "true",
                "h",
                "(ai 1)",
                "(p q)",
                // Members inference does not resolve: on a generic receiver,
                // and an array's intrinsic one.
                "xs.Count",
                "arr.Length",
            ])
            .to_string();
    }
    let f = rng.pick(&ARG_CHECK_FNS);
    let g = rng.pick(&ARG_CHECK_FNS);
    let x = arg_check_expr(rng, depth - 1);
    match rng.below(16) {
        0 => format!("({f} {x})"),
        1 => format!("({x} |> {f})"),
        2 => format!("({f} <| {x})"),
        3 => format!("(({f} >> {g}) {x})"),
        4 => format!("(({g} << {f}) {x})"),
        5 => format!("({x} |> {f} |> {g})"),
        6 => format!("({x}).Length"),
        7 => {
            let y = arg_check_expr(rng, depth - 1);
            // A condition on a parameter grounds it to `bool` where it is
            // the parameter's first use.
            let c = rng.pick(&["true", "p", "q", "h"]);
            format!("(if {c} then {x} else {y})")
        }
        8 | 9 => {
            let y = arg_check_expr(rng, depth - 1);
            format!("({x}, {y})")
        }
        10 => {
            let y = arg_check_expr(rng, depth - 1);
            format!("(let v = {x} in (v, {y}))")
        }
        // Two arguments of one application, which FCS checks left to right:
        // generic, and with parameters of their own that fix each argument.
        14 | 15 => {
            let y = arg_check_expr(rng, depth - 1);
            match rng.below(4) {
                0 => format!("(two {x} {y})"),
                1 => format!("({x} +? {y})"),
                2 => format!("(gib {x} {y})"),
                _ => format!("({x} +. {y})"),
            }
        }
        // Parenthesised: an argument that ends in a high-precedence
        // application (`gib "s".StartsWith(x) …`) is FS0597.
        12 => format!("(System.String.IsNullOrEmpty({x}))"),
        13 => format!("(\"s\".StartsWith({x}))"),
        _ => format!("({f} >> {g})"),
    }
}

/// Generated programs mixing every way inference relates an argument to a
/// parameter — direct applications, pipes, compositions, a member access, a
/// dropped branch relation, a local — over every kind of function, in
/// arbitrary order, mostly ill-typed. FCS checks in source order and keeps
/// the first relation to fix a variable; inference solves a binding at once
/// and must keep only what it can show is order-independent. Every committed
/// type must be FCS's.
#[test]
fn generated_argument_checks_commit_only_what_fcs_kept() {
    let files = crate::common::env_usize_or("BORZOI_ARG_CHECK_FILES", 24);
    let mut graded = 0;
    let mut committed = 0;
    let mut errors = 0;
    for seed in 0..files as u64 {
        let src = argument_check_file(Defs::InFile, seed);
        if let Some(c) = check("arg_checks", &src, &[], false) {
            graded += 1;
            committed += c.exprs + c.binders;
            errors += c.errors;
        }
    }
    eprintln!(
        "argument-check generator: {graded} of {files} files graded, {committed} commits, {errors} FCS errors"
    );
    assert!(
        graded * 4 >= files * 3,
        "too few files graded: {graded} of {files}"
    );
    assert!(
        errors >= files * 5,
        "the generator stopped producing rejected programs: {errors}"
    );
    assert!(
        committed >= files * 10,
        "the generator committed too little to be evidence: {committed}"
    );
}

/// The argument-check generator's file for `seed`, with its functions defined
/// where `defs` says. The functions after the header do not depend on `defs`.
pub(crate) fn argument_check_file(defs: Defs, seed: u64) -> String {
    let mut rng = Rng(seed ^ 0xa59c_4ec5);
    let mut src = defs.header(ARG_CHECK_HEADER);
    for i in 0..12 {
        let body = arg_check_expr(&mut rng, 3);
        src.push_str(&format!(
            "let g{i} (ai: int) (ss: string) (xs: System.Collections.Generic.List<int>) \
             (arr: int[]) p q h = {body}\n"
        ));
    }
    src
}

/// A value-restricted binding over a loosened function, which FCS rejects
/// (FS0030): nothing may be committed for it.
#[test]
fn a_value_restricted_loosened_binding_commits_nothing_wrong() {
    let src = "module Restricted\n\
               let fo (o: obj) = 1\n\
               let fi (o: int) = 1\n\
               let idf x = x\n\
               let b6 = idf fo\n\
               let b4 = fo >> fi\n";
    check("restricted", src, &[], false).expect("FCS checks the file");
}
