//! Applications of FSharp.Core's statically resolved arithmetic and bitwise
//! operators and conversion functions, against FCS's typed tree.
//!
//! `+`'s signature is `^T1 -> ^T2 -> ^T3`: it says nothing about the result
//! until the member constraint is solved, and over the primitive types F#
//! solves it by a built-in rule (`SolveMemberConstraint`) rather than a member
//! lookup. FCS applies the rule where it checks the application, from the
//! operand types it knows there; an operand it does not know yet leaves the
//! constraint pending, to be solved later or defaulted by weak resolution.
//! Inference types an application only when every operand type is known at
//! that point and on the rule's list, and otherwise records why it did not.
//!
//! # The grid
//!
//! Every use in [`USES`] (each operator infix, prefix `-`, each conversion
//! function by name and qualified, and spellings that use the operator as a
//! value) × every operand [`SORTS`] (every primitive, `bool`, `unit`, `obj`,
//! BCL types with their own operator members, an enum, a function, a tuple, a
//! number with a unit of measure) × operand forms (annotated parameters,
//! applications of a monomorphic function, literals, an open parameter) ×
//! positions (synth, argument, tuple, branch, local, receiver), one file per
//! use and sort, plus a twin over two different operand types.
//!
//! # What is asserted
//!
//! - **Soundness** through `infer_operators_diff::check_in`: every commit is
//!   FCS's.
//! - **The table is exact over the primitives.** Over an operand of a type
//!   the rule distinguishes, FCS accepts the file exactly when the grid's own
//!   [`holds`] says the rule solves it; an [`Expect::Rejected`] pair is one FCS
//!   rejects.
//! - **Completeness, per row.** In a file FCS accepts whose sort the table
//!   says the rule solves, every binding is complete and every synth row
//!   commits its application. The types oracle cannot see this: a declined
//!   row agrees with it vacuously.
//! - **Every decline says why.** A file over a modelled spelling whose
//!   operands inference reads records [`Incomplete::Builtin`] wherever the
//!   rule does not solve it — `Violated` where the table says FCS rejects,
//!   `Undecided` over a type off the list, `OpenOperand` over an operand not
//!   yet known — whether FCS accepts the file or not.

use crate::infer_operators_diff::{Defs, Row, check_in};
use borzoi_cst::syntax::SyntaxKind;
use borzoi_sema::{BuiltinGap, Incomplete};

/// How a [`Sort`] takes part in F#'s built-in rules (ConstraintSolver.fs's
/// `isSignedIntegerTy` and friends). The grid's own reading, which FCS checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Signed,
    Unsigned,
    Float,
    Decimal,
    Char,
    Str,
    /// A type the rules do not name: its own members, if any, decide.
    Other,
}

impl Kind {
    fn integer(self) -> bool {
        matches!(self, Kind::Signed | Kind::Unsigned)
    }
    fn numeric(self) -> bool {
        self.integer() || matches!(self, Kind::Float | Kind::Decimal)
    }
}

/// An operand type.
struct Sort {
    annotation: &'static str,
    literal: Option<&'static str>,
    kind: Kind,
    /// Whether inference reads the annotation at all.
    bridged: bool,
}

const fn s(annotation: &'static str, literal: Option<&'static str>, kind: Kind) -> Sort {
    Sort {
        annotation,
        literal,
        kind,
        bridged: true,
    }
}

