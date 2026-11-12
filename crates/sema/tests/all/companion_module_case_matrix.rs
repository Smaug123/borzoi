//! The **companion-module matrix**: a `[<RequireQualifiedAccess>] type Kind`
//! and its companion `module Kind` in one container, crossed with where the
//! `Kind.ArrayFold` reference sits and whether an earlier file declares a
//! same-leaf case, diffed against FCS.
//!
//! ## Why a matrix
//!
//! The type-and-companion-module pair is the single most common shape in real
//! F# (`type Kind` + `module Kind` with `toString`), and it sits exactly on the
//! seam between two resolution channels: the 2-segment type-qualified case
//! (`Resolver::type_case_path`) and the qualified-value / cross-file paths it
//! stands down for. Standing down was documented as costing only availability;
//! it does not — when an earlier file declares a same-named type with the same
//! case leaf, the fall-through *commits* that earlier case, a wrong
//! go-to-definition. One cell of this product found that; the product is small
//! and enumerable, so the machine should own it.
//!
//! ## The grid
//!
//! - **companion**: absent / a `module Kind` that provably lacks `ArrayFold` /
//!   one that owns it / a module *abbreviation* named `Kind` (target
//!   unmodelled).
//! - **site**: the reference sits beside the companion, or *inside* it (F#'s
//!   FS0039 own-name rule means the module is not in scope as a head there).
//! - **earlier file**: a `namespace Lib` file declaring `type Kind` with the
//!   same `ArrayFold` leaf, or nothing.
//! - **position**: expression or pattern.
//!
//! ## The property
//!
//! Certain-implies-exact: when we commit a definition site, FCS must report
//! *that* site. Deferring makes no claim. Non-vacuity floors keep the matrix
//! from passing by declining everything.

use borzoi_cst::parser::parse;
use std::path::{Path, PathBuf};

use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, ResolvedProject, SourceFile, SyntaxRecovery,
    qualified_names, resolve_project_files,
};
use rowan::TextRange;

use crate::common::{invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_tree};
use crate::resolve_signatures::source_file;

/// What sits next to `type Kind` in `module ForAnalyzer`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Companion {
    /// No same-named module at all — the control.
    Absent,
    /// A real nested `module Kind` with no `ArrayFold`.
    Lacks,
    /// A real nested `module Kind` whose own `ArrayFold` owns the residual.
    Owns,
    /// A module abbreviation `module Kind = Target`, whose target's members
    /// sema does not model.
    Abbrev,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    companion: Companion,
    /// The reference sits *inside* the companion module (only meaningful when
    /// one exists with a body).
    inside: bool,
    /// An earlier file declares `Lib.Kind` with the same `ArrayFold` leaf.
    earlier_file: bool,
    /// Pattern position rather than expression position.
    pattern: bool,
}

impl Cell {
    fn label(self) -> String {
        format!(
            "{:?}{}{}{}",
            self.companion,
            if self.inside { " inside" } else { "" },
            if self.earlier_file {
                " +earlier"
            } else {
                " solo"
            },
            if self.pattern { " pat" } else { " expr" },
        )
    }
}

/// The cells FCS resolves and we deliberately decline, sorted.
///
/// Both are the same conservatism: a module-like name that owns `ArrayFold`
/// blocks the type's case even in **pattern** position, where FCS backtracks
/// past it (a plain `let` value is no pattern constructor, and an abbreviation's
/// target is unmodelled). `DeclKinds` records neither the target of an
/// abbreviation nor `[<Literal>]`-ness — and a literal *is* a constant pattern —
/// so neither is provable here.
const KNOWN_GAPS: [&str; 4] = [
    "Abbrev +earlier pat",
    "Abbrev solo pat",
    "Owns +earlier pat",
    "Owns solo pat",
];

/// The earlier file: a `Lib.Kind` whose `ArrayFold` leaf collides with the one
/// declared inside `Lib.ForAnalyzer`.
const EARLIER: &str = "namespace Lib\n\ntype Kind =\n    | ArrayFold\n    | Const\n";

/// The probe reference, at `indent`.
fn probe(indent: &str, pattern: bool) -> String {
    if pattern {
        format!("{indent}let probe x = match x with Kind.ArrayFold -> 1 | _ -> 0\n")
    } else {
        format!("{indent}let probe = Kind.ArrayFold\n")
    }
}

/// The probe file's source for one cell.
fn probe_source(cell: Cell) -> String {
    let mut s = String::from(
        "namespace Lib\n\nmodule ForAnalyzer =\n    [<RequireQualifiedAccess>]\n    type Kind =\n        | ArrayFold\n        | Const\n\n",
    );
    match cell.companion {
        Companion::Absent => {}
        Companion::Lacks => {
            s.push_str("    module Kind =\n        let describe = 1\n");
            if cell.inside {
                s.push_str(&probe("        ", cell.pattern));
            }
        }
        Companion::Owns => {
            s.push_str("    module Kind =\n        let ArrayFold = 99\n");
            if cell.inside {
                s.push_str(&probe("        ", cell.pattern));
            }
        }
        Companion::Abbrev => {
            s.push_str(
                "    module Target =\n        let ArrayFold = 7\n    module Kind = Target\n",
            );
        }
    }
    if !cell.inside {
        s.push('\n');
        s.push_str(&probe("    ", cell.pattern));
    }
    s
}

/// A definition site: `(file index, byte range)`.
type Site = (usize, (usize, usize));

