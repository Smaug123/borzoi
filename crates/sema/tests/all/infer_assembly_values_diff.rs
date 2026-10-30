//! Uses of a referenced F# assembly's module functions and values against
//! FCS's typed tree: a use the resolver resolved into an assembly module is
//! typed by the member's IL signature read as its F# type
//! (`function_ty::module_function_scheme`), instantiated and loosened as FCS
//! types a use of a value (`TcVal`, `MakeApplicableExprWithFlex`).
//!
//! # What is compared
//!
//! Soundness (D5), certain-implies-exact against both FCS typed-tree oracles,
//! through `infer_operators_diff::check_in`: every expression and binder type
//! we commit, FCS reports identically at the same range.
//!
//! # The universes
//!
//! - **Relocated grids.** `infer_operators_diff`'s pipe grid, use-order grid
//!   and argument-check generator, with their functions moved into the
//!   fixture assembly (`module_value_corpus::RELOCATED`). FCS checks the
//!   relocated program as it checks the in-file one, so the in-file grids'
//!   per-row obligations carry over unchanged: what inference commits over an
//!   in-file function it must commit over the same function read from an
//!   assembly.
//! - **The shape grid.** Every signature shape of `module_value_corpus::SHAPES` × every use form
//!   (a bare value, applied, partially applied, piped, back-piped, composed,
//!   passed as an argument, a parenthesised callee, a parameter flowing into
//!   it) × every position × every spelling (qualified, bare after an `open`,
//!   fully qualified), one file per shape, with an ill-typed twin. Each row
//!   the reading admits, in a synth position where FCS's type is ground,
//!   carries an obligation to commit; each shape it declines must say why in
//!   the census.
//! - **FSharp.Core.** Every module function and value of FSharp.Core the
//!   reading admits, bound as a value and returned from a function.
//!

use std::collections::HashMap;

use crate::common::module_value_corpus::{self, MODULE, NS, OPS_MODULE, Rejection, SHAPES, Shape};
use crate::common::{
    full_bcl_env, invoke_fcs_dump_with_refs, module_value_fixture_env, module_value_fixture_refs,
    parse_fcs_binder_types_with_errors, temp_fs_file,
};
use crate::infer_operators_diff::{
    Defs, Row, argument_check_file, check_in, ordered_pair_grid, pipe_grid,
};
use borzoi_sema::{FunctionTyGap, Incomplete, OperatorGap};

/// FCS's canonical name for `unit`.
const UNIT: &str = "Microsoft.FSharp.Core.Unit";

/// The pipe grid over the fixture's `Pipes` module, with the in-file grid's
/// per-row obligations.
#[test]
fn pipes_and_composition_over_assembly_functions_agree_with_fcs() {
    pipe_grid(Defs::Assembly);
}

/// The use-order grid over the fixture's `Order` module, with the in-file
/// grid's per-row obligations.
#[test]
fn every_ordered_pair_of_uses_through_assembly_functions_agrees_with_fcs() {
    ordered_pair_grid(Defs::Assembly);
}

/// The argument-check generator over the fixture's `Checks` module: every
/// commit is FCS's, and every commit the in-file program makes after its
/// header, the relocated one makes at the same place — the functions are the
/// same functions, and FCS checks the two programs alike. The in-file
/// baseline opens an unrelated fixture module, so that the two differ only
/// in where the functions are defined ([`Defs::InFileBesideOpen`]).
#[test]
fn generated_argument_checks_over_assembly_functions_keep_every_in_file_commit() {
    let files = crate::common::env_usize_or("BORZOI_ARG_CHECK_FILES", 24);
    let mut kept = 0;
    let mut lost = Vec::new();
    for seed in 0..files as u64 {
        let in_file = argument_check_file(Defs::InFileBesideOpen, seed);
        let relocated = argument_check_file(Defs::Assembly, seed);
        let (Some(a), Some(b)) = (
            check_in(Defs::InFileBesideOpen, "arg_checks", &in_file, &[], false),
            check_in(Defs::Assembly, "arg_checks", &relocated, &[], false),
        ) else {
            continue;
        };
        let header = |src: &str| src.find("let g0 ").expect("the first generated function");
        let ours = body_commits(&a, header(&in_file));
        let theirs = body_commits(&b, header(&relocated));
        kept += ours.len();
        let body = &in_file[header(&in_file)..];
        lost.extend(ours.difference(&theirs).map(|(s, e, kind, ty)| {
            format!(
                "  seed {seed}: {kind} `{}` typed `{ty}` in-file only",
                &body[*s..*e]
            )
        }));
    }
    eprintln!("argument-check relocation: {kept} in-file commits after the header");
    assert!(
        lost.is_empty(),
        "{} commit(s) over in-file functions are lost when the functions come from an assembly:\n{}",
        lost.len(),
        lost.join("\n")
    );
    assert!(kept >= files * 4, "vacuous: only {kept} commits to keep");
}

/// A checked file's commits after its first `header` bytes, offsets relative
/// to that point.
fn body_commits(
    c: &crate::infer_operators_diff::Checked,
    header: usize,
) -> std::collections::BTreeSet<(usize, usize, &'static str, String)> {
    c.commits
        .iter()
        .filter(|(start, ..)| *start >= header)
        .map(|(start, end, kind, ty)| (start - header, end - header, *kind, ty.clone()))
        .collect()
}

/// How a shape's name is written at a use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `Fns.f`, under `open FSharpModuleValues`.
    Qualified,
    /// `f`, under `open FSharpModuleValues.Fns`.
    Bare,
    /// `FSharpModuleValues.Fns.f`.
    Full,
}