const SORTS: &[Sort] = &[
    s("int", Some("3"), Kind::Signed),
    s("int64", Some("3L"), Kind::Signed),
    s("sbyte", Some("3y"), Kind::Signed),
    s("int16", Some("3s"), Kind::Signed),
    s("nativeint", Some("3n"), Kind::Signed),
    s("byte", Some("3uy"), Kind::Unsigned),
    s("uint16", Some("3us"), Kind::Unsigned),
    s("uint32", Some("3u"), Kind::Unsigned),
    s("uint64", Some("3UL"), Kind::Unsigned),
    s("unativeint", Some("3un"), Kind::Unsigned),
    s("float", Some("3.0"), Kind::Float),
    s("float32", Some("3.0f"), Kind::Float),
    s("decimal", Some("3.0m"), Kind::Decimal),
    s("char", Some("'c'"), Kind::Char),
    s("string", Some("\"s\""), Kind::Str),
    s("bool", Some("true"), Kind::Other),
    s("unit", Some("()"), Kind::Other),
    s("obj", None, Kind::Other),
    // Their own operator members: `DateTime - DateTime` is a `TimeSpan`.
    s("System.DateTime", None, Kind::Other),
    s("System.TimeSpan", None, Kind::Other),
    // Not read as an annotation (its operand stays open), but FCS's own
    // members solve it.
    Sort {
        annotation: "System.Numerics.BigInteger",
        literal: None,
        kind: Kind::Other,
        bridged: false,
    },
    // An enum, which the rules take by its underlying type.
    s("System.DayOfWeek", None, Kind::Other),
    s("(int -> int)", None, Kind::Other),
    s("(int * int)", Some("(1, 2)"), Kind::Other),
    Sort {
        annotation: "float<m>",
        literal: Some("3.0<m>"),
        kind: Kind::Other,
        bridged: false,
    },
];

/// What a use applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Bitwise,
    Shift,
    Negate,
    /// A conversion to a type of this kind.
    Convert(Kind),
}

impl Op {
    fn unary(self) -> bool {
        matches!(self, Op::Negate | Op::Convert(_))
    }
}

/// A use under test.
struct Use {
    /// The application over `{a}` (and `{b}`).
    template: &'static str,
    op: Op,
    /// Whether inference models this spelling: an operator used as a value
    /// is not, and its rows are graded for soundness alone.
    modelled: bool,
}

const fn u(template: &'static str, op: Op) -> Use {
    Use {
        template,
        op,
        modelled: true,
    }
}

const fn value(template: &'static str, op: Op) -> Use {
    Use {
        template,
        op,
        modelled: false,
    }
}

const USES: &[Use] = &[
    u("{a} + {b}", Op::Add),
    u("{a} - {b}", Op::Sub),
    u("{a} * {b}", Op::Mul),
    u("{a} / {b}", Op::Div),
    u("{a} % {b}", Op::Mod),
    u("{a} &&& {b}", Op::Bitwise),
    u("{a} ||| {b}", Op::Bitwise),
    u("{a} ^^^ {b}", Op::Bitwise),
    u("{a} <<< {b}", Op::Shift),
    u("{a} >>> {b}", Op::Shift),
    u("-{a}", Op::Negate),
    u("int {a}", Op::Convert(Kind::Signed)),
    u("int64 {a}", Op::Convert(Kind::Signed)),
    u("sbyte {a}", Op::Convert(Kind::Signed)),
    u("nativeint {a}", Op::Convert(Kind::Signed)),
    u("byte {a}", Op::Convert(Kind::Unsigned)),
    u("uint64 {a}", Op::Convert(Kind::Unsigned)),
    u("unativeint {a}", Op::Convert(Kind::Unsigned)),
    u("float {a}", Op::Convert(Kind::Float)),
    u("float32 {a}", Op::Convert(Kind::Float)),
    u("decimal {a}", Op::Convert(Kind::Decimal)),
    u("char {a}", Op::Convert(Kind::Char)),
    u("Operators.int {a}", Op::Convert(Kind::Signed)),
    value("(+) {a} {b}", Op::Add),
    value("Operators.(*) {a} {b}", Op::Mul),
    value("{a} |> int", Op::Convert(Kind::Signed)),
    value("Checked.(+) {a} {b}", Op::Add),
    value("Checked.int {a}", Op::Convert(Kind::Signed)),
];

