//! The **module-value** universe: an F#-authored fixture assembly whose module
//! functions and values span the signature shapes a use of an assembly value
//! is typed through (`function_ty::module_function_scheme`), for the
//! differential that grades those uses (`infer_assembly_values_diff`).
//!
//! The fixture has two parts, both generated so the fixture and the probes
//! that index it cannot drift apart:
//!
//! - [`SHAPES`], the module `Fns`: one function or value per signature shape
//!   (generic and not, curried, a unit group, a tupled group, a tuple-typed
//!   parameter, a function returned, a function taken, `obj` taken, arrays,
//!   values, type functions) and per attribute or constraint that must make
//!   the reading decline, each with the census reason a use records.
//! - [`RELOCATED`], the headers of `infer_operators_diff`'s generated grids,
//!   each moved into a module of its own. A grid whose functions come from an
//!   assembly is the same program to FCS, so its rows, obligations included,
//!   grade the assembly route through every relation the in-file grid tests.

use borzoi_sema::FunctionTyGap;

/// The assembly name the fixture project emits.
pub const FIXTURE_ASM: &str = "SemaModuleValueFixture";

/// The namespace every fixture declaration sits in.
pub const NS: &str = "FSharpModuleValues";

/// The module holding [`SHAPES`].
pub const MODULE: &str = "Fns";

/// One function or value of [`MODULE`].
pub struct Shape {
    /// Its F# name.
    pub name: &'static str,
    /// Attributes written before the `let`, if any.
    pub attrs: &'static str,
    /// The declaration after `let `.
    pub decl: &'static str,
    /// Arguments of the types it takes, one per curried domain of its F#
    /// type, each an atomic expression.
    pub args: &'static [&'static str],
    /// An argument of the wrong type for the first domain, if it takes one.
    pub bad: Option<&'static str>,
    /// Whether its F# type mentions a type parameter, or a parameter FCS
    /// loosens at a use (`obj`): a partial application or a value use of it
    /// is generic, and a module-level value bound to one is value-restricted.
    pub generic: bool,
    /// The census reason every use of it records, or `None` when the reading
    /// admits it.
    pub gap: Option<FunctionTyGap>,
    /// Whether, and why, FCS rejects every use of it.
    pub rejected: Rejection,
}

/// Why FCS rejects every use of a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    No,
    /// An attribute that rejects the use: the reading declines it.
    ByAttribute,
    /// Not accessible from another assembly: the resolver resolves nothing,
    /// and the reading declines it too.
    Inaccessible,
}

const fn admitted(
    name: &'static str,
    decl: &'static str,
    args: &'static [&'static str],
    bad: Option<&'static str>,
    generic: bool,
) -> Shape {
    Shape {
        name,
        attrs: "",
        decl,
        args,
        bad,
        generic,
        gap: None,
        rejected: Rejection::No,
    }
}

const fn declined(
    name: &'static str,
    attrs: &'static str,
    decl: &'static str,
    args: &'static [&'static str],
    generic: bool,
    gap: FunctionTyGap,
    rejected: Rejection,
) -> Shape {
    Shape {
        name,
        attrs,
        decl,
        args,
        bad: None,
        generic,
        gap: Some(gap),
        rejected,
    }
}

