//! The **F#-authored referenced assembly** universe: one fixture assembly whose
//! metadata is what an F# compiler emits, and one probe file whose every line
//! reads a member off it.
//!
//! The member-hiding corpus next door is entirely C#, which covers the hiding
//! rule but not the *shapes* F# compiles to — and an F# `<ProjectReference>` is
//! the assembly every real solution actually points at. The data-member wake
//! reads one entity model for both, so a shape it mishandles here is a wrong
//! go-to-definition target on the most ordinary reference a project has.
//!
//! The dimensions are the places an F# member's **compiled** form differs from
//! its source form, since that gap is where a reader that assumed C# shapes
//! goes wrong:
//!
//! - a **record**, whose fields are properties over backing fields and which
//!   FCS reports as `field` rather than `member`;
//! - a **union**, through the case-test property `IsOne` — whose range carries
//!   *two* oracle records, the property and the case it tests;
//! - a **class**, the shape the C# corpus already covers, as the control;
//! - `[<CompiledName>]`, where the source name and the compiled name differ:
//!   FCS reports the access as `Renamed` while the metadata carries
//!   `CompiledRenamed`. The one dimension where reading the wrong name still
//!   *finds* something, so a mistake surfaces as a wrong target rather than a
//!   decline;
//! - an **abbreviation**, which names a type without declaring it, so every
//!   answer through it must land on what it abbreviates;
//! - **FSharp.Core's own `option` and `list`**, whose unions are hand-written
//!   in `prim-types.fsi` rather than generated whole from a `type U = A | B`,
//!   and which are the unions real F# actually touches. Their receivers are
//!   *generic*, which is the second half of what those cells cover.
//!
//! Two shapes the fixture declares but does not probe, both because measurement
//! said so rather than by choice. A union's `Tag` draws no use from FCS at all,
//! so a cell reading it would assert nothing while looking like coverage; and a
//! case's `Item`/`Item1` are reached by pattern matching rather than by member
//! access, which is a different surface from the one the wake serves. The
//! module's values are declared for the same reason — they compile to static
//! properties, but a `Values.Answer` is a qualified path the *resolver* answers,
//! not a receiver access.
//!
//! Both sources come from this one table, so the probe cannot reference a
//! member the fixture stopped declaring.

/// The assembly name the fixture project emits — the identity the differential
/// hands the oracle and expects our env to be built from.
pub const FIXTURE_ASM: &str = "SemaFSharpMemberFixture";

/// The namespace every fixture declaration sits in.
pub const NS: &str = "FSharpMember";

/// One probe site: a single `let r<n> = v<n>.<member>` in the generated F#.
#[derive(Debug, Clone)]
pub struct Site {
    /// 1-based line of the access. Every site sits on its own line, so our
    /// answer, FCS's, and the label are keyed by line alone.
    pub line: usize,
    /// Human-readable cell identity, printed verbatim on failure.
    pub label: String,
    /// The receiver's static type, **verbatim as written** in the probe — so a
    /// cell may name a fixture type (`FSharpMember.Rec`) or an FSharp.Core one
    /// (`int option`) without the generator having to know which.
    pub receiver_ty: String,
    /// The member name **as written** at the access.
    pub member: String,
    /// The name the member is expected to carry in metadata — the same as
    /// [`member`](Self::member) except where `[<CompiledName>]` splits them.
    /// Recorded so a differential can say which of the two a wrong answer
    /// reached rather than only that it was wrong.
    pub compiled_name: String,
    /// The compiled name of the type that *declares* the member — the currency
    /// the member-hiding differential settled on, since a rendered full name
    /// arrives decorated by `NicePrint`. Carries the ``\`n`` arity suffix
    /// exactly as FCS reports it, so [`declaring_arity`](Self::declaring_arity)
    /// cannot disagree with it.
    pub declaring_ty: String,
    /// The namespace the declaring type sits in, and the **F# source name** it is
    /// found under there — together, the search key into an [`AssemblyEnv`],
    /// which indexes by source name while FCS reports the compiled one
    /// (`Microsoft.FSharp.Core.Option` finds the entity whose compiled name is
    /// `FSharpOption`).
    ///
    /// Neither half is taken on trust: a lookup that finds nothing fails, and
    /// what it finds must carry [`declaring_ty`](Self::declaring_ty)'s compiled
    /// name. So a wrong key is a loud failure rather than a vacuous pass.
    pub declaring_ns: Vec<String>,
    /// See [`declaring_ns`](Self::declaring_ns).
    pub declaring_src: String,
    /// Half-open byte range of the access expression (`v<n>.<member>`) in the
    /// probe source, for the range-keyed `types` oracle.
    pub access: (usize, usize),
    /// The line's source text.
    pub text: String,
}