/// Whether F#'s built-in rule solves `op` over operands of kind `k` (both
/// operands of that kind, or for a shift the left one and an `int`): the
/// grid's own reading of `SolveMemberConstraint`, which FCS checks.
fn holds(op: Op, k: Kind) -> bool {
    match op {
        Op::Add => k.numeric() || matches!(k, Kind::Char | Kind::Str),
        Op::Sub => k.numeric() || k == Kind::Char,
        Op::Mul | Op::Div | Op::Mod => k.numeric(),
        Op::Bitwise | Op::Shift => k.integer(),
        Op::Negate => matches!(k, Kind::Signed | Kind::Float | Kind::Decimal),
        Op::Convert(_) => k.numeric() || matches!(k, Kind::Char | Kind::Str),
    }
}

/// What F# makes of a use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The built-in rule solves it, and inference types it.
    Solved,
    /// F# provably rejects it: the rule fires on one operand and equates the
    /// other with it.
    Rejected,
    /// Anything else: another type's own members decide, or a conversion F#
    /// inserts.
    Other,
}

/// What F# makes of `op` over a left operand of sort `a` and a right one of
/// sort `b` (unused for a unary `op`): the grid's own reading of
/// `SolveMemberConstraint`, which FCS checks.
///
/// `&&&`'s operands share one type parameter (`^T -> ^T -> ^T`), so FCS
/// checks the right operand against the left's type once it knows it, and
/// inserts F#'s `int` widening there (`int64`, `nativeint`, `float`): that
/// pair is not rejected. `+`'s three are independent, so nothing is
/// expected of its right operand and the pair is.
fn expectation(op: Op, a: &Sort, b: &Sort) -> Expect {
    let named = |k: Kind| !matches!(k, Kind::Other | Kind::Decimal);
    if !a.bridged || !b.bridged {
        return Expect::Other;
    }
    if op.unary() {
        return if holds(op, a.kind) {
            Expect::Solved
        } else {
            Expect::Other
        };
    }
    if op == Op::Shift {
        return match (a.kind.integer(), b.annotation) {
            (true, "int") => Expect::Solved,
            (true, _) if named(b.kind) => Expect::Rejected,
            _ => Expect::Other,
        };
    }
    if a.annotation == b.annotation {
        return if holds(op, a.kind) {
            Expect::Solved
        } else {
            Expect::Other
        };
    }
    let widened = op == Op::Bitwise
        && b.annotation == "int"
        && matches!(a.annotation, "int64" | "nativeint" | "float");
    if named(a.kind) && named(b.kind) && (holds(op, a.kind) || holds(op, b.kind)) && !widened {
        Expect::Rejected
    } else {
        Expect::Other
    }
}

/// The partner sort of a twin: an operand type different from `sort`'s.
fn partner(sort: &Sort) -> &'static Sort {
    sort_named(if sort.annotation == "int" {
        "int64"
    } else {
        "int"
    })
}

fn sort_named(annotation: &str) -> &'static Sort {
    SORTS
        .iter()
        .find(|s| s.annotation == annotation)
        .expect("a sort of the grid")
}

/// One generated row: where its application is, and whether it sits in a
/// synth position whose operands are all known where it is applied.
struct GridRow {
    range: (usize, usize),
    synth: bool,
}

/// The header every file shares: a unit of measure, and a monomorphic
/// function over each operand type, whose application fixes its result.
fn header(a: &Sort, b: &Sort) -> String {
    format!(
        "module Builtins\n\
         [<Measure>] type m\n\
         let idA (x: {}) = x\n\
         let idB (x: {}) = x\n",
        a.annotation, b.annotation
    )
}

