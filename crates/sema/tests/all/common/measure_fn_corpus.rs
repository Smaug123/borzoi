//! The **unit-of-measure module-function** universe: an F#-authored fixture
//! assembly whose module functions carry measure-annotated types, for the
//! differential that grades reading a module function's IL signature as its F#
//! type (`module_function_measures_diff`).
//!
//! IL erases units of measure: `let f (x: float<m>) : int` compiles to
//! `int32 f(float64)`, and nothing in the IL records the `m`. Worse, a measure
//! can hide behind a **named type** the signature pickle mentions without
//! expanding — `type Dist = float<m>` is an abbreviation, and a function over
//! `Dist` pickles the name `Dist` with no measure in sight. So a reader that
//! looks only for a measure the pickled type spells out reads `Dist -> int` as
//! `float -> int`, where FCS says `float<m> -> int`.
//!
//! The universe is the product of two tables, so a hole fails on every shape at
//! once rather than on whichever one a hand-written case happened to pick:
//!
//! - [`SHAPES`]: the type a parameter or return is declared with. FSharp.Core's
//!   measure-annotated primitives, compound measures, abbreviations that hide a
//!   measure (one level, two levels, a function abbreviation, a generic one,
//!   one over a measure-annotated abbreviation), user
//!   `[<MeasureAnnotatedAbbreviation>]`s (the `FSharp.UMX` pattern), and the
//!   same abbreviations declared in **another assembly** — plus measure-free
//!   controls of the same IL types, so a fix that declines by IL type, or by
//!   "is an abbreviation", is visible as a lost control.
//! - [`POSITIONS`]: where the shape sits in the function's type — a parameter,
//!   the return, both, a later curried parameter, a function-typed
//!   parameter's domain, and a returned function's range.
//!
//! [`EXTRAS`] adds the measure-*generic* functions, whose IL has no generic
//! parameter for the measure, and an unconstrained generic control.

/// The assembly name the fixture project emits.
pub const FIXTURE_ASM: &str = "SemaMeasureFnFixture";

/// The assembly name of the other-assembly half (`tests/fixtures/measure_fn_base_env`).
pub const BASE_ASM: &str = "SemaMeasureFnBase";

/// The namespace every fixture declaration sits in.
pub const NS: &str = "FSharpMeasureFn";

/// The module holding every function.
pub const MODULE: &str = "Fns";

/// One type a parameter or return can be declared with.
pub struct Shape {
    /// The F# type, verbatim as the fixture writes it.
    pub written: &'static str,
    /// Whether the F# type carries a unit of measure anywhere — directly, under
    /// an abbreviation, or as a type argument. Checked against the oracle, not
    /// trusted.
    pub measure_bearing: bool,
}

const fn shape(written: &'static str, measure_bearing: bool) -> Shape {
    Shape {
        written,
        measure_bearing,
    }
}

/// The shapes. See the module docs.
pub const SHAPES: &[Shape] = &[
    // Measure-free controls, of the IL types the measure shapes erase to.
    shape("float", false),
    shape("float32", false),
    shape("int", false),
    shape("int64", false),
    shape("string", false),
    shape("bool", false),
    shape("float -> int", false),
    shape("Plain", false),
    shape("Plain2", false),
    shape("PlainFn", false),
    shape("ToPlain<int>", false),
    shape("FSharpMeasureFnBase.BasePlain", false),
    // FSharp.Core's measure-annotated primitives.
    shape("float<m>", true),
    shape("float32<m>", true),
    shape("int<m>", true),
    shape("int64<m>", true),
    shape("int16<m>", true),
    shape("sbyte<m>", true),
    shape("byte<m>", true),
    shape("uint64<m>", true),
    shape("uint32<m>", true),
    shape("uint16<m>", true),
    shape("nativeint<m>", true),
    shape("unativeint<m>", true),
    // Compound measures.
    shape("float<m/s>", true),
    shape("float<m^2>", true),
    shape("float<m s>", true),
    shape("float<1/s>", true),
    // A measure as a type argument of a measure-free abbreviation.
    shape("ToPlain<float<m>>", true),
    shape("float<m> -> int", true),
    // Abbreviations hiding a measure.
    shape("Dist", true),
    shape("Dist2", true),
    shape("DistFn", true),
    shape("ToDist<int>", true),
    shape("Name", true),
    shape("NameInt", true),
    shape("PlainOfDist", true),
    shape("ViaBase", true),
    // User measure-annotated abbreviations.
    shape("Tagged<m>", true),
    shape("TaggedInt<m>", true),
    // The same, declared in another assembly.
    shape("FSharpMeasureFnBase.BaseDist", true),
    shape("FSharpMeasureFnBase.BaseName", true),
    shape("FSharpMeasureFnBase.BaseDistFn", true),
    shape("FSharpMeasureFnBase.BaseTagged<m>", true),
    shape("FSharpMeasureFnBase.BaseToDist<int>", true),
];