/// The shapes. See the module docs.
pub const SHAPES: &[Shape] = &[
    // Curried, sealed.
    admitted(
        "nInt",
        "nInt (x: int) (y: int) = x + y",
        &["1", "2"],
        Some("\"s\""),
        false,
    ),
    admitted(
        "c3",
        "c3 (a: int) (b: string) (c: bool) = a",
        &["1", "\"s\"", "true"],
        Some("true"),
        false,
    ),
    admitted(
        "showInt",
        "showInt (i: int) = \"s\"",
        &["1"],
        Some("\"s\""),
        false,
    ),
    admitted(
        "strLen",
        "strLen (s: string) = s.Length",
        &["\"s\""],
        Some("1"),
        false,
    ),
    // Generic.
    admitted("gId", "gId (x: 'a) = x", &["1"], None, true),
    admitted(
        "gPair",
        "gPair (x: 'a) (y: 'b) = (x, y)",
        &["1", "\"s\""],
        None,
        true,
    ),
    admitted(
        "gFst",
        "gFst (x: 'a) (y: int) = x",
        &["\"s\"", "2"],
        None,
        true,
    ),
    // The sole empty group.
    admitted("unitFn", "unitFn () = 42", &["()"], Some("1"), false),
    admitted(
        "unitGen",
        "unitGen<'a> () : 'a[] = [||]",
        &["()"],
        Some("1"),
        true,
    ),
    // A tupled group, and a parameter of a tuple type.
    admitted(
        "tup",
        "tup (a: int, b: string) = b.Length + a",
        &["(1, \"s\")"],
        Some("(\"s\", 1)"),
        false,
    ),
    admitted(
        "tupG",
        "tupG (a: 'a, b: 'b) = (b, a)",
        &["(1, \"s\")"],
        None,
        true,
    ),
    admitted(
        "tupParam",
        "tupParam (p: int * string) = fst p",
        &["(1, \"s\")"],
        Some("1"),
        false,
    ),
    // A function returned.
    admitted(
        "retFn",
        "retFn (x: int) = fun (y: string) -> y.Length + x",
        &["1", "\"s\""],
        Some("\"s\""),
        false,
    ),
    admitted(
        "retFnG",
        "retFnG (x: 'a) = fun (y: 'b) -> (y, x)",
        &["1", "\"s\""],
        None,
        true,
    ),
    admitted(
        "retFnObj",
        "retFnObj (x: int) = fun (o: obj) -> x",
        &["1", "\"s\""],
        Some("\"s\""),
        true,
    ),
    // A function taken.
    admitted(
        "hof",
        "hof (f: int -> string) (x: int) = f x",
        &["showInt", "1"],
        Some("strLen"),
        false,
    ),
    admitted(
        "applyG",
        "applyG (f: 'a -> 'b) (x: 'a) = f x",
        &["showInt", "1"],
        Some("1"),
        true,
    ),
    admitted(
        "composeG",
        "composeG (f: 'a -> 'b) (g: 'b -> 'c) = fun x -> g (f x)",
        &["showInt", "strLen", "1"],
        Some("1"),
        true,
    ),
    admitted(
        "hofObj",
        "hofObj (f: obj -> int) = f (box 1)",
        &["takesObj"],
        Some("strLen"),
        false,
    ),
    // `obj` taken, which FCS loosens at a use.
    admitted("takesObj", "takesObj (o: obj) = 1", &["1"], None, true),
    admitted(
        "takesObjTup",
        "takesObjTup (o: obj, i: int) = i",
        &["(\"s\", 1)"],
        Some("(1, \"s\")"),
        true,
    ),
    admitted(
        "objSecond",
        "objSecond (x: int) (o: obj) = x",
        &["1", "\"s\""],
        Some("\"s\""),
        true,
    ),
    // Named types: an interface FCS loosens, a struct and a class it does
    // not, and a class returned.
    admitted(
        "takesCmp",
        "takesCmp (x: System.IComparable) = 1",
        &["1"],
        None,
        true,
    ),
    admitted(
        "takesGuid",
        "takesGuid (g: System.Guid) = 1",
        &["guid0"],
        Some("1"),
        false,
    ),
    admitted("guid0", "guid0 = System.Guid.Empty", &[], None, false),
    admitted(
        "takesUri",
        "takesUri (u: System.Uri) = 1",
        &["uri0"],
        Some("1"),
        true,
    ),
    admitted("uri0", "uri0 = System.Uri(\"http://x\")", &[], None, false),
    admitted(
        "retExn",
        "retExn (s: string) = System.Exception(s)",
        &["\"s\""],
        Some("1"),
        false,
    ),
    // A type F# renames: FCS's canonical rendering is the compiled name, hover
    // must show the source one, and `Ty` carries one path.
    declined(
        "takesRenamed",
        "",
        "takesRenamed (r: Renamed) = r.X",
        &["renamed0"],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "renamed0",
        "",
        "renamed0 = { X = 1 }",
        &[],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    // Returns that need a rule of their own.
    admitted(
        "retObj",
        "retObj (x: int) : obj = box x",
        &["1"],
        Some("\"s\""),
        false,
    ),
    admitted(
        "retUnit",
        "retUnit (x: int) = ignore x",
        &["1"],
        Some("\"s\""),
        false,
    ),
    // Arrays.
    admitted(
        "arr",
        "arr (xs: int[]) = xs.Length",
        &["ints"],
        Some("1"),
        false,
    ),
    admitted(
        "arrG",
        "arrG (xs: 'a[]) = xs.Length",
        &["ints"],
        Some("1"),
        true,
    ),
    // Values.
    admitted("valInt", "valInt = 42", &[], None, false),
    admitted("valStr", "valStr = \"s\"", &[], None, false),
    admitted("valTup", "valTup = (1, \"s\")", &[], None, false),
    admitted("ints", "ints = [| 1 |]", &[], None, false),
    admitted("valMut", "mutable valMut = 0", &[], None, false),
    admitted(
        "valFn",
        "valFn = (fun (x: int) -> x) >> (fun x -> x + 1)",
        &["1"],
        Some("\"s\""),
        false,
    ),
    admitted(
        "valObjFn",
        "valObjFn : obj -> int = let k = 1 in fun _ -> k",
        &["1"],
        None,
        true,
    ),
    // Type functions.
    Shape {
        name: "gvEmpty",
        attrs: "[<GeneralizableValue>]",
        decl: "gvEmpty<'a> : 'a[] = [||]",
        args: &[],
        bad: None,
        generic: true,
        gap: None,
        rejected: Rejection::No,
    },
    admitted("typeFn", "typeFn<'a> : 'a[] = [||]", &[], None, true),
    // Inline, without a constraint.
    admitted(
        "inl",
        "inline inl (x: int) = x + 1",
        &["1"],
        Some("\"s\""),
        false,
    ),
    admitted("inlG", "inline inlG (x: 'a) = x", &["1"], None, true),
    // Renamed, and warned about: typed as written.
    Shape {
        name: "compiledNamed",
        attrs: "[<CompiledName(\"Renamed\")>]",
        decl: "compiledNamed (x: int) = x + 1",
        args: &["1"],
        bad: Some("\"s\""),
        generic: false,
        gap: None,
        rejected: Rejection::No,
    },
    Shape {
        name: "obsWarn",
        attrs: "[<System.Obsolete(\"no\")>]",
        decl: "obsWarn (x: int) = x",
        args: &["1"],
        bad: Some("\"s\""),
        generic: false,
        gap: None,
        rejected: Rejection::No,
    },
    Shape {
        name: "cmWarn",
        attrs: "[<CompilerMessage(\"no\", 9001)>]",
        decl: "cmWarn (x: int) = x",
        args: &["1"],
        bad: Some("\"s\""),
        generic: false,
        gap: None,
        rejected: Rejection::No,
    },
    // Declined: what the IL cannot carry.
    declined(
        "mixed",
        "",
        "mixed (a: int, b: int) (c: int) = a + b + c",
        &["(1, 2)", "3"],
        false,
        FunctionTyGap::Groups,
        Rejection::No,
    ),
    declined(
        "eqC",
        "",
        "eqC (x: 'a) (y: 'a) = (x = y)",
        &["1", "2"],
        true,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "cmpC",
        "",
        "cmpC (x: 'a) (y: 'a) = compare x y",
        &["1", "2"],
        true,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "srtp",
        "",
        "inline srtp (x: ^a) = x + x",
        &["1"],
        true,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "structC",
        "",
        "structC (x: 'a when 'a : struct) = x",
        &["1"],
        true,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "meas",
        "",
        "meas (x: float<m>) = 1",
        &["valMeas"],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "measAbbrev",
        "",
        "measAbbrev (x: Dist) = 1",
        &["valMeas"],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "valMeas",
        "",
        "valMeas = 1.0<m>",
        &[],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "lst",
        "",
        "lst (x: int list) = x.Length",
        &["[1]"],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    // `seq<int>` is an interface, which FCS loosens at a use.
    declined(
        "seqP",
        "",
        "seqP (xs: seq<int>) = 1",
        &["ints"],
        true,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "reqTa",
        "[<RequiresExplicitTypeArguments>]",
        "reqTa<'a> (x: int) = x",
        &["1"],
        true,
        FunctionTyGap::Rejected,
        Rejection::ByAttribute,
    ),
    declined(
        "obsErr",
        "[<System.Obsolete(\"no\", true)>]",
        "obsErr (x: int) = x",
        &["1"],
        false,
        FunctionTyGap::Rejected,
        Rejection::ByAttribute,
    ),
    declined(
        "cmErr",
        "[<CompilerMessage(\"no\", 9001, IsError = true)>]",
        "cmErr (x: int) = x",
        &["1"],
        false,
        FunctionTyGap::Rejected,
        Rejection::ByAttribute,
    ),
    // Not accessible from another assembly: FCS rejects the use (FS1094).
    declined(
        "internalFn",
        "",
        "internal internalFn (x: int) = x",
        &["1"],
        false,
        FunctionTyGap::Rejected,
        Rejection::Inaccessible,
    ),
    declined(
        "litVal",
        "[<Literal>]",
        "litVal = 3",
        &[],
        false,
        FunctionTyGap::NotAFunction,
        Rejection::No,
    ),
];

/// A header of `infer_operators_diff`'s grids, moved into an assembly module:
/// the module's name and the header as the grid writes it in-file.
pub struct Relocated {
    pub module: &'static str,
    pub header: &'static str,
}

/// The relocated headers. See the module docs.
pub const RELOCATED: &[Relocated] = &[
    Relocated {
        module: "Pipes",
        header: crate::infer_operators_diff::PIPE_HEADER,
    },
    Relocated {
        module: "Order",
        header: crate::infer_operators_diff::ORDER_HEADER,
    },
    Relocated {
        module: "Checks",
        header: crate::infer_operators_diff::ARG_CHECK_HEADER,
    },
];

/// The fixture's preamble: what the measure shapes name.
const PREAMBLE: &str = "\
[<Measure>]
type m

type Dist = float<m>

[<CompiledName(\"RenamedRecord\")>]
type Renamed = { X: int }
";

/// The module of operators, outside FSharp.Core: one sealed, one generic, one
/// constrained, and one over a type the bridge does not carry.
pub const OPS_MODULE: &str = "Ops";

const OPS: &str = "\
module Ops =
    let (+++) (a: int) (b: int) = a + b
    let (|>>) (x: 'a) (f: 'a -> 'b) = f x
    let (=~) (a: 'a) (b: 'a) = (a = b)
    let (+@) (a: int list) (b: int) = b
    let (|Even|Odd|) (x: int) = if x % 2 = 0 then Even else Odd
    let (|Pos|_|) (x: int) = if x > 0 then Some x else None
";

/// The fixture source: [`SHAPES`] as the module [`MODULE`], and each of
/// [`RELOCATED`] as its own module, its `module X` line dropped.
pub fn fixture_source() -> String {
    let mut out = format!("namespace {NS}\n\n#nowarn \"44\"\n#nowarn \"9001\"\n\n{PREAMBLE}\n");
    out.push_str(&format!("module {MODULE} =\n"));
    for shape in SHAPES {
        if !shape.attrs.is_empty() {
            out.push_str(&format!("    {}\n", shape.attrs));
        }
        out.push_str(&format!("    let {}\n", shape.decl));
    }
    out.push('\n');
    out.push_str(OPS);
    for relocated in RELOCATED {
        out.push_str(&format!("\nmodule {} =\n", relocated.module));
        let mut lines = relocated.header.lines();
        let first = lines.next().expect("a header starts with its module line");
        assert!(
            first.starts_with("module "),
            "a relocated header starts with its module line: {first}"
        );
        for line in lines {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out
}

/// A probe's header for a relocated grid: the in-file header's module line,
/// renamed so it is not the fixture module's, and an `open` of the fixture
/// module in place of the definitions.
pub fn relocated_probe_header(module: &str) -> String {
    format!("module {module}Use\nopen {NS}.{module}\n")
}
