//! Uses of functions whose type parameters carry F#'s `equality` or
//! `comparison` constraint, against FCS's typed tree.
//!
//! The two constraints never unify anything: FCS checks a solved type against
//! one (`SolveTypeSupportsEquality`, `SolveTypeSupportsComparison`) and either
//! accepts it or reports FS0001, leaving every type where it was. So a use is
//! typed by its signature as any other assembly value is, each constrained
//! instantiation is checked over the binding's final substitution against a
//! closed list of types known to satisfy it, and a variable left open is never
//! generalised: the canonical renderings cannot show the `when` a scheme would
//! drop.
//!
//! # The grid
//!
//! Every function in [`FUNCTIONS`] (FSharp.Core's operators and functions, the
//! fixture's own `eqC`/`cmpC`) × every operand [`SORTS`] × every [`POSITIONS`]
//! row, one file per function and sort, with the operands annotated
//! parameters, literals, nested applications, and open parameters.
//!
//! # What is asserted
//!
//! - **Soundness** through `infer_operators_diff::check_in`: every commit is
//!   FCS's, and no committed generic binder type hides a constraint FCS puts
//!   on it.
//! - **The table says what it claims.** A sort the grid's own table calls
//!   provable for a constraint is one FCS accepts; an FS0001 lands only in a
//!   file whose sort the table does not call provable.
//! - **Completeness, per row.** In a file FCS accepts whose sort is provable,
//!   every binding is complete (no census reason at all) and every synth row
//!   commits its application. The types oracle cannot see this: a declined
//!   row agrees with it vacuously.
//! - **Ill-typed twins.** A file FCS rejects with FS0001 records
//!   `Incomplete::Constraint` wherever inference reads its operands and models
//!   its callee, and is incomplete for some reason everywhere else.
//!
//! The ordered-pair grid and the argument-check generator of
//! `infer_operators_diff` carry constrained uses too, so the order FCS meets
//! them in is graded with every other relation's.

use crate::infer_operators_diff::{Defs, Row, check_in};
use borzoi_sema::{ConstraintGap, Incomplete};

/// Which constraint a function's operands must satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needs {
    Equality,
    Comparison,
}

/// A function under test.
struct Function {
    /// The use over two operands, `{a}` and `{b}`, as an expression.
    use_ab: &'static str,
    /// The use with the operands piped in last: `{b} |> F {a}`.
    piped: Option<&'static str>,
    needs: Needs,
    /// Whether inference models the use's callee. An operator applied as a
    /// prefix function (`(=) a b`) is not, so its rows are graded for
    /// soundness alone.
    modelled: bool,
}

const fn f(use_ab: &'static str, piped: Option<&'static str>, needs: Needs) -> Function {
    Function {
        use_ab,
        piped,
        needs,
        modelled: true,
    }
}

const fn unmodelled(use_ab: &'static str, needs: Needs) -> Function {
    Function {
        use_ab,
        piped: None,
        needs,
        modelled: false,
    }
}

const FUNCTIONS: &[Function] = &[
    f("{a} = {b}", None, Needs::Equality),
    f("{a} <> {b}", None, Needs::Equality),
    f("{a} < {b}", None, Needs::Comparison),
    f("{a} >= {b}", None, Needs::Comparison),
    unmodelled("(=) {a} {b}", Needs::Equality),
    unmodelled("Operators.(<=) {a} {b}", Needs::Comparison),
    f(
        "compare {a} {b}",
        Some("{b} |> compare {a}"),
        Needs::Comparison,
    ),
    f("hash {a}", Some("{a} |> hash"), Needs::Equality),
    f("max {a} {b}", Some("{b} |> max {a}"), Needs::Comparison),
    f(
        "List.contains {a} (List.singleton {b})",
        Some("List.singleton {b} |> List.contains {a}"),
        Needs::Equality,
    ),
    f(
        "FSharpModuleValues.Fns.eqC {a} {b}",
        Some("{b} |> FSharpModuleValues.Fns.eqC {a}"),
        Needs::Equality,
    ),
    f(
        "FSharpModuleValues.Fns.cmpC {a} {b}",
        Some("{b} |> FSharpModuleValues.Fns.cmpC {a}"),
        Needs::Comparison,
    ),
];

