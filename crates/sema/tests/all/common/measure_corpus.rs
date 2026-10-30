//! The **unit-of-measure** universe: an F#-authored fixture assembly whose
//! members carry measure-annotated types, and a probe reading every one of them
//! through every member-typing path inference commits on.
//!
//! IL erases units of measure. A member declared `float<m>` compiles to a
//! `float64` signature, and a measure-parameterised `Vec<[<Measure>] 'u>`
//! compiles to a type with no generic parameters at all. A reader that types a
//! member from its IL signature therefore sees `System.Double` where FCS sees
//! `Microsoft.FSharp.Core.float<FSharpMeasure.m>` — a well-formed answer for a
//! different type, which no guard on the *shape* of the signature can tell
//! apart from a plain `float`.
//!
//! The universe is the product of two tables, so a committing path that the
//! guard misses fails on every shape at once rather than on whichever one a
//! hand-written case happened to pick:
//!
//! - [`SHAPES`]: the type a member is declared with. The FSharp.Core
//!   measure-annotated primitives, compound measures, an abbreviation that
//!   *hides* a measure behind a measure-free name, a user-declared
//!   `[<MeasureAnnotatedAbbreviation>]` over `string` (the `FSharp.UMX`
//!   pattern, which makes *any* IL type a possible measure carrier), a
//!   measure-parameterised record, and an array of a measure type — plus
//!   measure-free controls of the same IL types, so a fix that declines by IL
//!   type rather than by the F# type is visible as a lost control.
//! - [`CARRIERS`]: where the shape sits. A record field, a struct record
//!   field, a class `val` field, a property, an instance and a static method
//!   with and without an argument, an interface's abstract property, and a
//!   property inherited from a base class — every member kind the wake reads
//!   a type off — and the module bindings a hover renders from IL too: a
//!   module value, a mutable one, a function returning the shape, and one
//!   taking it as its parameter.
//!
//! [`EXTRA_SITES`] adds the cells that are not a shape × carrier product: a
//! measure-*generic* method (whose IL has no generic parameter, so a
//! generic-method guard does not fire), a member reached *through* a
//! measure-parameterised receiver, an overload pair that differs only in a
//! measure, which FCS rejects for an unannotated argument, and a method whose
//! only measure is in its parameter.
//!
//! Two consumers read the universe: the sema crate's inference differential
//! (`infer_measure_erasure_diff`), and the LSP crate's hover differential
//! (`crates/lsp/tests/all/hover_measure_labels.rs`), which includes this file
//! by path.

/// The assembly name the fixture project emits.
pub const FIXTURE_ASM: &str = "SemaMeasureFixture";

/// The namespace every fixture declaration sits in.
pub const NS: &str = "FSharpMeasure";

/// One type a member can be declared with.
pub struct Shape {
    /// The F# type, verbatim as the fixture writes it.
    pub written: &'static str,
    /// Whether the F# type carries a unit of measure anywhere — directly, under
    /// an abbreviation, or as a type argument. Checked against the oracle, not
    /// trusted: a measure-bearing shape must be one FCS renders differently from
    /// its IL erasure, and a measure-free one must be one it renders identically.
    pub measure_bearing: bool,
    /// What the IL signature erases the type to, in `Ty::render`'s currency.
    pub erased: &'static str,
}

