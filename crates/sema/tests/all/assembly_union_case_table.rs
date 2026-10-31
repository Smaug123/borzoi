//! The **assembly union-case table**: every spelling F# allows for reaching —
//! or shadowing — a union case of a referenced assembly, each probe graded
//! against FCS.
//!
//! `Some`, `None`, `Ok`, `Error` and their siblings are FSharp.Core union cases
//! that reach every file through an `open` nobody writes: FSharp.Core's
//! `[<assembly: AutoOpen("Microsoft.FSharp.Core")>]`, which folds the
//! namespace's non-`[<RequireQualifiedAccess>]` union cases into bare scope at
//! position 0, below everything the file itself declares or opens. The same
//! cases arrive through an explicit `open` of a referenced namespace or module,
//! and through a qualified path (`Option.Some`, `Result.Ok`).
//!
//! The shadowing rules are FCS's, not a precedence ladder:
//!
//! - **expression position** reads FCS's `eUnqualifiedItems`, latest write
//!   wins — a later union case, exception, value or `open type` static of the
//!   name takes it, and so does a **constructible type** (class, struct,
//!   delegate), whose name enters the same table for construction. A record,
//!   union, interface or abbreviation does not.
//! - **pattern position** reads `ePatItems`: union cases, exceptions,
//!   active-pattern cases and literals. A type or a plain value never shadows
//!   a case there.
//!
//! Each row is one file — plus, for the project-half rows, the Compile-order
//! files before it — and each probe is one written name in it. Every probe
//! asserts two things:
//!
//! - **certain-implies-exact**: when we commit, FCS's record at the same span
//!   names the same symbol — the same union case of the same union in the same
//!   assembly, or the same project declaration;
//! - **its pinned verdict**, in both directions: [`Ours::Commit`] must commit
//!   (the must-commit obligation on the shapes this slice exists to answer),
//!   and [`Ours::Defer`] must not, so a guard that starts committing a shape we
//!   chose to decline is as visible as one that stops committing.
//!
//! FCS's own answer is **pinned** per probe too ([`Probe::fcs`]): agreement
//! between the two sides cannot catch a row whose premise is wrong — a shadower
//! that does not in fact shadow — and the pin can.
//!
//! The referenced-assembly shadowers live in
//! `tests/fixtures/union_case_shadow_env`; each namespace there is opened only
//! by the rows written against it.

use std::path::{Path, PathBuf};

use crate::common::{
    FileUses, NormalisedProjectUse, ensure_fsharp_core_dll,
    ensure_union_case_autoopen_fixture_built, ensure_union_case_shadow_fixture_built,
    invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::test_support::{DeclaringEntity, StructuralName, certified_expected};
use borzoi_sema::{AssemblyEnv, Resolution, ResolvedProject, resolve_project};
use rowan::TextRange;

/// Our pinned verdict for a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ours {
    /// We must commit, and to exactly FCS's symbol.
    Commit,
    /// We must not commit. The reason is the guard that declines it.
    Defer(&'static str),
}

/// One written name in a row.
struct Probe {
    /// The exact text of the name as written — a dotted path when FCS reports
    /// the use over the whole path (`Option.Some`).
    at: &'static str,
    /// Which occurrence of [`Self::at`] (0-based), matched at identifier
    /// boundaries.
    nth: usize,
    /// FCS's answer, pinned: `{assembly}:{full name}:{kind}` for a symbol of a
    /// referenced assembly, `proj:{full name}:{kind}` for one declared in the
    /// project, `-` for no symbol at this span. A constructor use renders its
    /// kind as `ctor`.
    fcs: &'static str,
    ours: Ours,
}

/// One row: a file, the Compile-order files before it, and its probes.
struct Row {
    label: &'static str,
    /// Labels of [`DECLS`] entries that precede the row in Compile order.
    decls: &'static [&'static str],
    src: &'static str,
    probes: &'static [Probe],
}

const SOME: &str = "FSharp.Core:Microsoft.FSharp.Core.Option<_>.Some:unioncase";
const NONE: &str = "FSharp.Core:Microsoft.FSharp.Core.Option<_>.None:unioncase";
const VSOME: &str = "FSharp.Core:Microsoft.FSharp.Core.ValueOption<_>.ValueSome:unioncase";
const VNONE: &str = "FSharp.Core:Microsoft.FSharp.Core.ValueOption<_>.ValueNone:unioncase";
const OK: &str = "FSharp.Core:Microsoft.FSharp.Core.Result<_,_>.Ok:unioncase";
const ERROR: &str = "FSharp.Core:Microsoft.FSharp.Core.Result<_,_>.Error:unioncase";
const CHOICE1OF2: &str = "FSharp.Core:Microsoft.FSharp.Core.Choice<_,_>.Choice1Of2:unioncase";
const CHOICE1OF3: &str = "FSharp.Core:Microsoft.FSharp.Core.Choice<_,_,_>.Choice1Of3:unioncase";

const fn commit(at: &'static str, nth: usize, fcs: &'static str) -> Probe {
    Probe {
        at,
        nth,
        fcs,
        ours: Ours::Commit,
    }
}

const fn defer(at: &'static str, nth: usize, fcs: &'static str, why: &'static str) -> Probe {
    Probe {
        at,
        nth,
        fcs,
        ours: Ours::Defer(why),
    }
}

/// Files that precede a row in Compile order, by label.
const DECLS: &[(&str, &str)] = &[
    (
        "ns_class",
        "namespace UcTable.NsClass\ntype Error(x: string) = member _.X = x\n",
    ),
    (
        "ns_union",
        "namespace UcTable.NsUnion\ntype U = Ok of int | Nope\n",
    ),
    (
        "other_class",
        "module UcTable.OtherClass\ntype Error(x: string) = member _.X = x\n",
    ),
    (
        "alias_auto_open",
        "namespace UcTable.Alias\ntype AO = AutoOpenAttribute\n[<AO>]\nmodule A =\n    let Some (x: int) = x + 1\n",
    ),
    (
        "hdr_alias",
        "namespace UcTable.Hdr\ntype AO = AutoOpenAttribute\n",
    ),
    (
        "hdr_module",
        "[<UcTable.Hdr.AO>]\nmodule UcTable.Hdr.Helpers\nlet Some (x: int) = x + 1\n",
    ),
    (
        "root_auto_open",
        "[<AutoOpen>]\nmodule UcRootAuto\nlet ValueSome (x: int) = x + 1\n",
    ),
    (
        "auto_open_union",
        "namespace UcTable.AoUnion\n[<AutoOpen>]\nmodule A =\n    type U = Some of int | None\n",
    ),
];