/// An operand type.
struct Sort {
    /// Its annotation, parenthesised where it must be.
    annotation: &'static str,
    /// A literal of it, where one is spelled without a construct inference
    /// does not model.
    literal: Option<&'static str>,
    /// Whether the grid's table calls it provably equatable, and comparable,
    /// for the stage modelled: primitives and `string` both; `obj`, `unit` and
    /// a non-generic type of a non-F# assembly equatable; tuples and arrays
    /// element-wise. Anything else (a function, an in-file type, an F#
    /// generic, a BCL type's interface hierarchy) is not on the list.
    equality: bool,
    comparison: bool,
}

impl Sort {
    /// Whether inference reads the annotation: an in-file type it does not.
    fn bridged(&self) -> bool {
        !self.annotation.starts_with('R')
    }
}

const fn s(
    annotation: &'static str,
    literal: Option<&'static str>,
    equality: bool,
    comparison: bool,
) -> Sort {
    Sort {
        annotation,
        literal,
        equality,
        comparison,
    }
}

const SORTS: &[Sort] = &[
    s("int", Some("1"), true, true),
    s("int64", Some("1L"), true, true),
    s("byte", Some("1uy"), true, true),
    s("sbyte", Some("1y"), true, true),
    s("int16", Some("1s"), true, true),
    s("uint16", Some("1us"), true, true),
    s("uint32", Some("1u"), true, true),
    s("uint64", Some("1UL"), true, true),
    s("nativeint", Some("1n"), true, true),
    s("unativeint", Some("1un"), true, true),
    s("float", Some("1.0"), true, true),
    s("float32", Some("1.0f"), true, true),
    s("decimal", Some("1.0m"), true, true),
    s("char", Some("'c'"), true, true),
    s("bool", Some("true"), true, true),
    s("string", Some("\"s\""), true, true),
    s("unit", Some("()"), true, false),
    s("obj", None, true, false),
    s("(int * string)", Some("(1, \"s\")"), true, true),
    s("(int * (int -> int))", None, false, false),
    s("int[]", None, true, true),
    s("(int -> int)[]", None, false, false),
    s("(int -> int)", None, false, false),
    s("System.Text.StringBuilder", None, true, false),
    s("System.IDisposable", None, true, false),
    s("System.DateTime", None, true, false),
    s("System.Uri", None, true, false),
    // F# types of the fixture assembly: a record with generated equality, a
    // class, and a record whose function field leaves it no equality at all,
    // with no attribute to say so.
    s("FSharpModuleValues.Solo", None, false, false),
    s("FSharpModuleValues.SoloClass", None, false, false),
    s("FSharpModuleValues.FnRec", None, false, false),
    s("R", None, false, false),
    s("RNoEq", None, false, false),
    s("RNoCmp", None, false, false),
    s("RCustomEq", None, false, false),
];

/// The in-file types the sorts name.
const TYPES: &str = r#"type R = { X: int }
[<NoEquality; NoComparison>] type RNoEq = { Y: int }
[<NoComparison>] type RNoCmp = { Z: int }
[<CustomEquality; NoComparison>]
type RCustomEq =
    { W: int }
    override this.Equals(o) = true
    override this.GetHashCode() = 0
"#;

/// One generated row's text and where its application is.
struct GridRow {
    range: (usize, usize),
    synth: bool,
}

