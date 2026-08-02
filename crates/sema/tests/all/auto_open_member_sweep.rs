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

use std::collections::BTreeSet;
use std::path::PathBuf;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
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
    /// `exception Nn of int` — the constructor namespace via an exception.
    Exception,
    /// `let (|Nn|_|) x = Some x` — a module-level active-pattern case. Not a
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
    ];

    fn slot(self) -> Slot {
        match self {
            Member::Value | Member::Extern => Slot::Value,
            Member::Exception | Member::TypeCtor => Slot::TypeName,
            Member::ActivePattern => Slot::Recognizer,
            Member::UnionCase | Member::NestedAuto | Member::AutoOpenStatic => Slot::Indirect,
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
            Member::Exception => vec![format!("exception {acc}{NAME} of int")],
            Member::ActivePattern => vec![format!("let {acc}(|{NAME}|_|) x = Some x")],
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
        }
    }
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

/// One generated cell: a fragment holding `members` in order, probed twice.
struct Cell {
    label: String,
    src: String,
    /// The probe spans, in [`Probe`] order.
    probes: Vec<(Probe, TextRange)>,
    /// The span of the enclosing `type Nn()` declaration's name — what FCS must
    /// pick for [`Probe::AfterEnclosingType`].
    enclosing_type: TextRange,
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
];

/// Build the whole grid: each shape alone, then every ordered pair.
fn cells() -> Vec<Cell> {
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

    sequences
        .into_iter()
        .enumerate()
        .map(|(n, members)| build_cell(n, &members))
        .collect()
}

fn build_cell(n: usize, members: &[Shape]) -> Cell {
    let label = members
        .iter()
        .map(|s| s.tag())
        .collect::<Vec<_>>()
        .join(" ; ");

    let mut src = String::new();
    // A distinct top-level module per cell: every probe file joins ONE batched
    // FCS project, so two cells sharing a module path would be a duplicate
    // definition poisoning both. The fragment is nested *inside* that module,
    // so its fold reaches this cell's block and no other — an `[<AutoOpen>]`
    // module at the shared `Demo.FbSweep` namespace level would reach them all.
    src.push_str(&format!("module Demo.FbSweep.C{n}\n\n"));
    // The enclosing binding the fold must take the name back from.
    src.push_str(&format!("let {NAME} = 99\n\n"));
    src.push_str("[<AutoOpen>]\n");
    src.push_str(&format!("module Fold{n} =\n"));
    for (i, shape) in members.iter().enumerate() {
        for line in shape.kind.lines(shape.private, i) {
            src.push_str("    ");
            src.push_str(&line);
            src.push('\n');
        }
    }
    src.push('\n');

    let mut probes = Vec::new();
    src.push_str("let probeExpr = ");
    probes.push((Probe::Expr, push_probe(&mut src)));
    src.push_str("\n\nlet probePat x =\n    match x with\n    | ");
    probes.push((Probe::Pattern, push_probe(&mut src)));
    src.push_str(" -> 1\n");

    // The enclosing block's own constructible type, *after* the fragment, and a
    // probe below it. Both probes above sit before this declaration, so they
    // cannot see it and their answers are unchanged.
    src.push_str("\ntype ");
    let enclosing_type = push_probe(&mut src);
    src.push_str("() = class end\n");
    src.push_str("\nlet probeAfterType = ");
    probes.push((Probe::AfterEnclosingType, push_probe(&mut src)));
    src.push('\n');

    Cell {
        label,
        src,
        probes,
        enclosing_type,
    }
}