impl Site {
    /// The declaring type's generic arity, read off the ``\`n`` suffix of
    /// [`declaring_ty`](Self::declaring_ty) — the compiled-name convention FCS
    /// reports and the projector strips. Derived rather than declared so the two
    /// cannot drift apart.
    pub fn declaring_arity(&self) -> usize {
        match self.declaring_ty.rsplit_once('`') {
            Some((_, n)) => n.parse().expect("a compiled arity suffix is a number"),
            None => 0,
        }
    }

    /// [`declaring_ty`](Self::declaring_ty) without its arity suffix — the name
    /// the projector stores on the entity.
    pub fn declaring_stem(&self) -> &str {
        match self.declaring_ty.rsplit_once('`') {
            Some((stem, _)) => stem,
            None => &self.declaring_ty,
        }
    }
}

/// The generated universe: the fixture assembly's source, the probe file's
/// source, and the site index tying them together.
#[derive(Debug)]
pub struct Corpus {
    pub fixture: String,
    pub probe: String,
    pub sites: Vec<Site>,
}

/// One cell before line numbers and ranges are known.
struct Cell {
    /// The receiver's type **verbatim as it is written in the probe**, so a cell
    /// is free to name a fixture type or an FSharp.Core one.
    receiver_ty: &'static str,
    member: &'static str,
    compiled_name: &'static str,
    declaring_ty: &'static str,
    /// Dotted namespace of the declaring type, and its F# source name there.
    declaring_ns: &'static str,
    declaring_src: &'static str,
    label: &'static str,
}

/// Every access the probe makes.
///
/// `declaring_ty` is the **compiled** name of the declaring type: a module
/// compiles to a type of the module's own name, and a union case's fields are
/// declared on a nested type named for the case. An abbreviation declares
/// nothing, so a cell reached through one names the type it abbreviates — which
/// is the whole point of that dimension.
const CELLS: &[Cell] = &[
    Cell {
        receiver_ty: "FSharpMember.Rec",
        member: "Payload",
        compiled_name: "Payload",
        declaring_ty: "Rec",
        declaring_src: "Rec",
        declaring_ns: NS,
        label: "record field",
    },
    Cell {
        receiver_ty: "FSharpMember.Rec",
        member: "Label",
        compiled_name: "Label",
        declaring_ty: "Rec",
        declaring_src: "Rec",
        declaring_ns: NS,
        label: "record field, second",
    },
    Cell {
        receiver_ty: "FSharpMember.Klass",
        member: "Plain",
        compiled_name: "Plain",
        declaring_ty: "Klass",
        declaring_src: "Klass",
        declaring_ns: NS,
        label: "class property (control)",
    },
    // The dimension with the widest gap between the two names, and the only one
    // where reading the wrong one still *finds* something rather than finding
    // nothing: FCS reports this access as `Renamed`, the source name, while the
    // metadata our reader sees carries `CompiledRenamed`.
    Cell {
        receiver_ty: "FSharpMember.Klass",
        member: "Renamed",
        compiled_name: "CompiledRenamed",
        declaring_ty: "Klass",
        declaring_src: "Klass",
        declaring_ns: NS,
        label: "class property under [<CompiledName>]",
    },
    Cell {
        receiver_ty: "FSharpMember.Alias",
        member: "Plain",
        compiled_name: "Plain",
        declaring_ty: "Klass",
        declaring_src: "Klass",
        declaring_ns: NS,
        label: "class property through an abbreviation",
    },
    Cell {
        receiver_ty: "FSharpMember.Alias",
        member: "Renamed",
        compiled_name: "CompiledRenamed",
        declaring_ty: "Klass",
        declaring_src: "Klass",
        declaring_ns: NS,
        label: "[<CompiledName>] property through an abbreviation",
    },
    // A union case *test* property. Its range carries two oracle records — the
    // `IsOne` member and the `One` union case it tests — so a comparison keyed
    // on "the record at this range" has to say which one it means, exactly as
    // the attribute sites do with their constructor records.
    //
    // The union's `Tag` is deliberately absent: FCS reports no use at all for
    // `u.Tag`, so a cell probing it would assert nothing while looking like
    // coverage.
    Cell {
        receiver_ty: "FSharpMember.Union",
        member: "IsOne",
        compiled_name: "IsOne",
        declaring_ty: "Union",
        declaring_src: "Union",
        declaring_ns: NS,
        label: "union case test property",
    },
    // FSharp.Core's own unions. Every other cell reads a union fsc generated
    // whole from a plain `type U = A | B`; `option` and `list` are hand-written
    // in `prim-types.fsi`, carry `[<CompilationRepresentation>]`, and publish
    // members whose names merely *coincide* with a case-derived spelling. They
    // are also the unions real F# actually touches — `x.IsSome` is about as
    // common as member access gets — so a rule that happens to work on the
    // generated shape and not on these would be wrong everywhere that matters.
    //
    // `IsSome`/`IsNone` compile to **static** properties over a null receiver
    // check, which is why they are here rather than assumed: an instance-only
    // wake declines them while FCS binds them.
    Cell {
        receiver_ty: "int option",
        member: "IsSome",
        compiled_name: "IsSome",
        declaring_ty: "FSharpOption`1",
        declaring_src: "Option",
        declaring_ns: "Microsoft.FSharp.Core",
        label: "FSharp.Core option case test property",
    },
    Cell {
        receiver_ty: "int option",
        member: "Value",
        compiled_name: "Value",
        declaring_ty: "FSharpOption`1",
        declaring_src: "Option",
        declaring_ns: "Microsoft.FSharp.Core",
        label: "FSharp.Core option instance property",
    },
    // `list`'s cases are logically `[]` and `::`, so nothing about `IsEmpty` is
    // derivable from a case name — it is published, and that is the whole of the
    // rule. The cell fails on any reading of the metadata rows alone.
    Cell {
        receiver_ty: "int list",
        member: "IsEmpty",
        compiled_name: "IsEmpty",
        declaring_ty: "FSharpList`1",
        declaring_src: "List",
        declaring_ns: "Microsoft.FSharp.Collections",
        label: "FSharp.Core list published property",
    },
    Cell {
        receiver_ty: "int list",
        member: "Head",
        compiled_name: "Head",
        declaring_ty: "FSharpList`1",
        declaring_src: "List",
        declaring_ns: "Microsoft.FSharp.Collections",
        label: "FSharp.Core list published property, second",
    },
];