/// One function × sort file: a binding per operand form and position.
fn grid_file(func: &Function, sort: &Sort) -> (String, Vec<GridRow>) {
    let mut src = format!("module Constrained\n{TYPES}");
    let mut rows = Vec::new();
    let t = sort.annotation;
    let use_of = |template: &str, a: &str, b: &str| template.replace("{a}", a).replace("{b}", b);
    let mut push = |src: &mut String, head: String, u: String, tail: &str, synth: bool| {
        let start = src.len() + head.len();
        src.push_str(&head);
        src.push_str(&u);
        src.push_str(tail);
        src.push('\n');
        rows.push(GridRow {
            range: (start, start + u.len()),
            synth,
        });
    };
    let params = format!("(a: {t}) (b: {t})");
    let u = use_of(func.use_ab, "a", "b");
    // Annotated parameters, in each position.
    push(&mut src, format!("let r0 {params} = "), u.clone(), "", true);
    push(
        &mut src,
        format!("let r1 {params} = id ("),
        u.clone(),
        ")",
        false,
    );
    push(
        &mut src,
        format!("let r2 {params} = ("),
        u.clone(),
        ", 0)",
        true,
    );
    push(
        &mut src,
        format!("let r3 {params} (c: bool) = if c then "),
        u.clone(),
        &format!(" else {u}"),
        true,
    );
    // Nested applications as operands.
    push(
        &mut src,
        format!("let r4 {params} = "),
        use_of(func.use_ab, "(id a)", "(id b)"),
        "",
        true,
    );
    // Piped.
    if let Some(piped) = func.piped {
        push(
            &mut src,
            format!("let r5 {params} = "),
            use_of(piped, "a", "b"),
            "",
            true,
        );
    }
    // Literals.
    if let Some(lit) = sort.literal {
        push(
            &mut src,
            "let r6 () = ".to_string(),
            use_of(func.use_ab, lit, lit),
            "",
            true,
        );
        push(
            &mut src,
            "let r7 = ".to_string(),
            use_of(func.use_ab, lit, lit),
            "",
            true,
        );
    }
    // Open parameters, one of them fixed by the other: the constrained
    // variable is solved by the annotation, or stays open with it.
    push(
        &mut src,
        format!("let r8 a (b: {t}) = "),
        use_of(func.use_ab, "a", "b"),
        "",
        true,
    );
    // An operand whose type only a relation inference drops fixes (a branch
    // joining the open `p` to `a`), directly and piped: the constraint is
    // met only once that relation is applied, which happens in
    // `Gen::verified`'s probe and nowhere else. Graded for soundness alone.
    let joined = "(if true then p else a)";
    push(
        &mut src,
        format!("let r9 (a: {t}) p = "),
        use_of(func.use_ab, joined, joined),
        "",
        false,
    );
    if let Some(piped) = func.piped {
        push(
            &mut src,
            format!("let r10 (a: {t}) p = "),
            use_of(piped, joined, joined),
            "",
            false,
        );
    }
    (src, rows)
}

/// A file of uses whose operands are open: each constrained variable is left
/// open, so nothing may generalise over it.
fn open_file(func: &Function) -> (String, Vec<GridRow>) {
    let mut src = String::from("module ConstrainedOpen\n");
    let mut rows = Vec::new();
    for (i, (head, tail)) in [
        ("let o0 a b = ", ""),
        ("let o1 a b = (", ", a)"),
        ("let o2 a b = id (", ")"),
    ]
    .into_iter()
    .enumerate()
    {
        let u = func.use_ab.replace("{a}", "a").replace("{b}", "b");
        let start = src.len() + head.len();
        src.push_str(&format!("{head}{u}{tail}\n"));
        rows.push(GridRow {
            range: (start, start + u.len()),
            synth: i != 2,
        });
    }
    // A rigid explicit type parameter carrying no constraint: FCS rejects the
    // use (FS0001), and the binding must not commit as though it were legal.
    let u = func.use_ab.replace("{a}", "a").replace("{b}", "b");
    let head = "let o3<'T> (a: 'T) (b: 'T) = ";
    let start = src.len() + head.len();
    src.push_str(&format!("{head}{u}\n"));
    rows.push(GridRow {
        range: (start, start + u.len()),
        synth: true,
    });
    (src, rows)
}

fn needs_holds(func: &Function, sort: &Sort) -> bool {
    match func.needs {
        Needs::Equality => sort.equality,
        Needs::Comparison => sort.comparison,
    }
}

fn is_constraint_reason(r: &Incomplete) -> bool {
    matches!(r, Incomplete::Constraint(_))
}