/// A declaration range rendered as the source line that holds it — a bare byte
/// pair says nothing about *which member* a side picked, and the whole point of
/// a cell is which one won.
fn at(src: &str, range: Option<(usize, usize)>) -> String {
    match range {
        None => "nothing".to_string(),
        Some((s, e)) => {
            let line = src[..s].matches('\n').count() + 1;
            let ls = src[..s].rfind('\n').map_or(0, |i| i + 1);
            let le = src[s..].find('\n').map_or(src.len(), |i| s + i);
            format!("{:?} in {:?} (line {line})", &src[s..e], src[ls..le].trim())
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

#[test]
fn the_fold_agrees_with_fcs_over_every_member_pair() {
    let cells = cells();

    // Parse-check every cell up front and report *all* failures at once: a
    // panic on the first would hide the rest behind a costly FCS round-trip.
    let mut parse_failures: Vec<String> = Vec::new();
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for cell in &cells {
        let parsed = parse(&cell.src);
        if !parsed.errors.is_empty() {
            parse_failures.push(format!(
                "  {}\n    {:?}\n    {:?}",
                cell.label,
                cell.src.replace('\n', "\\n"),
                parsed.errors
            ));
        }
        files.push((
            temp_fs_file("borzoi_sema_fb_sweep", &cell.src),
            cell.src.clone(),
        ));
    }
    assert!(
        parse_failures.is_empty(),
        "{} of {} sweep cells do not parse:\n{}",
        parse_failures.len(),
        cells.len(),
        parse_failures.join("\n")
    );

    let paths: Vec<&std::path::Path> = files.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs_files = parse_fcs_uses_project(&json, &files);
    for (p, _) in &files {
        let _ = std::fs::remove_file(p);
    }

    let mut mismatches: Vec<String> = Vec::new();
    let mut adjudicated = 0usize;
    // Cells whose `after-enclosing-type` probe names the enclosing *local*
    // binding — task #52, a defect no fold is involved in. Pinned by shape
    // above and by count below; down is a fix, up is a new defect.
    let mut local_eviction_gaps = 0usize;
    let mut seen_gaps: BTreeSet<String> = BTreeSet::new();

    for (cell, (path, _)) in cells.iter().zip(files.iter()) {
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for cell {:?} ({path:?})", cell.label));
        assert!(
            !fu.uses.is_empty(),
            "FCS reported nothing at all for cell {:?} — it likely failed to parse there:\n{}",
            cell.label,
            cell.src
        );

        let parsed = parse(&cell.src);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(
            &file,
            &ProjectItems::default(),
            &AssemblyEnv::default(),
            &recovery,
        );

        for (probe, span) in &cell.probes {
            let key = format!("{}/{}", cell.label, probe.tag());
            let ours = rf.resolution_at(*span);
            let our_def = match ours {
                Some(Resolution::Local(_) | Resolution::Item(_)) => rf
                    .resolved_def(ours.expect("checked"))
                    .map(|d| d.range)
                    .map(|r| (usize::from(r.start()), usize::from(r.end()))),
                _ => None,
            };

            // FCS's answer for exactly this occurrence, and only when the
            // declaration is in this same file: a use it resolves elsewhere
            // (FSharp.Core's `Some`, say) is out of this sweep's scope.
            let fcs_use = fu
                .uses
                .iter()
                .find(|u| u.start == usize::from(span.start()) && u.end == usize::from(span.end()));
            let fcs_decl = fcs_use
                .and_then(|u| u.decl.as_ref())
                .filter(|d| d.file.file_name() == path.file_name())
                .map(|d| (d.start, d.end));

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
                        at(&cell.src, our_def),
                        cell.src
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
                let want = (
                    usize::from(cell.enclosing_type.start()),
                    usize::from(cell.enclosing_type.end()),
                );
                if fcs_decl != Some(want) {
                    mismatches.push(format!(
                        "  {key}\n    the enclosing `type {NAME}()` should take the name, but \
                         FCS picks {}\n{}",
                        at(&cell.src, fcs_decl),
                        cell.src
                    ));
                } else if our_def.is_some() {
                    // Naming the enclosing `let Nn = 99` is task #52 — the same
                    // eviction against a plain local binding, which no fold
                    // touches (`let Nn = 99; type Nn(); Nn` reproduces it with
                    // no `[<AutoOpen>]` in the source at all). Ratcheted by
                    // shape, so a *folded* member standing here — the door this
                    // branch opens and closes — still fails.
                    let enclosing_binding = cell.src.find(&format!("let {NAME} = 99")).map(|i| {
                        let at = i + "let ".len();
                        (at, at + NAME.len())
                    });
                    if our_def == enclosing_binding {
                        local_eviction_gaps += 1;
                    } else {
                        mismatches.push(format!(
                            "  {key}\n    FCS picks the enclosing type, we pick {} (resolution \
                             {ours:?})\n{}",
                            at(&cell.src, our_def),
                            cell.src
                        ));
                    }
                }
                adjudicated += 1;
                continue;
            }

            // Counted before the ratchet: a KNOWN_GAPS cell asserts FCS
            // resolved the probe in-file, so it is adjudication too — and the
            // vacuity floor below must not fall as gaps are fixed.
            if fcs_decl.is_some() {
                adjudicated += 1;
            }

            if let Some((_, reason)) = KNOWN_GAPS.iter().find(|(k, _)| *k == key) {
                seen_gaps.insert(key.clone());
                if our_def.is_some() || fcs_decl.is_none() {
                    mismatches.push(format!(
                        "  {key} [KNOWN GAP: {reason}]\n    no longer behaves as the gap \
                         describes — if fixed, delete its entry; if we now name a target, that \
                         is a wrong resolution\n    FCS:  {}\n    ours: {}\n{}",
                        at(&cell.src, fcs_decl),
                        at(&cell.src, our_def),
                        cell.src
                    ));
                }
                continue;
            }

            match (fcs_decl, our_def) {
                (None, None) => {}
                (Some(_), _) => {
                    if fcs_decl != our_def {
                        mismatches.push(format!(
                            "  {key}\n    FCS picks {}\n    we pick   {} (resolution \
                             {ours:?})\n{}",
                            at(&cell.src, fcs_decl),
                            at(&cell.src, our_def),
                            cell.src
                        ));
                    }
                }
                (None, Some(_)) => mismatches.push(format!(
                    "  {key}\n    FCS resolved nothing in-file, we pick {} (resolution \
                     {ours:?})\n{}",
                    at(&cell.src, our_def),
                    cell.src
                )),
            }
        }
    }

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
        "{} of {} probes across {} cells disagree with FCS ({adjudicated} adjudicated):\n{}",
        mismatches.len(),
        cells.len() * 3,
        cells.len(),
        mismatches.join("\n")
    );

    assert_eq!(
        local_eviction_gaps, 78,
        "the count of #52 cells moved; down is a fix (update this number), up is a new defect"
    );

    // Vacuity: the grid proves nothing if FCS declined to adjudicate it.
    assert!(
        adjudicated > cells.len(),
        "FCS adjudicated only {adjudicated} probes across {} cells",
        cells.len()
    );
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

    for cell in cells() {
        let base = resolutions(&cell.src, &cell.probes);
        for invisible in INVISIBLE {
            // Into the fragment, after its members: the fold's own block, at the
            // indent its declarations use.
            let Some(blank) = cell.src.find("\n\nlet probeExpr") else {
                panic!("cell {:?} has no probe tail", cell.label);
            };
            let mut perturbed = cell.src.clone();
            perturbed.insert_str(blank, &format!("\n    {invisible}"));

            // The probes moved by exactly the inserted text.
            let shift = u32::try_from(perturbed.len() - cell.src.len()).expect("fits");
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
        62,
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
        &AssemblyEnv::default(),
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