const ROWS: &[Row] = &[
    // ---------------- the implicit fold, unshadowed ----------------
    Row {
        label: "base expression",
        decls: &[],
        src: "module UcR01\n\
              let a = Some 1\n\
              let b = None\n\
              let c = ValueSome 1\n\
              let d = ValueNone\n\
              let e : Result<int,string> = Ok 1\n\
              let f : Result<int,string> = Error \"x\"\n\
              let g : Choice<int,string> = Choice1Of2 1\n\
              let h = List.map Some [1]\n\
              let l = Some (Some 1)\n\
              let m : Choice<int,int,int> = Choice1Of3 1\n",
        probes: &[
            commit("Some", 0, SOME),
            commit("None", 0, NONE),
            commit("ValueSome", 0, VSOME),
            commit("ValueNone", 0, VNONE),
            commit("Ok", 0, OK),
            commit("Error", 0, ERROR),
            commit("Choice1Of2", 0, CHOICE1OF2),
            commit("Some", 1, SOME),
            commit("Some", 2, SOME),
            commit("Some", 3, SOME),
            commit("Choice1Of3", 0, CHOICE1OF3),
        ],
    },
    Row {
        label: "base qualified",
        decls: &[],
        src: "module UcR02\n\
              let i = Option.Some 2\n\
              let j : Result<int,string> = Result.Ok 3\n\
              let k = Option.None\n\
              let n = Option<int>.None\n",
        probes: &[
            commit("Option.Some", 0, SOME),
            commit("Result.Ok", 0, OK),
            commit("Option.None", 0, NONE),
            defer(
                "Option<int>.None",
                0,
                NONE,
                "a type-applied qualifier is not walked as a case path",
            ),
        ],
    },
    Row {
        label: "base patterns",
        decls: &[],
        src: "module UcR03\n\
              let f x =\n    match x with\n    | Some (Some y) -> y\n    | Some None -> 0\n    | None -> 1\n\
              let g x = match x with Option.Some y -> y | Option.None -> 0\n\
              let h x = match x with | Some _ as s -> s | None -> None\n\
              let (Some z) = Some 1\n\
              let k (Some x) = x\n\
              let m = fun (Some x) -> x\n\
              let n = function Ok x -> x | Error (_: string) -> 0\n\
              let o x = match x with Some 1 | None -> 0 | Some _ -> 1\n\
              let p x = match x with ValueSome y -> y | ValueNone -> 0\n\
              let q (x: Choice<int,string>) = match x with Choice1Of2 a -> a | Choice2Of2 _ -> 0\n",
        probes: &[
            commit("Some", 0, SOME),
            commit("Some", 1, SOME),
            commit("Some", 2, SOME),
            commit("None", 0, NONE),
            commit("None", 1, NONE),
            commit("Option.Some", 0, SOME),
            commit("Option.None", 0, NONE),
            commit("Some", 3, SOME),
            commit("None", 2, NONE),
            commit("None", 3, NONE),
            commit("Some", 4, SOME),
            commit("Some", 5, SOME),
            commit("Some", 6, SOME),
            commit("Some", 7, SOME),
            commit("Ok", 0, OK),
            commit("Error", 0, ERROR),
            commit("Some", 8, SOME),
            commit("None", 4, NONE),
            commit("ValueSome", 0, VSOME),
            commit("ValueNone", 0, VNONE),
            commit("Choice1Of2", 0, CHOICE1OF2),
        ],
    },
    // ---------------- shadowed by this file ----------------
    Row {
        label: "local union before",
        decls: &[],
        src: "module UcR04\n\
              type U = Some of int | Nope\n\
              let a = Some 1\n\
              let f x = match x with Some y -> y | Nope -> 0\n\
              let b = None\n",
        probes: &[
            commit("Some", 1, "proj:UcR04.U.Some:unioncase"),
            commit("Some", 2, "proj:UcR04.U.Some:unioncase"),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "local union after",
        decls: &[],
        src: "module UcR05\n\
              let a = Some 1\n\
              type U = Some of int | Nope\n\
              let b = Some 2\n",
        probes: &[
            commit("Some", 0, SOME),
            commit("Some", 2, "proj:UcR05.U.Some:unioncase"),
        ],
    },
    Row {
        label: "local active pattern",
        decls: &[],
        src: "module UcR06\n\
              let (|Some|_|) (x: int) = if x > 0 then Option.Some x else Option.None\n\
              let f (x: int) = match x with Some y -> y | _ -> 0\n\
              let a = Some 1\n",
        probes: &[
            commit("Some", 1, "proj:UcR06.(|Some|_|).Some:activepatterncase"),
            commit("Some", 2, SOME),
        ],
    },
    Row {
        label: "local class evicts in expression only",
        decls: &[],
        src: "module UcR07\n\
              let before : Result<int,string> = Error \"x\"\n\
              type Error(x: string) = member _.X = x\n\
              let a = Error \"x\"\n\
              let f (x: Result<int,string>) = match x with Ok y -> y | Error _ -> 0\n\
              let b : Result<int,string> = Ok 1\n",
        probes: &[
            commit("Error", 0, ERROR),
            defer(
                "Error",
                2,
                "proj:UcR07.Error:ctor",
                "a constructible type takes the bare constructor slot",
            ),
            commit("Error", 3, ERROR),
            commit("Ok", 0, OK),
            commit("Ok", 1, OK),
        ],
    },
    Row {
        label: "local struct and record",
        decls: &[],
        src: "module UcR08\n\
              [<Struct>]\n\
              type Some = val V: int\n\
              type Ok = { V: int }\n\
              let a = Some\n\
              let b : Result<int,string> = Ok 1\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR08.Some:ctor",
                "a struct takes the bare constructor slot",
            ),
            commit("Ok", 1, OK),
        ],
    },
    Row {
        label: "nested module scope",
        decls: &[],
        src: "module UcR09\n\
              module Inner =\n    type Error(x: string) = member _.X = x\n    type U = Some of int | Nope\n\
              let a : Result<int,string> = Error \"x\"\n\
              let b = Some 1\n\
              open Inner\n\
              let c = Error \"y\"\n\
              let d = Some 2\n",
        probes: &[
            commit("Error", 1, ERROR),
            commit("Some", 1, SOME),
            defer(
                "Error",
                2,
                "proj:UcR09.Inner.Error:ctor",
                "a constructible type takes the bare constructor slot",
            ),
            commit("Some", 2, "proj:UcR09.Inner.U.Some:unioncase"),
        ],
    },
    Row {
        label: "local exception",
        decls: &[],
        src: "module UcR10\n\
              exception Error of string\n\
              let a = Error \"x\"\n\
              let f x = match x with Error s -> s | _ -> \"\"\n\
              let b = Ok 1\n",
        probes: &[
            commit("Error", 1, "proj:UcR10.Error:type"),
            commit("Error", 2, "proj:UcR10.Error:type"),
            commit("Ok", 0, OK),
        ],
    },
    Row {
        label: "local rqa union",
        decls: &[],
        src: "module UcR11\n\
              [<RequireQualifiedAccess>]\n\
              type U = Some of int | None\n\
              let a = Some 1\n\
              let b = U.Some 1\n\
              let f x = match x with Some y -> y | None -> 0\n",
        probes: &[
            commit("Some", 1, SOME),
            commit("U.Some", 0, "proj:UcR11.U.Some:unioncase"),
            commit("Some", 2, SOME),
            commit("None", 1, NONE),
        ],
    },
    Row {
        label: "record fields of the name",
        decls: &[],
        src: "module UcR12\n\
              type R = { Some: int; None: string }\n\
              let a = Some 1\n",
        probes: &[commit("Some", 1, SOME)],
    },
    Row {
        label: "same-file auto-open module",
        decls: &[],
        src: "namespace UcR13\n\
              [<AutoOpen>]\n\
              module A =\n    type T = Some of int | Nope\n\
              module Use =\n    let a = Some 1\n    let b = None\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR13.A.T.Some:unioncase",
                "the walk does not fold a same-file auto-open module's cases",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "same-file auto-open module after an explicit open",
        decls: &[],
        src: "namespace UcR13b\n\
              open Shadow.Plain\n\
              [<AutoOpen>]\n\
              module A =\n    type T = Circle of int | Square\n\
              module Use =\n    let a = Circle 1\n    let b = Empty\n",
        probes: &[
            defer(
                "Circle",
                1,
                "proj:UcR13b.A.T.Circle:unioncase",
                "the walk does not fold a same-file auto-open module's cases",
            ),
            commit(
                "Empty",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Plain.Shape.Empty:unioncase",
            ),
        ],
    },
    Row {
        label: "same-file auto-open type",
        decls: &[],
        src: "namespace UcR13c\n\
              [<AutoOpen>]\n\
              type Helpers =\n    static member Ok = 1\n\
              module Use =\n    let a = Ok\n    let b = None\n",
        probes: &[
            defer(
                "Ok",
                1,
                "proj:UcR13c.Helpers.Ok:member",
                "the walk does not fold a same-file auto-open type's statics",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "same-file auto-open module only using the name",
        decls: &[],
        src: "namespace UcR13h\n\
              [<AutoOpen>]\n\
              module A =\n    let bump x = match x with Some y -> Some (y + 1) | None -> None\n\
              module Use =\n    let a = Some 1\n    let f x = match x with None -> 0 | Some y -> y\n",
        probes: &[
            commit("Some", 2, SOME),
            commit("None", 2, NONE),
            commit("Some", 3, SOME),
        ],
    },
    Row {
        label: "same-file auto-open module declaring an exception",
        decls: &[],
        src: "namespace UcR13i\n\
              [<AutoOpen>]\n\
              module A =\n    exception Error of string\n\
              module Use =\n    let a = Error \"x\"\n    let b = Ok 1\n",
        probes: &[
            defer(
                "Error",
                1,
                "proj:UcR13i.A.Error:type",
                "the walk does not fold a same-file auto-open module's exceptions",
            ),
            commit("Ok", 0, OK),
        ],
    },
    Row {
        label: "same-file auto-open module declaring an active pattern",
        decls: &[],
        src: "namespace UcR13j\n\
              [<AutoOpen>]\n\
              module A =\n    let (|Some|_|) (x: int) = if x > 0 then Option.Some x else Option.None\n\
              module Use =\n    let f (x: int) = match x with Some y -> y | _ -> 0\n    let b = None\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR13j.A.(|Some|_|).Some:activepatterncase",
                "the walk does not fold a same-file auto-open module's active patterns",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "recursive module declares the case later",
        decls: &[],
        src: "module rec UcR13d\n\
              let a = Some 1\n\
              let b = None\n\
              type U = Some of int | Other\n",
        probes: &[
            defer(
                "Some",
                0,
                "proj:UcR13d.U.Some:unioncase",
                "a rec block's cases are in scope before their declaration",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "recursive module binds a function of the name later",
        decls: &[],
        src: "module rec UcR13k\n\
              let f () = Some 1\n\
              let Some x = x + 1\n\
              let g () = None\n",
        probes: &[
            defer(
                "Some",
                0,
                "proj:UcR13k.Some:member",
                "a rec block's bindings are in scope before their declaration",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "a function head spelled like a case, then used",
        decls: &[],
        src: "module UcR13l\n\
              let Some x = x + 1\n\
              let a = Some 1\n\
              let b = None\n",
        probes: &[
            commit("Some", 1, "proj:UcR13l.Some:member"),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "recursive type group",
        decls: &[],
        src: "module UcR13e\n\
              type C() =\n    member _.M() = Some 1\n    member _.N() = None\n\
              and U = Some of int | Other\n\
              type V =\n    | Ok of int\n    | Fail\n    member this.Next = Ok 2\n",
        probes: &[
            commit("Some", 0, "proj:UcR13e.U.Some:unioncase"),
            commit("None", 0, NONE),
            commit("Ok", 1, "proj:UcR13e.V.Ok:unioncase"),
        ],
    },
    Row {
        label: "function head spelled like a case",
        decls: &[],
        src: "module UcR13f\n\
              let a : Result<int,string> = Error \"x\"\n\
              let f (x: int) = Some x\n",
        probes: &[commit("Error", 0, ERROR), commit("Some", 0, SOME)],
    },
    Row {
        label: "dotted heads spelled like a case",
        decls: &[],
        src: "module UcR13g\n\
              module Error =\n    let x = 1\n\
              type Ok =\n    static member Make = 2\n\
              let a = Error.x\n\
              let b = Ok.Make\n\
              let c : Result<int,string> = Error \"e\"\n",
        probes: &[
            commit("Error.x", 0, "proj:UcR13g.Error.x:member"),
            commit("Ok.Make", 0, "proj:UcR13g.Ok.Make:member"),
            commit("Error", 1, ERROR),
        ],
    },
    Row {
        label: "union case in a closed nested module",
        decls: &[],
        src: "module UcR14\n\
              module Inner =\n    type U = None | Other\n    let x = None\n\
              let y = None\n",
        probes: &[
            commit("None", 1, "proj:UcR14.Inner.U.None:unioncase"),
            commit("None", 2, NONE),
        ],
    },
    Row {
        label: "project module open",
        decls: &[],
        src: "module UcR15\n\
              module M =\n    type R = Ok of int | Bad\n\
              open M\n\
              let a = Ok 1\n\
              let f x = match x with Ok y -> y | Bad -> 0\n\
              let b : Result<int,string> = Error \"x\"\n",
        probes: &[
            commit("Ok", 1, "proj:UcR15.M.R.Ok:unioncase"),
            commit("Ok", 2, "proj:UcR15.M.R.Ok:unioncase"),
            commit("Error", 0, ERROR),
        ],
    },
    // ---------------- shadowed by an earlier file ----------------
    Row {
        label: "earlier file's class in the enclosing namespace",
        decls: &["ns_class"],
        src: "namespace UcTable.NsClass\n\
              module Use =\n    let a = Error \"x\"\n    let b = Ok 1\n",
        probes: &[
            defer(
                "Error",
                0,
                "proj:UcTable.NsClass.Error:ctor",
                "a constructible type takes the bare constructor slot",
            ),
            commit("Ok", 0, OK),
        ],
    },
    Row {
        label: "earlier file's union in the enclosing namespace",
        decls: &["ns_union"],
        src: "namespace UcTable.NsUnion\n\
              module Use =\n    let a = Ok 1\n    let f x = match x with Ok y -> y | Nope -> 0\n",
        probes: &[
            commit("Ok", 0, "proj:UcTable.NsUnion.U.Ok:unioncase"),
            commit("Ok", 1, "proj:UcTable.NsUnion.U.Ok:unioncase"),
        ],
    },
    Row {
        label: "earlier file's auto-open module spelled through an abbreviation",
        decls: &["alias_auto_open"],
        src: "namespace UcTable.Alias\n\
              module Use =\n    let a = Some 1\n    let b = None\n",
        probes: &[
            defer(
                "Some",
                0,
                "proj:UcTable.Alias.A.Some:member",
                "an earlier file's container only an abbreviation marks auto-open",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "earlier file's module header auto-opened through an abbreviation",
        decls: &["hdr_alias", "hdr_module"],
        src: "module UcTable.Hdr.Consumer\n\
              let a = Some 1\n\
              let b = None\n",
        probes: &[
            defer(
                "Some",
                0,
                "proj:UcTable.Hdr.Helpers.Some:member",
                "an earlier file declares a binder of the name",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "earlier file's auto-open module union in the namespace",
        decls: &["auto_open_union"],
        src: "namespace UcTable.AoUnion\n\
              module Use =\n    let f x = match x with Some v -> v | None -> 0\n    let a = Some 1\n",
        probes: &[
            defer(
                "Some",
                0,
                "proj:UcTable.AoUnion.A.U.Some:unioncase",
                "the enclosing-namespace fold withdraws the earlier file's case",
            ),
            defer(
                "None",
                0,
                "proj:UcTable.AoUnion.A.U.None:unioncase",
                "the enclosing-namespace fold withdraws the earlier file's case",
            ),
            defer(
                "Some",
                1,
                "proj:UcTable.AoUnion.A.U.Some:unioncase",
                "the enclosing-namespace fold withdraws the earlier file's case",
            ),
        ],
    },
    Row {
        label: "earlier file's class, opened later",
        decls: &["other_class"],
        src: "module UcR18\n\
              let a : Result<int,string> = Error \"x\"\n\
              open UcTable.OtherClass\n\
              let b = Error \"x\"\n",
        probes: &[
            commit("Error", 0, ERROR),
            defer(
                "Error",
                1,
                "proj:UcTable.OtherClass.Error:ctor",
                "a constructible type takes the bare constructor slot",
            ),
        ],
    },
    // ---------------- shadowed by a referenced assembly ----------------
    Row {
        label: "assembly namespace union open",
        decls: &[],
        src: "module UcR19\n\
              open Shadow.Cases\n\
              let a = Ok 1\n\
              let b = Error \"x\"\n\
              let c = Some 1\n\
              let d = None\n\
              let f x = match x with Ok y -> y | Error _ -> 0\n\
              let g = Maybe.Some 1\n\
              let h = Option.Some 1\n",
        probes: &[
            commit(
                "Ok",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Ok:unioncase",
            ),
            commit(
                "Error",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Error:unioncase",
            ),
            commit(
                "Some",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Maybe<_>.Some:unioncase",
            ),
            commit(
                "None",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Maybe<_>.None:unioncase",
            ),
            commit(
                "Ok",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Ok:unioncase",
            ),
            commit(
                "Error",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Error:unioncase",
            ),
            commit(
                "Maybe.Some",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Maybe<_>.Some:unioncase",
            ),
            commit("Option.Some", 0, SOME),
        ],
    },
    Row {
        label: "assembly rqa union open",
        decls: &[],
        src: "module UcR20\n\
              open Shadow.Rqa\n\
              let a : Result<int,string> = Ok 1\n\
              let b = Alt.Ok 1\n",
        probes: &[
            commit("Ok", 0, OK),
            commit(
                "Alt.Ok",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Rqa.Alt.Ok:unioncase",
            ),
        ],
    },
    Row {
        label: "assembly class open",
        decls: &[],
        src: "module UcR21\n\
              let before : Result<int,string> = Error \"x\"\n\
              open Shadow.Classes\n\
              let a = Error \"x\"\n\
              let b = Ok 1\n\
              let c = None\n\
              let f (x: Result<int,string>) = match x with Ok y -> y | Error _ -> 0\n",
        probes: &[
            commit("Error", 0, ERROR),
            defer(
                "Error",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Classes.Error:ctor",
                "a constructible type takes the bare constructor slot",
            ),
            defer(
                "Ok",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Classes.Ok:ctor",
                "a struct takes the bare constructor slot",
            ),
            commit("None", 0, NONE),
            commit("Ok", 1, OK),
            commit("Error", 2, ERROR),
        ],
    },
    Row {
        label: "assembly auto-open module in an opened namespace",
        decls: &[],
        src: "module UcR22\n\
              open Shadow.AutoNs\n\
              let a = Choice1Of2 1\n\
              let b : Choice<int,int,int> = Choice1Of3 1\n",
        probes: &[
            commit(
                "Choice1Of2",
                0,
                "SemaUnionCaseShadowFixture:Shadow.AutoNs.Auto.Choice.Choice1Of2:unioncase",
            ),
            commit("Choice1Of3", 0, CHOICE1OF3),
        ],
    },
    Row {
        label: "assembly module open",
        decls: &[],
        src: "module UcR23\n\
              open Shadow.Modules.Inner\n\
              let a = Some 1\n\
              let b = None\n\
              let c = Ok 1\n\
              let f x = match x with Some y -> y | None -> 0\n",
        probes: &[
            commit(
                "Some",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Inner.Opt.Some:unioncase",
            ),
            commit(
                "None",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Inner.Opt.None:unioncase",
            ),
            commit("Ok", 0, OK),
            commit(
                "Some",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Inner.Opt.Some:unioncase",
            ),
            commit(
                "None",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Inner.Opt.None:unioncase",
            ),
        ],
    },
    Row {
        label: "an opened module's class beside a local case of the name",
        decls: &[],
        src: "module UcR40\n\
              type U = Error of int | Other\n\
              open Shadow.Modules.WithClass\n\
              let f x = match x with Error y -> y | _ -> 0\n",
        probes: &[commit("Error", 1, "proj:UcR40.U.Error:unioncase")],
    },
    Row {
        label: "two assembly module opens",
        decls: &[],
        src: "module UcR24\n\
              open Shadow.Modules.Inner\n\
              open Shadow.Modules.Other\n\
              let a = Some \"x\"\n\
              let b = None\n\
              let c = ValueNone\n",
        probes: &[
            commit(
                "Some",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Other.Opt2.Some:unioncase",
            ),
            commit(
                "None",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Inner.Opt.None:unioncase",
            ),
            commit(
                "ValueNone",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Other.Opt2.ValueNone:unioncase",
            ),
        ],
    },
    Row {
        label: "an opened module's two unions sharing a case",
        decls: &[],
        src: "module UcR24b\n\
              open Shadow.Modules.Dup\n\
              let a = Twice \"s\"\n\
              let f x = match x with Twice _ -> 1 | _ -> 0\n\
              let b = OnlySecond\n",
        probes: &[
            defer(
                "Twice",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Dup.Second.Twice:unioncase",
                "the fold cannot vouch for the two unions' declaration order",
            ),
            defer(
                "Twice",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Dup.Second.Twice:unioncase",
                "the fold cannot vouch for the two unions' declaration order",
            ),
            commit(
                "OnlySecond",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Modules.Dup.Second.OnlySecond:unioncase",
            ),
        ],
    },
    Row {
        label: "assembly plain union open",
        decls: &[],
        src: "module UcR25\n\
              open Shadow.Plain\n\
              let a = Circle 1.0\n\
              let b = Empty\n\
              let f x = match x with Circle r -> r | Empty -> 0.0\n\
              let c = Shape.Circle 2.0\n",
        probes: &[
            commit(
                "Circle",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Plain.Shape.Circle:unioncase",
            ),
            commit(
                "Empty",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Plain.Shape.Empty:unioncase",
            ),
            commit(
                "Circle",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Plain.Shape.Circle:unioncase",
            ),
            commit(
                "Empty",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Plain.Shape.Empty:unioncase",
            ),
            commit(
                "Shape.Circle",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Plain.Shape.Circle:unioncase",
            ),
        ],
    },
    Row {
        label: "open type of a union",
        decls: &[],
        src: "module UcR27\n\
              open type Shadow.Cases.Alt\n\
              let a = Ok 1\n\
              let f x = match x with Ok y -> y | Error _ -> 0\n\
              let b = Some 1\n",
        probes: &[
            defer(
                "Ok",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Ok:unioncase",
                "an `open type` brings names nothing lists",
            ),
            commit(
                "Ok",
                1,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Ok:unioncase",
            ),
            commit(
                "Error",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Cases.Alt.Error:unioncase",
            ),
            commit("Some", 0, SOME),
        ],
    },
    Row {
        label: "open type of a type a source extension augments",
        decls: &[],
        src: "module UcR27b\n\
              open System\n\
              type String with\n    static member Some = 42\n\
              open type String\n\
              let answer = Some\n\
              let f x = match x with None -> 0 | _ -> 1\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR27b.Some:member",
                "an `open type` brings names nothing lists",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "open type of an rqa union",
        decls: &[],
        src: "module UcR28\n\
              open type Shadow.Rqa.Alt\n\
              let a : Result<int,string> = Ok 1\n\
              let f (x: Result<int,string>) = match x with Ok y -> y | Error _ -> 0\n",
        probes: &[
            defer("Ok", 0, OK, "an `open type` brings names nothing lists"),
            commit("Ok", 1, OK),
            commit("Error", 0, ERROR),
        ],
    },
    Row {
        label: "an extern prototype named like a case",
        decls: &[],
        src: "module UcR29\n\
              open System.Runtime.InteropServices\n\
              [<DllImport(\"probe\")>]\n\
              extern int Some(int x)\n\
              let a = Some 1\n\
              let b = None\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR29.Some:member",
                "an extern prototype is a value binder the walk does not intern",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "a join range variable named like a case",
        decls: &[],
        src: "module UcR30\n\
              let q =\n    query {\n        for x in [ 1 ] do\n        join Ok in [ 2 ] on (x = Ok)\n        select Ok\n    }\n\
              let b = Some 1\n",
        probes: &[
            defer(
                "Ok",
                1,
                "proj:Ok:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            defer(
                "Ok",
                2,
                "proj:Ok:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            commit("Some", 0, SOME),
        ],
    },
    Row {
        label: "join range variables named like implicit values",
        decls: &[],
        src: "module UcR30b\n\
              let q =\n    query {\n        for x in [ 1 ] do\n        join id in [ 2 ] on (x = id)\n        groupJoin y in [ 3 ] on (x = y) into ignore\n        select (id, ignore)\n    }\n\
              let b = id 1\n",
        probes: &[
            defer(
                "id",
                1,
                "proj:id:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            defer(
                "id",
                2,
                "proj:id:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            defer(
                "ignore",
                1,
                "proj:ignore:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            commit(
                "id",
                3,
                "FSharp.Core:Microsoft.FSharp.Core.Operators.id:member",
            ),
        ],
    },
    Row {
        label: "a join's range variables bound by a tuple",
        decls: &[],
        src: "module UcR30c\n\
              let q =\n    query {\n        for i in [ 1 ] do\n        join (Some, y) in [ (1, 1) ] on (i = y)\n        select Some\n    }\n\
              let s = Some 1\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:Some:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            commit("Some", 2, SOME),
        ],
    },
    Row {
        label: "an auto-open type abbreviation borrowing statics",
        decls: &[],
        src: "namespace UcR31\n\
              [<AutoOpen>]\n\
              type Helpers = Shadow.Statics.Holder\n\
              module Use =\n    let a = Ok\n    let b = None 3\n    let c = Some 1\n",
        probes: &[
            defer(
                "Ok",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Statics.Holder.Ok:member",
                "an auto-open type's borrowed statics cannot be listed",
            ),
            defer(
                "None",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Statics.Holder.None:member",
                "an auto-open type's borrowed statics cannot be listed",
            ),
            defer(
                "Some",
                0,
                SOME,
                "an auto-open type's borrowed statics cannot be listed",
            ),
        ],
    },
    Row {
        label: "a custom operation spelled like a case",
        decls: &[],
        src: "module UcR32\n\
              type B() =\n    member _.Yield(_: unit) = 0\n    [<CustomOperation(\"Some\")>]\n    member _.RunSome(state: int, v: int) = state + v\n\
              let builder = B()\n\
              let r = builder { Some 42 }\n\
              let o = Some 1\n",
        probes: &[
            // Occurrence 0 is the custom operation's name, in its attribute.
            defer(
                "Some",
                1,
                "proj:Some:member",
                "a computation statement's head may be a custom operation",
            ),
            commit("Some", 2, SOME),
        ],
    },
    Row {
        label: "a custom operation inside a computation loop",
        decls: &[],
        src: "module UcR32b\n\
              type B() =\n    member _.Yield(x: int) = x\n    member _.For(xs: int list, f: int -> int) = List.sum (List.map f xs)\n    [<CustomOperation(\"Some\", MaintainsVariableSpace = true)>]\n    member _.RunSome(state: int, [<ProjectionParameter>] v: int -> int) = state\n\
              let builder = B()\n\
              let r = builder { for x in [ 1 ] do Some x }\n\
              let o = Some 1\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:Some:member",
                "a computation statement's head may be a custom operation",
            ),
            commit("Some", 2, SOME),
        ],
    },
    Row {
        label: "a custom operation inside a computation conditional",
        decls: &[],
        src: "module UcR32c\n\
              type B() =\n    member _.Yield(_: unit) = 0\n    member _.Zero() = 0\n    [<CustomOperation(\"Some\")>]\n    member _.RunSome(state: int, v: int) = state + v\n\
              let builder = B()\n\
              let r = builder { if true then Some 42 }\n\
              let o = async { return Some 1 }\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:Some:member",
                "a computation statement's head may be a custom operation",
            ),
            commit("Some", 2, SOME),
        ],
    },
    Row {
        label: "a reopened namespace block keeps its earlier cases",
        decls: &[],
        src: "namespace UcR34\n\
              type U = Some of int | Other\n\
              namespace UcR34\n\
              module Use =\n    let a = Some 1\n    let f x = match x with Some y -> y | _ -> 0\n    let b = None\n",
        probes: &[
            commit("Some", 1, "proj:UcR34.U.Some:unioncase"),
            commit("Some", 2, "proj:UcR34.U.Some:unioncase"),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "an auto-open attribute spelled through an abbreviation",
        decls: &[],
        src: "namespace UcR33\n\
              type AO = AutoOpenAttribute\n\
              [<AO>]\n\
              module A =\n    let Some (x: int) = x + 1\n\
              module Use =\n    let a = Some 1\n    let b = None\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR33.A.Some:member",
                "an attribute spelled as an abbreviation may be `[<AutoOpen>]`",
            ),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "open type statics",
        decls: &[],
        src: "module UcR26\n\
              open type Shadow.Statics.Holder\n\
              let a = Ok\n\
              let b = None 3\n\
              let f x = match x with Ok y -> y | Error (_: string) -> 0\n\
              let g x = match x with None -> 0 | Some y -> y\n",
        probes: &[
            commit(
                "Ok",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Statics.Holder.Ok:member",
            ),
            commit(
                "None",
                0,
                "SemaUnionCaseShadowFixture:Shadow.Statics.Holder.None:member",
            ),
            commit("Ok", 1, OK),
            commit("Error", 0, ERROR),
            commit("None", 1, NONE),
            commit("Some", 0, SOME),
        ],
    },
    // Last, because a root-namespace union reaches every later file of the one
    // FCS project the rows share.
    Row {
        label: "a reopened global namespace keeps its earlier cases",
        decls: &[],
        src: "namespace global\n\
              type UcR99U = ValueNone | UcR99Other\n\
              namespace global\n\
              module UcR99Use =\n    let a = ValueNone\n    let f x = match x with ValueNone -> 0 | _ -> 1\n    let b = None\n",
        probes: &[
            commit("ValueNone", 1, "proj:UcR99U.ValueNone:unioncase"),
            commit("ValueNone", 2, "proj:UcR99U.ValueNone:unioncase"),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "query range variables an into binds",
        decls: &[],
        src: "module UcR41\n\
              let q =\n    query {\n        for i in [ 1 ] do\n        groupBy i into Some\n        select Some\n    }\n\
              let r =\n    query {\n        for i in [ 1 ] do\n        groupBy i into id\n        select id\n    }\n\
              let s = Some (id 1)\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:Some:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            defer(
                "id",
                1,
                "proj:id:member",
                "a query range variable is a value binder the walk does not intern",
            ),
            commit("Some", 2, SOME),
            commit(
                "id",
                2,
                "FSharp.Core:Microsoft.FSharp.Core.Operators.id:member",
            ),
        ],
    },
    // Last: a `namespace global` block's declarations reach every later file
    // of the project, so no row may follow it.
    Row {
        label: "an earlier namespace global block of the same file",
        decls: &[],
        src: "namespace global\n\
              type UcR40U = Some of int | Other\n\
              [<AutoOpen>]\n\
              module UcR40M =\n    let id (x: int) = x + 1\n\
              namespace UcR40\n\
              module Use =\n    let a = Some 1\n    let b = id 1\n",
        probes: &[
            defer(
                "Some",
                1,
                "proj:UcR40U.Some:unioncase",
                "an earlier block's root contribution",
            ),
            defer(
                "id",
                1,
                "proj:UcR40M.id:member",
                "an earlier block's root contribution",
            ),
        ],
    },
];

/// Rows FCS reports errors for — a declaration or expression cut off where an
/// editor buffer is cut off between keystrokes. Every probe is still graded:
/// a commit must name what FCS bound.
const RECOVERED: &[Row] = &[
    Row {
        label: "an applied pattern over a literal of the name",
        decls: &[],
        src: "module UcX00\n\
              [<Literal>]\n\
              let Some = 42\n\
              let f x = match x with Some y -> 1 | _ -> 0\n",
        probes: &[defer(
            "Some",
            1,
            "proj:UcX00.Some:member",
            "FCS binds the literal, misapplied",
        )],
    },
    Row {
        label: "a pattern cut off mid-name",
        decls: &[],
        src: "module UcX01\n\
              let a = Some 1\n\
              let b = match a with | Some x -> x | Non\n\
              let c = None\n\
              let d = (\n",
        probes: &[
            commit("Some", 0, SOME),
            commit("Some", 1, SOME),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "a class declaration cut off before its body",
        decls: &[],
        src: "module UcX02\n\
              type Error(x: string) =\n\
              let a = Error \"x\"\n\
              let b = Ok 1\n",
        probes: &[
            defer(
                "Error",
                1,
                "proj:UcX02.Error:ctor",
                "a constructible type of the name is declared in the file",
            ),
            commit("Ok", 0, OK),
        ],
    },
    Row {
        label: "an open cut off mid-path",
        decls: &[],
        src: "module UcX03\n\
              open Shadow.\n\
              let a = Some 1\n\
              let b = Error \"x\"\n",
        probes: &[commit("Some", 0, SOME), commit("Error", 0, ERROR)],
    },
    Row {
        label: "a union declaration cut off mid-case",
        decls: &[],
        src: "module UcX04\n\
              type U =\n    | Some of int\n    | \n\
              let a = Some 1\n\
              let b = None\n",
        probes: &[
            commit("Some", 1, "proj:UcX04.U.Some:unioncase"),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "an auto-open module cut off mid-declaration",
        decls: &[],
        src: "namespace UcX05\n\
              [<AutoOpen>]\n\
              module A =\n    type T = Ok of int | \n\
              module Use =\n    let a = Ok 1\n    let b = Some 2\n",
        probes: &[
            defer(
                "Ok",
                1,
                "proj:UcX05.A.T.Ok:unioncase",
                "the walk does not fold a same-file auto-open module's cases",
            ),
            commit("Some", 0, SOME),
        ],
    },
    Row {
        label: "primary constructors over patterns that are not simple",
        decls: &[],
        src: "module UcX06\n\
              type C1 (Option.None) =\n\
              \x20   member _.V = 0\n\
              type C2 (Result.Ok x) =\n\
              \x20   member _.V = 0\n\
              type C3 (Some (y: int)) =\n\
              \x20   member _.V = 0\n\
              type C4 (Choice1Of2 z, w: int) =\n\
              \x20   member _.V = w\n\
              type C5 ([Some 1]) =\n\
              \x20   member _.V = 0\n\
              let after = Some 1\n",
        probes: &[
            defer("Option.None", 0, "-", "FCS reads nothing of the pattern"),
            defer("Result.Ok", 0, "-", "FCS reads nothing of the pattern"),
            defer("Some", 0, "-", "FCS reads nothing of the pattern"),
            defer("Choice1Of2", 0, "-", "FCS reads nothing of the pattern"),
            defer("Some", 1, "-", "FCS reads nothing of the pattern"),
            commit("w", 1, "proj:w:member"),
            commit("Some", 2, SOME),
        ],
    },
];

/// Rows graded in an env where a second assembly auto-opens `Shadow.Implicit`
/// into every file (`tests/fixtures/union_case_autoopen_env`) — the contests
/// the implicit fold settles before a file says anything.
const IMPLICIT: &[Row] = &[
    Row {
        label: "an implicit case another implicit surface also supplies",
        decls: &[],
        src: "module UcI01\n\
              let a = Ok 1\n\
              let f x = match x with Ok y -> y | _ -> 0\n",
        probes: &[
            defer(
                "Ok",
                0,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Alt.Ok:unioncase",
                "two implicit surfaces supply the name, in assembly reference order",
            ),
            defer(
                "Ok",
                1,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Alt.Ok:unioncase",
                "two implicit surfaces supply the name, in assembly reference order",
            ),
        ],
    },
    Row {
        label: "an implicit case only its own assembly supplies",
        decls: &[],
        src: "module UcI02\n\
              let a = Fresh 1\n\
              let f x = match x with Fresh y -> y | _ -> 0\n",
        probes: &[
            commit(
                "Fresh",
                0,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Alt.Fresh:unioncase",
            ),
            commit(
                "Fresh",
                1,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Alt.Fresh:unioncase",
            ),
        ],
    },
    Row {
        label: "two implicit unions of one namespace sharing a case",
        decls: &[],
        src: "module UcI03\n\
              let a = Twice \"s\"\n\
              let f x = match x with Twice _ -> 1 | _ -> 0\n",
        probes: &[
            defer(
                "Twice",
                0,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Second.Twice:unioncase",
                "the fold cannot vouch for the two unions' declaration order",
            ),
            defer(
                "Twice",
                1,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Second.Twice:unioncase",
                "the fold cannot vouch for the two unions' declaration order",
            ),
        ],
    },
    Row {
        label: "an implicit class takes the slot",
        decls: &[],
        src: "module UcI04\n\
              let a = Error \"x\"\n\
              let f (x: Result<int,string>) = match x with Error e -> e | _ -> \"\"\n",
        probes: &[
            defer(
                "Error",
                0,
                "SemaUnionCaseAutoOpenFixture:Shadow.Implicit.Error:ctor",
                "a constructible type takes the bare constructor slot",
            ),
            commit("Error", 1, ERROR),
        ],
    },
    Row {
        label: "FSharp.Core's other cases beside a second implicit surface",
        decls: &[],
        src: "module UcI05\n\
              let a = Some 1\n\
              let f x = match x with Some y -> y | None -> 0\n",
        probes: &[
            commit("Some", 0, SOME),
            commit("Some", 1, SOME),
            commit("None", 0, NONE),
        ],
    },
    Row {
        label: "an earlier file's root-level auto-open module binder",
        decls: &["root_auto_open"],
        src: "module UcI06\n\
              let a = ValueSome 1\n\
              let b = ValueNone\n",
        probes: &[
            defer(
                "ValueSome",
                0,
                "proj:UcRootAuto.ValueSome:member",
                "an earlier file declares a binder of the name",
            ),
            commit("ValueNone", 0, VNONE),
        ],
    },
];

/// The span of the `nth` occurrence of `needle` in `src` that sits at
/// identifier boundaries on both sides. A `.` counts as part of an identifier,
/// so a segment of a dotted path is never counted as a bare occurrence.
fn probe_span(src: &str, needle: &str, nth: usize) -> TextRange {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '.';
    let mut seen = 0;
    let mut from = 0;
    while let Some(i) = src[from..].find(needle) {
        let start = from + i;
        let end = start + needle.len();
        let before_ok = src[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident(c));
        let after_ok = src[end..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            if seen == nth {
                return TextRange::new(
                    u32::try_from(start).unwrap().into(),
                    u32::try_from(end).unwrap().into(),
                );
            }
            seen += 1;
        }
        from = start + 1;
    }
    panic!("occurrence {nth} of {needle:?} not found in {src:?}");
}

/// FCS's record at exactly `span`, if any — ignoring definitions. Two
/// different records at one span would make the probe ambiguous; the table is
/// written so that never happens, and it is asserted rather than assumed.
fn fcs_at(uses: &FileUses, span: TextRange) -> Option<&NormalisedProjectUse> {
    let (start, end) = (usize::from(span.start()), usize::from(span.end()));
    let at: Vec<&NormalisedProjectUse> = uses
        .uses
        .iter()
        .filter(|u| !u.is_from_definition && u.start == start && u.end == end)
        .collect();
    if let Some(first) = at.first() {
        assert!(
            at.iter()
                .all(|u| u.full_name == first.full_name && u.kind == first.kind),
            "FCS reports more than one symbol at {span:?}: {at:?}"
        );
    }
    at.first().copied()
}

/// The pinned rendering of FCS's record.
fn render_fcs(u: Option<&NormalisedProjectUse>) -> String {
    let Some(u) = u else {
        return "-".to_string();
    };
    let kind = if u.is_constructor {
        "ctor"
    } else {
        u.kind.as_deref().unwrap_or("?")
    };
    let origin = match (&u.decl, &u.assembly) {
        (Some(_), _) => "proj",
        (None, Some(assembly)) => assembly.as_str(),
        (None, None) => "?",
    };
    format!("{origin}:{}:{kind}", u.full_name.as_deref().unwrap_or("?"))
}

/// Whether our committed `res` names exactly FCS's record `u`.
fn agrees(
    env: &AssemblyEnv,
    proj: &ResolvedProject,
    paths: &[PathBuf],
    row_idx: usize,
    res: Resolution,
    u: &NormalisedProjectUse,
) -> bool {
    match res {
        Resolution::Local(_) | Resolution::Item(_) => {
            let Some(decl) = &u.decl else {
                return false;
            };
            let (file_idx, range) = match res {
                Resolution::Item(_) => match proj.item_def(res) {
                    Some((i, def)) => (i, def.range),
                    None => return false,
                },
                _ => match proj.file(row_idx).resolved_def(res) {
                    Some(def) => (row_idx, def.range),
                    None => return false,
                },
            };
            paths[file_idx].file_name() == decl.file.file_name()
                && (usize::from(range.start()), usize::from(range.end())) == (decl.start, decl.end)
        }
        Resolution::UnionCase { union, case } => {
            let entity = env.entity(union);
            let Some(path) = &u.declaring_path else {
                return false;
            };
            let structural = StructuralName {
                declaring: DeclaringEntity {
                    namespace: u
                        .declaring_namespace
                        .as_deref()
                        .map(|ns| ns.split('.').map(str::to_string).collect())
                        .unwrap_or_default(),
                    path: path.clone(),
                    is_constructor: false,
                },
                leaf: u.name.clone(),
                leaf_arity: None,
            };
            let ours = format!(
                "{}.{}",
                env.entity_full_name(union),
                env.union_case_name(union, case)
            );
            u.decl.is_none()
                && u.kind.as_deref() == Some("unioncase")
                && u.assembly.as_deref() == Some(entity.assembly.name.as_str())
                && certified_expected(env, res, &structural).as_deref() == Some(ours.as_str())
        }
        Resolution::Entity(_) | Resolution::Member { .. } => {
            let Some(full_name) = &u.full_name else {
                return false;
            };
            let ours = match res {
                Resolution::Entity(h) => {
                    (env.entity(h).assembly.name.clone(), env.entity_full_name(h))
                }
                Resolution::Member { parent, idx } => (
                    env.entity(parent).assembly.name.clone(),
                    format!(
                        "{}.{}",
                        env.entity_full_name(parent),
                        env.member_display_name(parent, idx)
                    ),
                ),
                _ => unreachable!(),
            };
            u.decl.is_none()
                && u.assembly.as_deref() == Some(ours.0.as_str())
                && full_name.as_str() == ours.1
        }
        Resolution::Deferred(_) | Resolution::Unresolved => false,
    }
}

fn env_of(dlls: &[&Path]) -> AssemblyEnv {
    let bytes: Vec<Vec<u8>> = dlls
        .iter()
        .map(|p| std::fs::read(p).unwrap_or_else(|e| panic!("read {p:?}: {e}")))
        .collect();
    let views: Vec<Ecma335Assembly> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).expect("parse assembly"))
        .collect();
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

/// The row's tree. A recovered row is expected to carry parse errors, and is
/// resolved from the tree the parser recovered, as the LSP resolves a buffer
/// mid-edit; every other row must parse cleanly.
fn impl_file(src: &str, recovered: bool) -> ImplFile {
    let parsed = parse(src);
    assert!(
        recovered || parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    ImplFile::cast(parsed.root).expect("impl file")
}

#[test]
fn every_union_case_spelling_agrees_with_fcs() {
    run_table(ROWS, false, ensure_union_case_shadow_fixture_built());
}

/// The same grading over **recovered** input — a file mid-edit, which is the
/// LSP's steady state and which no committed corpus contains. FCS still reports
/// what it bound in an erroring file, so certain-implies-exact holds against it
/// exactly as for a clean row; only the clean-check requirement is lifted.
#[test]
fn every_recovered_union_case_spelling_agrees_with_fcs() {
    run_table(RECOVERED, true, ensure_union_case_shadow_fixture_built());
}

/// The same grading with a second assembly auto-opening a namespace into every
/// file beside FSharp.Core's ([`IMPLICIT`]): the implicit fold's own contests,
/// which no source `open` can reach and which every file of such a project
/// meets.
#[test]
fn every_implicitly_contested_union_case_agrees_with_fcs() {
    run_table(IMPLICIT, false, ensure_union_case_autoopen_fixture_built());
}

/// Grade `rows` against FCS, with `fixture` referenced beside FSharp.Core on
/// both sides.
fn run_table(rows: &[Row], recovered: bool, fixture: &Path) {
    let fsharp_core = ensure_fsharp_core_dll();
    let env = env_of(&[fsharp_core.as_path(), fixture]);

    // One FCS project: every decl file first, then every row. Rows are
    // isolated by their module / namespace names.
    // Only the decls this table's rows name join its FCS project: a decl can
    // reach every later file of it (a root-level auto-open module does).
    let decl_paths: Vec<(&str, PathBuf, &str)> = DECLS
        .iter()
        .filter(|(label, _)| rows.iter().any(|row| row.decls.contains(label)))
        .map(|(label, src)| (*label, temp_fs_file("uc_table_decl", src), *src))
        .collect();
    let row_paths: Vec<PathBuf> = rows
        .iter()
        .map(|row| temp_fs_file("uc_table_row", row.src))
        .collect();
    let mut sources: Vec<(PathBuf, String)> = decl_paths
        .iter()
        .map(|(_, p, s)| (p.clone(), (*s).to_string()))
        .collect();
    sources.extend(
        rows.iter()
            .zip(&row_paths)
            .map(|(row, p)| (p.clone(), row.src.to_string())),
    );
    let paths: Vec<&Path> = sources.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
    let fcs = parse_fcs_uses_project(&json, &sources);
    for (p, _) in &sources {
        let _ = std::fs::remove_file(p);
    }

    let mut failures: Vec<String> = Vec::new();
    let mut probes = 0;
    for (row, row_path) in rows.iter().zip(&row_paths) {
        let fu = fcs
            .iter()
            .find(|f| f.path.file_name() == row_path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for row {:?}", row.label));
        if fu.errors.is_empty() == recovered {
            failures.push(format!(
                "  {}: a {} row, but FCS reports errors {:?}",
                row.label,
                if recovered { "recovered" } else { "clean" },
                fu.errors
            ));
            continue;
        }
        // Our side: the row's decls, in order, then the row.
        let mut files: Vec<ImplFile> = Vec::new();
        let mut file_paths: Vec<PathBuf> = Vec::new();
        for label in row.decls {
            let (_, p, s) = decl_paths
                .iter()
                .find(|(l, _, _)| l == label)
                .unwrap_or_else(|| panic!("unknown decl {label:?}"));
            files.push(impl_file(s, false));
            file_paths.push(p.clone());
        }
        files.push(impl_file(row.src, recovered));
        file_paths.push(row_path.clone());
        let row_idx = files.len() - 1;
        let proj = resolve_project(&files, &env);
        let rf = proj.file(row_idx);

        for probe in row.probes {
            probes += 1;
            let span = probe_span(row.src, probe.at, probe.nth);
            let theirs = fcs_at(fu, span);
            let rendered = render_fcs(theirs);
            let ours = rf.resolution_at(span);
            let committed =
                ours.filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved));
            let at = format!("{}: {:?} #{}", row.label, probe.at, probe.nth);
            if rendered != probe.fcs {
                failures.push(format!(
                    "  {at} [FCS PIN]\n    pinned: {}\n    FCS:    {rendered}",
                    probe.fcs
                ));
            }
            if let Some(res) = committed {
                let ok = theirs.is_some_and(|u| agrees(&env, &proj, &file_paths, row_idx, res, u));
                if !ok {
                    failures.push(format!(
                        "  {at} [WRONG]\n    FCS:  {rendered}\n    ours: {res:?}"
                    ));
                    continue;
                }
            }
            match (probe.ours, committed) {
                (Ours::Commit, None) => failures.push(format!(
                    "  {at} [MUST COMMIT]\n    FCS:  {rendered}\n    ours: {ours:?}"
                )),
                (Ours::Defer(why), Some(res)) => failures.push(format!(
                    "  {at} [PINNED DEFERRAL: {why}]\n    now commits {res:?} — if that is \
                     right, make the probe a commit"
                )),
                (Ours::Commit, Some(_)) | (Ours::Defer(_), None) => {}
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {probes} union-case probes disagree:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The generated grid
// ---------------------------------------------------------------------------

/// A same-file declaration that may take an implicitly opened case's name.
#[derive(Clone, Copy, Debug)]
enum Shadower {
    /// Nothing: the implicit case is the only reading.
    Nothing,
    /// A union with a case of the name.
    UnionCase,
    /// An `[<RequireQualifiedAccess>]` union with a case of the name — it
    /// shadows nothing bare.
    RqaUnionCase,
    /// An exception of the name.
    Exception,
    /// A class of the name: takes the bare constructor slot.
    Class,
    /// A struct of the name: takes the slot too.
    Struct,
    /// A record of the name: takes no slot.
    Record,
    /// An interface of the name: takes no slot.
    Interface,
    /// An abbreviation of the name.
    Abbreviation,
    /// A partial active pattern whose case is the name — the pattern namespace
    /// only.
    ActivePattern,
    /// An `[<AutoOpen>]` module holding a union with a case of the name.
    AutoOpenUnion,
    /// An `[<AutoOpen>]` module holding an exception of the name.
    AutoOpenException,
    /// An `[<AutoOpen>]` module holding an active pattern whose case is the
    /// name.
    AutoOpenActivePattern,
    /// An `[<AutoOpen>]` module that only *uses* the name, declaring nothing
    /// of it.
    AutoOpenUse,
    /// An `extern` prototype of the name — a value binder the walk never
    /// interns.
    Extern,
    /// An `[<AutoOpen>]` type abbreviation, whose borrowed statics are not
    /// spelled anywhere in the file.
    AutoOpenAbbreviation,
    /// A function whose head is spelled like the case (`let Some x = …`),
    /// which FCS reads as a function definition.
    LetFunction,
}

impl Shadower {
    const ALL: [Shadower; 17] = [
        Shadower::Nothing,
        Shadower::UnionCase,
        Shadower::RqaUnionCase,
        Shadower::Exception,
        Shadower::Class,
        Shadower::Struct,
        Shadower::Record,
        Shadower::Interface,
        Shadower::Abbreviation,
        Shadower::ActivePattern,
        Shadower::AutoOpenUnion,
        Shadower::AutoOpenException,
        Shadower::AutoOpenActivePattern,
        Shadower::AutoOpenUse,
        Shadower::Extern,
        Shadower::AutoOpenAbbreviation,
        Shadower::LetFunction,
    ];

    /// The declaration's lines, unindented.
    fn decl(self, name: &str) -> Vec<String> {
        match self {
            Shadower::Nothing => vec![],
            Shadower::UnionCase => vec![format!("type GridU = {name} of int | GridOther")],
            Shadower::RqaUnionCase => vec![
                "[<RequireQualifiedAccess>]".to_string(),
                format!("type GridU = {name} of int | GridOther"),
            ],
            Shadower::Exception => vec![format!("exception {name} of int")],
            Shadower::Class => vec![format!("type {name}(x: int) = member _.X = x")],
            Shadower::Struct => vec![
                "[<Struct>]".to_string(),
                format!("type {name} = val V: int"),
            ],
            Shadower::Record => vec![format!("type {name} = {{ V: int }}")],
            Shadower::Interface => vec![format!("type {name} = abstract M: int")],
            Shadower::Abbreviation => vec![format!("type {name} = int")],
            Shadower::ActivePattern => vec![format!(
                "let (|{name}|_|) (x: int) = if x > 0 then Option.Some x else Option.None"
            )],
            Shadower::AutoOpenUnion => vec![
                "[<AutoOpen>]".to_string(),
                "module GridAuto =".to_string(),
                format!("    type GridU = {name} of int | GridOther"),
            ],
            Shadower::AutoOpenException => vec![
                "[<AutoOpen>]".to_string(),
                "module GridAuto =".to_string(),
                format!("    exception {name} of int"),
            ],
            Shadower::AutoOpenActivePattern => vec![
                "[<AutoOpen>]".to_string(),
                "module GridAuto =".to_string(),
                format!(
                    "    let (|{name}|_|) (x: int) = if x > 0 then Option.Some x else Option.None"
                ),
            ],
            Shadower::Extern => vec![
                "[<System.Runtime.InteropServices.DllImport(\"probe\")>]".to_string(),
                format!("extern int {name}(int x)"),
            ],
            Shadower::LetFunction => vec![format!("let {name} (x: int) = x + 1")],
            Shadower::AutoOpenAbbreviation => vec![
                "[<AutoOpen>]".to_string(),
                "type GridHelpers = System.Object".to_string(),
            ],
            Shadower::AutoOpenUse => vec![
                "[<AutoOpen>]".to_string(),
                "module GridAuto =".to_string(),
                format!("    let gridUse (x: obj) = match x with :? int -> {name} | _ -> {name}"),
            ],
        }
    }
}

/// Where the shadower sits relative to the use.
#[derive(Clone, Copy, Debug)]
enum Placement {
    /// Declared before the use, in the same module.
    Before,
    /// Declared after the use.
    After,
    /// Inside a nested module that closes before the use.
    NestedClosed,
    /// Inside a nested module that the file then opens.
    NestedOpened,
    /// In a `module rec` file, after the use.
    RecAfter,
}

impl Placement {
    const ALL: [Placement; 5] = [
        Placement::Before,
        Placement::After,
        Placement::NestedClosed,
        Placement::NestedOpened,
        Placement::RecAfter,
    ];
}

/// How the probe uses the name.
#[derive(Clone, Copy, Debug)]
enum Use {
    /// `let probe = Name 1`.
    Applied,
    /// `List.map Name [1]` — the name as a function value.
    FunctionValue,
    /// `match x with Name _ -> …` — an applied pattern head.
    AppliedPattern,
    /// `match x with Name -> …` — a bare pattern head.
    BarePattern,
    /// `type Probe (Name: int) = …` — a primary constructor's parameter,
    /// which FCS compiles to an anonymous `_arg1` without reading the name.
    CtorParam,
    /// `type Probe (x: int, Name as y) = …` — the same as a later tupled
    /// parameter, aliased.
    CtorParamAs,
    /// `type Probe (Name x) = …` — a pattern a primary constructor rejects,
    /// reading nothing of it.
    CtorParamApplied,
}

impl Use {
    const ALL: [Use; 7] = [
        Use::Applied,
        Use::FunctionValue,
        Use::AppliedPattern,
        Use::BarePattern,
        Use::CtorParam,
        Use::CtorParamAs,
        Use::CtorParamApplied,
    ];

    /// The line holding the probe, and where in it the name starts.
    fn line(self, name: &str) -> (String, usize) {
        let (prefix, suffix) = match self {
            Use::Applied => ("let probe = ", " 1"),
            Use::FunctionValue => ("let probe = List.map ", " [ 1 ]"),
            Use::AppliedPattern => ("let probe x = match x with ", " _ -> 1 | _ -> 0"),
            Use::BarePattern => ("let probe x = match x with ", " -> 1 | _ -> 0"),
            Use::CtorParam => ("type Probe (", ": int) = member _.V = 0"),
            Use::CtorParamAs => ("type Probe (x: int, ", " as y) = member _.V = y"),
            Use::CtorParamApplied => ("type Probe (", " x) = member _.V = 0"),
        };
        (format!("{prefix}{name}{suffix}"), prefix.len())
    }
}

/// One generated cell: its source and the probe's span.
struct GridCell {
    label: String,
    src: String,
    probe: TextRange,
    /// No shadower anywhere: the implicit case is the only reading, so we must
    /// commit to it.
    must_commit: bool,
}

fn grid_cell(
    index: usize,
    name: &str,
    shadower: Shadower,
    placement: Placement,
    use_: Use,
) -> GridCell {
    let module = format!("UcGrid{index:04}");
    let header = match placement {
        Placement::RecAfter => format!("module rec {module}\n"),
        _ => format!("module {module}\n"),
    };
    let decl = shadower.decl(name);
    let nested = |lines: &[String]| -> Vec<String> {
        std::iter::once("module GridInner =".to_string())
            .chain(lines.iter().map(|l| format!("    {l}")))
            // A nested module needs a body even with no shadower in it.
            .chain(std::iter::once("    let gridFiller = 0".to_string()))
            .collect()
    };
    let (before, after): (Vec<String>, Vec<String>) = match placement {
        Placement::Before => (decl, vec![]),
        Placement::After | Placement::RecAfter => (vec![], decl),
        Placement::NestedClosed => (nested(&decl), vec![]),
        Placement::NestedOpened => {
            let mut lines = nested(&decl);
            lines.push("open GridInner".to_string());
            (lines, vec![])
        }
    };
    let (line, offset) = use_.line(name);
    let mut src = header;
    for l in &before {
        src.push_str(l);
        src.push('\n');
    }
    let start = src.len() + offset;
    src.push_str(&line);
    src.push('\n');
    for l in &after {
        src.push_str(l);
        src.push('\n');
    }
    let probe = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + name.len()).unwrap().into(),
    );
    GridCell {
        label: format!("{name} / {shadower:?} / {placement:?} / {use_:?}"),
        src,
        probe,
        must_commit: matches!(shadower, Shadower::Nothing),
    }
}

/// Every shadowing declaration F# offers, crossed with every placement and
/// every way of using the name, for each of FSharp.Core's implicitly opened
/// case shapes (a field-carrying case and a nullary one, of a one-parameter
/// and a two-parameter union). Each cell is graded certain-implies-exact
/// against FCS; a cell with no shadower must commit; and the grid as a whole
/// must commit somewhere it is shadowed, or it has stopped exercising the
/// guards it exists for.
///
/// Erroring cells are graded too — `None 1` is a type error, and so is a use
/// whose shadower takes an argument of another type — because FCS still
/// reports what it bound, and an editor sends exactly such text.
#[test]
fn the_shadowing_grid_agrees_with_fcs() {
    let fsharp_core = ensure_fsharp_core_dll();
    let env = env_of(&[fsharp_core.as_path()]);
    let mut cells = Vec::new();
    for name in ["Some", "None", "Ok", "Error"] {
        for shadower in Shadower::ALL {
            for placement in Placement::ALL {
                for use_ in Use::ALL {
                    cells.push(grid_cell(cells.len(), name, shadower, placement, use_));
                }
            }
        }
    }
    let sources: Vec<(PathBuf, String)> = cells
        .iter()
        .map(|c| (temp_fs_file("uc_grid", &c.src), c.src.clone()))
        .collect();
    let paths: Vec<&Path> = sources.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[]);
    let fcs = parse_fcs_uses_project(&json, &sources);
    for (p, _) in &sources {
        let _ = std::fs::remove_file(p);
    }

    let mut failures = Vec::new();
    let mut shadowed_commits = 0;
    let mut commits = 0;
    for (cell, (path, _)) in cells.iter().zip(&sources) {
        let fu = fcs
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for {}", cell.label));
        let parsed = parse(&cell.src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let proj = resolve_project(std::slice::from_ref(&file), &env);
        let ours = proj
            .file(0)
            .resolution_at(cell.probe)
            .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved));
        let theirs = fcs_at(fu, cell.probe);
        if let Some(res) = ours {
            commits += 1;
            if !cell.must_commit {
                shadowed_commits += 1;
            }
            let paths = vec![path.clone()];
            if !theirs.is_some_and(|u| agrees(&env, &proj, &paths, 0, res, u)) {
                failures.push(format!(
                    "  {} [WRONG]\n    FCS:  {}\n    ours: {res:?}\n    source: {:?}",
                    cell.label,
                    render_fcs(theirs),
                    cell.src
                ));
            }
        } else if cell.must_commit && theirs.is_some() {
            failures.push(format!(
                "  {} [MUST COMMIT]\n    FCS:  {}\n    source: {:?}",
                cell.label,
                render_fcs(theirs),
                cell.src
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} grid cells disagree:\n{}",
        failures.len(),
        cells.len(),
        failures.join("\n")
    );
    // Non-vacuity: the unshadowed cells are 140 of the 2380, and a grid whose
    // shadowed cells all decline would pass the property above while
    // exercising none of the guards that decide when a shadower does not, in
    // fact, shadow.
    assert!(
        shadowed_commits >= 400,
        "only {shadowed_commits} of {commits} commits are in shadowed cells"
    );
    eprintln!(
        "{commits} of {} grid cells commit, {shadowed_commits} of them shadowed",
        cells.len()
    );
}

/// A namespace F# opens without an `open` whose projection dropped a type may
/// hide a union case of any name, even when no type of it survives: the root
/// namespace of every referenced assembly here, which the fixtures leave
/// empty. Then no implicitly opened case can be committed, in either
/// position: a pattern is not protected by the expression slot's guard.
#[test]
fn a_dropped_type_in_an_empty_implicit_namespace_withholds_every_implicit_case() {
    let fsharp_core = ensure_fsharp_core_dll();
    let mut env = env_of(&[fsharp_core.as_path()]);
    env.mark_namespace_dropped_type(Vec::new());
    let src = "module UcDropped\nlet a = Some 1\nlet f x = match x with Some y -> y | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src, false)], &env);
    for nth in 0..2 {
        let span = probe_span(src, "Some", nth);
        let ours = proj.file(0).resolution_at(span);
        assert!(
            ours.is_none_or(|r| matches!(r, Resolution::Deferred(_))),
            "`Some` #{nth} committed {ours:?} past a dropped root-namespace type"
        );
    }
}