/// The member shapes. See the module docs.
pub const SHAPES: &[Shape] = &[
    Shape {
        written: "float",
        measure_bearing: false,
        erased: "System.Double",
    },
    Shape {
        written: "int",
        measure_bearing: false,
        erased: "System.Int32",
    },
    Shape {
        written: "string",
        measure_bearing: false,
        erased: "System.String",
    },
    Shape {
        written: "decimal",
        measure_bearing: false,
        erased: "System.Decimal",
    },
    Shape {
        written: "float<m>",
        measure_bearing: true,
        erased: "System.Double",
    },
    Shape {
        written: "float32<m>",
        measure_bearing: true,
        erased: "System.Single",
    },
    Shape {
        written: "decimal<m>",
        measure_bearing: true,
        erased: "System.Decimal",
    },
    Shape {
        written: "int<m>",
        measure_bearing: true,
        erased: "System.Int32",
    },
    Shape {
        written: "int64<m>",
        measure_bearing: true,
        erased: "System.Int64",
    },
    Shape {
        written: "int16<m>",
        measure_bearing: true,
        erased: "System.Int16",
    },
    Shape {
        written: "sbyte<m>",
        measure_bearing: true,
        erased: "System.SByte",
    },
    Shape {
        written: "byte<m>",
        measure_bearing: true,
        erased: "System.Byte",
    },
    Shape {
        written: "uint64<m>",
        measure_bearing: true,
        erased: "System.UInt64",
    },
    Shape {
        written: "float<m/s>",
        measure_bearing: true,
        erased: "System.Double",
    },
    Shape {
        written: "float<m^2>",
        measure_bearing: true,
        erased: "System.Double",
    },
    Shape {
        written: "Dist",
        measure_bearing: true,
        erased: "System.Double",
    },
    Shape {
        written: "Plain",
        measure_bearing: false,
        erased: "System.Double",
    },
    Shape {
        written: "Tagged<m>",
        measure_bearing: true,
        erased: "System.String",
    },
    Shape {
        written: "Vec<m>",
        measure_bearing: true,
        erased: "FSharpMeasure.Vec",
    },
    Shape {
        written: "float<m>[]",
        measure_bearing: true,
        erased: "System.Double[]",
    },
    Shape {
        written: "float[]",
        measure_bearing: false,
        erased: "System.Double[]",
    },
];

/// Where a shape sits, and how the probe reads it.
///
/// Each carrier declares **its own type** per shape, holding that one member and
/// nothing else. A verdict about measures is drawn per declaring type, so two
/// carriers sharing a type would let one carrier's member decide for the other:
/// a reading that never looked at properties would still decline a property
/// beside a measure-typed field.
pub struct Carrier {
    pub label: &'static str,
    /// The fixture declaration, with `{t}` for the carrier's type name and `{s}`
    /// for the shape.
    pub decl: &'static str,
    /// Whether the probe needs a receiver of type `{t}`. A static access needs
    /// none.
    pub needs_receiver: bool,
    /// The access, with `{v}` for the receiver binding and `{t}` for the type
    /// name.
    pub access: &'static str,
    /// The IL erasure of the access's type, in `Ty::render`'s currency, with
    /// `{e}` for the shape's own erasure: `{e}` where the access is typed as the
    /// shape, `{e} -> System.Int32` for an unapplied function taking it.
    pub erased: &'static str,
    /// Whether inference types this carrier's accesses today, so that its
    /// measure-free controls must commit. A carrier it does not reach still
    /// grades soundness (nothing it publishes there may be wrong) and the cells'
    /// own claims.
    pub inferred: bool,
}

