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
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

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
    ];

    fn slot(self) -> Slot {
        match self {
            Member::Value | Member::Extern => Slot::Value,
            Member::Exception | Member::TypeCtor => Slot::TypeName,
            Member::ActivePattern => Slot::Recognizer,
            Member::UnionCase | Member::NestedAuto => Slot::Indirect,
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
            Probe::Pattern => "pattern",
        }
    }
}

/// One generated cell: a fragment holding `members` in order, probed twice.
struct Cell {
    label: String,
    src: String,
    /// The two probe spans, in [`Probe`] order.
    probes: Vec<(Probe, TextRange)>,
}

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

    Cell { label, src, probes }
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
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());

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
        cells.len() * 2,
        cells.len(),
        mismatches.join("\n")
    );

    // Vacuity: the grid proves nothing if FCS declined to adjudicate it.
    assert!(
        adjudicated > cells.len(),
        "FCS adjudicated only {adjudicated} probes across {} cells",
        cells.len()
    );
}