/// The grid. See the module docs.
#[test]
fn every_constrained_use_over_every_operand_sort_agrees_with_fcs() {
    let mut obligations = 0;
    let mut missed = Vec::new();
    let mut incomplete = Vec::new();
    let mut table = Vec::new();
    let mut twins = 0;
    let mut untracked = Vec::new();
    for func in FUNCTIONS {
        for sort in SORTS {
            let (src, rows) = grid_file(func, sort);
            let grid: Vec<Row> = rows
                .iter()
                .map(|r| Row {
                    range: r.range,
                    synth: r.synth,
                })
                .collect();
            let label = format!("{} over {}", func.use_ab, sort.annotation);
            let c = check_in(Defs::Assembly, "eqcmp", &src, &grid, false)
                .unwrap_or_else(|| panic!("FCS checks `{label}`\n{src}"));
            let provable = needs_holds(func, sort);
            let fs0001 = c.fcs_error_codes.contains(&1);
            if provable && c.errors > 0 {
                table.push(format!(
                    "  `{label}` is called provable, but FCS rejects it: {:?}",
                    c.fcs_error_codes
                ));
                continue;
            }
            if fs0001 {
                // Every twin is incomplete; one whose operands inference
                // reads and whose callee it models says why.
                twins += 1;
                let says_why = c.reasons.iter().any(is_constraint_reason)
                    || (!(func.modelled && sort.bridged()) && !c.reasons.is_empty());
                if !says_why {
                    untracked.push(format!("  `{label}`: {:?}", c.reasons));
                }
                continue;
            }
            if !provable || c.errors > 0 || !func.modelled {
                continue;
            }
            if !c.reasons.is_empty() {
                incomplete.push(format!("  `{label}`: {:?}", c.reasons));
            }
            for row in &rows {
                if row.synth {
                    obligations += 1;
                    if !c.committed_rows.contains(&row.range) {
                        missed.push(format!("  `{label}`: `{}`", &src[row.range.0..row.range.1]));
                    }
                }
            }
        }
        let (src, rows) = open_file(func);
        let grid: Vec<Row> = rows
            .iter()
            .map(|r| Row {
                range: r.range,
                synth: r.synth,
            })
            .collect();
        let c = check_in(Defs::Assembly, "eqcmp_open", &src, &grid, false)
            .unwrap_or_else(|| panic!("FCS checks the open uses of `{}`\n{src}", func.use_ab));
        assert!(
            c.fcs_error_codes.contains(&1),
            "a rigid `'T` compared is FS0001 for `{}`",
            func.use_ab
        );
    }
    eprintln!(
        "equality/comparison grid: {obligations} obligations, {} missed, {twins} FS0001 twins",
        missed.len()
    );
    assert!(
        table.is_empty(),
        "the table is wrong about {} sort(s):\n{}",
        table.len(),
        table.join("\n")
    );
    assert!(
        untracked.is_empty(),
        "{} file(s) FCS rejects with FS0001 record no constraint reason:\n{}",
        untracked.len(),
        untracked.join("\n")
    );
    assert!(
        incomplete.is_empty(),
        "{} provable file(s) hold an incomplete binding:\n{}",
        incomplete.len(),
        incomplete.join("\n")
    );
    assert!(
        missed.is_empty(),
        "{} synth row(s) over a provable sort did not commit:\n{}",
        missed.len(),
        missed.join("\n")
    );
    assert!(
        obligations >= 1000,
        "vacuous: only {obligations} obligations"
    );
    assert!(twins >= 50, "vacuous: only {twins} FS0001 twins");
}

/// A violation is recorded as one, not as an undecided type: a function
/// compared is FS0001 and `Incomplete::Constraint(Violated)`.
#[test]
fn a_function_compared_records_a_violation() {
    let src = "module Violation\nlet r (a: int -> int) (b: int -> int) = a = b\n";
    let c =
        check_in(Defs::Assembly, "eqcmp_violation", src, &[], false).expect("FCS checks the file");
    assert!(c.fcs_error_codes.contains(&1), "FS0001");
    assert!(
        c.reasons
            .contains(&Incomplete::Constraint(ConstraintGap::Violated)),
        "{:?}",
        c.reasons
    );
}