/// Where a resolution points, with "nothing committed" as a value.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// No commitment — no resolution, or a `Deferred` one. Makes no claim.
    Nothing,
    At(Site),
}

fn our_verdict(proj: &ResolvedProject, probe_idx: usize, span: TextRange) -> Verdict {
    let Some(res) = proj.file(probe_idx).resolution_at(span) else {
        return Verdict::Nothing;
    };
    if matches!(res, Resolution::Deferred(_) | Resolution::Unresolved) {
        return Verdict::Nothing;
    }
    if let Some((idx, def)) = proj.item_def(res) {
        return Verdict::At((idx, (def.range.start().into(), def.range.end().into())));
    }
    match proj.file(probe_idx).resolved_def(res) {
        Some(def) => Verdict::At((
            probe_idx,
            (def.range.start().into(), def.range.end().into()),
        )),
        // An assembly / member resolution has no project site; with an empty
        // env this is unreachable, but it is a commitment we cannot compare, so
        // report it as such rather than silently passing.
        None => panic!("resolution {res:?} names no project definition"),
    }
}

#[test]
fn companion_module_case_matrix_agrees_with_fcs() {
    let mut cells: Vec<Cell> = Vec::new();
    for companion in [
        Companion::Absent,
        Companion::Lacks,
        Companion::Owns,
        Companion::Abbrev,
    ] {
        for inside in [false, true] {
            // Only a companion with a body can host the reference.
            if inside && !matches!(companion, Companion::Lacks | Companion::Owns) {
                continue;
            }
            for earlier_file in [false, true] {
                for pattern in [false, true] {
                    cells.push(Cell {
                        companion,
                        inside,
                        earlier_file,
                        pattern,
                    });
                }
            }
        }
    }

    let mut commits = 0usize;
    let mut gaps: Vec<String> = Vec::new();
    assert_eq!(cells.len(), 24, "the grid shape changed");
    let mut wrong: Vec<String> = Vec::new();

    for cell in cells {
        let src = probe_source(cell);
        let mut files: Vec<(&str, &str)> = Vec::new();
        if cell.earlier_file {
            files.push(("Types.fs", EARLIER));
        }
        files.push(("ForAnalyzer.fs", src.as_str()));
        let probe_idx = files.len() - 1;

        let (root, written) = temp_fs_tree("companion_case", &files);
        let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let json = invoke_fcs_dump_project_with_refs(&paths, &[]);
        let fcs_files = parse_fcs_uses_project(&json, &written);

        let recoveries: Vec<SyntaxRecovery> = files
            .iter()
            .map(|(_, s)| SyntaxRecovery::of(&parse(s)))
            .collect();
        let srcs: Vec<SourceFile> = files.iter().map(|(rel, s)| source_file(rel, s)).collect();
        let full_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
        let qnofs = qualified_names(&srcs, &full_paths);
        let input: Vec<ProjectFile> = srcs
            .into_iter()
            .zip(qnofs)
            .zip(recoveries)
            .map(|((file, qnof), recovery)| ProjectFile::new(file, qnof, recovery))
            .collect();
        let proj = resolve_project_files(&input, &AssemblyEnv::default());

        let start = src.find("Kind.ArrayFold").expect("the probe is present");
        assert!(
            src[start + 1..].find("Kind.ArrayFold").is_none(),
            "the probe must be unique in {src:?}"
        );
        let end = start + "Kind.ArrayFold".len();
        let span = TextRange::new(
            u32::try_from(start).unwrap().into(),
            u32::try_from(end).unwrap().into(),
        );

        // FCS's verdict for the whole `Kind.ArrayFold` span.
        let by_index: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let fcs_probe = fcs_files
            .iter()
            .find(|f| f.path.file_name() == written[probe_idx].0.file_name())
            .expect("FCS reported uses for the probe file");
        let fcs = fcs_probe
            .uses
            .iter()
            .find(|u| u.start == start && u.end == end)
            .and_then(|u| u.decl.as_ref())
            .map_or(Verdict::Nothing, |d| {
                let idx = by_index
                    .iter()
                    .position(|p| p.file_name() == Path::new(&d.file).file_name())
                    .unwrap_or_else(|| panic!("FCS declared in an unknown file {:?}", d.file));
                Verdict::At((idx, (d.start, d.end)))
            });

        let ours = our_verdict(&proj, probe_idx, span);
        let _ = std::fs::remove_dir_all(&root);

        match (&ours, &fcs) {
            // Certain-implies-exact: a commitment must be FCS's exact site.
            (Verdict::At(a), Verdict::At(b)) if a == b => commits += 1,
            (Verdict::At(_), _) => wrong.push(format!(
                "{}: we committed {ours:?}, FCS says {fcs:?}\n{src}",
                cell.label()
            )),
            // Availability only — FCS resolved and we declined.
            (Verdict::Nothing, Verdict::At(_)) => gaps.push(cell.label()),
            (Verdict::Nothing, Verdict::Nothing) => {}
        }
    }

    assert!(
        wrong.is_empty(),
        "wrong targets:\n{}",
        wrong.join("\n---\n")
    );
    // The ratchet only tightens: the deferrals this slice makes on purpose are
    // listed, so a gap that starts naming a target — or one FCS stops resolving
    // — fails rather than passing silently.
    gaps.sort();
    assert_eq!(gaps, KNOWN_GAPS, "the availability gaps moved");
    // Non-vacuity: the matrix must be deciding cells, not declining to green.
    assert_eq!(
        commits,
        24 - KNOWN_GAPS.len(),
        "every non-gap cell must agree exactly"
    );
}
