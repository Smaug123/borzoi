//! The **auto-open member sweep**: every ordered pair of members a fold-back
//! fragment can declare, all contributing the *same* name, checked against FCS
//! by **in-file binder identity**.
//!
//! ## Why this is not a [`fold_matrix`](crate::common::fold_matrix) grid
//!
//! That harness's currency is *assembly reach*: a probe resolving into a
//! fixture assembly on both sides, or into neither. A resolution to the probe
//! file's own binder renders as `None` there — and so does a **deferral**. So a
//! cell where the fold should take a name back but we decline it instead is
//! `None == None`: agreement, unmeasured. Every ordering defect this fold has
//! shipped lives in exactly that blind spot.
//!
//! The currency here is `resolve_diff`'s instead: FCS reports the *declaration
//! range* of the binder each use resolves to, so a probe discriminates three
//! outcomes rather than two — the fold's member, the enclosing scope's earlier
//! same-named binding, or nothing. A deferral is no longer indistinguishable
//! from a correct answer.
//!
//! Each cell therefore writes an enclosing `let N = 99` **before** the
//! `[<AutoOpen>]` module. It is what makes the grid sharp: when the fold
//! under-reaches we bind that earlier `N` (a wrong range, not a silence), and
//! when it over-reaches — a `private` member that must not escape its module —
//! we bind the fold's member where FCS keeps the earlier one.
//!
//! ## The space
//!
//! [`Member`] × `private` is how a fragment can contribute one name; the grid
//! is every such shape alone, and every *ordered pair* of them. Order is the
//! dimension under test: within one module FCS's own source order decides which
//! member owns the name, so `(case, extern)` and `(extern, case)` are different
//! questions and the fold must answer both.
//!
//! [`KNOWN_GAPS`] is the certain-implies-exact ratchet: an entry claims exactly
//! "FCS resolves the probe in-file and we name nothing". Naming a target, or
//! FCS falling silent, fails the entry rather than passing it.
//!
//! ## The channel
//!
//! A fragment's members reach a use through more than the fold-back: an
//! explicit `open` of the module that holds it folds it too, and so does an
//! `open` of its namespace from a later file. Each [`Channel`] runs the same
//! machinery from a different caller, with its own source of members (this
//! file's items, or an earlier file's exports) and its own eviction ordering,
//! so the grid crosses every cell with every channel. The fold-back's gaps are
//! [`KNOWN_GAPS`]; the other channels' are the exact manifest
//! `tests/manifests/auto_open_member_sweep_channels.txt`, on the same terms.
//! Two channels that fold an *earlier file's* members are not gated in the
//! pair grid: they disagree with FCS on `main` too (see
//! `the_namespace_fold_agrees_with_fcs_over_every_member_pair`). Two more,
//! [`Channel::Inline`] and [`Channel::OpenPlain`], are no-fold **controls**:
//! the members written straight into the block, or `open`ed from a plain
//! module, so a defect they share is the shape's own and not the fold's.
//!
//! ## What follows the fold
//!
//! The pair grid asks how a fold orders its own members. The follower grid
//! ([`Follower`]) asks what happens *after* it: the block goes on to declare
//! something that binds the same name, and FCS hands the name to the later
//! declaration wherever its environment says so. A later declaration that
//! sema pushes as a scope entry shadows the fold by position for free; the
//! ones it does not — a constructible type, an `[<AutoOpen>]` type's statics,
//! a bar-less union case, a `rec` block's forward visibility — are where a
//! folded member used to stand after FCS had moved on. The grid is a product,
//! [`Decl`] × [`Place`], crossed with every channel, so a route is a row
//! rather than a reviewer's afterthought.
//!
//! ## Every cell is a program FCS accepts
//!
//! A cell FCS rejects would grade its error recovery, which is no
//! specification of name resolution. The sweep reads FCS's diagnostics and
//! fails on any error except a probe's own `FS0039` or `FS0800`, whose answer —
//! nothing in value scope — is checked like any other.

use std::collections::BTreeSet;
use std::path::PathBuf;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{
    ProjectItems, Resolution, ResolvedFile, ResolvedProject, SyntaxRecovery, resolve_file,
    resolve_project,
};
use rowan::{TextRange, TextSize};

use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};

/// The name every member in the grid contributes, and every probe spells.
const NAME: &str = "Nn";

/// How a fold-back fragment contributes [`NAME`].
///
/// Each variant is a distinct *enumerability* class for the fold, which is what
/// makes it worth a row: [`Member::Value`] is pushed by `open_module_values`,
/// [`Member::ActivePattern`] and [`Member::Extern`] cannot be pointed at and are
/// declined by name, [`Member::TypeCtor`] evicts a same-named value from FCS's
/// unqualified slot, and [`Member::NestedAuto`] arrives through a second fold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Member {
    /// `let Nn = 1` — the plainly enumerable case.
    Value,
    /// `type NnHolder = | Nn` — the constructor namespace, enumerable.
    UnionCase,
    /// `exception Nn` — the constructor namespace via an exception. Field-less,
    /// so the pattern probe `| Nn ->` is well-typed against it.
    Exception,
    /// `let (|Nn|_|) (x: int) = …` — a module-level active-pattern case, with
    /// no result to bind, so the pattern probe needs no argument. Not a
    /// value at all to FCS, and not interned as a binder by sema.
    ActivePattern,
    /// `extern int Nn()` — a value-namespace name sema does not intern.
    Extern,
    /// `type Nn() = class end` — a constructible type, which takes FCS's
    /// unqualified slot from a same-named value.
    TypeCtor,
    /// An `[<AutoOpen>]` submodule holding `let Nn`, so the name arrives through
    /// two folds rather than one.
    NestedAuto,
    /// An `[<AutoOpen>]` **type** holding `static member Nn`. Its statics fold
    /// into the enclosing frame like a module's values, but sema names none of
    /// them (task #48), so every cell it wins is a decline for us. It ranks
    /// with the tycon tier, which is why it is ordered against a union case by
    /// source position but beats an exception from either side.
    AutoOpenStatic,
    /// `type NnBar = Nn` — a bar-less single-case union, spelled exactly like
    /// an abbreviation. FCS reads it as a union whose case is `Nn` exactly when
    /// no type `Nn` is in scope there (`TyconCoreAbbrevThatIsReallyAUnion`), so
    /// whether it contributes the name at all depends on its neighbours.
    BarlessUnion,
    /// `[<Literal>] let Nn = 1` — a value that is also a constant pattern, so
    /// it contests the pattern namespace as a case does (FCS's `ePatItems`).
    Literal,
    /// `[<RequireQualifiedAccess>] type NnRqa = | Nn` — a case reached only as
    /// `NnRqa.Nn`, so it contributes nothing bare.
    RqaUnionCase,
    /// `[<RequireQualifiedAccess>] type NnRqaBar = Nn` — the bar-less twin.
    RqaBarlessUnion,
    /// A submodule holding `let Nn`, marked by an abbreviation of
    /// `AutoOpenAttribute` under another name — which FCS auto-opens as surely
    /// as the spelling, since it chases the abbreviation.
    AliasNestedAuto,
}

/// What a [`Member`] *declares* under [`NAME`], as opposed to what it
/// contributes to the enclosing scope. Two members of the same slot in one
/// module are `FS0037 Duplicate definition`, and an ill-formed program's
/// error recovery is not a specification of name resolution — so the grid
/// omits those pairs rather than gating on FCS's choice among them. Probed:
/// every same-slot pair errors and every cross-slot pair does not.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A module-level value binding called `Nn`.
    Value,
    /// A type (or exception, which is one) called `Nn`.
    TypeName,
    /// The value `(|Nn|_|)`.
    Recognizer,
    /// Nothing called `Nn` at this module's level: a union case belongs to its
    /// own holder type, and a nested module's `let` to that module.
    Indirect,
}

