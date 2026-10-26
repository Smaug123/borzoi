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
//! FSharp.Core's own operators commit nothing at the application yet, and
//! that is the ordinary application rule, not a gap in this one: their results
//! are fixed only through an argument whose domain is a function type (`|>`'s
//! `'T1 -> 'U`) or a type parameter another argument settles, and an argument
//! check discharges against neither (`Gen::no_subsumption_domain`).

use std::collections::HashMap;

use crate::common::{
    full_bcl_env, invoke_fcs_dump, parse_fcs_binder_types_with_errors, parse_fcs_types_with_errors,
    temp_fs_file, try_invoke_fcs_dump,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind};
use borzoi_sema::{
    Incomplete, OperatorGap, ProjectItems, SyntaxRecovery, infer_file, resolve_file,
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
    // A signature over a type the bridge does not carry (`'T list` and `'T
    // ref` are F# types under a compiled name), or a tupled argument group.
    op(
        "@",
        Sort::IntList,
        Sort::IntList,
        Sort::IntList,
        Expect::Declined(OperatorGap::Signature),
    ),
    op(
        ":=",
        Sort::RefInt,
        Sort::Int,
        Sort::Unit,
        Expect::Declined(OperatorGap::Signature),
    ),
    op(
        "||>",
        Sort::Pair,
        Sort::Curried,
        Sort::Int,
        Expect::Declined(OperatorGap::Signature),
    ),
    op(
        "<||",
        Sort::Curried,
        Sort::Pair,
        Sort::Int,
        Expect::Declined(OperatorGap::Signature),
    ),
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
struct Row {
    range: (usize, usize),
    synth: bool,
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
struct Checked {
    exprs: usize,
    binders: usize,
    /// Committed expression types at a row's whole application, in synth
    /// position.
    at_application: usize,
    /// FCS errors on the file.
    errors: usize,
    /// Every incompleteness reason any binding recorded.
    reasons: Vec<Incomplete>,
}

/// Resolve, infer and diff `src` against both oracles. `None` when FCS cannot
/// produce a typed tree for the file (it throws materialising some rejected
/// operators), which the ill-typed family counts rather than grades.
fn check(label: &str, src: &str, rows: &[Row], expect_clean: bool) -> Option<Checked> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "generated source has parse errors: {:?}\n{src}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let path = temp_fs_file(label, src);
    let oracle = |sub: &str| {
        if expect_clean {
            Some(invoke_fcs_dump(sub, &path))
        } else {
            try_invoke_fcs_dump(sub, &path).ok()
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
    for (range, ty) in inferred.types() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let ours = ty.render();
        let theirs = fcs_types.get(&key).unwrap_or_else(|| {
            panic!(
                "we typed `{}` at {key:?} as `{ours}`, but FCS has no node there\n{src}",
                &src[key.0..key.1]
            )
        });
        assert_eq!(
            &ours,
            theirs,
            "expression type mismatch at {key:?} (`{}`)\n{src}",
            &src[key.0..key.1]
        );
        c.exprs += 1;
        if synth_rows.contains(&key) {
            c.at_application += 1;
        }
    }
    for (def_id, ty) in inferred.def_types() {
        let def = resolved.def(*def_id);
        let key = (
            u32::from(def.range.start()) as usize,
            u32::from(def.range.end()) as usize,
        );
        let ours = ty.render();
        let theirs = fcs_binders.get(&key).unwrap_or_else(|| {
            panic!(
                "we typed binder `{}` at {key:?} as `{ours}`, but FCS has no binder there\n{src}",
                def.name
            )
        });
        assert_eq!(
            &ours, theirs,
            "binder type mismatch for `{}` at {key:?}\n{src}",
            def.name
        );
        c.binders += 1;
    }
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
    // commits it: one per well-typed operand-kind pair.
    for spelling in ["+.", "&&."] {
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
/// grid cannot — FSharp.Core's operators commit nothing at an application yet
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
