//! The shared harness behind the fold matrices (`namespace_fold_matrix`,
//! `module_open_matrix`, `project_half_matrix`): FCS-diffed cell grids over
//! the two fixture assemblies — and, for the project-half grid, per-cell
//! Compile-order-preceding project decl files — with the
//! certain-implies-exact `KNOWN_GAPS` ratchet.
//!
//! A matrix module owns its cell list and gap list and calls [`run_matrix`];
//! everything else — snippet construction, the one-invocation FCS batch, the
//! per-cell resolution on both sides, the bijection with the ratchet — lives
//! here so the grids cannot drift apart in mechanics.

use std::path::{Path, PathBuf};

use super::{
    FileUses, ensure_abbrev_fixture_built, ensure_autoopen_fixture_built,
    invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_file,
};
use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ItemId, Resolution, resolve_project};
use rowan::TextRange;

/// One cell: a probe reached through a body of `open`s (and, for the contest
/// cells, interleaved `let` bindings), in expression or pattern position.
pub struct Cell {
    pub label: &'static str,
    /// The probe file's own top-level header — which decides what FCS opens
    /// *implicitly*, before any `open` in [`Cell::body`] and before every
    /// declaration.
    pub container: Container,
    /// Whole project FILES preceding the probe file in Compile order — the
    /// project-half matrix's declaring files (`namespace Demo.PjFold.<Shape>`
    /// with the shape under test). Empty for the single-file matrices. Each
    /// cell resolves against its OWN decls only on our side; on the FCS side
    /// every file joins one batched project (isolated by FQN).
    pub decls: &'static [&'static str],
    /// The declaration lines preceding the probe — `open`s, and for the
    /// contest cells project `let` bindings between them (the round-10 shape:
    /// a binding a *later* cross-kind open's generation barrier stales).
    pub body: &'static [&'static str],
    /// The declaration lines *following* the probe, at the same indent. A
    /// declaration a use precedes is invisible to it in an ordinary block, so
    /// these are only interesting where F# makes them visible anyway: a
    /// `module rec` block, where every declaration is in scope for every other
    /// — including an `[<AutoOpen>]` submodule's folded-back surface
    /// (fcs-dump-probed).
    pub after: &'static [&'static str],
    /// The exact source text of the reference — the probe span.
    pub probe: &'static str,
    /// Where the probe sits.
    pub position: Position,
}