impl Member {
    const ALL: &'static [Member] = &[
        Member::Value,
        Member::UnionCase,
        Member::Exception,
        Member::ActivePattern,
        Member::Extern,
        Member::TypeCtor,
        Member::NestedAuto,
        Member::AutoOpenStatic,
        Member::BarlessUnion,
        Member::Literal,
        Member::RqaUnionCase,
        Member::RqaBarlessUnion,
        Member::AliasNestedAuto,
    ];

    fn slot(self) -> Slot {
        match self {
            Member::Value | Member::Extern | Member::Literal => Slot::Value,
            Member::Exception | Member::TypeCtor => Slot::TypeName,
            Member::ActivePattern => Slot::Recognizer,
            Member::UnionCase
            | Member::NestedAuto
            | Member::AutoOpenStatic
            | Member::BarlessUnion
            | Member::RqaUnionCase
            | Member::RqaBarlessUnion
            | Member::AliasNestedAuto => Slot::Indirect,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Member::Value => "value",
            Member::UnionCase => "case",
            Member::Exception => "exn",
            Member::ActivePattern => "ap",
            Member::Extern => "extern",
            Member::TypeCtor => "type",
            Member::NestedAuto => "nested-auto",
            Member::AutoOpenStatic => "auto-open-static",
            Member::BarlessUnion => "barless-case",
            Member::Literal => "literal",
            Member::RqaUnionCase => "rqa-case",
            Member::RqaBarlessUnion => "rqa-barless-case",
            Member::AliasNestedAuto => "alias-nested-auto",
        }
    }

    /// The declaration lines contributing [`NAME`], at zero indent. `uniq`
    /// disambiguates the helper names a cell needs twice when it declares the
    /// same kind in both positions.
    fn lines(self, private: bool, uniq: usize) -> Vec<String> {
        // Where the accessibility modifier goes differs per construct, which is
        // half the point of the dimension: `extern` carries it between the
        // return type and the name (`pars.fsy`'s `opt_access`), a union case
        // can only be made private through its type, and a nested auto-open
        // module through the module header.
        let acc = if private { "private " } else { "" };
        match self {
            Member::Value => vec![format!("let {acc}{NAME} = 1")],
            Member::UnionCase => vec![
                format!("type {acc}NnHolder{uniq} ="),
                format!("    | {NAME}"),
            ],
            Member::Exception => vec![format!("exception {acc}{NAME}")],
            Member::ActivePattern => vec![format!(
                "let {acc}(|{NAME}|_|) (x: int) = if x > 0 then Some () else None"
            )],
            Member::Extern => vec![format!("extern int {acc}{NAME}()")],
            Member::TypeCtor => vec![format!("type {acc}{NAME}() = class end")],
            Member::NestedAuto => vec![
                "[<AutoOpen>]".to_string(),
                format!("module {acc}NnInner{uniq} ="),
                format!("    let {NAME} = 2"),
            ],
            Member::AutoOpenStatic => vec![
                "[<AutoOpen>]".to_string(),
                format!("type {acc}NnHost{uniq} ="),
                format!("    static member {NAME} = 3"),
            ],
            Member::BarlessUnion => vec![format!("type {acc}NnBar{uniq} = {NAME}")],
            Member::Literal => vec!["[<Literal>]".to_string(), format!("let {acc}{NAME} = 1")],
            Member::RqaUnionCase => vec![
                "[<RequireQualifiedAccess>]".to_string(),
                format!("type {acc}NnRqa{uniq} ="),
                format!("    | {NAME}"),
            ],
            Member::RqaBarlessUnion => vec![
                "[<RequireQualifiedAccess>]".to_string(),
                format!("type {acc}NnRqaBar{uniq} = {NAME}"),
            ],
            Member::AliasNestedAuto => vec![
                format!("type NnOpen{uniq}Attribute = Microsoft.FSharp.Core.AutoOpenAttribute"),
                format!("[<NnOpen{uniq}>]"),
                format!("module {acc}NnAlias{uniq} ="),
                format!("    let {NAME} = 2"),
            ],
        }
    }
}

/// A declaration the enclosing block writes **after** the fold that binds
/// [`NAME`] in the value or pattern namespace — one row per route by which
/// FCS's name environment can re-bind such a name: `AddValRefsToNameEnv` (a
/// `let`, an `extern`), the union-case and exception tiers of
/// `AddTyconRefsToNameEnv` (a case, a bar-less case, an exception), an
/// active-pattern tag, a constructible type's unqualified slot, and an
/// `[<AutoOpen>]` type's statics.
///
/// The fold pushes its members as scope entries at its own position, so a
/// later declaration that is pushed the same way shadows it by position for
/// free. One that is not — a type's constructor slot, an auto-open type's
/// statics, a case spelled like an abbreviation — is where a folded member
/// can stand after FCS has moved on, and every such route needs a row here
/// rather than a reviewer who thinks of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decl {
    /// `let Nn = 3`.
    Let,
    /// `let Nn (x: int) = x` — a function binding, whose parameter is not a
    /// name of the module.
    LetFunction,
    /// `[<Literal>] let Nn = 3` — a constant pattern as well as a value.
    Literal,
    /// `type NnFU = | Nn`.
    Case,
    /// `type NnFB = Nn`.
    Barless,
    /// `[<RequireQualifiedAccess>] type NnFR = Nn` — a case that binds nothing
    /// bare.
    RqaBarless,
    /// `exception Nn`.
    Exception,
    /// `let (|Nn|_|) …` — the pattern namespace only.
    ActivePattern,
    /// `extern int Nn()`.
    Extern,
    /// `type Nn() = class end` — the constructor slot.
    TypeCtor,
    /// `type Nn = NnClass` over a class — an abbreviation takes the slot when
    /// its target does.
    AbbrevOfClass,
    /// `type Nn = NnUnion` over a union, which leaves the slot alone.
    AbbrevOfUnion,
    /// `[<AutoOpen>] type T = static member Nn`.
    AutoOpenStatic,
    /// `[<AutoOpen>] module NnAo = let Nn = 5` — a second fold, which inside a
    /// `rec` block is forward-visible like any other declaration.
    AutoOpenModule,
    /// `[<AutoOpen>] type E = | Nn = 2` — an enum's fields fold like statics
    /// (FCS warns FS0842 and folds them anyway).
    AutoOpenEnum,
    /// `[<AutoOpen>] exception NnEx with static member Nn = 2` — an
    /// exception's augmentation folds like a type's statics.
    AutoOpenException,
    /// [`Self::AutoOpenStatic`] marked by an abbreviation of
    /// `AutoOpenAttribute` under another name, which FCS chases.
    AliasAutoOpenStatic,
    /// [`Self::AutoOpenModule`] marked the same way.
    AliasAutoOpenModule,
    /// Nothing: the placement alone, which the fold must reach unchanged.
    Nothing,
}