/// The member kinds the wake types. See the module docs.
pub const CARRIERS: &[Carrier] = &[
    Carrier {
        label: "record field",
        decl: "type {t} = { F : {s} }",
        needs_receiver: true,
        access: "{v}.F",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "struct record field",
        decl: "[<Struct>]\ntype {t} = { F : {s} }",
        needs_receiver: true,
        access: "{v}.F",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "record member property",
        decl: "type {t} =\n    { N : int }\n    member _.P : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: true,
        access: "{v}.P",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "union member property",
        decl: "type {t} =\n    | A\n    | B\n    member _.P : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: true,
        access: "{v}.P",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "class val field",
        decl: "type {t}() =\n    [<DefaultValue(false)>] val mutable Fld : {s}",
        needs_receiver: true,
        access: "{v}.Fld",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "class property",
        decl: "type {t}() =\n    member _.P : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: true,
        access: "{v}.P",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "class auto-property",
        decl: "type {t}() =\n    member val AP : {s} = Unchecked.defaultof<{s}> with get, set",
        needs_receiver: true,
        access: "{v}.AP",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "instance method, unit",
        decl: "type {t}() =\n    member _.M() : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: true,
        access: "{v}.M()",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "instance method, one argument",
        decl: "type {t}() =\n    member _.M1(x : int) : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: true,
        access: "{v}.M1(1)",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "static method, unit",
        decl: "type {t}() =\n    static member SM() : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: false,
        access: "{t}.SM()",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "static method, one argument",
        decl: "type {t}() =\n    static member SM1(x : int) : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: false,
        access: "{t}.SM1(1)",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "interface abstract property",
        decl: "type {t} =\n    abstract P : {s}",
        needs_receiver: true,
        access: "{v}.P",
        erased: "{e}",
        inferred: true,
    },
    Carrier {
        label: "inherited class property",
        decl: "type {t}Base() =\n    member _.P : {s} = Unchecked.defaultof<{s}>\n\n\
               type {t}() =\n    inherit {t}Base()",
        needs_receiver: true,
        access: "{v}.P",
        erased: "{e}",
        inferred: true,
    },
    // Module bindings: a module value compiles to a static property, a function
    // to a static method, both on the module's class.
    Carrier {
        label: "module value",
        decl: "module {t} =\n    let V : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: false,
        access: "{t}.V",
        erased: "{e}",
        inferred: false,
    },
    Carrier {
        label: "module mutable value",
        decl: "module {t} =\n    let mutable MV : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: false,
        access: "{t}.MV",
        erased: "{e}",
        inferred: false,
    },
    Carrier {
        label: "module function, unit",
        decl: "module {t} =\n    let F () : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: false,
        access: "{t}.F ()",
        erased: "{e}",
        inferred: false,
    },
    Carrier {
        label: "module function, one argument",
        decl: "module {t} =\n    let F1 (x : int) : {s} = Unchecked.defaultof<{s}>",
        needs_receiver: false,
        access: "{t}.F1 1",
        erased: "{e}",
        inferred: false,
    },
    // The shape in *parameter* position, read through an unapplied access whose
    // type is the whole function: an IL parameter type erases a measure as
    // surely as a return type does. (FCS types an unapplied *method* at its
    // range by the call it elaborates to, so only a module function's unapplied
    // access carries the parameter type.)
    Carrier {
        label: "module function, shape parameter",
        decl: "module {t} =\n    let G (x : {s}) : int = 0",
        needs_receiver: false,
        access: "{t}.G",
        erased: "{e} -> System.Int32",
        inferred: false,
    },
];

/// The type a carrier declares for one shape.
fn carrier_type(carrier: usize, shape: usize) -> String {
    format!("C{carrier}S{shape}")
}

/// What FCS is expected to make of a site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// FCS accepts the access; its type at the access range is measure-bearing
    /// iff the flag says so.
    Accepts { measure_bearing: bool },
    /// FCS rejects the line, so nothing may be published at the access and no
    /// member may be recorded there.
    Rejects,
}

/// A cell outside the shape × carrier product.
pub struct ExtraSite {
    pub label: &'static str,
    pub receiver: Option<&'static str>,
    pub access: &'static str,
    pub expect: Expect,
}