/// One use × operand-sort file: a binding per operand form and position,
/// the left operand of `a`'s sort and the right of `b`'s.
fn grid_file(use_: &Use, a: &Sort, b: &Sort) -> (String, Vec<GridRow>) {
    let mut src = header(a, b);
    let mut rows = Vec::new();
    let apply = |x: &str, y: &str| use_.template.replace("{a}", x).replace("{b}", y);
    let mut push = |src: &mut String, head: String, app: String, tail: &str, synth: bool| {
        let start = src.len() + head.len();
        src.push_str(&head);
        src.push_str(&app);
        src.push_str(tail);
        src.push('\n');
        rows.push(GridRow {
            range: (start, start + app.len()),
            synth,
        });
    };
    let params = if use_.op.unary() {
        format!("(a: {})", a.annotation)
    } else {
        format!("(a: {}) (b: {})", a.annotation, b.annotation)
    };
    let app = apply("a", "b");
    push(
        &mut src,
        format!("let r0 {params} = "),
        app.clone(),
        "",
        true,
    );
    push(
        &mut src,
        format!("let r1 {params} = id ("),
        app.clone(),
        ")",
        false,
    );
    push(
        &mut src,
        format!("let r2 {params} = ("),
        app.clone(),
        ", 0)",
        true,
    );
    push(
        &mut src,
        format!("let r3 {params} (c: bool) = if c then "),
        app.clone(),
        &format!(" else {app}"),
        true,
    );
    push(
        &mut src,
        format!("let r4 {params} = "),
        apply("(idA a)", "(idB b)"),
        "",
        true,
    );
    push(
        &mut src,
        format!("let r5 {params} = let s = "),
        app.clone(),
        " in s",
        true,
    );
    // A member of the result, resolved only on what FCS knows of it there.
    push(
        &mut src,
        format!("let r6 {params} = ("),
        app.clone(),
        ").ToString()",
        false,
    );
    if let (Some(la), Some(lb)) = (a.literal, b.literal) {
        push(
            &mut src,
            "let r7 () = ".to_string(),
            apply(la, lb),
            "",
            true,
        );
        push(&mut src, "let r8 = ".to_string(), apply(la, lb), "", true);
    }
    // The left operand open where the operator is applied: FCS leaves the
    // constraint pending and weak resolution settles it, which inference does
    // not model. A conversion's result is its signature's whatever the
    // operand, so it is typed even so.
    let open_params = if use_.op.unary() {
        "a".to_string()
    } else {
        format!("a (b: {})", b.annotation)
    };
    let convert = matches!(use_.op, Op::Convert(_));
    push(
        &mut src,
        format!("let r9 {open_params} = "),
        app.clone(),
        "",
        convert,
    );
    // The same operand fixed by an application after the operator's: FCS
    // knows it only once the constraint is already pending.
    push(
        &mut src,
        format!("let r10 {open_params} = ("),
        app.clone(),
        ", idA a)",
        convert,
    );
    // An operand inference does not model at all.
    push(
        &mut src,
        format!("let r11 {params} = "),
        apply("(match a with x -> x)", "b"),
        "",
        convert,
    );
    (src, rows)
}

/// What one file was expected to show, and what it did.
#[derive(Default)]
struct Tally {
    obligations: usize,
    missed: Vec<String>,
    incomplete: Vec<String>,
    table: Vec<String>,
    unexplained: Vec<String>,
    twins: usize,
    undecided_accepted: usize,
    ungradable: usize,
}

fn builtin_reasons(reasons: &[Incomplete]) -> Vec<BuiltinGap> {
    reasons
        .iter()
        .filter_map(|r| match r {
            Incomplete::Builtin(g) => Some(*g),
            _ => None,
        })
        .collect()
}