impl Decl {
    const ALL: &'static [Decl] = &[
        Decl::Let,
        Decl::LetFunction,
        Decl::Literal,
        Decl::Case,
        Decl::Barless,
        Decl::RqaBarless,
        Decl::Exception,
        Decl::ActivePattern,
        Decl::Extern,
        Decl::TypeCtor,
        Decl::AbbrevOfClass,
        Decl::AbbrevOfUnion,
        Decl::AutoOpenStatic,
        Decl::AutoOpenModule,
        Decl::AutoOpenEnum,
        Decl::AutoOpenException,
        Decl::AliasAutoOpenStatic,
        Decl::AliasAutoOpenModule,
        Decl::Nothing,
    ];

    fn tag(self) -> &'static str {
        match self {
            Decl::Let => "let",
            Decl::LetFunction => "let-fn",
            Decl::Literal => "literal",
            Decl::Case => "case",
            Decl::Barless => "barless-case",
            Decl::RqaBarless => "rqa-barless-case",
            Decl::Exception => "exn",
            Decl::ActivePattern => "ap",
            Decl::Extern => "extern",
            Decl::TypeCtor => "type",
            Decl::AbbrevOfClass => "abbrev-of-class",
            Decl::AbbrevOfUnion => "abbrev-of-union",
            Decl::AutoOpenStatic => "auto-open-static",
            Decl::AutoOpenModule => "auto-open-module",
            Decl::AutoOpenEnum => "auto-open-enum",
            Decl::AutoOpenException => "auto-open-exn",
            Decl::AliasAutoOpenStatic => "alias-auto-open-static",
            Decl::AliasAutoOpenModule => "alias-auto-open-module",
            Decl::Nothing => "nothing",
        }
    }

    /// What the declaration names [`NAME`] as at its own module's level, for
    /// the same duplicate-definition screen [`Slot`] applies to the pair grid.
    fn slot(self) -> Slot {
        match self {
            Decl::Let | Decl::LetFunction | Decl::Literal | Decl::Extern => Slot::Value,
            Decl::Exception | Decl::TypeCtor | Decl::AbbrevOfClass | Decl::AbbrevOfUnion => {
                Slot::TypeName
            }
            Decl::ActivePattern => Slot::Recognizer,
            Decl::Case
            | Decl::Barless
            | Decl::RqaBarless
            | Decl::AutoOpenStatic
            | Decl::AutoOpenModule
            | Decl::AutoOpenEnum
            | Decl::AutoOpenException
            | Decl::AliasAutoOpenStatic
            | Decl::AliasAutoOpenModule
            | Decl::Nothing => Slot::Indirect,
        }
    }

    /// The declaration's lines at zero indent; `n` keeps helper names unique.
    fn lines(self, n: usize) -> Vec<String> {
        match self {
            Decl::Let => vec![format!("let {NAME} = 3")],
            Decl::LetFunction => vec![format!("let {NAME} (x: int) = x")],
            Decl::Literal => vec!["[<Literal>]".to_string(), format!("let {NAME} = 3")],
            Decl::Case => vec![format!("type NnFU{n} ="), format!("    | {NAME}")],
            Decl::Barless => vec![format!("type NnFB{n} = {NAME}")],
            Decl::RqaBarless => vec![
                "[<RequireQualifiedAccess>]".to_string(),
                format!("type NnFR{n} = {NAME}"),
            ],
            Decl::Exception => vec![format!("exception {NAME}")],
            Decl::ActivePattern => vec![format!(
                "let (|{NAME}|_|) (x: int) = if x > 0 then Some () else None"
            )],
            Decl::Extern => vec![format!("extern int {NAME}()")],
            Decl::TypeCtor => vec![format!("type {NAME}() = class end")],
            Decl::AbbrevOfClass => vec![
                format!("type NnTc{n}() = class end"),
                format!("type {NAME} = NnTc{n}"),
            ],
            Decl::AbbrevOfUnion => vec![
                format!("type NnTu{n} = | NnTuc{n}"),
                format!("type {NAME} = NnTu{n}"),
            ],
            Decl::AutoOpenStatic => vec![
                "[<AutoOpen>]".to_string(),
                format!("type NnFS{n} ="),
                format!("    static member {NAME} = 4"),
            ],
            Decl::AutoOpenModule => vec![
                "[<AutoOpen>]".to_string(),
                format!("module NnAo{n} ="),
                format!("    let {NAME} = 5"),
            ],
            Decl::AutoOpenEnum => vec![
                "[<AutoOpen>]".to_string(),
                format!("type NnEn{n} ="),
                format!("    | {NAME} = 2"),
            ],
            Decl::AutoOpenException => vec![
                "[<AutoOpen>]".to_string(),
                format!("exception NnEx{n}"),
                format!("    with static member {NAME} = 2"),
            ],
            Decl::AliasAutoOpenStatic => vec![
                format!("type NnOpenS{n}Attribute = Microsoft.FSharp.Core.AutoOpenAttribute"),
                format!("[<NnOpenS{n}>]"),
                format!("type NnFSA{n} ="),
                format!("    static member {NAME} = 4"),
            ],
            Decl::AliasAutoOpenModule => vec![
                format!("type NnOpenM{n}Attribute = Microsoft.FSharp.Core.AutoOpenAttribute"),
                format!("[<NnOpenM{n}>]"),
                format!("module NnAoA{n} ="),
                format!("    let {NAME} = 5"),
            ],
            Decl::Nothing => Vec::new(),
        }
    }
}

/// Where a [`Decl`] sits relative to the fold and the probes. Each is a
/// different *scope route*: the decl's own block, a nested module that
/// inherits the fold, a later container whose surface folds or is opened into
/// the block, and a `rec` block, inside which every declaration is visible to
/// every other and position orders nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    /// The decl in the fold's own block, the probes below it.
    Top,
    /// `module After =` holding the decl, probed inside it.
    Nested,
    /// `[<AutoOpen>] module Later =` holding the decl, probed below it.
    AutoOpenModule,
    /// `module Other =` holding the decl, then `open Other`, probed below.
    Opened,
    /// `module rec AfterRec =` probed **before** its own decl.
    RecForward,
    /// `module rec AfterRec =` probed after its own decl.
    RecBackward,
    /// Probed inside the members of a type `Host` that an `and` joins to the
    /// decl's own type — which is therefore in scope in `Host`'s members
    /// although it is written after them. Types only.
    AndGroup,
}

impl Place {
    const ALL: &'static [Place] = &[
        Place::Top,
        Place::Nested,
        Place::AutoOpenModule,
        Place::Opened,
        Place::RecForward,
        Place::RecBackward,
        Place::AndGroup,
    ];

    fn tag(self) -> &'static str {
        match self {
            Place::Top => "top",
            Place::Nested => "nested",
            Place::AutoOpenModule => "auto-open-module",
            Place::Opened => "opened",
            Place::RecForward => "rec-forward",
            Place::RecBackward => "rec-backward",
            Place::AndGroup => "and-group",
        }
    }
}

/// What follows the fold in a follower cell: one [`Decl`] at one [`Place`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Follower {
    decl: Decl,
    place: Place,
}

impl Follower {
    fn tag(self) -> String {
        format!("{} {}", self.place.tag(), self.decl.tag())
    }

    /// Every follower worth a cell. [`Decl::Nothing`] needs a container to
    /// mean anything, so it is skipped at [`Place::Top`], and an `and` group
    /// joins types only.
    fn all() -> Vec<Follower> {
        Place::ALL
            .iter()
            .flat_map(|&place| Decl::ALL.iter().map(move |&decl| Follower { decl, place }))
            .filter(|f| !(f.place == Place::Top && f.decl == Decl::Nothing))
            // FCS checks a `rec` block's module attributes before the block's
            // types exist, so an alias declared beside the module is FS0039.
            .filter(|f| {
                !(f.decl == Decl::AliasAutoOpenModule
                    && matches!(f.place, Place::RecForward | Place::RecBackward))
            })
            .filter(|f| {
                f.place != Place::AndGroup
                    || matches!(
                        f.decl,
                        Decl::Case
                            | Decl::Barless
                            | Decl::RqaBarless
                            | Decl::TypeCtor
                            | Decl::AutoOpenStatic
                            | Decl::AutoOpenEnum
                    )
            })
            .collect()
    }

    /// Append the follower and its two probes at zero indent, returning the
    /// probe spans.
    fn push(self, n: usize, src: &mut String) -> Vec<(Probe, TextRange)> {
        let decl = |src: &mut String, indent: &str| {
            for line in self.decl.lines(n) {
                src.push_str(indent);
                src.push_str(&line);
                src.push('\n');
            }
            src.push('\n');
        };
        match self.place {
            Place::Top => {
                decl(src, "");
                push_probes(src, "")
            }
            Place::Nested => {
                src.push_str(&format!("module After{n} =\n    let anchor{n} = 0\n\n"));
                decl(src, "    ");
                push_probes(src, "    ")
            }
            Place::AutoOpenModule => {
                src.push_str(&format!(
                    "[<AutoOpen>]\nmodule Later{n} =\n    let anchor{n} = 0\n\n"
                ));
                decl(src, "    ");
                push_probes(src, "")
            }
            Place::Opened => {
                src.push_str(&format!("module Other{n} =\n    let anchor{n} = 0\n\n"));
                decl(src, "    ");
                src.push_str(&format!("open Other{n}\n\n"));
                push_probes(src, "")
            }
            Place::RecForward => {
                src.push_str(&format!("module rec AfterRec{n} =\n"));
                let probes = push_probes(src, "    ");
                src.push('\n');
                decl(src, "    ");
                probes
            }
            Place::RecBackward => {
                src.push_str(&format!(
                    "module rec AfterRec{n} =\n    let anchor{n} = 0\n\n"
                ));
                decl(src, "    ");
                push_probes(src, "    ")
            }
            Place::AndGroup => {
                let mut out = Vec::new();
                src.push_str(&format!("type Host{n}() =\n    member _.ProbeExpr = "));
                out.push((Probe::Expr, push_probe(src)));
                src.push_str("\n    member _.ProbePat x =\n        match x with\n        | ");
                out.push((Probe::Pattern, push_probe(src)));
                src.push_str(" -> 1\n");
                // `[<A>] type T` joins a group as `and [<A>] T`.
                let lines = self.decl.lines(n);
                let (attrs, body): (Vec<&String>, Vec<&String>) =
                    lines.iter().partition(|l| l.starts_with("[<"));
                let (first, rest) = body.split_first().expect("a type decl has a line");
                let attrs: String = attrs.iter().map(|a| format!("{a} ")).collect();
                src.push_str(&format!(
                    "and {attrs}{}\n",
                    first
                        .strip_prefix("type ")
                        .expect("an `and`-group decl is a type")
                ));
                for line in rest {
                    src.push_str(line);
                    src.push('\n');
                }
                src.push('\n');
                out
            }
        }
    }
}

