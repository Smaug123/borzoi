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
    // Values of a record and of a class, whose members a use reaches.
    admitted("solo0", "solo0 : Solo = { Q = 1 }", &[], None, false),
    admitted("soloUnion0", "soloUnion0 = SoloCase 1", &[], None, false),
    admitted("soloClass0", "soloClass0 = SoloClass(1)", &[], None, false),
    admitted(
        "soloStruct0",
        "soloStruct0 : SoloStruct = { Q = 1 }",
        &[],
        None,
        false,
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
    // Several groups, some tupled: the pickle's group sizes split the IL
    // parameters.
    admitted(
        "mixed",
        "mixed (a: int, b: int) (c: int) = a + b + c",
        &["(1, 2)", "3"],
        Some("1"),
        false,
    ),
    admitted(
        "mixedG",
        "mixedG (a: 'a, b: string) (c: 'b) (d: int, e: bool) = (a, c)",
        &["(1, \"s\")", "true", "(2, false)"],
        Some("1"),
        true,
    ),
    admitted(
        "mixedTupleParam",
        "mixedTupleParam (p: int * string) (c: int) = c",
        &["(1, \"s\")", "3"],
        Some("1"),
        false,
    ),
    // `equality` and `comparison`, the constraints that unify nothing: read,
    // each instantiation checked against the closed list of types known to
    // satisfy it.
    admitted(
        "eqC",
        "eqC (x: 'a) (y: 'a) = (x = y)",
        &["1", "2"],
        None,
        true,
    ),
    admitted(
        "cmpC",
        "cmpC (x: 'a) (y: 'a) = compare x y",
        &["1", "2"],
        None,
        true,
    ),
    admitted(
        "bothC",
        "bothC (x: 'a when 'a : equality and 'a : comparison) (y: 'a) = (x = y, compare x y)",
        &["\"s\"", "\"t\""],
        None,
        true,
    ),
    // Declined: what the IL cannot carry, and constraints nothing checks.
    declined(
        "nullC",
        "",
        "nullC (x: 'a when 'a : null) = x",
        &["\"s\""],
        true,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "eqNullC",
        "",
        "eqNullC (x: 'a when 'a : equality and 'a : null) (y: 'a) = (x = y)",
        &["\"s\"", "\"t\""],
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
    // Generic named types, read by their compiled path. F# lists, options,
    // results, value options, refs, choices and asyncs (FSharp.Core renames
    // each), nested and in arrays.
    admitted("intList", "intList = [1]", &[], None, false),
    admitted(
        "lst",
        "lst (x: int list) = x.Length",
        &["intList"],
        Some("ints"),
        false,
    ),
    admitted(
        "lstG",
        "lstG (xs: 'a list) = xs",
        &["intList"],
        Some("ints"),
        true,
    ),
    admitted(
        "lstMap",
        "lstMap (f: 'a -> 'b) (xs: 'a list) = List.map f xs",
        &["showInt", "intList"],
        Some("1"),
        true,
    ),
    admitted("lstRet", "lstRet (x: 'a) = [x]", &["1"], None, true),
    admitted("intLists", "intLists = [[1]]", &[], None, false),
    admitted(
        "lstNest",
        "lstNest (xs: int list list) = 1",
        &["intLists"],
        Some("intList"),
        false,
    ),
    admitted("lstArr", "lstArr = [| [1] |]", &[], None, false),
    admitted(
        "arrLst",
        "arrLst (xs: int list[]) = xs.Length",
        &["lstArr"],
        Some("ints"),
        false,
    ),
    admitted("optVal", "optVal = Some 1", &[], None, false),
    admitted(
        "optInt",
        "optInt (o: int option) = 1",
        &["optVal"],
        Some("1"),
        false,
    ),
    admitted("optG", "optG (x: 'a) = Some x", &["\"s\""], None, true),
    admitted("optLstVal", "optLstVal = Some [1]", &[], None, false),
    admitted(
        "optLst",
        "optLst (o: int list option) = 1",
        &["optLstVal"],
        Some("optVal"),
        false,
    ),
    admitted("optTupVal", "optTupVal = Some (1, \"s\")", &[], None, false),
    admitted(
        "optTup",
        "optTup (o: (int * string) option) = 1",
        &["optTupVal"],
        Some("optVal"),
        false,
    ),
    admitted("optFnVal", "optFnVal = Some showInt", &[], None, false),
    admitted(
        "optFn",
        "optFn (o: (int -> string) option) = 1",
        &["optFnVal"],
        Some("optVal"),
        false,
    ),
    admitted(
        "resVal",
        "resVal : Result<int, string> = Ok 1",
        &[],
        None,
        false,
    ),
    admitted(
        "res",
        "res (r: Result<int, string>) = 1",
        &["resVal"],
        Some("optVal"),
        false,
    ),
    admitted(
        "resG",
        "resG (x: 'a) : Result<'a, string> = Ok x",
        &["1"],
        None,
        true,
    ),
    admitted("voptVal", "voptVal = ValueSome 1", &[], None, false),
    admitted(
        "vopt",
        "vopt (x: int voption) = 1",
        &["voptVal"],
        Some("optVal"),
        false,
    ),
    admitted("refVal", "refVal = ref 1", &[], None, false),
    admitted(
        "deref",
        "deref (r: int ref) = r.Value",
        &["refVal"],
        Some("optVal"),
        false,
    ),
    admitted(
        "choiceVal",
        "choiceVal : Choice<int, string> = Choice1Of2 1",
        &[],
        None,
        false,
    ),
    admitted(
        "choice",
        "choice (c: Choice<int, string>) = 1",
        &["choiceVal"],
        Some("resVal"),
        false,
    ),
    admitted(
        "asyncVal",
        "asyncVal = async { return 1 }",
        &[],
        None,
        false,
    ),
    admitted(
        "runA",
        "runA (a: Async<int>) = 1",
        &["asyncVal"],
        Some("optVal"),
        false,
    ),
    // `seq<int>` is an interface and `ResizeArray<int>` an unsealed class,
    // which FCS loosens at a use; returned, neither is loosened.
    admitted(
        "seqP",
        "seqP (xs: seq<int>) = 1",
        &["intList"],
        Some("1"),
        true,
    ),
    admitted(
        "seqRet",
        "seqRet (n: int) : seq<int> = Seq.singleton n",
        &["1"],
        Some("\"s\""),
        false,
    ),
    admitted("raVal", "raVal = ResizeArray<int>()", &[], None, false),
    admitted(
        "raP",
        "raP (xs: ResizeArray<int>) = xs.Count",
        &["raVal"],
        Some("intList"),
        true,
    ),
    // A BCL struct and a delegate, both sealed.
    admitted(
        "kvpVal",
        "kvpVal = System.Collections.Generic.KeyValuePair<int, string>(1, \"s\")",
        &[],
        None,
        false,
    ),
    admitted(
        "kvp",
        "kvp (p: System.Collections.Generic.KeyValuePair<int, string>) = p.Key",
        &["kvpVal"],
        Some("optVal"),
        false,
    ),
    admitted(
        "funcVal",
        "funcVal = System.Func<int, int>(fun x -> x)",
        &[],
        None,
        false,
    ),
    // User generic types, and abbreviations of generic types.
    admitted("gboxVal", "gboxVal = { G = 1 }", &[], None, false),
    admitted(
        "gbox",
        "gbox (b: GBox<int>) = b.G",
        &["gboxVal"],
        Some("optVal"),
        false,
    ),
    admitted("gboxG", "gboxG (x: 'a) = { G = x }", &["1"], None, true),
    admitted("guVal", "guVal : GU<string> = GA \"s\"", &[], None, false),
    admitted(
        "gu",
        "gu (u: GU<string>) = 1",
        &["guVal"],
        Some("gboxVal"),
        false,
    ),
    // A generic F# class FCS loosens at a use, and a sealed one it does not.
    admitted("gclassVal", "gclassVal = GClass<int>(1)", &[], None, false),
    admitted(
        "gclassP",
        "gclassP (c: GClass<int>) = c.X",
        &["gclassVal"],
        Some("optVal"),
        true,
    ),
    admitted(
        "gsealedVal",
        "gsealedVal = GSealed<int>(1)",
        &[],
        None,
        false,
    ),
    admitted(
        "gsealedP",
        "gsealedP (c: GSealed<int>) = c.X",
        &["gsealedVal"],
        Some("gclassVal"),
        false,
    ),
    admitted(
        "intBox",
        "intBox (b: IntBox) = b.G",
        &["gboxVal"],
        Some("optVal"),
        false,
    ),
    admitted(
        "lstAbbrev",
        "lstAbbrev (xs: Lst<int>) = 1",
        &["intList"],
        Some("ints"),
        false,
    ),
    // Declined generic named types: a constrained type parameter, a measure
    // parameter, a renamed type outside FSharp.Core's, and the heads FCS
    // renders structurally.
    declined(
        "cboxVal",
        "",
        "cboxVal = { C = 1 }",
        &[],
        false,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "cbox",
        "",
        "cbox (b: CBox<int>) = 1",
        &["cboxVal"],
        false,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "mapVal",
        "",
        "mapVal : Map<int, string> = Map.empty",
        &[],
        false,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "mapP",
        "",
        "mapP (m: Map<int, string>) = m.Count",
        &["mapVal"],
        false,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "nullableVal",
        "",
        "nullableVal = System.Nullable<int>(1)",
        &[],
        false,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "nullableP",
        "",
        "nullableP (x: System.Nullable<int>) = 1",
        &["nullableVal"],
        false,
        FunctionTyGap::Constrained,
        Rejection::No,
    ),
    declined(
        "mboxVal",
        "",
        "mboxVal : MBox<m> = { V = 1.0<m> }",
        &[],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "mbox",
        "",
        "mbox (b: MBox<m>) = 1",
        &["mboxVal"],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "mboxAbbrev",
        "",
        "mboxAbbrev (b: MBoxM) = 1",
        &["mboxVal"],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "mpairVal",
        "",
        "mpairVal : MPair<int, m> = { A = 1; W = 1.0<m> }",
        &[],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "mpair",
        "",
        "mpair (p: MPair<int, m>) = p.A",
        &["mpairVal"],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "mpairG",
        "",
        "mpairG (p: MPair<'a, 'u>) = p.A",
        &["mpairVal"],
        true,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "rgboxVal",
        "",
        "rgboxVal = { R = 1 }",
        &[],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "rgbox",
        "",
        "rgbox (b: RGBox<int>) = b.R",
        &["rgboxVal"],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    // Renamed with the arity suffix spelled out, so the IL names it as a
    // generic type of its arity: only the rename tells it apart.
    declined(
        "rgbox2Val",
        "",
        "rgbox2Val = { R2 = 1 }",
        &[],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "rgbox2",
        "",
        "rgbox2 (b: RGBox2<int>) = b.R2",
        &["rgbox2Val"],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "structTupVal",
        "",
        "structTupVal = struct (1, 2)",
        &[],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "structTup",
        "",
        "structTup (p: struct (int * int)) = 1",
        &["structTupVal"],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "tup1Val",
        "",
        "tup1Val = System.Tuple<int>(1)",
        &[],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    declined(
        "tup8Val",
        "",
        "tup8Val = (1, 2, 3, 4, 5, 6, 7, 8)",
        &[],
        false,
        FunctionTyGap::Type,
        Rejection::No,
    ),
    // A parameter of an eight-tuple type compiles to eight parameters, one
    // group, so it is read as the tuple it is.
    admitted(
        "tup8",
        "tup8 (t: int * int * int * int * int * int * int * int) = 1",
        &["(1, 2, 3, 4, 5, 6, 7, 8)"],
        Some("1"),
        false,
    ),
    // A generic type nested in a generic type: its pickled head is not one
    // the measure check can place, so it declines there.
    declined(
        "keysVal",
        "",
        "keysVal = System.Collections.Generic.Dictionary<int, string>().Keys",
        &[],
        false,
        FunctionTyGap::Measures,
        Rejection::No,
    ),
    declined(
        "keys",
        "",
        "keys (k: System.Collections.Generic.Dictionary<int, string>.KeyCollection) = k.Count",
        &["keysVal"],
        false,
        FunctionTyGap::Measures,
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

type Solo =
    { Q: int }
    member this.M = this.Q

type SoloUnion =
    | SoloCase of q: int
    member this.Q = match this with SoloCase q -> q
    member this.M = 1

type SoloClass(q: int) =
    member _.Q = q
    member _.M = q

type FnRec = { F: int -> int }

[<Struct>]
type SoloStruct =
    { Q: int }
    member this.M = this.Q

type GBox<'T> = { G: 'T }

type GU<'T> =
    | GA of 'T
    | GB

type CBox<'T when 'T : comparison> = { C: 'T }

type MBox<[<Measure>] 'u> = { V: float<'u> }

type MBoxM = MBox<m>

type MPair<'a, [<Measure>] 'u> = { A: 'a; W: float<'u> }

type GClass<'T>(x: 'T) =
    member _.X = x

[<Sealed>]
type GSealed<'T>(x: 'T) =
    member _.X = x

type IntBox = GBox<int>

type Lst<'a> = 'a list

[<CompiledName(\"RenamedGBox\")>]
type RGBox<'T> = { R: 'T }

[<CompiledName(\"RenamedGBox2`1\")>]
type RGBox2<'T> = { R2: 'T }
";

/// The receiver types of the member-access grid, and a module value of each
/// in [`MODULE`]: an F# record, union, class and struct record, each with a
/// member `Q` (a field where the kind has fields) and a property `M`.
pub const RECEIVERS: &[(&str, &str)] = &[
    ("Solo", "solo0"),
    ("SoloUnion", "soloUnion0"),
    ("SoloClass", "soloClass0"),
    ("SoloStruct", "soloStruct0"),
];

/// The module of operators, outside FSharp.Core: one sealed, one generic, one
/// `equality`-constrained, one constrained by `null`, which nothing checks,
/// and one over a type the bridge does not carry.
pub const OPS_MODULE: &str = "Ops";

const OPS: &str = "\
module Ops =
    let (+++) (a: int) (b: int) = a + b
    let (|>>) (x: 'a) (f: 'a -> 'b) = f x
    let (=~) (a: 'a) (b: 'a) = (a = b)
    let (=?) (a: 'a when 'a : null) (b: 'a) = obj.ReferenceEquals(a, b)
    let (+@) (a: struct (int * int)) (b: int) = b
    let (+?) (a: int option) (b: int) = b
    let (|Even|Odd|) (x: int) = if x % 2 = 0 then Even else Odd
    let (|Pos|_|) (x: int) = if x > 0 then Some x else None
";

/// Modules F# rejects a use through, by an error-level attribute on the
/// module itself or on one enclosing it, each holding a function and a value
/// whose own attributes are clean; and warning-level twins, which type
/// normally.
const REJECTED_MODULES: &str = "
[<System.Obsolete(\"no\", true)>]
module ObsMod =
    let f (x: int) = x
    let v = 1

[<CompilerMessage(\"no\", 9001, IsError = true)>]
module CmMod =
    let f (x: int) = x
    let v = 1

[<System.Obsolete(\"no\", true)>]
module ObsOuter =
    module Inner =
        let f (x: int) = x
        let v = 1

[<System.Obsolete(\"no\")>]
module ObsWarnMod =
    let f (x: int) = x
    let v = 1

[<CompilerMessage(\"no\", 9001)>]
module CmWarnMod =
    let f (x: int) = x
    let v = 1
";

/// The module of functions over a byref-like generic, which no use may read
/// (F# rejects such a function as a first-class value), beside a control.
pub const BYREF_LIKE_MODULE: &str = "ByRefLike";

const BYREF_LIKE: &str = "
module ByRefLike =
    let spanLen (s: System.ReadOnlySpan<int>) = s.Length
    let spanOf (xs: int[]) = System.ReadOnlySpan<int>(xs)
    let control (x: int list) = x
";

/// `Collision.Thing`, which the twin assembly (`module_value_twin_env`)
/// declares too, with a member of another type.
const COLLISION: &str = "
namespace Collision

type Thing = { P: string }

type GThing<'T> = { P: string; T: 'T }

module A =
    let item: Thing = { P = \"s\" }
    let take (t: Thing) = t.P
    let gitem: GThing<int> = { P = \"s\"; T = 1 }
    let gtake (t: GThing<int>) = t.P
";

/// The fixture source: [`SHAPES`] as the module [`MODULE`], each of
/// [`RELOCATED`] as its own module, its `module X` line dropped, and
/// [`COLLISION`].
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
    out.push_str(REJECTED_MODULES);
    out.push_str(BYREF_LIKE);
    out.push_str(COLLISION);
    out
}

/// A probe's header for a relocated grid: the in-file header's module line,
/// renamed so it is not the fixture module's, and an `open` of the fixture
/// module in place of the definitions.
pub fn relocated_probe_header(module: &str) -> String {
    format!("module {module}Use\nopen {NS}.{module}\n")
}