/// The fixture assembly's source.
pub fn fixture_source() -> String {
    format!(
        "// GENERATED by crates/sema/tests/all/common/fsharp_member_corpus.rs \
         — do not edit.\n\
         namespace {NS}\n\
         \n\
         type Rec =\n    {{ Payload : int\n      Label : string }}\n\
         \n\
         type Union =\n    | One of int\n    | Two of int * string\n\
         \n\
         type Klass(x : int) =\n\
         \x20   member _.Plain = x\n\
         \x20   [<CompiledName(\"CompiledRenamed\")>]\n\
         \x20   member _.Renamed = x + 1\n\
         \x20   member _.Combine (y : int) = x + y\n\
         \n\
         type Alias = Klass\n\
         \n\
         module Values =\n\
         \x20   let Answer = 42\n\
         \x20   [<CompiledName(\"CompiledValue\")>]\n\
         \x20   let SourceValue = 7\n"
    )
}

/// The probe file, plus the site index into it.
///
/// Each receiver is introduced by an annotated `failwith` binding rather than a
/// constructed value: the differential is about what a member access *resolves
/// to*, and a constructor call would add its own resolution to every line.
pub fn corpus() -> Corpus {
    let mut probe = String::from(
        "// GENERATED by crates/sema/tests/all/common/fsharp_member_corpus.rs — do not edit.\n\
         module FSharpMemberProbe\n\n",
    );
    for (idx, cell) in CELLS.iter().enumerate() {
        probe.push_str(&format!(
            "let v{idx} : {} = failwith \"fixture\"\n",
            cell.receiver_ty
        ));
    }
    probe.push('\n');

    let mut sites = Vec::new();
    for (idx, cell) in CELLS.iter().enumerate() {
        let text = format!("let r{idx} = v{idx}.{}\n", cell.member);
        let line = probe.lines().count() + 1;
        let expr = format!("v{idx}.{}", cell.member);
        let start = probe.len()
            + text
                .find(&expr)
                .expect("the access expression is on its own line");
        sites.push(Site {
            line,
            label: format!("{} ({}.{})", cell.label, cell.receiver_ty, cell.member),
            receiver_ty: cell.receiver_ty.to_string(),
            member: cell.member.to_string(),
            compiled_name: cell.compiled_name.to_string(),
            declaring_ty: cell.declaring_ty.to_string(),
            declaring_ns: cell.declaring_ns.split('.').map(str::to_owned).collect(),
            declaring_src: cell.declaring_src.to_string(),
            access: (start, start + expr.len()),
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