/// Append the expression and pattern probes at `indent`, returning their
/// spans in [`Probe`] order.
fn push_probes(src: &mut String, indent: &str) -> Vec<(Probe, TextRange)> {
    let mut out = Vec::new();
    src.push_str(&format!("{indent}let probeExpr = "));
    out.push((Probe::Expr, push_probe(src)));
    src.push_str(&format!(
        "\n\n{indent}let probePat x =\n{indent}    match x with\n{indent}    | "
    ));
    out.push((Probe::Pattern, push_probe(src)));
    src.push_str(" -> 1\n");
    out
}

/// One shape a fragment member can take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Shape {
    kind: Member,
    private: bool,
}

impl Shape {
    fn tag(self) -> String {
        if self.private {
            format!("private {}", self.kind.tag())
        } else {
            self.kind.tag().to_string()
        }
    }
}

/// Where a cell spells [`NAME`] after the fold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Probe {
    /// `let probeExpr = Nn` — expression position.
    Expr,
    /// `let probeAfterType = Nn`, below an enclosing `type Nn() = class end`
    /// declared after the fragment. A constructible type the *enclosing* block
    /// declares below the fold takes the name from anything folded — probed
    /// both ways round, and the mirror shape with the type above the module
    /// leaves the folded value standing. FCS names the type; sema models no
    /// project type constructor, so the only sound answer is to name nothing.
    AfterEnclosingType,
    /// `match x with | Nn -> 1` — pattern position. A name that is neither a
    /// case nor an exception is a *fresh binder* here, which FCS reports as a
    /// definition at the occurrence itself; that is a checkable answer too, so
    /// the probe stays in the grid for every kind.
    Pattern,
}

impl Probe {
    fn tag(self) -> &'static str {
        match self {
            Probe::Expr => "expr",
            Probe::AfterEnclosingType => "after-enclosing-type",
            Probe::Pattern => "pattern",
        }
    }
}

/// How a cell's fragment reaches its probes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Channel {
    /// The fragment is the enclosing block's own `[<AutoOpen>]` module, folded
    /// back at its closing position.
    FoldBack,
    /// The fragment sits in a plain module of the same file, which the block
    /// then `open`s: FCS opens the module's contents and then its
    /// `[<AutoOpen>]` children.
    OpenSameFile,
    /// As [`Self::OpenSameFile`], with the module in an earlier file.
    OpenCrossFile,
    /// The fragment sits directly in a namespace an earlier file declares, and
    /// the block `open`s the namespace.
    NamespaceCrossFile,
    /// The fragment sits directly in a namespace an earlier file declares, and
    /// the block is a module *in* that namespace, which folds it implicitly at
    /// position 0.
    NamespaceImplicit,
    /// **No fold** — a control. The fragment's members are written straight
    /// into the block, so a follower cell asks whether the follower takes the
    /// name back from an ordinary binder. A defect here is the follower's own,
    /// not the fold's.
    Inline,
    /// **No fold** — a control. The fragment is a plain (not `[<AutoOpen>]`)
    /// module the block `open`s explicitly.
    OpenPlain,
    /// As [`Self::OpenPlain`], with the module in an earlier file — whose
    /// members reach the `open` through the cross-file indices rather than
    /// this file's own.
    OpenPlainCrossFile,
}

impl Channel {
    /// The channels the gate holds. [`Self::NamespaceCrossFile`] is swept by
    /// its own ignored test, which documents why.
    const GATED: &'static [Channel] = &[
        Channel::FoldBack,
        Channel::OpenSameFile,
        Channel::OpenCrossFile,
        Channel::OpenPlain,
    ];

    /// The channels the follower grid crosses: every way a member reaches the
    /// block, the no-fold controls included — except the two that `open` a
    /// module holding the fragment, which fold its members only as declines
    /// (`ChildFolds::Declined`), so no follower can make them commit anything.
    /// The pair grid keeps them.
    const FOLLOWED: &'static [Channel] = &[
        Channel::FoldBack,
        Channel::NamespaceImplicit,
        Channel::Inline,
        Channel::OpenPlain,
        Channel::OpenPlainCrossFile,
    ];

    fn tag(self) -> &'static str {
        match self {
            Channel::FoldBack => "fold-back",
            Channel::OpenSameFile => "open-same-file",
            Channel::OpenCrossFile => "open-cross-file",
            Channel::NamespaceCrossFile => "namespace-cross-file",
            Channel::NamespaceImplicit => "namespace-implicit",
            Channel::Inline => "inline",
            Channel::OpenPlain => "open-plain",
            Channel::OpenPlainCrossFile => "open-plain-cross-file",
        }
    }
}

/// One generated cell: a fragment holding `members` in order, probed three
/// times. The probes sit in the **last** file of `files`.
struct Cell {
    channel: Channel,
    /// What the block declares after the fold, for a follower cell.
    follower: Option<Follower>,
    /// The fragment's members, in order.
    members: Vec<Shape>,
    label: String,
    /// The cell's files, in Compile order.
    files: Vec<String>,
    /// The probe spans, in [`Probe`] order.
    probes: Vec<(Probe, TextRange)>,
    /// The span of the enclosing `type Nn()` declaration's name — what FCS must
    /// pick for [`Probe::AfterEnclosingType`].
    enclosing_type: TextRange,
}

impl Cell {
    /// The file holding the probes.
    fn src(&self) -> &str {
        self.files.last().expect("a cell has a file")
    }

    /// Whether a gap in this cell is a [`KNOWN_GAPS`] entry rather than a
    /// manifest one: the fold-back pair grid's, whose classes predate the
    /// manifest. A bar-less case and a literal are one class each wherever
    /// they appear — sema interns no case for the first and declines the name,
    /// and a maybe-literal value defers the constant pattern it may be — so
    /// their cells are pinned in the manifest with every other channel's, as
    /// are the `[<RequireQualifiedAccess>]` rows added with them.
    fn gaps_are_known_gaps(&self) -> bool {
        self.channel == Channel::FoldBack
            && self.follower.is_none()
            && !self.members.iter().any(|m| {
                matches!(
                    m.kind,
                    Member::BarlessUnion
                        | Member::Literal
                        | Member::RqaUnionCase
                        | Member::RqaBarlessUnion
                        | Member::AliasNestedAuto
                )
            })
    }

    /// The key a gap is recorded under: `label/probe`, prefixed by the channel
    /// for every channel but the fold-back, whose keys predate the dimension.
    fn key(&self, probe: Probe) -> String {
        match (self.follower, self.channel) {
            (Some(f), channel) => format!(
                "{}: {} then {}/{}",
                channel.tag(),
                self.label,
                f.tag(),
                probe.tag()
            ),
            (None, Channel::FoldBack) => format!("{}/{}", self.label, probe.tag()),
            (None, other) => format!("{}: {}/{}", other.tag(), self.label, probe.tag()),
        }
    }
}

/// The inaccessible members the metamorphic property perturbs each cell with.
/// Each is `private` at the fragment's own level, so none of them is in scope
/// where the probe sits — and a member that is not in scope must not change
/// what the probe resolves to.
const INVISIBLE: &[&str] = &[
    "let private Nn = 42",
    "let private (|Nn|_|) x = Some x",
    "extern int private Nn()",
    "type private Nn() = class end",
    "type private NnHolderX =\n        | Nn",
    "type private NnBarX = Nn",
];