const SPELLINGS: [Spelling; 3] = [Spelling::Qualified, Spelling::Bare, Spelling::Full];

impl Spelling {
    fn spell(self, name: &str) -> String {
        match self {
            Spelling::Qualified => format!("{MODULE}.{name}"),
            Spelling::Bare => name.to_string(),
            Spelling::Full => format!("{NS}.{MODULE}.{name}"),
        }
    }
}

/// How a use is formed from the shape `f` and its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `f`.
    Value,
    /// `f a1 … an`.
    Applied,
    /// `f a1`, of a function of two or more domains.
    Partial,
    /// `an |> f a1 … an-1`.
    Pipe,
    /// `f a1 … an-1 <| an`.
    BackPipe,
    /// `(f a1 … an-1 >> id) an`.
    Compose,
    /// `f a1 … an-1 >> id`, a function value.
    ComposeValue,
    /// `applyG (f a1 … an-1) an`, through the fixture's own generic
    /// higher-order function.
    Argument,
    /// `applyTo (f a1 … an-1) an`, through an in-file one. Inference does not
    /// generalise `let applyTo g x = g x` (an applied parameter's use is
    /// undecided), so this form is graded but owes no commit.
    InFileArgument,
    /// `(f) a1 … an`.
    ParenCallee,
    /// `id f`.
    IdOf,
    /// `f p a2 … an`, over the function's own unannotated parameter `p`.
    ParamApplied,
    /// `p |> f a1 … an-1`.
    ParamPiped,
    /// `f<int> a1 … an`: an explicit type application, which is not modelled
    /// and must decline. Its type arguments may not fit the shape's type
    /// parameters, so its rows are in the ill-typed family.
    TypeApplied,
}

const FORMS: [Form; 14] = [
    Form::Value,
    Form::Applied,
    Form::Partial,
    Form::Pipe,
    Form::BackPipe,
    Form::Compose,
    Form::ComposeValue,
    Form::Argument,
    Form::InFileArgument,
    Form::ParenCallee,
    Form::IdOf,
    Form::ParamApplied,
    Form::ParamPiped,
    Form::TypeApplied,
];

impl Form {
    /// The use, over `f` and the arguments `args` (the first replaced by
    /// `first` where given), or `None` when the form does not apply to a
    /// shape with this many domains.
    fn render(self, f: &str, args: &[&str], first: Option<&str>) -> Option<String> {
        let mut args: Vec<&str> = args.to_vec();
        if let (Some(first), Some(a1)) = (first, args.first_mut()) {
            *a1 = first;
        }
        let n = args.len();
        let applied = |k: usize| {
            if k == 0 {
                f.to_string()
            } else {
                format!("{f} {}", args[..k].join(" "))
            }
        };
        let paren = |s: String| {
            if s.contains(' ') { format!("({s})") } else { s }
        };
        Some(match self {
            Form::Value => f.to_string(),
            Form::Applied if n >= 1 => applied(n),
            Form::Partial if n >= 2 => applied(1),
            Form::Pipe if n >= 1 => format!("{} |> {}", args[n - 1], applied(n - 1)),
            Form::BackPipe if n >= 1 => format!("{} <| {}", applied(n - 1), args[n - 1]),
            Form::Compose if n >= 1 => format!("({} >> id) {}", applied(n - 1), args[n - 1]),
            Form::ComposeValue if n >= 1 => format!("{} >> id", applied(n - 1)),
            Form::Argument if n >= 1 => {
                format!("applyG {} {}", paren(applied(n - 1)), args[n - 1])
            }
            Form::InFileArgument if n >= 1 => {
                format!("applyTo {} {}", paren(applied(n - 1)), args[n - 1])
            }
            Form::ParenCallee if n >= 1 => format!("({f}) {}", args.join(" ")),
            Form::IdOf => format!("id {f}"),
            Form::ParamApplied if n >= 1 => {
                let mut with_p = args.clone();
                with_p[0] = "p";
                format!("{f} {}", with_p.join(" "))
            }
            Form::ParamPiped if n >= 1 => format!("p |> {}", applied(n - 1)),
            Form::TypeApplied => {
                let mut s = format!("{f}<int>");
                for a in &args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            _ => return None,
        })
    }

    /// Whether the form puts the function's own parameter `p` in the use.
    fn takes_param(self) -> bool {
        matches!(self, Form::ParamApplied | Form::ParamPiped)
    }
}

/// Where a use sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// `let r = U`: a module-level value, which FCS value-restricts when `U`
    /// is generic and not a generalisable expression.
    Top,
    /// `let r () = U`.
    Body,
    /// `let r () = (U, 0)`.
    Tuple,
    /// `let r (b: bool) = if b then U else U`.
    Then,
    /// `let r () = id (U)`: an argument, a check position.
    Arg,
    /// `let r p = (U, p)`, for a form over the parameter `p`.
    WithParam,
}

const POSITIONS: [Position; 6] = [
    Position::Top,
    Position::Body,
    Position::Tuple,
    Position::Then,
    Position::Arg,
    Position::WithParam,
];

impl Position {
    /// The binding's head and the text around `U`; `U` is repeated where the
    /// position writes it twice.
    fn wrap(self, n: usize, u: &str) -> (String, String, String) {
        match self {
            Position::Top => (format!("let r{n} = "), String::new(), String::new()),
            Position::Body => (format!("let r{n} () = "), String::new(), String::new()),
            Position::Tuple => (
                format!("let r{n} () = ("),
                String::new(),
                ", 0)".to_string(),
            ),
            Position::Then => (
                format!("let r{n} (b: bool) = if b then "),
                String::new(),
                format!(" else {u}"),
            ),
            Position::Arg => (
                format!("let r{n} () = id ("),
                String::new(),
                ")".to_string(),
            ),
            Position::WithParam => (format!("let r{n} p = ("), String::new(), ", p)".to_string()),
        }
    }