/// Grade one use over operand sorts `a` and `b` (the same sort, or a twin's
/// two).
fn grade(use_: &Use, a: &Sort, b: &Sort, t: &mut Tally) {
    let (src, rows) = grid_file(use_, a, b);
    let grid: Vec<Row> = rows
        .iter()
        .map(|r| Row {
            range: r.range,
            synth: r.synth,
        })
        .collect();
    let label = format!(
        "`{}` over {} and {}",
        use_.template, a.annotation, b.annotation
    );
    let expect = expectation(use_.op, a, b);
    let Some(c) = check_in(Defs::InFile, "builtins", &src, &grid, false) else {
        // FCS produced no typed tree: a constraint it rejects, re-raised while
        // it converts the file. Only a rejected program may do that.
        if expect == Expect::Solved {
            t.table.push(format!(
                "  {label}: FCS cannot check a file the rule solves"
            ));
        }
        t.ungradable += 1;
        return;
    };
    let solved = expect == Expect::Solved;
    let provably_rejected = expect == Expect::Rejected;
    let gaps = builtin_reasons(&c.reasons);
    // The table, against FCS, over the types the rules name.
    if solved && c.errors > 0 {
        t.table.push(format!(
            "  {label}: the rule solves it, but FCS rejects it: {:?}",
            c.fcs_error_codes
        ));
        return;
    }
    if provably_rejected && c.errors == 0 {
        t.table.push(format!(
            "  {label}: called provably rejected, but FCS accepts it"
        ));
        return;
    }
    let named = a.kind != Kind::Other && (use_.op.unary() || b.kind != Kind::Other);
    let same = use_.op.unary() || a.annotation == b.annotation;
    if named && same && expect == Expect::Other && c.errors == 0 {
        t.table.push(format!(
            "  {label}: the rule is said not to solve it, but FCS accepts it"
        ));
        return;
    }
    if !use_.modelled || !a.bridged || !b.bridged {
        return;
    }
    // Every file holds the open rows (r9, r10).
    if !gaps.contains(&BuiltinGap::OpenOperand) {
        t.unexplained.push(format!(
            "  {label}: no OpenOperand for the open rows: {gaps:?}"
        ));
    }
    if solved {
        let other: Vec<_> = c
            .reasons
            .iter()
            // The open rows (r9, r10) and the unmodelled operand (r11).
            .filter(|r| {
                !matches!(
                    r,
                    Incomplete::Builtin(BuiltinGap::OpenOperand)
                        | Incomplete::Unmodelled(SyntaxKind::MATCH_EXPR)
                )
            })
            .collect();
        if !other.is_empty() {
            t.incomplete.push(format!("  {label}: {other:?}"));
        }
        for row in rows.iter().filter(|r| r.synth) {
            t.obligations += 1;
            if !c.committed_rows.contains(&row.range) {
                t.missed
                    .push(format!("  {label}: `{}`", &src[row.range.0..row.range.1]));
            }
        }
        return;
    }
    let wanted = if provably_rejected {
        BuiltinGap::Violated
    } else {
        BuiltinGap::Undecided
    };
    if c.errors > 0 {
        t.twins += 1;
    } else {
        t.undecided_accepted += 1;
    }
    if !gaps.contains(&wanted) {
        t.unexplained.push(format!(
            "  {label}: wanted {wanted:?} (FCS errors {:?}), got {gaps:?}",
            c.fcs_error_codes
        ));
    }
}