/// Build the whole grid over `channels`: each shape alone, then every ordered
/// pair.
fn cells(channels: &[Channel]) -> Vec<Cell> {
    let shapes: Vec<Shape> = Member::ALL
        .iter()
        .flat_map(|&kind| {
            [false, true]
                .into_iter()
                .map(move |private| Shape { kind, private })
        })
        .collect();

    let mut sequences: Vec<Vec<Shape>> = shapes.iter().map(|s| vec![*s]).collect();
    for a in &shapes {
        for b in &shapes {
            // See [`Slot`]: a same-slot pair does not compile, so FCS's answer
            // there is error recovery rather than a rule to reproduce.
            if a.kind.slot() != Slot::Indirect && a.kind.slot() == b.kind.slot() {
                continue;
            }
            sequences.push(vec![*a, *b]);
        }
    }

    let mut out = Vec::new();
    for &channel in channels {
        for members in &sequences {
            let n = out.len();
            out.push(build_cell(channel, n, members, None));
        }
    }
    out
}

/// The follower grid over `channels`, numbered from `first`: each public
/// [`Member`] alone in the fragment, and a `private` type — an inaccessible
/// type of the probed name, which must not make a later `type T = Nn` an
/// abbreviation — then each [`Follower`] after it.
fn follower_cells(channels: &[Channel], first: usize) -> Vec<Cell> {
    let shapes: Vec<Shape> = Member::ALL
        .iter()
        .map(|&kind| Shape {
            kind,
            private: false,
        })
        .chain([Shape {
            kind: Member::TypeCtor,
            private: true,
        }])
        .collect();
    let mut out = Vec::new();
    for &channel in channels {
        for &shape in &shapes {
            // Written straight into the probe's own block, a `private` member
            // is in reach after all, and a public probe exposing it is FS0410.
            if shape.private && channel == Channel::Inline {
                continue;
            }
            let kind = shape.kind;
            for follower in Follower::all() {
                // The control that writes the member straight into the block
                // would otherwise declare the name twice in one module: see
                // [`Slot`].
                if channel == Channel::Inline
                    && matches!(follower.place, Place::Top | Place::AndGroup)
                    && kind.slot() != Slot::Indirect
                    && kind.slot() == follower.decl.slot()
                {
                    continue;
                }
                let n = first + out.len();
                out.push(build_cell(channel, n, &[shape], Some(follower)));
            }
        }
    }
    out
}

fn build_cell(channel: Channel, n: usize, members: &[Shape], follower: Option<Follower>) -> Cell {
    let label = members
        .iter()
        .map(|s| s.tag())
        .collect::<Vec<_>>()
        .join(" ; ");

    // The fragment's members at `indent`, and the fragment itself.
    let member_lines = |indent: &str| {
        let mut out = String::new();
        for (i, shape) in members.iter().enumerate() {
            for line in shape.kind.lines(shape.private, i) {
                out.push_str(indent);
                out.push_str(&line);
                out.push('\n');
            }
        }
        out.push('\n');
        out
    };
    let fragment = |indent: &str| {
        format!(
            "{indent}[<AutoOpen>]\n{indent}module Fold{n} =\n{}",
            member_lines(&format!("{indent}    "))
        )
    };
    // Every cell's paths are distinct: every probe file joins ONE batched FCS
    // project, so two cells sharing a module path would be a duplicate
    // definition poisoning both, and a fragment at a *shared* namespace would
    // reach every cell. Each block writes the enclosing binding the fold must
    // take the name back from before the fragment reaches it.
    //
    // A follower cell writes none: the contest it asks about is between the
    // fold and what follows it, so what the follower takes the name from is the
    // fold's member alone.
    let enclosing = if follower.is_some() {
        String::new()
    } else {
        format!("let {NAME} = 99\n\n")
    };
    let (mut files, mut src) = match channel {
        Channel::FoldBack => (
            Vec::new(),
            format!("module Demo.FbSweep.C{n}\n\n{enclosing}{}", fragment("")),
        ),
        Channel::OpenSameFile => (
            Vec::new(),
            format!(
                "module Demo.FbSweep.C{n}\n\n{enclosing}module Host{n} =\n{}open Host{n}\n\n",
                fragment("    ")
            ),
        ),
        Channel::OpenCrossFile => (
            vec![format!("module Demo.FbSweepHost.H{n}\n\n{}", fragment(""))],
            format!("module Demo.FbSweep.C{n}\n\n{enclosing}open Demo.FbSweepHost.H{n}\n\n"),
        ),
        Channel::NamespaceCrossFile => (
            vec![format!("namespace Demo.FbSweepNs.N{n}\n\n{}", fragment(""))],
            format!("module Demo.FbSweep.C{n}\n\n{enclosing}open Demo.FbSweepNs.N{n}\n\n"),
        ),
        Channel::NamespaceImplicit => (
            vec![format!("namespace Demo.FbSweepIm.N{n}\n\n{}", fragment(""))],
            format!("module Demo.FbSweepIm.N{n}.Use{n}\n\n{enclosing}"),
        ),
        Channel::Inline => (
            Vec::new(),
            format!(
                "module Demo.FbSweep.C{n}\n\n{enclosing}{}",
                member_lines("")
            ),
        ),
        Channel::OpenPlain => (
            Vec::new(),
            format!(
                "module Demo.FbSweep.C{n}\n\n{enclosing}module Plain{n} =\n{}open Plain{n}\n\n",
                member_lines("    ")
            ),
        ),
        Channel::OpenPlainCrossFile => (
            vec![format!(
                "module Demo.FbSweepPlain.P{n}\n\n{}",
                member_lines("")
            )],
            format!("module Demo.FbSweep.C{n}\n\n{enclosing}open Demo.FbSweepPlain.P{n}\n\n"),
        ),
    };

    if let Some(follower) = follower {
        let probes = follower.push(n, &mut src);
        files.push(src);
        return Cell {
            channel,
            follower: Some(follower),
            members: members.to_vec(),
            label,
            files,
            probes,
            enclosing_type: TextRange::default(),
        };
    }

    let mut probes = push_probes(&mut src, "");

    // The enclosing block's own constructible type, *after* the fragment, and a
    // probe below it. Both probes above sit before this declaration, so they
    // cannot see it and their answers are unchanged.
    src.push_str("\ntype ");
    let enclosing_type = push_probe(&mut src);
    src.push_str("() = class end\n");
    src.push_str("\nlet probeAfterType = ");
    probes.push((Probe::AfterEnclosingType, push_probe(&mut src)));
    src.push('\n');
    files.push(src);

    Cell {
        channel,
        follower: None,
        members: members.to_vec(),
        label,
        files,
        probes,
        enclosing_type,
    }
}

/// A declaration range rendered as the source line that holds it — a bare byte
/// pair says nothing about *which member* a side picked, and the whole point of
/// a cell is which one won.
fn at(files: &[String], range: Option<(usize, usize, usize)>) -> String {
    match range {
        None => "nothing".to_string(),
        Some((f, s, e)) => {
            let src = &files[f];
            let line = src[..s].matches('\n').count() + 1;
            let ls = src[..s].rfind('\n').map_or(0, |i| i + 1);
            let le = src[s..].find('\n').map_or(src.len(), |i| s + i);
            format!(
                "{:?} in {:?} (file {f}, line {line})",
                &src[s..e],
                src[ls..le].trim()
            )
        }
    }
}

/// Append [`NAME`] and return the span it occupies — recorded as the source is
/// built rather than searched for afterwards, so a cell whose members mention
/// the name cannot be probed at the wrong occurrence.
fn push_probe(src: &mut String) -> TextRange {
    let start = u32::try_from(src.len()).expect("source fits in u32");
    src.push_str(NAME);
    let end = u32::try_from(src.len()).expect("source fits in u32");
    TextRange::new(start.into(), end.into())
}