/// Where a shape sits in a function's type: its parameter types and its
/// return type, each with `{s}` for the shape.
pub struct Position {
    pub label: &'static str,
    pub params: &'static [&'static str],
    pub ret: &'static str,
}

/// The positions. See the module docs.
pub const POSITIONS: &[Position] = &[
    Position {
        label: "parameter",
        params: &["{s}"],
        ret: "int",
    },
    Position {
        label: "return",
        params: &["int"],
        ret: "{s}",
    },
    Position {
        label: "parameter and return",
        params: &["{s}"],
        ret: "{s}",
    },
    Position {
        label: "second curried parameter",
        params: &["int", "{s}"],
        ret: "int",
    },
    Position {
        label: "function parameter's domain",
        params: &["({s}) -> int"],
        ret: "int",
    },
    Position {
        label: "returned function's range",
        params: &["int"],
        ret: "(int -> {s})",
    },
];

/// A function outside the shape × position product.
pub struct Extra {
    pub label: &'static str,
    /// The `let` after `let f{n}`: type parameters, parameters, return, body.
    pub decl: &'static str,
    /// The arguments an application passes and how many there are, or `None`
    /// when the applied result would be a value-restricted generic.
    pub applied: Option<(&'static str, usize)>,
    pub measure_bearing: bool,
    /// Whether the reader is required to commit this function's type.
    pub is_control: bool,
}

/// See the module docs.
pub const EXTRAS: &[Extra] = &[
    Extra {
        label: "explicit measure parameter",
        decl: "<[<Measure>] 'u> (x: float<'u>) : float<'u> = x",
        applied: Some(("1.0<m>", 1)),
        measure_bearing: true,
        is_control: false,
    },
    Extra {
        label: "implicit measure parameter",
        decl: " (x: float<'u>) : int = 0",
        applied: Some(("1.0<m>", 1)),
        measure_bearing: true,
        is_control: false,
    },
    Extra {
        label: "type and measure parameters",
        decl: "<'a, [<Measure>] 'u> (a: 'a) (x: float<'u>) : 'a = a",
        applied: Some(("1 1.0<m>", 2)),
        measure_bearing: true,
        is_control: false,
    },
    Extra {
        label: "measure parameter in the return only",
        decl: "<[<Measure>] 'u> (x: int) : float<'u> = LanguagePrimitives.FloatWithMeasure 0.0",
        applied: None,
        measure_bearing: true,
        is_control: false,
    },
    Extra {
        label: "unconstrained type parameter (control)",
        decl: "<'a> (x: 'a) (y: int) : 'a = x",
        // Applied, the result is the type parameter, which the reading leaves
        // to the application to solve.
        applied: None,
        measure_bearing: false,
        is_control: true,
    },
];

/// One generated function, and how the probe uses it.
#[derive(Debug, Clone)]
pub struct Row {
    pub label: String,
    /// The function's name in [`MODULE`].
    pub name: String,
    pub measure_bearing: bool,
    pub is_control: bool,
    /// Half-open byte range of the value binder `v{n}` (`let v{n} = Fns.f{n}`).
    pub value_binder: (usize, usize),
    /// Half-open byte range of the application binder `a{n}`
    /// (`let a{n} = Fns.f{n} args…`), when the probe applies it.
    pub applied_binder: Option<(usize, usize)>,
    /// How many arguments the application passes.
    pub applied_arity: usize,
}

/// The generated universe.
#[derive(Debug)]
pub struct Corpus {
    pub fixture: String,
    pub probe: String,
    pub rows: Vec<Row>,
}

/// The fixture's fixed preamble: the measures and abbreviations the shapes name.
const PREAMBLE: &str = "\
#nowarn \"42\"

[<Measure>]
type m

[<Measure>]
type s

/// An abbreviation whose name says nothing about the measure it hides.
type Dist = float<m>

/// Two levels of abbreviation over a measure.
type Dist2 = Dist

/// The measure-free twins of `Dist` and `Dist2`.
type Plain = float
type Plain2 = Plain

/// A function abbreviation hiding a measure in its domain, and its twin.
type DistFn = float<m> -> int
type PlainFn = float -> int

/// A generic abbreviation hiding a measure in its range, and its twin.
type ToDist<'a> = 'a -> Dist
type ToPlain<'a> = 'a -> Plain

/// The `FSharp.UMX` pattern: a measure on a type FSharp.Core never annotates.
[<MeasureAnnotatedAbbreviation>]
type Tagged<[<Measure>] 'u> = string

[<MeasureAnnotatedAbbreviation>]
type TaggedInt<[<Measure>] 'u> = int

/// Abbreviations over the measure-annotated ones, hiding their measures.
type Name = Tagged<m>
type NameInt = TaggedInt<m>

/// A measure-free abbreviation applied to one hiding a measure.
type PlainOfDist = ToPlain<Dist>

/// An abbreviation over another assembly's abbreviation hiding a measure.
type ViaBase = FSharpMeasureFnBase.BaseDist
";

fn substitute(template: &str, shape: &str) -> String {
    template.replace("{s}", shape)
}

/// One function of the fixture.
struct Function {
    label: String,
    /// The `let` after `let f{n}`.
    decl: String,
    /// The arguments an application passes, and how many there are.
    applied: Option<(String, usize)>,
    measure_bearing: bool,
    is_control: bool,
}

/// Every function of the fixture, in declaration order.
fn functions() -> Vec<Function> {
    let mut out = Vec::new();
    for position in POSITIONS {
        for shape in SHAPES {
            let params: Vec<String> = position
                .params
                .iter()
                .map(|p| substitute(p, shape.written))
                .collect();
            let ret = substitute(position.ret, shape.written);
            let decl = format!(
                " {} : {ret} = Unchecked.defaultof<{ret}>",
                params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("(p{i}: {p})"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let applied = params
                .iter()
                .map(|p| format!("Unchecked.defaultof<{p}>"))
                .collect::<Vec<_>>()
                .join(" ");
            out.push(Function {
                label: format!("{}: {}", position.label, shape.written),
                decl,
                applied: Some((applied, params.len())),
                measure_bearing: shape.measure_bearing,
                is_control: !shape.measure_bearing,
            });
        }
    }
    for extra in EXTRAS {
        out.push(Function {
            label: extra.label.to_string(),
            decl: extra.decl.to_string(),
            applied: extra.applied.map(|(args, n)| (args.to_owned(), n)),
            measure_bearing: extra.measure_bearing,
            is_control: extra.is_control,
        });
    }
    out
}

/// The fixture assembly's source.
pub fn fixture_source() -> String {
    let mut out = format!(
        "// GENERATED by crates/sema/tests/all/common/measure_fn_corpus.rs — do not edit.\n\
         namespace {NS}\n\n{PREAMBLE}\nmodule {MODULE} =\n"
    );
    for (n, f) in functions().iter().enumerate() {
        out.push_str(&format!("    // {}\n    let f{n}{}\n", f.label, f.decl));
    }
    out
}

/// The fixture source, the probe, and the row index into the probe.
pub fn corpus() -> Corpus {
    let mut probe = format!(
        "// GENERATED by crates/sema/tests/all/common/measure_fn_corpus.rs — do not edit.\n\
         module MeasureFnProbe\n\
         \n\
         open {NS}\n\
         open FSharpMeasureFnBase\n\
         \n"
    );
    let binder = |probe: &mut String, name: &str, rhs: &str| -> (usize, usize) {
        let start = probe.len() + "let ".len();
        probe.push_str(&format!("let {name} = {rhs}\n"));
        (start, start + name.len())
    };
    let mut rows = Vec::new();
    for (n, f) in functions().into_iter().enumerate() {
        let name = format!("f{n}");
        let value_binder = binder(&mut probe, &format!("v{n}"), &format!("{MODULE}.{name}"));
        let (applied_binder, applied_arity) = match &f.applied {
            Some((args, arity)) => (
                Some(binder(
                    &mut probe,
                    &format!("a{n}"),
                    &format!("{MODULE}.{name} {args}"),
                )),
                *arity,
            ),
            None => (None, 0),
        };
        rows.push(Row {
            label: f.label,
            name,
            measure_bearing: f.measure_bearing,
            is_control: f.is_control,
            value_binder,
            applied_binder,
            applied_arity,
        });
    }
    Corpus {
        fixture: fixture_source(),
        probe,
        rows,
    }
}