    fn synth(self) -> bool {
        !matches!(self, Position::Arg)
    }
}

/// The shape grid's probe header, with the in-file `applyTo` of
/// [`Form::InFileArgument`].
fn shape_header() -> String {
    format!(
        "module ShapeUse\n\
         open {NS}\n\
         open {NS}.{MODULE}\n\
         let applyTo g x = g x\n"
    )
}

/// Shapes whose first domain takes `obj`: a parameter the form fixes to it is
/// a use whose loosening FCS decides on a type inference does not know at
/// the use, so the rule declines the whole binding (`Gen::value_use`'s
/// undecided uses), and the parameter forms owe nothing.
const UNDECIDED_PARAMETER_FLOW: &[&str] = &["hofObj"];

/// Shapes taking a type FCS loosens to a bound other than `obj`. The loosened
/// variable is a subtype constraint inference does not check, so a use whose
/// checks fix it undoes them (`Gen::bounded_flex`), and FCS condenses a
/// generalised bounded variable back to its bound, which is not modelled:
/// these owe no commit, as the in-file pipe grid's `fc` and `fv` owe none.
const BOUNDED_LOOSENING: &[&str] = &["takesCmp", "takesUri", "seqP", "raP", "gclassP"];

/// One generated use of a shape.
struct ShapeRow {
    range: (usize, usize),
    synth: bool,
    form: Form,
    text: String,
}

/// One shape's file: every form × position × spelling, the well-typed ones
/// (`ill_typed == false`) or the rejected ones. A module-level value bound to
/// a generic use goes to the rejected family, since FCS may value-restrict
/// it; so does every use of a shape whose attributes reject it.
fn shape_file(shape: &Shape, ill_typed: bool) -> (String, Vec<ShapeRow>) {
    let mut src = shape_header();
    let mut rows = Vec::new();
    let firsts: Vec<Option<&str>> = if ill_typed {
        match shape.bad {
            Some(bad) => vec![None, Some(bad)],
            None => vec![None],
        }
    } else {
        vec![None]
    };
    for spelling in SPELLINGS {
        let f = spelling.spell(shape.name);
        for form in FORMS {
            for &first in &firsts {
                let Some(u) = form.render(&f, shape.args, first) else {
                    continue;
                };
                for position in POSITIONS {
                    if form.takes_param() != (position == Position::WithParam) {
                        continue;
                    }
                    let restricted = position == Position::Top && shape.generic;
                    if form == Form::TypeApplied && (!shape.generic || first.is_some()) {
                        continue;
                    }
                    let row_ill = shape.rejected != Rejection::No
                        || first.is_some()
                        || restricted
                        || form == Form::TypeApplied;
                    if row_ill != ill_typed {
                        continue;
                    }
                    let (head, _, after) = position.wrap(rows.len(), &u);
                    let start = src.len() + head.len();
                    src.push_str(&head);
                    src.push_str(&u);
                    src.push_str(&after);
                    src.push('\n');
                    rows.push(ShapeRow {
                        range: (start, start + u.len()),
                        synth: position.synth(),
                        form,
                        text: format!("{position:?} {spelling:?} {form:?}: {u}"),
                    });
                }
            }
        }
    }
    (src, rows)
}

/// The shape grid: every shape × form × position × spelling, against FCS.
///
/// - Every type committed is FCS's, in both families.
/// - Each well-typed row of a shape the reading admits, in a synth position
///   where FCS's type is ground, commits — a per-row obligation.
/// - A shape the reading admits leaves no assembly-value reason in its file's
///   census, and one it declines records exactly the reason the table says.
/// - The table is what it claims: the well-typed file checks clean (the
///   oracle asserts it), and each rejected shape draws an FCS error.
#[test]
fn every_signature_shape_in_every_use_form_agrees_with_fcs() {
    let mut obligations = 0;
    let mut committed = 0;
    let mut missed = Vec::new();
    let mut census = Vec::new();
    let mut ill_errors = 0;
    for shape in SHAPES {
        let (src, rows) = shape_file(shape, false);
        let grid: Vec<Row> = rows
            .iter()
            .map(|r| Row {
                range: r.range,
                synth: r.synth,
            })
            .collect();
        let c = check_in(Defs::Assembly, "shapes", &src, &grid, !rows.is_empty())
            .unwrap_or_else(|| panic!("FCS checks the `{}` file\n{src}", shape.name));
        committed += c.exprs + c.binders;
        for row in &rows {
            let ground = c
                .fcs_types
                .get(&row.range)
                .is_some_and(|t| !t.contains('\''));
            let undecided = (row.form.takes_param()
                && UNDECIDED_PARAMETER_FLOW.contains(&shape.name))
                || row.form == Form::InFileArgument
                || BOUNDED_LOOSENING.contains(&shape.name);
            if shape.gap.is_none() && row.synth && ground && !undecided {
                obligations += 1;
                if !c.committed_rows.contains(&row.range) {
                    missed.push(format!("  {}: {}", shape.name, row.text));
                }
            }
        }
        let assembly_reasons: Vec<&Incomplete> = c
            .reasons
            .iter()
            .filter(|r| matches!(r, Incomplete::AssemblyValue(_) | Incomplete::NotInFileValue))
            .collect();
        match shape.gap {
            None if !assembly_reasons.is_empty() => census.push(format!(
                "  {} is admitted but its file records {assembly_reasons:?}",
                shape.name
            )),
            Some(gap)
                if !c.reasons.contains(&Incomplete::AssemblyValue(gap)) && !rows.is_empty() =>
            {
                census.push(format!(
                    "  {} declines for {gap:?} but its file records {assembly_reasons:?}",
                    shape.name
                ));
            }
            _ => {}
        }

        let (ill_src, ill_rows) = shape_file(shape, true);
        if ill_rows.is_empty() {
            continue;
        }
        let ill_grid: Vec<Row> = ill_rows
            .iter()
            .map(|r| Row {
                range: r.range,
                synth: r.synth,
            })
            .collect();
        let ill = check_in(Defs::Assembly, "shapes_ill", &ill_src, &ill_grid, false)
            .unwrap_or_else(|| panic!("FCS checks the ill-typed `{}` file\n{ill_src}", shape.name));
        committed += ill.exprs + ill.binders;
        ill_errors += ill.errors;
        let says_so = match shape.rejected {
            Rejection::No => continue,
            Rejection::ByAttribute => Incomplete::AssemblyValue(FunctionTyGap::Rejected),
            Rejection::Inaccessible => Incomplete::NotInFileValue,
        };
        assert!(
            ill.errors > 0,
            "{} is claimed to be rejected, but FCS accepts every use:\n{ill_src}",
            shape.name
        );
        if !ill.reasons.contains(&says_so) {
            census.push(format!(
                "  {} is rejected but its file does not record {says_so:?}: {:?}",
                shape.name, ill.reasons
            ));
        }
    }
    eprintln!(
        "shape grid: {} shapes, {committed} commits, {obligations} obligations, \
         {ill_errors} FCS errors in the ill-typed family",
        SHAPES.len()
    );
    assert!(
        census.is_empty(),
        "{} shape(s) record the wrong census reason:\n{}",
        census.len(),
        census.join("\n")
    );
    assert!(
        missed.is_empty(),
        "{} well-typed row(s) over an admitted shape did not commit:\n{}",
        missed.len(),
        missed.join("\n")
    );
    assert!(
        obligations >= 300,
        "vacuous: only {obligations} obligations"
    );
    assert!(
        ill_errors >= 200,
        "the ill-typed family stopped being rejected: {ill_errors}"
    );
}