/// Deferrals this channel makes on purpose: `("<label>/<probe>", reason)`. Each
/// must stay *exactly* "FCS resolves the probe to an in-file declaration and we
/// name nothing" — see the module docs.
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "value ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private value ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private value ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "case ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "case ; private ap/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "case ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "case ; extern/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "case ; private extern/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "case ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "private case ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private case ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "private case ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "exn ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "exn ; private ap/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "exn ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "exn ; extern/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "exn ; private extern/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "private exn ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private exn ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "ap ; value/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; private value/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; case/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; private case/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; exn/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; private exn/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "ap ; extern/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; private extern/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "ap ; type/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; private type/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; nested-auto/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "ap ; private nested-auto/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private ap ; case/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "private ap ; exn/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "private ap ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "private ap ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "extern ; case/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; case/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "extern ; private case/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; exn/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; exn/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "extern ; private exn/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; ap/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "extern ; private ap/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; type/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; private type/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "extern ; private nested-auto/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "private extern ; case/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "private extern ; exn/pattern",
        "`hidden` is derived module-wide, so an unenumerable value-namespace member defers a case (#51)",
    ),
    (
        "private extern ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private extern ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "type ; private value/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "type ; private case/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "type ; ap/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "type ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "type ; private ap/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "type ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "type ; private extern/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "type ; private nested-auto/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "private type ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private type ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "nested-auto ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private nested-auto ; ap/pattern",
        "the fold declines an active-pattern case rather than naming its recognizer (#50)",
    ),
    (
        "private nested-auto ; extern/expr",
        "sema does not intern an `extern` prototype, so the fold can only decline its name",
    ),
    (
        "private nested-auto ; type/expr",
        "sema models no project type constructor (#45), so the eviction override declines",
    ),
    (
        "auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private value ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "case ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "case ; auto-open-static/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "case ; private auto-open-static/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private case ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "exn ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "exn ; auto-open-static/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "exn ; private auto-open-static/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private exn ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "ap ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "ap ; auto-open-static/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "ap ; private auto-open-static/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private ap ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "extern ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "extern ; private auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private extern ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "type ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "type ; private auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private type ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private nested-auto ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private value/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; case/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private case/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; exn/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; exn/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private exn/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; ap/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; ap/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private ap/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; extern/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private extern/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; type/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private type/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private nested-auto/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "auto-open-static ; private auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private auto-open-static ; case/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private auto-open-static ; exn/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private auto-open-static ; ap/pattern",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private auto-open-static ; extern/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private auto-open-static ; type/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
    (
        "private auto-open-static ; auto-open-static/expr",
        "sema models no `[<AutoOpen>]` type statics (#48), so the fold declines the name it takes",
    ),
];

/// What one run of the grid over some channels found.
struct SweepOutcome {
    cells: usize,
    probes: usize,
    mismatches: Vec<String>,
    adjudicated: usize,
    seen_gaps: BTreeSet<String>,
    channel_entries: Vec<String>,
}