/// The probe file's top-level header.
///
/// This is the dimension that reaches FCS's **implicit** open of a block's own
/// enclosing namespace (`ImplicitlyOpenOwnNamespace`): a grid whose probe is
/// always a header-less top-level `let` can only ever exercise the `open`s a
/// cell writes, so every contest the implicit channel decides — a project half
/// against a colliding assembly auto-open, a same-FQN module half against the
/// namespace half — is *outside* it. Both wrong-commit regressions the first
/// implicit-open attempt shipped were cells this dimension covers.
#[derive(Clone, Copy, PartialEq)]
pub enum Container {
    /// No header: an anonymous top-level module, whose enclosing namespace is
    /// empty, so nothing is opened implicitly.
    Anon,
    /// `namespace <path>`, with the probe in a nested module (F# forbids values
    /// at namespace scope). The enclosing namespace is `<path>` itself.
    ///
    /// The nested module is named from the cell's label, because every probe
    /// file of a matrix joins ONE batched FCS project: two cells sharing a
    /// namespace and a module name would be a duplicate definition, which
    /// poisons resolution for both.
    Namespace(&'static str),
    /// [`Self::Namespace`] with the probe's nested module marked
    /// `[<AutoOpen>]` — the shape that dominates real code (`namespace N` plus
    /// one auto-open module holding the whole file). A use *inside* such a
    /// module can never be shadowed by that module's own fold-back: FCS folds
    /// its surface into the ENCLOSING scope when the declaration is processed,
    /// while inside it the same names are in scope by ordinary position.
    NamespaceAutoOpen(&'static str),
    /// `module <path>` — a dotted top-level module header, whose enclosing
    /// namespace is everything *before* the last segment. `<path>` must be
    /// distinct per cell, for the same batched-project reason.
    Module(&'static str),
    /// `module rec <path>` — [`Self::Module`] where every declaration is in
    /// scope for every other, so a probe may precede the declarations in
    /// [`Cell::after`] that bind it.
    ModuleRec(&'static str),
}

/// How a cell places its probe.
#[derive(Clone, Copy, PartialEq)]
pub enum Position {
    /// `let probeResult = <probe>` — expression position.
    Expr,
    /// `match x with | <probe> _ -> …` — pattern position, constructor-shaped:
    /// an argument follows, so a case/exception reading is arity-correct.
    PatternCtor,
    /// `match x with | <probe> -> …` — pattern position, bare: no argument,
    /// the shape a constant/literal pattern takes (§8 cell 8b).
    PatternBare,
}

/// Build the snippet for a cell and return `(source, probe_span)`. Always
/// parse-clean on both sides; a *contest* cell may deliberately fail FCS's
/// type check (a value-captured head has no such member) — FCS still reports
/// its uses, and "no use spans the probe" is exactly the `None` the bijection
/// wants for it.
fn cell_source(cell: &Cell) -> (String, TextRange) {
    // A `namespace` header cannot hold values, so its probe goes in a nested
    // module and every line below it is indented one level.
    let probe_module: String = std::iter::once('P')
        .chain(cell.label.chars().filter(char::is_ascii_alphanumeric))
        .collect();
    let (mut src, indent) = match cell.container {
        Container::Anon => (String::new(), ""),
        Container::Namespace(path) => (
            format!("namespace {path}\n\nmodule {probe_module} =\n"),
            "    ",
        ),
        Container::NamespaceAutoOpen(path) => (
            format!("namespace {path}\n\n[<AutoOpen>]\nmodule {probe_module} =\n"),
            "    ",
        ),
        Container::Module(path) => (format!("module {path}\n\n"), ""),
        Container::ModuleRec(path) => (format!("module rec {path}\n\n"), ""),
    };
    for line in cell.body {
        src.push_str(indent);
        src.push_str(line);
        src.push('\n');
    }
    let (prefix, suffix) = match cell.position {
        Position::PatternCtor => (
            "let probeFn x =\n    match x with\n    | ",
            " _ -> 1\n    | _ -> 0\n",
        ),
        Position::PatternBare => (
            "let probeFn x =\n    match x with\n    | ",
            " -> 1\n    | _ -> 0\n",
        ),
        Position::Expr => ("let probeResult = ", "\n"),
    };
    // The prefix starts fresh lines, so every one of its lines is indented; the
    // suffix *continues* the probe's line, so only its later lines are.
    for (i, line) in prefix.split('\n').enumerate() {
        if i > 0 {
            src.push('\n');
        }
        src.push_str(indent);
        src.push_str(line);
    }
    let start = src.len();
    src.push_str(cell.probe);
    let end = src.len();
    for (i, line) in suffix.split('\n').enumerate() {
        if i > 0 {
            src.push('\n');
            if !line.is_empty() {
                src.push_str(indent);
            }
        }
        src.push_str(line);
    }
    // The suffix always ends the probe's last line, so `after` starts fresh.
    for line in cell.after {
        src.push_str(indent);
        src.push_str(line);
        src.push('\n');
    }
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    );
    (src, span)
}

/// What a side resolved the probe to: the symbol's full name, or `None` for "nothing".
type Resolved = Option<String>;

/// The matrix currency covers targets in the two fixture assemblies (rendered
/// as their full name) AND — for a cell with [`Cell::decls`] — targets
/// declared in one of that cell's OWN decl files, rendered as their
/// **declaration site** (`pj:<decl index>:<byte range>`): FCS type-qualifies a
/// union case's full name while our export paths are container-qualified, so
/// names cannot be the project currency, but the declaration site is exact on
/// both sides — go-to-def identity. A use into the probe file itself (a
/// contest `let`, a fresh binder) stays out of the currency on both sides.
fn fcs_resolved(file: &FileUses, probe: TextRange, decl_paths: &[PathBuf]) -> Resolved {
    let start: usize = usize::from(probe.start());
    let end: usize = usize::from(probe.end());
    file.uses
        .iter()
        .filter(|u| u.start != u.end)
        .filter(|u| {
            matches!(
                u.assembly.as_deref(),
                Some("SemaAutoOpenFixture") | Some("SemaFSharpAbbrevFixture")
            ) || u
                .decl
                .as_ref()
                .is_some_and(|d| decl_paths.iter().any(|p| p == &d.file))
        })
        .find(|u| u.start <= start && u.end >= end)
        .and_then(|u| {
            if let Some(d) = u
                .decl
                .as_ref()
                .filter(|d| decl_paths.iter().any(|p| p == &d.file))
            {
                let i = decl_paths
                    .iter()
                    .position(|p| p == &d.file)
                    .expect("filtered");
                Some(format!("pj:{i}:{}..{}", d.start, d.end))
            } else {
                u.full_name.clone()
            }
        })
}

/// Resolve the probe file against ITS cell's decl files (Compile-order
/// preceding, threaded via [`resolve_project`]) and render the probe's
/// resolution in the matrix currency.
fn our_resolved(env: &AssemblyEnv, decls: &[&str], src: &str, probe: TextRange) -> Resolved {
    let parse_clean = |text: &str| {
        let parsed = parse(text);
        assert!(
            parsed.errors.is_empty(),
            "parse errors in {text:?}: {:?}",
            parsed.errors
        );
        ImplFile::cast(parsed.root).expect("impl file")
    };
    let files: Vec<ImplFile> = decls
        .iter()
        .copied()
        .chain(std::iter::once(src))
        .map(parse_clean)
        .collect();
    let project = resolve_project(&files, env);
    let (probe_file, decl_files) = project
        .files()
        .split_last()
        .expect("at least the probe file");
    // A decl file's exported item, rendered at its DECLARATION SITE — the
    // decl-file index plus the defining binder's byte range, matching
    // [`fcs_resolved`]'s rendering of a project-decl target. The PROBE file's
    // own items are deliberately absent: an in-file binding (a contest `let`,
    // a fresh binder) is "nothing" on both sides of the bijection.
    let decl_item_path = |id: ItemId| -> Option<String> {
        decl_files.iter().enumerate().find_map(|(i, f)| {
            f.exports().iter().find(|e| e.id() == id).map(|e| {
                let r = f
                    .def(e.def().expect("own-arena def in an impl-only fold"))
                    .range;
                format!(
                    "pj:{i}:{}..{}",
                    usize::from(r.start()),
                    usize::from(r.end())
                )
            })
        })
    };
    match probe_file.resolution_at(probe) {
        // `entity_full_name` renders the ENCLOSING chain too — an entity
        // nested in a module (an exception or submodule declared inside
        // `module Demo.MOpen.<Shape>`) has an empty IL namespace, so joining
        // `namespace + name` would drop the module path and never match FCS.
        Some(Resolution::Entity(h)) => Some(env.entity_full_name(h)),
        Some(Resolution::Member { parent, idx }) => {
            let m = env.member_at(parent, idx);
            let member = match m {
                Member::Method(x) => x.source_name.as_deref().unwrap_or(&x.name),
                Member::Field(x) => &x.name,
                Member::Property(x) => &x.name,
                Member::Event(x) => &x.name,
            };
            Some(format!("{}.{}", env.entity_full_name(parent), member))
        }
        None | Some(Resolution::Deferred(_)) => None,
        // An `Item` into one of the cell's DECL files is a project-half target
        // and joins the currency; the probe file's own items (a contest `let`)
        // and locals (a fresh binder) are "nothing" on both sides — a
        // divergence (we bind an in-file binder where FCS binds the assembly
        // or a decl file, or vice versa) still surfaces as a mismatch.
        Some(Resolution::Item(id)) => decl_item_path(id),
        Some(Resolution::Local(_)) => None,
        Some(other) => panic!("unexpected resolution for an assembly probe: {other:?}"),
    }
}

/// Run a fold matrix: diff every cell against FCS under the exact bijection,
/// with `known_gaps` as the certain-implies-exact ratchet — each listed cell
/// must remain *exactly* "we defer while FCS resolves"; naming a target, or
/// FCS falling silent, fails. `fcs_pins` additionally pins a cell's FCS-side
/// value to an exact rendering: a gap entry alone only asserts FCS resolves
/// *something*, which cannot hold a claim about WHICH contestant FCS picked
/// (codex review of the project-half grid) — pin the cells whose reason text
/// makes such a claim. `file_prefix` names the per-cell temp files (they must
/// not collide across matrices in one test binary).
pub fn run_matrix(
    cells: &[Cell],
    known_gaps: &[(&str, &str)],
    fcs_pins: &[(&str, String)],
    file_prefix: &str,
) {
    let autoopen = ensure_autoopen_fixture_built();
    let abbrev = ensure_abbrev_fixture_built();

    let env = {
        let a = std::fs::read(autoopen).expect("read autoopen fixture dll");
        let b = std::fs::read(abbrev).expect("read abbrev fixture dll");
        let va = Ecma335Assembly::parse(&a).expect("parse autoopen fixture");
        let vb = Ecma335Assembly::parse(&b).expect("parse abbrev fixture");
        AssemblyEnv::from_views(&[va, vb]).expect("build AssemblyEnv")
    };

    // Build one PROBE file per cell — each gets a distinct filename (hence its
    // own anonymous module) — plus one file per project DECL, so a SINGLE
    // `uses-project` invocation type-checks them all with one .NET startup.
    // Spawning `fcs-dump` per cell dominated the run (~30 s of repeated
    // runtime startup), against the repo's amortise-startup rule. Decl files
    // go FIRST in the Compile order (a probe file references only earlier
    // files); the cells stay isolated by FQN, so one shared FCS project
    // cannot cross-contaminate them, while our side resolves each cell
    // against its own decls only ([`our_resolved`]).
    struct BuiltCell {
        decls: Vec<(PathBuf, String)>,
        path: PathBuf,
        src: String,
        probe: TextRange,
    }
    // Decl files DEDUPE by content across the batch: several cells share one
    // shape's decl source, and writing it once per cell would declare the
    // same namespace/module repeatedly in the one FCS project — duplicate
    // definitions that poison resolution for every probe of that shape.
    let mut decl_files: Vec<(&str, PathBuf)> = Vec::new();
    let built: Vec<BuiltCell> = cells
        .iter()
        .map(|cell| {
            let (src, probe) = cell_source(cell);
            let path = temp_fs_file(file_prefix, &src);
            let decls = cell
                .decls
                .iter()
                .map(|d| {
                    let dp = match decl_files.iter().find(|(s, _)| s == d) {
                        Some((_, p)) => p.clone(),
                        None => {
                            let p = temp_fs_file(&format!("{file_prefix}_decl"), d);
                            decl_files.push((d, p.clone()));
                            p
                        }
                    };
                    (dp, (*d).to_string())
                })
                .collect();
            BuiltCell {
                decls,
                path,
                src,
                probe,
            }
        })
        .collect();
    let mut paths: Vec<&Path> = Vec::new();
    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    for (dsrc, dp) in &decl_files {
        paths.push(dp.as_path());
        sources.push((dp.clone(), (*dsrc).to_string()));
    }
    for b in &built {
        paths.push(b.path.as_path());
        sources.push((b.path.clone(), b.src.clone()));
    }
    let json = invoke_fcs_dump_project_with_refs(&paths, &[autoopen, abbrev]);
    let fcs_files = parse_fcs_uses_project(&json, &sources);
    for (p, _) in &sources {
        let _ = std::fs::remove_file(p);
    }

    let mut mismatches: Vec<String> = Vec::new();
    for (cell, b) in cells.iter().zip(built.iter()) {
        let BuiltCell {
            decls,
            path,
            src,
            probe,
        } = b;
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for cell {:?} ({path:?})", cell.label));
        let decl_paths: Vec<PathBuf> = decls.iter().map(|(p, _)| p.clone()).collect();
        let fcs = fcs_resolved(fu, *probe, &decl_paths);
        let ours = our_resolved(&env, cell.decls, src, *probe);

        if let Some((_, want)) = fcs_pins.iter().find(|(label, _)| *label == cell.label)
            && fcs.as_deref() != Some(want.as_str())
        {
            mismatches.push(format!(
                "  {} [FCS PIN]\n    FCS no longer resolves the probe to the pinned target\n    \
                 want: {:?}\n    FCS:  {:?}",
                cell.label, want, fcs
            ));
        }

        if let Some((_, reason)) = known_gaps.iter().find(|(label, _)| *label == cell.label) {
            if ours.is_some() || fcs.is_none() {
                mismatches.push(format!(
                    "  {} [KNOWN GAP: {}]\n    no longer behaves as the gap describes — if fixed, \
                     delete its KNOWN_GAPS entry; if we now name a target, that is a wrong \
                     resolution\n    FCS:  {:?}\n    ours: {:?}",
                    cell.label, reason, fcs, ours
                ));
            }
            continue;
        }

        if fcs != ours {
            mismatches.push(format!(
                "  {}\n    source: {:?}\n    FCS:  {:?}\n    ours: {:?}",
                cell.label,
                src.replace('\n', "\\n"),
                fcs,
                ours
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} fold-matrix cells disagree with FCS:\n{}",
        mismatches.len(),
        cells.len(),
        mismatches.join("\n")
    );
}