/// See the module docs.
pub const EXTRA_SITES: &[ExtraSite] = &[
    // A measure-generic method: its IL MethodDef has no generic parameter, so
    // the wake's IL-generic guard lets it through. FCS solves `'u := m` from the
    // argument.
    ExtraSite {
        label: "measure-generic method, measure-literal argument",
        receiver: Some("Gen"),
        access: "{v}.G1(1.0<m>)",
        expect: Expect::Accepts {
            measure_bearing: true,
        },
    },
    ExtraSite {
        label: "measure-generic static method, measure-literal argument",
        receiver: None,
        access: "Gen.SG1(1.0<m>)",
        expect: Expect::Accepts {
            measure_bearing: true,
        },
    },
    // Unit call of a measure-generic method: nothing fixes `'u`.
    ExtraSite {
        label: "measure-generic method, unit",
        receiver: Some("Gen"),
        access: "{v}.G()",
        expect: Expect::Accepts {
            measure_bearing: true,
        },
    },
    // Members reached *through* a measure-parameterised receiver. The receiver
    // `v.Vec` is typed off a property whose IL type is the measure-erased `Vec`,
    // so if that commits, the chained member wakes on `Vec` too.
    ExtraSite {
        label: "measure-typed member through a measure-parameterised receiver",
        receiver: Some("Gen"),
        access: "{v}.Vec.X",
        expect: Expect::Accepts {
            measure_bearing: true,
        },
    },
    ExtraSite {
        label: "measure-free member through a measure-parameterised receiver",
        receiver: Some("Gen"),
        access: "{v}.Vec.N",
        expect: Expect::Accepts {
            measure_bearing: false,
        },
    },
    // An overload pair whose parameter types differ only by a measure-free
    // spelling: IL sees `O(float64)` and `O(string)`, so a `float` argument
    // picks the first. FCS sees `O(float<m>)` and rejects `1.0`.
    ExtraSite {
        label: "overload whose parameter carries a measure, unannotated argument",
        receiver: Some("Gen"),
        access: "{v}.O(1.0)",
        expect: Expect::Rejects,
    },
    // The same pair, but the argument is a plain `float` FCS *does* accept for a
    // measure-free overload: the control that keeps the cell above
    // attributable to the measure rather than to the overload engine.
    ExtraSite {
        label: "overload control, string argument",
        receiver: Some("Gen"),
        access: "{v}.O(\"s\")",
        expect: Expect::Accepts {
            measure_bearing: false,
        },
    },
    // A method whose only measure is in its parameter, on a type with no other
    // member: the access is a measure-free `int`, so only the argument's type
    // says the parameter was erased.
    ExtraSite {
        label: "static method whose only measure is its parameter's",
        receiver: None,
        access: "ParamOnly.SP(1.0<m>)",
        expect: Expect::Accepts {
            measure_bearing: false,
        },
    },
];

/// One probe site.
#[derive(Debug, Clone)]
pub struct Site {
    /// 1-based line of the access; every site sits on its own line.
    pub line: usize,
    pub label: String,
    /// Half-open byte range of the access expression in the probe.
    pub access: (usize, usize),
    pub expect: Expect,
    /// The IL erasure of the access's type, when the site is a shape × carrier
    /// cell — the string a measure-blind reader would publish.
    pub erased: Option<String>,
    /// Whether the site is a measure-free shape × carrier cell: the control that
    /// proves its carrier is reached at all.
    pub is_control: bool,
    /// Whether inference reaches the site's carrier ([`Carrier::inferred`]);
    /// always `true` for an extra cell.
    pub inferred: bool,
    /// The line's source text.
    pub text: String,
}

/// The generated universe.
#[derive(Debug)]
pub struct Corpus {
    pub fixture: String,
    pub probe: String,
    pub sites: Vec<Site>,
}