#[test]
fn the_fold_agrees_with_fcs_over_every_member_pair() {
    let SweepOutcome {
        cells,
        probes,
        mismatches,
        adjudicated,
        seen_gaps,
        channel_entries,
    } = sweep(cells(Channel::GATED));

    let stale: Vec<&str> = KNOWN_GAPS
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| !seen_gaps.contains(*k))
        .collect();
    assert!(
        stale.is_empty(),
        "KNOWN_GAPS entries name cells the grid no longer generates: {stale:?}"
    );

    assert!(
        mismatches.is_empty(),
        "{} of {probes} probes across {cells} cells disagree with FCS ({adjudicated} adjudicated):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    let manifest =
        Manifest::from_entries(channel_entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    borzoi_oracle_harness::manifest::check(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/manifests/auto_open_member_sweep_channels.txt"),
        &manifest,
        "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all \
         auto_open_member_sweep::the_fold_agrees_with_fcs_over_every_member_pair",
    );

    // Vacuity: the grid proves nothing if FCS declined to adjudicate it.
    assert!(
        adjudicated > cells,
        "FCS adjudicated only {adjudicated} probes across {cells} cells"
    );
}

/// The grid through two folds of an **earlier file's** members: its namespace,
/// opened by a later file — the implicit namespace fold `main` already
/// performs for a fragment another file declares — and an explicit `open` of
/// a plain module it declares.
///
/// Ignored because it fails with 27 disagreements (14 and 13), every one of
/// which `main` shares — `main` gives 516 on this grid. All are one defect of
/// those folds, not of the fold-back: an earlier file's same-named members
/// are not ranked by kind (`ProjectItems::fragment_value_children` and
/// `direct_value_children` keep the last declaration), so of a module's `let`
/// and union case, or case and exception, or case and `extern`, the wrong one
/// stands. The follower grid crosses the explicit-`open` channel with one
/// member at a time, where that ranking never arises, and is gated
/// (`what_follows_the_fold_agrees_with_fcs`).
#[test]
#[ignore = "27 disagreements, all shared by main: see the doc comment"]
fn the_namespace_fold_agrees_with_fcs_over_every_member_pair() {
    let outcome = sweep(cells(&[
        Channel::NamespaceCrossFile,
        Channel::OpenPlainCrossFile,
    ]));
    assert!(
        outcome.mismatches.is_empty(),
        "{} of {} probes across {} cells disagree with FCS:\n{}",
        outcome.mismatches.len(),
        outcome.probes,
        outcome.cells,
        outcome.mismatches.join("\n")
    );
}

/// **What follows the fold** — the follower grid ([`Follower`]) over every
/// [`Channel::FOLLOWED`] channel, graded like the pair grid.
///
/// Each cell's fragment holds one public member, and the block then writes one
/// [`Decl`] at one [`Place`]. Every probe must agree with FCS or be a decline
/// pinned in `tests/manifests/auto_open_member_sweep_followers.txt`. The two
/// no-fold channels ([`Channel::Inline`], [`Channel::OpenPlain`]) are the
/// controls: they tell a follower that mis-shadows the fold apart from one that
/// mis-shadows any binder.
#[test]
fn what_follows_the_fold_agrees_with_fcs() {
    let SweepOutcome {
        cells,
        probes,
        mismatches,
        adjudicated,
        channel_entries,
        ..
    } = sweep(follower_cells(Channel::FOLLOWED, 0));

    assert!(
        mismatches.is_empty(),
        "{} of {probes} probes across {cells} cells disagree with FCS ({adjudicated} adjudicated):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    let manifest =
        Manifest::from_entries(channel_entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    borzoi_oracle_harness::manifest::check(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/manifests/auto_open_member_sweep_followers.txt"),
        &manifest,
        "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all \
         auto_open_member_sweep::what_follows_the_fold_agrees_with_fcs",
    );

    assert!(
        adjudicated > cells,
        "FCS adjudicated only {adjudicated} probes across {cells} cells"
    );
}

/// How many projects a sweep splits its cells into, so the resident oracle
/// pool checks them in parallel. Cells are independent (every path is
/// distinct), so the split changes no answer.
const FCS_CHUNKS: usize = 3;

/// Run `cells` against FCS and grade every probe.
fn sweep(cells: Vec<Cell>) -> SweepOutcome {
    // Parse-check every cell up front and report *all* failures at once: a
    // panic on the first would hide the rest behind a costly FCS round-trip.
    let mut parse_failures: Vec<String> = Vec::new();
    // Every cell's files, in one Compile order: a cell's earlier file precedes
    // its probe file, and no cell reaches another's paths.
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let mut cell_paths: Vec<Vec<PathBuf>> = Vec::new();
    for cell in &cells {
        let mut paths = Vec::new();
        for src in &cell.files {
            let parsed = parse(src);
            if !parsed.errors.is_empty() {
                parse_failures.push(format!(
                    "  {}\n    {:?}\n    {:?}",
                    cell.key(Probe::Expr),
                    src.replace('\n', "\\n"),
                    parsed.errors
                ));
            }
            let path = temp_fs_file("borzoi_sema_fb_sweep", src);
            files.push((path.clone(), src.clone()));
            paths.push(path);
        }
        cell_paths.push(paths);
    }
    assert!(
        parse_failures.is_empty(),
        "{} of {} sweep cells do not parse:\n{}",
        parse_failures.len(),
        cells.len(),
        parse_failures.join("\n")
    );

    // One project per chunk of whole cells, checked concurrently.
    let per_chunk = cell_paths.len().div_ceil(FCS_CHUNKS).max(1);
    let file_chunks: Vec<&[(PathBuf, String)]> = {
        let mut out = Vec::new();
        let mut at = 0;
        for chunk in cell_paths.chunks(per_chunk) {
            let len: usize = chunk.iter().map(Vec::len).sum();
            out.push(&files[at..at + len]);
            at += len;
        }
        out
    };
    let replies: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = file_chunks
            .iter()
            .map(|chunk| {
                scope.spawn(move || {
                    let paths: Vec<&std::path::Path> =
                        chunk.iter().map(|(p, _)| p.as_path()).collect();
                    invoke_fcs_dump_project(&paths)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("an FCS chunk"))
            .collect()
    });
    let mut fcs_files = Vec::new();
    let mut fcs_errors = std::collections::HashMap::new();
    for (json, chunk) in replies.iter().zip(&file_chunks) {
        fcs_files.extend(parse_fcs_uses_project(json, chunk));
        fcs_errors.extend(fcs_errors_by_file(json));
    }
    for (p, _) in &files {
        let _ = std::fs::remove_file(p);
    }

    // **FCS binding a name is not FCS accepting the program.** A cell FCS
    // rejects is graded on its error recovery, which is no specification of
    // name resolution. The one rejection the grid tolerates is a probe's own
    // `FS0039` or `FS0800` — `let probeExpr = Nn` with only an active-pattern
    // case, or only a type, of the name in scope — whose answer, "nothing",
    // is checkable: each such probe is recorded here and graded as a probe
    // FCS resolves to nothing.
    let mut rejected_probes: std::collections::HashSet<(usize, TextRange)> =
        std::collections::HashSet::new();
    let mut ill_formed: Vec<String> = Vec::new();
    for (ci, (cell, paths)) in cells.iter().zip(cell_paths.iter()).enumerate() {
        for (fi, path) in paths.iter().enumerate() {
            let name = path.file_name().expect("a temp file has a name");
            for (line, col, number, message) in fcs_errors.get(name).into_iter().flatten() {
                let on_probe = (fi == paths.len() - 1)
                    .then(|| {
                        cell.probes
                            .iter()
                            .find(|(_, span)| line_col(cell.src(), span.start()) == (*line, *col))
                    })
                    .flatten();
                match on_probe {
                    // FS0039 (not defined) or FS0800 (a type name in
                    // expression position): nothing of the name in scope.
                    Some((_, span)) if matches!(*number, 39 | 800) => {
                        rejected_probes.insert((ci, *span));
                    }
                    _ => ill_formed.push(format!(
                        "  {} (file {fi}, line {line}, col {col}): FS{number:04} {message}\n{}",
                        cell.key(Probe::Expr),
                        cell.files.join("\n----\n")
                    )),
                }
            }
        }
    }
    assert!(
        ill_formed.is_empty(),
        "{} FCS errors in sweep cells, which would grade error recovery:\n{}",
        ill_formed.len(),
        ill_formed.join("\n")
    );

    let mut mismatches: Vec<String> = Vec::new();
    let mut adjudicated = 0usize;
    let mut seen_gaps: BTreeSet<String> = BTreeSet::new();
    // The other channels' gaps, as exact manifest entries.
    let mut channel_entries: Vec<String> = Vec::new();

    for (ci, (cell, paths)) in cells.iter().zip(cell_paths.iter()).enumerate() {
        let probe_path = paths.last().expect("a cell has a file");
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == probe_path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for cell {:?} ({probe_path:?})", cell.label));
        assert!(
            !fu.uses.is_empty(),
            "FCS reported nothing at all for cell {:?} — it likely failed to parse there:\n{}",
            cell.label,
            cell.src()
        );
        // A declaration in one of this cell's files, as `(file, start, end)`.
        let file_of =
            |path: &std::path::Path| paths.iter().position(|p| p.file_name() == path.file_name());

        let resolved = resolve_cell(cell);
        let render = |d: Option<(usize, usize, usize)>| at(&cell.files, d);

        for (probe, span) in &cell.probes {
            let key = cell.key(*probe);
            let (ours, our_def) = resolved.at(*span);

            // FCS's answer for exactly this occurrence, and only when the
            // declaration is in one of this cell's files: a use it resolves
            // elsewhere (FSharp.Core's `Some`, say) is out of this sweep's scope.
            let fcs_use = fu
                .uses
                .iter()
                .find(|u| u.start == usize::from(span.start()) && u.end == usize::from(span.end()));
            let fcs_decl = fcs_use
                .and_then(|u| u.decl.as_ref())
                .and_then(|d| file_of(&d.file).map(|f| (f, d.start, d.end)));

            // FCS rejects the probe itself: nothing is in scope under the name
            // in that namespace, so naming anything is a wrong answer.
            if rejected_probes.contains(&(ci, *span)) {
                if our_def.is_some() {
                    mismatches.push(format!(
                        "  {key}\n    FCS rejects the probe (FS0039/FS0800), we pick {} (resolution \
                         {ours:?})\n{}",
                        render(our_def),
                        cell.files.join("\n----\n")
                    ));
                }
                adjudicated += 1;
                continue;
            }

            // A pattern occurrence FCS calls a *definition* is a fresh binder:
            // the name reached nothing in the pattern namespace. Sema records a
            // binder rather than a resolution there, so the checkable claim is
            // the soundness half — we must not point somewhere *else*, which is
            // what binding the fold's member here would look like.
            if fcs_use.is_some_and(|u| u.is_from_definition) {
                if our_def.is_some_and(|d| Some(d) != fcs_decl) {
                    mismatches.push(format!(
                        "  {key}\n    FCS binds a fresh pattern binder, we point at {} \
                         (resolution {ours:?})\n{}",
                        render(our_def),
                        cell.files.join("\n----\n")
                    ));
                }
                continue;
            }

            // The enclosing type's probe needs no ratchet: FCS's answer is
            // known by construction (the type is the last declaration and takes
            // the name), and ours can only ever be "nothing", since sema models
            // no project type constructor. Assert both — a change on either
            // side is news, not a gap to record.
            if *probe == Probe::AfterEnclosingType {
                let last = cell.files.len() - 1;
                let want = (
                    last,
                    usize::from(cell.enclosing_type.start()),
                    usize::from(cell.enclosing_type.end()),
                );
                if fcs_decl != Some(want) {
                    mismatches.push(format!(
                        "  {key}\n    the enclosing `type {NAME}()` should take the name, but \
                         FCS picks {}\n{}",
                        render(fcs_decl),
                        cell.files.join("\n----\n")
                    ));
                } else if our_def.is_some() {
                    mismatches.push(format!(
                        "  {key}\n    FCS picks the enclosing type, we pick {} (resolution \
                         {ours:?})\n{}",
                        render(our_def),
                        cell.files.join("\n----\n")
                    ));
                }
                adjudicated += 1;
                continue;
            }

            // Counted before the ratchet: a gap asserts FCS resolved the probe
            // in-file, so it is adjudication too — and the vacuity floor below
            // must not fall as gaps are fixed.
            if fcs_decl.is_some() {
                adjudicated += 1;
            }

            if cell.gaps_are_known_gaps()
                && let Some((_, reason)) = KNOWN_GAPS.iter().find(|(k, _)| *k == key)
            {
                seen_gaps.insert(key.clone());
                if our_def.is_some() || fcs_decl.is_none() {
                    mismatches.push(format!(
                        "  {key} [KNOWN GAP: {reason}]\n    no longer behaves as the gap \
                         describes — if fixed, delete its entry; if we now name a target, that \
                         is a wrong resolution\n    FCS:  {}\n    ours: {}\n{}",
                        render(fcs_decl),
                        render(our_def),
                        cell.files.join("\n----\n")
                    ));
                }
                continue;
            }

            match (fcs_decl, our_def) {
                (None, None) => {}
                // The other channels' gaps are manifest entries, on the
                // ratchet's terms: FCS resolved the probe in-file and we named
                // nothing.
                (Some(_), None) if !cell.gaps_are_known_gaps() => {
                    channel_entries.push(format!("{key} gap"));
                }
                (Some(_), _) => {
                    if fcs_decl != our_def {
                        mismatches.push(format!(
                            "  {key}\n    FCS picks {}\n    we pick   {} (resolution \
                             {ours:?})\n{}",
                            render(fcs_decl),
                            render(our_def),
                            cell.files.join("\n----\n")
                        ));
                    }
                }
                (None, Some(_)) => mismatches.push(format!(
                    "  {key}\n    FCS resolved nothing in-file, we pick {} (resolution \
                     {ours:?})\n{}",
                    render(our_def),
                    cell.files.join("\n----\n")
                )),
            }
        }
    }

    SweepOutcome {
        cells: cells.len(),
        probes: cells.iter().map(|c| c.probes.len()).sum(),
        mismatches,
        adjudicated,
        seen_gaps,
        channel_entries,
    }
}

/// Every FCS **error** in a `uses-project` reply, keyed by file name, as
/// `(line, column, number, message)` — 1-based line, 0-based column, the
/// convention [`line_col`] reproduces.
fn fcs_errors_by_file(
    json: &str,
) -> std::collections::HashMap<std::ffi::OsString, Vec<(u64, u64, u64, String)>> {
    let dump: serde_json::Value = serde_json::from_str(json).expect("uses-project JSON");
    let mut out = std::collections::HashMap::new();
    for file in dump["Files"].as_array().expect("a Files array") {
        let path = PathBuf::from(file["Path"].as_str().expect("a Path"));
        let errors: Vec<(u64, u64, u64, String)> = file["Diagnostics"]
            .as_array()
            .expect("a Diagnostics array")
            .iter()
            .filter(|d| d["Severity"] == "Error")
            .map(|d| {
                (
                    d["Range"]["Start"]["Line"].as_u64().expect("a line"),
                    d["Range"]["Start"]["Col"].as_u64().expect("a column"),
                    d["ErrorNumber"].as_u64().expect("an error number"),
                    d["Message"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        out.insert(path.file_name().expect("a file name").to_owned(), errors);
    }
    out
}

/// FCS's `(line, column)` for byte `at` of an ASCII `src`.
fn line_col(src: &str, at: TextSize) -> (u64, u64) {
    let at = usize::from(at);
    let line = src[..at].matches('\n').count() + 1;
    let col = at - src[..at].rfind('\n').map_or(0, |i| i + 1);
    (line as u64, col as u64)
}

/// Our side of one cell: its probe file resolved, alone or after the cell's
/// earlier file.
enum ResolvedCell {
    File(Box<ResolvedFile>),
    Project(ResolvedProject),
}

impl ResolvedCell {
    /// The resolution at `span` in the probe file, and the declaration it
    /// names as `(file, start, end)` when it names one in this cell.
    fn at(&self, span: TextRange) -> (Option<Resolution>, Option<(usize, usize, usize)>) {
        let range = |r: TextRange| (usize::from(r.start()), usize::from(r.end()));
        match self {
            ResolvedCell::File(rf) => {
                let ours = rf.resolution_at(span);
                let def = match ours {
                    Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => rf
                        .resolved_def(res)
                        .map(|d| range(d.range))
                        .map(|(s, e)| (0, s, e)),
                    _ => None,
                };
                (ours, def)
            }
            ResolvedCell::Project(proj) => {
                let last = proj.files().len() - 1;
                let ours = proj.file(last).resolution_at(span);
                let def = match ours {
                    Some(Resolution::Item(_)) => proj
                        .item_def(ours.expect("checked"))
                        .map(|(f, d)| (f, range(d.range).0, range(d.range).1)),
                    Some(res @ Resolution::Local(_)) => proj
                        .file(last)
                        .resolved_def(res)
                        .map(|d| (last, range(d.range).0, range(d.range).1)),
                    _ => None,
                };
                (ours, def)
            }
        }
    }
}

fn resolve_cell(cell: &Cell) -> ResolvedCell {
    let env = crate::common::fsharp_core_env();
    let impl_file = |src: &str| {
        let parsed = parse(src);
        ImplFile::cast(parsed.root).expect("impl file")
    };
    if let [only] = cell.files.as_slice() {
        let parsed = parse(only);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        ResolvedCell::File(Box::new(resolve_file(
            &file,
            &ProjectItems::default(),
            env,
            &recovery,
        )))
    } else {
        let files: Vec<ImplFile> = cell.files.iter().map(|s| impl_file(s)).collect();
        ResolvedCell::Project(resolve_project(&files, env))
    }
}

/// **A member that is not in scope cannot change what is.**
///
/// Append an inaccessible declaration to each cell's fragment and the probe must
/// resolve exactly as before — same variant, same target range. `private` at the
/// fragment's own level means the member never reaches the enclosing block, so
/// its only possible effect is on machinery that scans the fragment *without*
/// applying the accessibility test the fold itself applies.
///
/// This is the sweep's answer to its own arity. The grid above is pairwise, and
/// the first defect of exactly this shape took **three** members to build — an
/// earlier public case, a later class, and a `let private` that decided which
/// side of the fold the type-eviction override landed on (codex review). Adding
/// a third member to the grid would multiply it by fourteen and cost an FCS
/// round-trip per cell; this needs no oracle at all, because the invariant is
/// stated against our own resolver, and it covers every cell for the price of a
/// few hundred local resolutions.
#[test]
fn an_inaccessible_member_does_not_change_any_cell() {
    let mut mismatches: Vec<String> = Vec::new();
    let mut losses: Vec<String> = Vec::new();

    // The fold-back channel only: the perturbation is spliced into the probe
    // file's own fragment.
    for cell in cells(&[Channel::FoldBack]) {
        let src = cell.src().to_string();
        let base = resolutions(&src, &cell.probes);
        for invisible in INVISIBLE {
            // Into the fragment, after its members: the fold's own block, at the
            // indent its declarations use.
            let Some(blank) = src.find("\n\nlet probeExpr") else {
                panic!("cell {:?} has no probe tail", cell.label);
            };
            let mut perturbed = src.clone();
            perturbed.insert_str(blank, &format!("\n    {invisible}"));

            // The probes moved by exactly the inserted text.
            let shift = u32::try_from(perturbed.len() - src.len()).expect("fits");
            let moved: Vec<(Probe, TextRange)> = cell
                .probes
                .iter()
                .map(|(probe, span)| (*probe, *span + TextSize::from(shift)))
                .collect();
            for ((probe, _), (before, after)) in cell
                .probes
                .iter()
                .zip(base.iter().zip(resolutions(&perturbed, &moved)))
            {
                if *before == after {
                    continue;
                }
                // Losing a target to a deferral is an availability cost: the
                // invisible member is over-counted somewhere, but nothing wrong
                // is claimed. Those are ratcheted below by shape and size.
                if after == NOTHING && *before != NOTHING {
                    losses.push(format!("{}/{} + {invisible:?}", cell.label, probe.tag()));
                    continue;
                }
                // Anything else is a target we would not otherwise name — the
                // grade codex's finding was, where an out-of-scope `let private`
                // moved the type-eviction override and let a case stand.
                mismatches.push(format!(
                    "  {}/{} + {:?}\n    was {before:?}\n    now {after:?}\n{perturbed}",
                    cell.label,
                    probe.tag(),
                    invisible,
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} probes name a different target when an out-of-scope member is added:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    // The availability ratchet, pinned by **shape and size** so a fix and a
    // fresh defect cannot cancel out in a bare count. Every loss today is an
    // inaccessible member of the *value* namespace — a `private` recognizer or
    // `extern` — deferring a case in *pattern* position, because
    // `open_module_values` derives its `hidden` flag module-wide and without an
    // accessibility test (task #51). When that lands this number goes down; it
    // must never go up.
    let unexpected: Vec<&String> = losses
        .iter()
        .filter(|l| {
            !l.contains("/pattern + \"let private (|Nn|_|)")
                && !l.contains("/pattern + \"extern int private Nn()")
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "{} availability losses of an unratcheted shape:\n  {}",
        unexpected.len(),
        unexpected
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    assert_eq!(
        losses.len(),
        126,
        "the count of #51 availability losses moved; down is a fix (update this \
         number), up is a new defect:\n  {}",
        losses.join("\n  ")
    );
}

/// How [`resolutions`] renders "we named nothing" — a deferral and a silence
/// alike, since neither claims a target.
const NOTHING: &str = "None";

/// Each probe's resolution, rendered so two runs over different source text are
/// comparable: a target becomes its declaration range, everything else its
/// variant name.
fn resolutions(src: &str, probes: &[(Probe, TextRange)]) -> Vec<String> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(
        &file,
        &ProjectItems::default(),
        crate::common::fsharp_core_env(),
        &recovery,
    );
    probes
        .iter()
        .map(|(_, span)| match rf.resolution_at(*span) {
            Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => rf
                .resolved_def(res)
                .map_or_else(|| "no-def".to_string(), |d| format!("def {:?}", d.range)),
            None => NOTHING.to_string(),
            Some(Resolution::Deferred(_)) => NOTHING.to_string(),
            Some(other) => format!("{other:?}"),
        })
        .collect()
}