/// What the reading says of each shape it admits is the type FCS gives a
/// value bound to it, and it admits exactly the shapes the table says.
#[test]
fn every_admitted_shape_reads_as_the_type_fcs_gives_it() {
    let env = module_value_fixture_env();
    let module = env
        .lookup_type(&[NS.to_owned()], MODULE, 0)
        .unwrap_or_else(|| panic!("the fixture declares {NS}.{MODULE}"));
    let read: HashMap<String, String> =
        borzoi_sema::test_support::module_function_types(env, module)
            .into_iter()
            .collect();
    // Returned from a function, which generalises the use: a module-level
    // value bound to it may not (FCS defaults a type function's use that is
    // not a `[<GeneralizableValue>]` to `obj`).
    let mut src = format!("module Readings\nopen {NS}\n");
    let mut binders = Vec::new();
    for shape in SHAPES {
        let prefix = format!("let v{} () = ", binders.len());
        let start = src.len() + "let ".len();
        binders.push((shape, (start, start + prefix.len() - "let  () = ".len())));
        src.push_str(&format!("{prefix}{MODULE}.{}\n", shape.name));
    }
    let path = temp_fs_file("module_value_readings", &src);
    let json = invoke_fcs_dump_with_refs("binder-types", &path, &module_value_fixture_refs());
    let _ = std::fs::remove_file(&path);
    let (fcs, errors) = parse_fcs_binder_types_with_errors(&json, &src);
    let erroring: Vec<u32> = errors.iter().map(|e| e.line).collect();
    let mut wrong = Vec::new();
    for (line, (shape, range)) in binders.iter().enumerate() {
        // The first binder is on line 3.
        let line = line as u32 + 3;
        let ours = read.get(shape.name);
        match (shape.gap, ours) {
            (None, None) => {
                wrong.push(format!("  {}: admitted by the table, not read", shape.name))
            }
            (Some(gap), Some(ty)) => wrong.push(format!(
                "  {}: declined by the table for {gap:?}, read as `{ty}`",
                shape.name
            )),
            (None, Some(ty)) if !erroring.contains(&line) => {
                // `unit -> T`: the arrow is right-associative and looser than
                // every other constructor, so the prefix needs no parentheses.
                let ty = format!("{UNIT} -> {ty}");
                match fcs.get(range) {
                    Some(theirs) if *theirs == ty => {}
                    theirs => wrong.push(format!(
                        "  {}: we read `{ty}`, FCS says {theirs:?}",
                        shape.name
                    )),
                }
            }
            _ => {}
        }
    }
    assert!(
        wrong.is_empty(),
        "{} reading(s) disagree:\n{}\n{src}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Operators defined outside FSharp.Core are typed as FSharp.Core's are:
/// sealed and generic ones commit, a constrained one and one over a type the
/// bridge does not carry decline with their reasons, and an ill-typed operand
/// commits nothing FCS did not keep.
#[test]
fn an_assembly_operator_outside_fsharp_core_is_an_application_of_its_signature() {
    let src = format!(
        "module OpsUse\n\
         open {NS}.{MODULE}\n\
         open {NS}.{OPS_MODULE}\n\
         let r0 () = 1 +++ 2\n\
         let r1 () = 1 |>> showInt\n\
         let r2 p = (p +++ 1, p)\n\
         let r3 p = (p |>> strLen, p)\n\
         let r4 () = 1 =~ 2\n\
         let r5 () = struct (1, 2) +@ 2\n\
         let r6 () = (|Even|Odd|) 3\n\
         let r7 () = {NS}.{OPS_MODULE}.(|Pos|_|) 3\n\
         let r8 () = (|Even|Odd|)\n\
         let r9 () = match 3 with Even -> 1 | Odd -> 2\n\
         let r10 () = optVal +? 2\n\
         let r11 () = \"s\" =? \"t\"\n"
    );
    let at = |needle: &str| {
        let start = src.find(needle).expect("the row");
        (start, start + needle.len())
    };
    let must = [
        at("1 +++ 2"),
        at("1 |>> showInt"),
        at("p +++ 1"),
        at("p |>> strLen"),
        at("optVal +? 2"),
        at("1 =~ 2"),
    ];
    let rows: Vec<Row> = must
        .iter()
        .map(|&range| Row { range, synth: true })
        .collect();
    let c = check_in(Defs::Assembly, "ops", &src, &rows, true).expect("FCS checks the file");
    assert_eq!(c.committed_rows.len(), must.len(), "{c:?}\n{src}");
    for gap in [OperatorGap::Constrained, OperatorGap::Signature] {
        assert!(
            c.reasons.contains(&Incomplete::Operator(gap)),
            "no {gap:?} decline: {:?}",
            c.reasons
        );
    }
    let ill = format!(
        "module OpsIll\n\
         open {NS}.{MODULE}\n\
         open {NS}.{OPS_MODULE}\n\
         let r0 () = \"s\" +++ 2\n\
         let r1 p = (p +++ 1, strLen p, p)\n\
         let r2 p = (strLen p, p +++ 1, p)\n\
         let r3 () = 1 |>> strLen\n"
    );
    let c = check_in(Defs::Assembly, "ops_ill", &ill, &[], false).expect("FCS checks the file");
    assert!(
        c.errors >= 4,
        "the ill-typed operator file stopped being rejected: {c:?}"
    );
}

/// Two assemblies declare `Collision.Thing`, with `P` a `string` in one and
/// an `int` in the other. `Ty` names a type by its path alone, so reading a
/// value of either assembly's `Thing` as that path would resolve its members
/// against whichever one a path lookup finds: neither may be read, and what
/// is committed must be FCS's.
#[test]
fn a_named_type_two_assemblies_declare_is_not_read() {
    let src = "module Collide\n\
               let a () = let v = Collision.A.item in v.P\n\
               let b () = let v = Collision.B.item in v.P\n\
               let c () = Collision.B.item\n\
               let d () = Collision.A.take Collision.A.item\n\
               let e () = Collision.B.take Collision.B.item\n\
               let f () = let v = Collision.A.gitem in v.P\n\
               let g () = let v = Collision.B.gitem in v.P\n\
               let h () = Collision.A.gitem\n\
               let i () = Collision.B.gitem\n\
               let j () = Collision.A.gtake Collision.A.gitem\n\
               let k () = Collision.B.gtake Collision.B.gitem\n";
    // Both sides decline, the one a path lookup happens to land on too: which
    // one that is depends on reference order, which `Ty` does not carry. The
    // generic `GThing<'T>` is declared by both too.
    let accesses: Vec<(usize, usize)> = [
        "Collision.A.item in v.P",
        "Collision.B.item in v.P",
        "Collision.A.gitem in v.P",
        "Collision.B.gitem in v.P",
    ]
    .iter()
    .map(|body| {
        let at = src.find(body).expect("the row") + body.len() - "v.P".len();
        (at, at + "v.P".len())
    })
    .collect();
    let rows: Vec<Row> = accesses
        .iter()
        .map(|&range| Row { range, synth: true })
        .collect();
    let c = check_in(Defs::Assembly, "collide", src, &rows, true).expect("FCS checks the file");
    assert!(
        c.committed_rows.is_empty() && c.binders == 0,
        "a value of the ambiguous type is read: {c:?}"
    );
    assert!(
        c.reasons
            .contains(&Incomplete::AssemblyValue(FunctionTyGap::Type)),
        "the ambiguous type is read: {c:?}"
    );
}

/// A generic record and union the twin assembly alone declares: a function or
/// value over them reads them from the twin, through the reference's own
/// scope, and commits FCS's type.
#[test]
fn a_generic_type_of_another_assembly_is_read_through_its_scope() {
    let src = "module TwinUse\n\
               open TwinOnly\n\
               let a () = TwinFns.tbox TwinFns.tboxVal\n\
               let b () = TwinFns.tboxVal\n\
               let c () = TwinFns.tboxG 1\n\
               let d () = TwinFns.tu TwinFns.tuVal\n\
               let e () = TwinFns.tuVal\n\
               let f () = TwinFns.tboxVal |> TwinFns.tbox\n";
    let must: Vec<(usize, usize)> = [
        "TwinFns.tbox TwinFns.tboxVal",
        "TwinFns.tboxVal\n",
        "TwinFns.tboxG 1",
        "TwinFns.tu TwinFns.tuVal",
        "TwinFns.tuVal\n",
        "TwinFns.tboxVal |> TwinFns.tbox",
    ]
    .iter()
    .map(|body| {
        let body = body.trim_end();
        let at = src
            .match_indices(body)
            .map(|(i, _)| i)
            .find(|&i| src[..i].ends_with("= "))
            .expect("the row");
        (at, at + body.len())
    })
    .collect();
    let rows: Vec<Row> = must
        .iter()
        .map(|&range| Row { range, synth: true })
        .collect();
    let c =
        check_in(Defs::Assembly, "twin_generic", src, &rows, true).expect("FCS checks the file");
    assert_eq!(
        c.committed_rows.len(),
        must.len(),
        "a function over the twin's generic types is not typed: {c:?}\n{src}"
    );
    let ill = "module TwinIll\n\
               open TwinOnly\n\
               let a () = TwinFns.tbox TwinFns.tuVal\n\
               let b () = TwinFns.tu TwinFns.tboxVal\n\
               let c () = TwinFns.tbox (TwinFns.tboxG \"s\")\n";
    let c =
        check_in(Defs::Assembly, "twin_generic_ill", ill, &[], false).expect("FCS checks the file");
    assert!(
        c.errors >= 3,
        "the ill-typed twin file stopped being rejected: {c:?}"
    );
}

/// A member access on an assembly value of a generic type — an F# list,
/// option, ref, a generic record, union and struct, a BCL class — through a
/// local, a parameter, the path itself and a pipe. A generic receiver's
/// members are instantiation-dependent and the member lookup is not, so the
/// accesses are only graded: whatever is committed is FCS's.
#[test]
fn a_member_of_an_assembly_value_of_a_generic_type_commits_only_what_fcs_says() {
    let receivers: &[(&str, &str, &[&str])] = &[
        (
            "int list",
            "intList",
            &["Length", "Head", "Tail", "IsEmpty"],
        ),
        ("int option", "optVal", &["Value", "IsSome"]),
        ("int ref", "refVal", &["Value", "contents"]),
        ("GBox<int>", "gboxVal", &["G"]),
        ("GU<string>", "guVal", &["IsGA"]),
        (
            "System.Collections.Generic.KeyValuePair<int, string>",
            "kvpVal",
            &["Key", "Value"],
        ),
        ("ResizeArray<int>", "raVal", &["Count"]),
        ("Result<int, string>", "resVal", &["IsOk"]),
    ];
    let mut src = format!(
        "module GenericMembers\n\
         open {NS}\n\
         open {NS}.{MODULE}\n"
    );
    let mut n = 0;
    for (ty, value, members) in receivers {
        for member in *members {
            src.push_str(&format!("let a{n} () = let v = {value} in v.{member}\n"));
            src.push_str(&format!("let b{n} (v: {ty}) = v.{member}\n"));
            src.push_str(&format!("let c{n} () = {value}.{member}\n"));
            src.push_str(&format!("let d{n} () = {MODULE}.{value}.{member}\n"));
            src.push_str(&format!("let e{n} () = ({value}.{member}, {value})\n"));
            src.push_str(&format!("let f{n} p = (p.{member}, lstG [p])\n"));
            n += 1;
        }
    }
    let c =
        check_in(Defs::Assembly, "generic_members", &src, &[], false).expect("FCS checks the file");
    eprintln!("generic receivers: {c:?}");
}

/// A function over a byref-like generic (`ReadOnlySpan<int>`), taken or
/// returned, is not read: F# admits such a type as a parameter of a function
/// it applies directly, never in a function value's type, which is what a
/// reading claims. The control beside them is.
#[test]
fn a_function_over_a_byref_like_generic_is_not_read() {
    let env = module_value_fixture_env();
    let module = env
        .lookup_type(&[NS.to_owned()], module_value_corpus::BYREF_LIKE_MODULE, 0)
        .expect("the fixture declares the byref-like module");
    let read: HashMap<String, String> =
        borzoi_sema::test_support::module_function_types(env, module)
            .into_iter()
            .collect();
    assert!(
        read.contains_key("control"),
        "the control is not read: {read:?}"
    );
    for name in ["spanLen", "spanOf"] {
        assert!(!read.contains_key(name), "{name} is read: {read:?}");
    }
}

/// Every renamed FSharp.Core generic the bridge may carry is FSharp.Core's
/// own type, at the compiled path the table says, with the source name the
/// table says: the table `Ty::render_fsharp` reads its F# names from cannot
/// name a type F# spells differently.
#[test]
fn every_renamed_fsharp_core_generic_is_the_type_the_table_says() {
    let env = full_bcl_env();
    let mut wrong = Vec::new();
    for &(namespace, compiled, source, display) in borzoi_sema::test_support::fsharp_core_renamed()
    {
        let ns: Vec<String> = namespace.split('.').map(str::to_owned).collect();
        let found: Vec<_> = env
            .all_handles()
            .filter(|&h| {
                let e = env.entity(h);
                e.namespace == ns && e.name == compiled && !e.generic_parameters.is_empty()
            })
            .collect();
        if found.is_empty() {
            wrong.push(format!("  {namespace}.{compiled}: not in FSharp.Core"));
        }
        for h in found {
            let e = env.entity(h);
            if e.assembly.name != "FSharp.Core" || e.source_name.as_deref() != Some(source) {
                wrong.push(format!(
                    "  {namespace}.{compiled}: {} names it {:?}",
                    e.assembly.name, e.source_name
                ));
            }
        }
        // The display name is F#'s: the source name, or an FSharp.Core
        // abbreviation of the type.
        if display != source {
            let abbrev = env.lookup_type(&ns, display, 1).or_else(|| {
                env.lookup_type(
                    &["Microsoft".into(), "FSharp".into(), "Core".into()],
                    display,
                    1,
                )
            });
            if abbrev.is_none_or(|h| !env.is_abbreviation(h)) {
                wrong.push(format!(
                    "  {namespace}.{compiled}: `{display}` is not an FSharp.Core abbreviation"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A member access on an assembly value of a record or a class type, through a
/// local, a parameter and the path itself: the receiver's and the access's
/// nodes must be FCS's, and the accesses commit. FCS keeps no node at a
/// record field get's receiver (`let b (v: Solo) = v.Q` has one at `v.Q`
/// only), where a property's receiver has one.
#[test]
fn a_member_of_an_assembly_value_is_typed_as_fcs_types_it() {
    let mut src = format!(
        "module Members\n\
         open {NS}\n\
         open {NS}.{MODULE}\n"
    );
    // The accesses through a local bound to an assembly value, which must
    // commit; the others are graded only.
    let mut must = Vec::new();
    for (ty, value) in module_value_corpus::RECEIVERS {
        for member in ["Q", "M"] {
            let n = must.len();
            let local = format!("let a{n} () = let v = {value} in ");
            let start = src.len() + local.len();
            src.push_str(&format!("{local}v.{member}\n"));
            must.push((start, start + 2 + member.len()));
            src.push_str(&format!("let b{n} (v: {ty}) = v.{member}\n"));
            src.push_str(&format!("let c{n} () = {value}.{member}\n"));
            src.push_str(&format!("let d{n} () = {MODULE}.{value}.{member}\n"));
            src.push_str(&format!("let e{n} () = ({value}.{member}, {value})\n"));
        }
    }
    let rows: Vec<Row> = must
        .iter()
        .map(|&range| Row { range, synth: true })
        .collect();
    let c = check_in(Defs::Assembly, "members", &src, &rows, true).expect("FCS checks the file");
    let missed: Vec<&str> = must
        .iter()
        .filter(|r| !c.committed_rows.contains(r))
        .map(|&(s, e)| &src[s..e])
        .collect();
    assert!(
        missed.len() * 4 <= must.len(),
        "most member accesses on an assembly value do not commit: {missed:?}\n{src}"
    );
}

/// A function or value of a module F# rejects a use through — an error-level
/// `[<Obsolete>]` or `[<CompilerMessage>]` on the module, or on one enclosing
/// it — is not typed by its signature (FCS reports the attribute and types the
/// use as its error type), qualified or opened; the warning-level twins are,
/// and must commit.
#[test]
fn a_member_of_a_module_f_sharp_rejects_is_not_read() {
    let rejected = ["ObsMod", "CmMod", "ObsOuter.Inner"];
    let warned = ["ObsWarnMod", "CmWarnMod"];
    let mut src = String::from("module Rejected\n#nowarn \"44\"\n#nowarn \"9001\"\n");
    let mut must = Vec::new();
    for (n, m) in rejected.iter().chain(&warned).enumerate() {
        let applied = format!("{NS}.{m}.f 1");
        let start = src.len() + format!("let a{n} () = ").len();
        src.push_str(&format!("let a{n} () = {applied}\n"));
        src.push_str(&format!("let b{n} () = ({NS}.{m}.v, {NS}.{m}.f)\n"));
        if warned.contains(m) {
            must.push((start, start + applied.len()));
        }
    }
    let rows: Vec<Row> = must
        .iter()
        .map(|&range| Row { range, synth: true })
        .collect();
    let c = check_in(Defs::Assembly, "rejected_modules", &src, &rows, false)
        .expect("FCS checks the file");
    assert!(
        c.errors >= rejected.len(),
        "FCS accepts the rejected modules: {c:?}\n{src}"
    );
    assert_eq!(
        c.committed_rows.len(),
        must.len(),
        "a warned module's function is not typed: {c:?}\n{src}"
    );
    assert!(
        c.reasons
            .contains(&Incomplete::AssemblyValue(FunctionTyGap::Rejected)),
        "no use records the rejection: {:?}",
        c.reasons
    );
}

/// The in-file counterpart of [`a_member_of_a_module_f_sharp_rejects_is_not_read`]:
/// a binder of an in-file module carrying a rejecting attribute.
#[test]
fn a_binder_of_an_in_file_module_f_sharp_rejects_commits_nothing_wrong() {
    let src = "module InFileRejected\n\
               #nowarn \"44\"\n\
               [<System.Obsolete(\"no\", true)>]\n\
               module ObsMod =\n    let f (x: int) = x\n    let v = 1\n\
               [<CompilerMessage(\"no\", 9001, IsError = true)>]\n\
               module CmMod =\n    let f (x: int) = x\n    let v = 1\n\
               [<System.Obsolete(\"no\", true)>]\n\
               module ObsOuter =\n    module Inner =\n        let f (x: int) = x\n\
               let a0 () = ObsMod.f 1\n\
               let a1 () = CmMod.f 1\n\
               let a2 () = ObsOuter.Inner.f 1\n\
               let b0 () = (ObsMod.v, CmMod.v)\n";
    let c = check_in(Defs::InFile, "in_file_rejected_modules", src, &[], false)
        .expect("FCS checks the file");
    eprintln!("in-file rejected modules: {c:?}");
}

/// Every module function and value of FSharp.Core the reading admits, used as
/// a value: the reading is FCS's type for `let v = M.f`, and inference's
/// binder type for `let w () = M.f` — which generalises — is FCS's too, with a
/// floor on how many it commits.
#[test]
fn every_admitted_fsharp_core_module_value_is_the_type_fcs_gives_it() {
    let env = full_bcl_env();
    let mut src = String::from("module CoreValues\n");
    // (line of `w`, binder range of `w`, the use's name, our reading). The
    // reading is compared at `w`, which generalises the use; `v` is graded
    // by inference alone, since FCS may default a value it does not
    // generalise.
    let mut rows: Vec<(u32, (usize, usize), String, String)> = Vec::new();
    // The rows a use of which no loosening touches, by a closed list of shapes
    // FCS treats as sealed: each of these `w` must commit.
    let mut owed: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut handles: Vec<_> = env
        .all_handles()
        .filter(|&h| {
            env.is_module(h) && env.is_public(h) && env.entity(h).assembly.name == "FSharp.Core"
        })
        .collect();
    handles.sort_by_key(|&h| env.entity_full_name(h));
    for handle in handles {
        let path = env.entity_full_name(handle);
        let schemes: HashMap<String, borzoi_sema::Ty> =
            borzoi_sema::test_support::module_function_schemes(env, handle)
                .into_iter()
                .collect();
        let constrained = borzoi_sema::test_support::module_function_constrained(env, handle);
        for (name, ty) in borzoi_sema::test_support::module_function_types(env, handle) {
            let n = rows.len();
            // A value whose type parameter carries `equality` or `comparison`
            // is read but not generalised over: no scheme says `when`.
            let unloosened = schemes.get(&name).is_some_and(takes_only_surely_sealed)
                && !constrained.contains(&name);
            let spelled = format!("{path}.``{name}``");
            src.push_str(&format!("let v{n} = {spelled}\n"));
            let w = format!("w{n}");
            let start = src.len() + "let ".len();
            let line = src.lines().count() as u32 + 1;
            src.push_str(&format!("let {w} () = {spelled}\n"));
            if unloosened {
                owed.insert((start, start + w.len()));
            }
            rows.push((
                line,
                (start, start + w.len()),
                format!("{path}.{name}"),
                format!("{UNIT} -> {ty}"),
            ));
        }
    }
    assert!(
        rows.len() >= 100,
        "vacuous: only {} FSharp.Core values read",
        rows.len()
    );
    let path = temp_fs_file("core_values", &src);
    let json = invoke_fcs_dump_with_refs("binder-types", &path, &[]);
    let _ = std::fs::remove_file(&path);
    let (fcs, errors) = parse_fcs_binder_types_with_errors(&json, &src);
    let erroring: std::collections::HashSet<u32> = errors.iter().map(|e| e.line).collect();
    let mut wrong = Vec::new();
    let mut compared = 0;
    for (line, range, name, ty) in &rows {
        if erroring.contains(line) {
            continue;
        }
        compared += 1;
        match fcs.get(range) {
            Some(theirs) if theirs == ty => {}
            theirs => wrong.push(format!("  {name}: we read `{ty}`, FCS says {theirs:?}")),
        }
    }
    eprintln!(
        "FSharp.Core values: {} read, {compared} compared, {} lines FCS rejects",
        rows.len(),
        erroring.len()
    );
    assert!(
        wrong.is_empty(),
        "{} FSharp.Core reading(s) disagree with FCS:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert!(
        compared * 10 >= rows.len() * 9,
        "too many rejected: {compared} of {}",
        rows.len()
    );

    // Inference over the same file: every commit is FCS's, and the `w`
    // functions, which generalise, commit wherever no loosening touches the
    // value.
    let c = check_in(Defs::InFile, "core_values", &src, &[], false).expect("FCS checks the file");
    let mut reasons: HashMap<String, usize> = HashMap::new();
    for r in &c.reasons {
        *reasons.entry(format!("{r:?}")).or_default() += 1;
    }
    let committed: std::collections::HashSet<(usize, usize)> = c
        .commits
        .iter()
        .filter(|(_, _, kind, _)| *kind == "binder")
        .map(|(s, e, _, _)| (*s, *e))
        .collect();
    let missed: Vec<String> = rows
        .iter()
        .filter(|(line, range, ..)| owed.contains(range) && !erroring.contains(line))
        .filter(|(_, range, ..)| !committed.contains(range))
        .map(|(_, _, name, ty)| format!("  {name}: {ty}"))
        .collect();
    eprintln!(
        "FSharp.Core values: {} binder commits, {} owed; incompleteness: {reasons:?}",
        c.binders,
        owed.len()
    );
    assert!(
        missed.is_empty(),
        "{} FSharp.Core value(s) no loosening touches are not typed when returned:\n{}",
        missed.len(),
        missed.join("\n")
    );
    assert!(
        owed.len() * 2 >= rows.len(),
        "vacuous: only {} of {} values owe a commit",
        owed.len(),
        rows.len()
    );
}

/// Whether every parameter of the scheme `ty` — of each curried group, and
/// each element of a tupled one — has a shape on a closed list FCS treats as
/// sealed (`isSealedTy`), so that a use of the value is not loosened: a type
/// parameter, a function, an array, `unit`, a sealed primitive, or one of
/// FSharp.Core's sealed generics. A closed list rather than inference's own
/// test, so the obligation it decides does not move with that test.
fn takes_only_surely_sealed(ty: &borzoi_sema::Ty) -> bool {
    use borzoi_sema::Ty;
    const SEALED: &[&str] = &[
        "System.Int32",
        "System.Int64",
        "System.Double",
        "System.Single",
        "System.Boolean",
        "System.String",
        "System.Char",
        "System.Byte",
        "System.Decimal",
        "Microsoft.FSharp.Core.Unit",
        "Microsoft.FSharp.Collections.FSharpList",
        "Microsoft.FSharp.Core.FSharpOption",
        "Microsoft.FSharp.Core.FSharpValueOption",
        "Microsoft.FSharp.Core.FSharpResult",
        "Microsoft.FSharp.Core.FSharpChoice",
        "Microsoft.FSharp.Core.FSharpRef",
    ];
    let sealed = |t: &Ty| match t {
        Ty::Param(_) | Ty::Fun { .. } | Ty::Array { .. } | Ty::Tuple(_) => true,
        Ty::Named { path, .. } => SEALED.contains(&path.join(".").as_str()),
        Ty::Var(_) => false,
    };
    let mut ty = ty;
    while let Ty::Fun { arg, ret } = ty {
        let ok = match arg.as_ref() {
            Ty::Tuple(elems) => elems.iter().all(sealed),
            other => sealed(other),
        };
        if !ok {
            return false;
        }
        ty = ret;
    }
    true
}