/// The grid. See the module docs.
#[test]
fn every_builtin_operator_over_every_operand_sort_agrees_with_fcs() {
    let mut t = Tally::default();
    for use_ in USES {
        for sort in SORTS {
            if use_.op == Op::Shift {
                // The right operand is an `int`, and the twin's an `int64`.
                grade(use_, sort, sort_named("int"), &mut t);
                grade(use_, sort, sort_named("int64"), &mut t);
            } else {
                grade(use_, sort, sort, &mut t);
                if !use_.op.unary() {
                    grade(use_, sort, partner(sort), &mut t);
                }
            }
        }
    }
    eprintln!(
        "builtin grid: {} obligations, {} missed, {} rejected files, {} accepted but undecided, {} ungradable",
        t.obligations,
        t.missed.len(),
        t.twins,
        t.undecided_accepted,
        t.ungradable
    );
    let mut failures = Vec::new();
    for (what, list) in [
        ("the grid's table is wrong about", &t.table),
        ("declined without the reason:", &t.unexplained),
        (
            "solved, yet hold another incomplete binding:",
            &t.incomplete,
        ),
        ("synth rows the rule solves did not commit:", &t.missed),
    ] {
        if !list.is_empty() {
            failures.push(format!("{} {what}\n{}", list.len(), list.join("\n")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        t.obligations >= 1000,
        "vacuous: only {} obligations",
        t.obligations
    );
    assert!(t.twins >= 200, "vacuous: only {} rejected files", t.twins);
    assert!(
        t.undecided_accepted >= 20,
        "vacuous: only {} accepted files off the list",
        t.undecided_accepted
    );
}

/// The recogniser, against FSharp.Core itself: exactly the operators and
/// conversion functions the rule is modelled for are read as one, each as
/// what it is. A different FSharp.Core whose signatures do not have the shape
/// the rule assumes would drop one from this list, not misread it.
#[test]
fn fsharp_cores_builtin_operators_are_recognised() {
    let env = crate::common::full_bcl_env();
    let core = ["Microsoft", "FSharp", "Core"].map(String::from);
    let operators = env
        .lookup_type(&core, "Operators", 0)
        .expect("FSharp.Core declares Operators");
    let mut found = borzoi_sema::test_support::module_builtin_operators(env, operators);
    found.sort();
    let target = |t: &str| {
        format!("Convert {{ target: Named {{ path: [\"System\", \"{t}\"], args: [] }} }}")
    };
    let mut expected: Vec<(String, String)> = [
        ("op_Addition", "Arith(Add)".to_string()),
        ("op_Subtraction", "Arith(Sub)".to_string()),
        ("op_Multiply", "Arith(Mul)".to_string()),
        ("op_Division", "Arith(Div)".to_string()),
        ("op_Modulus", "Arith(Mod)".to_string()),
        ("op_BitwiseAnd", "Bitwise".to_string()),
        ("op_BitwiseOr", "Bitwise".to_string()),
        ("op_ExclusiveOr", "Bitwise".to_string()),
        ("op_LeftShift", "Shift".to_string()),
        ("op_RightShift", "Shift".to_string()),
        ("op_UnaryNegation", "Negate".to_string()),
        ("byte", target("Byte")),
        ("sbyte", target("SByte")),
        ("int16", target("Int16")),
        ("uint16", target("UInt16")),
        ("int", target("Int32")),
        ("int32", target("Int32")),
        ("uint", target("UInt32")),
        ("uint32", target("UInt32")),
        ("int64", target("Int64")),
        ("uint64", target("UInt64")),
        ("nativeint", target("IntPtr")),
        ("unativeint", target("UIntPtr")),
        ("float", target("Double")),
        ("float32", target("Single")),
        ("decimal", target("Decimal")),
        ("char", target("Char")),
    ]
    .into_iter()
    .map(|(n, b)| (n.to_string(), b))
    .collect();
    expected.sort();
    assert_eq!(found, expected, "FSharp.Core's operators read as builtins");
    // `Checked` declares the same operators and conversions under the same
    // compiled names and shapes; the rule is modelled for `Operators`' alone.
    let checked = env
        .nested_module(operators, "Checked")
        .expect("FSharp.Core declares Operators.Checked");
    assert!(
        env.member_indices(checked)
            .any(|idx| env.member_display_name(checked, idx) == "op_Addition"),
        "Checked declares `+`"
    );
    assert_eq!(
        borzoi_sema::test_support::module_builtin_operators(env, checked),
        Vec::new(),
        "Operators.Checked's functions read as builtins"
    );
}

/// A shadowing definition is the operator, never FSharp.Core's: an in-file
/// `+`, `int` and prefix `-` whose results are not FSharp.Core's.
#[test]
fn a_shadowed_builtin_is_the_shadows() {
    let src = "module Shadow\n\
               let (+) (a: int) (b: int) = \"s\"\n\
               let int (a: float) = \"t\"\n\
               let (~-) (a: int) = 'u'\n\
               let r0 (a: int) (b: int) = a + b\n\
               let r1 (a: float) = int a\n\
               let r2 = 1 + 2\n\
               let r3 (n: int) = -n\n";
    // `-n` applies the in-file `~-`, a prefix operator inference does not
    // apply as a callee: graded for soundness alone.
    let rows: Vec<Row> = ["a + b", "int a", "1 + 2"]
        .iter()
        .map(|app| {
            let start = src.find(app).expect("the application");
            Row {
                range: (start, start + app.len()),
                synth: true,
            }
        })
        .collect();
    let c = check_in(Defs::InFile, "builtin_shadow", src, &rows, true).expect("a clean file");
    assert_eq!(c.at_application, rows.len(), "{c:?}");
}

/// Where the application's type is already expected, FCS unifies the
/// result's type variable with it before it checks the operands, and only
/// then solves the constraint: `let (s: float) = a + b` over two `int`s is
/// rejected at the operator, where a function returning `int` would have
/// been rejected at the binding. Every way an expected type reaches an
/// application -- an annotated value, local and pattern, a return
/// annotation, a branch after another, an argument, a tuple element under a
/// pattern annotation -- once matching and once not; every commit must be
/// FCS's.
#[test]
fn an_expected_type_reaches_the_operator_before_its_operands() {
    for (expected, ok) in [("int", true), ("float", false), ("obj", false)] {
        let src = format!(
            "module Expected\n\
             let fx (x: {expected}) = x\n\
             let v0 (a: int) (b: int) : {expected} = a + b\n\
             let v1 (a: int) (b: int) = let (s: {expected}) = a + b in s\n\
             let v2 (a: int) (b: int) = let s : {expected} = a + b in s\n\
             let v3 (a: int) (b: int) (c: bool) = if c then fx a else a + b\n\
             let v4 (a: int) (b: int) = fx (a + b)\n\
             let v5 (a: int) (b: int) = let (s: {expected}, t) = (a + b, -a) in (s, t)\n\
             let v6 (a: int) (b: int) = (a + b : {expected})\n\
             let v7 (a: int) : {expected} = int a\n\
             let v8 = (1 + 2 : {expected})\n\
             let v9 : {expected} = 1 + 2\n"
        );
        let c = check_in(Defs::InFile, "builtin_expected", &src, &[], false)
            .unwrap_or_else(|| panic!("FCS checks the file\n{src}"));
        assert_eq!(
            c.errors == 0,
            ok,
            "{expected}: {:?}\n{src}",
            c.fcs_error_codes
        );
    }
}

/// FCS names a type by the assembly it reached it through: `name.Length` on
/// a `string` is `netstandard`'s `System.String.Length`, since FSharp.Core
/// names `string` through the facade it compiles against, while the same
/// member on a `String` reached by `open System` is `System.Runtime`'s. The
/// whole-project differential accepts the facade's name for our entity only
/// where the facade's type forwarders send that path there, which this pins
/// against the reference pack: the declaring assembly and the facade both
/// name `String`, and neither names FSharp.Core's `Unit`, which only
/// FSharp.Core does.
#[test]
fn a_facade_names_the_types_it_forwards() {
    let env = crate::common::full_bcl_env();
    let string = env
        .lookup_type(&["System".to_string()], "String", 0)
        .expect("the reference pack declares System.String");
    let unit = env
        .lookup_type(
            &["Microsoft", "FSharp", "Core"].map(String::from),
            "Unit",
            0,
        )
        .expect("FSharp.Core declares Unit");
    let names = |assembly: &str, handle| {
        borzoi_sema::test_support::assembly_names_type(env, assembly, handle)
    };
    assert!(names("System.Runtime", string), "the declaring assembly");
    assert!(names("netstandard", string), "the facade forwarding it");
    assert!(!names("FSharp.Core", string), "an assembly naming neither");
    assert!(names("FSharp.Core", unit), "FSharp.Core's own type");
    assert!(
        !names("netstandard", unit),
        "a facade forwarding nothing there"
    );
    assert!(!names("System.Runtime", unit), "another assembly's type");
}