/// The fixture assembly's source.
pub fn fixture_source() -> String {
    let mut out = format!(
        "// GENERATED by crates/sema/tests/all/common/measure_corpus.rs — do not edit.\n\
         namespace {NS}\n\
         \n\
         #nowarn \"42\"\n\
         \n\
         [<Measure>] type m\n\
         [<Measure>] type s\n\
         \n\
         /// An abbreviation whose name says nothing about the measure it hides.\n\
         type Dist = float<m>\n\
         \n\
         /// The measure-free twin of [`Dist`].\n\
         type Plain = float\n\
         \n\
         /// The `FSharp.UMX` pattern: a measure on a type FSharp.Core never annotates.\n\
         [<MeasureAnnotatedAbbreviation>]\n\
         type Tagged<[<Measure>] 'u> = string\n\
         \n\
         /// Measure-parameterised: compiles to a type with no generic parameter.\n\
         type Vec<[<Measure>] 'u> = {{ X : float<'u>; N : int }}\n\
         \n"
    );
    for (i, shape) in SHAPES.iter().enumerate() {
        for (c, carrier) in CARRIERS.iter().enumerate() {
            out.push_str(
                &carrier
                    .decl
                    .replace("{t}", &carrier_type(c, i))
                    .replace("{s}", shape.written),
            );
            out.push_str("\n\n");
        }
    }
    out.push_str(
        "type Gen() =\n\
         \x20   member _.G<[<Measure>] 'u>() : float<'u> = Unchecked.defaultof<float<'u>>\n\
         \x20   member _.G1<[<Measure>] 'u>(x : float<'u>) : float<'u> = x\n\
         \x20   static member SG1<[<Measure>] 'u>(x : float<'u>) : float<'u> = x\n\
         \x20   member _.Vec : Vec<m> = Unchecked.defaultof<Vec<m>>\n\
         \x20   member _.O(x : float<m>) : int = 0\n\
         \x20   member _.O(x : string) : string = x\n\
         \n\
         type ParamOnly() =\n\
         \x20   static member SP(x : float<m>) : int = 0\n",
    );
    out
}

/// The probe file, plus the site index into it.
pub fn corpus() -> Corpus {
    struct Row {
        label: String,
        receiver: Option<String>,
        access: String,
        expect: Expect,
        erased: Option<String>,
        is_control: bool,
        inferred: bool,
    }
    let mut rows = Vec::new();
    for (i, shape) in SHAPES.iter().enumerate() {
        for (c, carrier) in CARRIERS.iter().enumerate() {
            let ty = carrier_type(c, i);
            rows.push(Row {
                label: format!("{} : {}", carrier.label, shape.written),
                receiver: carrier.needs_receiver.then(|| ty.clone()),
                access: carrier.access.replace("{t}", &ty),
                expect: Expect::Accepts {
                    measure_bearing: shape.measure_bearing,
                },
                erased: Some(carrier.erased.replace("{e}", shape.erased)),
                is_control: !shape.measure_bearing,
                inferred: carrier.inferred,
            });
        }
    }
    for extra in EXTRA_SITES {
        rows.push(Row {
            label: extra.label.to_string(),
            receiver: extra.receiver.map(str::to_owned),
            access: extra.access.to_string(),
            expect: extra.expect,
            erased: None,
            is_control: false,
            inferred: true,
        });
    }

    let mut probe = format!(
        "// GENERATED by crates/sema/tests/all/common/measure_corpus.rs — do not edit.\n\
         module MeasureProbe\n\
         \n\
         open {NS}\n\
         \n"
    );
    for (n, row) in rows.iter().enumerate() {
        if let Some(receiver) = &row.receiver {
            probe.push_str(&format!("let v{n} : {receiver} = failwith \"fixture\"\n"));
        }
    }
    probe.push('\n');

    let mut sites = Vec::new();
    for (n, row) in rows.iter().enumerate() {
        let expr = row.access.replace("{v}", &format!("v{n}"));
        let text = format!("let r{n} = {expr}\n");
        let line = probe.lines().count() + 1;
        let start = probe.len() + text.find(&expr).expect("the access is on its own line");
        sites.push(Site {
            line,
            label: row.label.clone(),
            access: (start, start + expr.len()),
            expect: row.expect,
            erased: row.erased.clone(),
            is_control: row.is_control,
            inferred: row.inferred,
            text: text.clone(),
        });
        probe.push_str(&text);
    }

    Corpus {
        fixture: fixture_source(),
        probe,
        sites,
    }
}
