//! The source-doc differential over **whole projects** loaded through the LSP's
//! runtime path: the workspace's `.fsproj` evaluation (Compile order, `#if`
//! symbols, `<LangVersion>`), the project-assets reference closure and the
//! `AssemblyEnv` built from it, and the signature-aware `resolve_project`
//! fold. So the doc of a `Resolution::Item` — a declaration in an *earlier*
//! Compile file, or a signature's — is graded as hover meets it, not only the
//! single-file cases of the rest of this group.
//!
//! - [`fixture_project_docs_agree_with_fcs`] (always on) builds a multi-file
//!   project with a `.fsi`, stages it as restored, and holds a list of
//!   **per-point obligations**: named cross-file occurrences that must be
//!   graded as agreeing on a doc. A count floor can rot silently as the
//!   corpus grows (an unrelated gain hides a lost case); a named site cannot.
//! - [`pinned_projects_docs_agree_with_fcs`] (`#[ignore]`d; CI runs it in the
//!   `corpus-diff` job over the pinned project corpus) grades every project in
//!   `BORZOI_PROJECT_LIST` with FCS reading **exactly** the reference set our
//!   `AssemblyEnv` was built from (`--noframework`), and pins the outcome in an
//!   exact manifest (`tests/manifests/xml_doc_source_projects.txt`).
//!
//! The gate is the rest of the group's: certain-implies-exact, and in a file
//! FCS checks cleanly an unpaired doc or a site whose FCS records disagree is a
//! failure too.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::workspace::Workspace;
use borzoi_cst::language_version::LanguageVersion;
use borzoi_oracle_harness::corpus_key::Positions;
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};
use borzoi_oracle_harness::panic_silence::silence_panics_here;
use borzoi_sema::resolve_project_files;

use super::harness::{
    Graded, OracleFile, OurProject, Verdict, census, describe, failures, fcs_check_with, grade,
};
use crate::common::runtime_project_state_files;

/// A project loaded the way the LSP loads it, with the reference set its
/// `AssemblyEnv` was built from.
struct Loaded {
    ours: OurProject,
    paths: Vec<PathBuf>,
    refs: Vec<PathBuf>,
    symbols: Vec<String>,
    lang_version: Option<String>,
}

fn load(project: &Path, workspace: &mut Workspace, sema: &mut SemanticState) -> Loaded {
    let docs: HashMap<lsp_types::Url, String> = HashMap::new();
    let parses = sema
        .parses_for_project(project, workspace, &docs)
        .unwrap_or_else(|| panic!("the LSP declined to load {project:?}"))
        .clone();
    let lang_version = {
        let v = workspace.lang_version_for_project(project);
        (v != LanguageVersion::DEFAULT).then(|| v.to_string())
    };
    let mut symbols: Vec<String> = workspace.symbols_for_project(project).into_iter().collect();
    symbols.sort();
    let dotnet_root = workspace.dotnet_root_for_project(project);
    let target_framework = workspace.served_tfm_for_project(project);
    let _silence = silence_panics_here();
    let (refs, retryable) = sema.env_reference_dlls_for_project(
        project,
        dotnet_root.as_deref(),
        &target_framework,
        workspace,
    );
    assert!(
        !retryable,
        "the reference set for {project:?} was not stable — rerun"
    );
    let env = sema.assembly_env_for_project(
        project,
        dotnet_root.as_deref(),
        &target_framework,
        workspace,
    );
    drop(_silence);
    let resolved = resolve_project_files(&parses.files, &env);
    Loaded {
        ours: OurProject {
            texts: parses.texts.iter().map(|t| t.to_string()).collect(),
            files: parses.files.clone(),
            resolved,
        },
        paths: parses.paths.clone(),
        refs,
        symbols,
        lang_version,
    }
}

/// FCS's records for `loaded`, reading `refs` as the whole reference set when
/// `exclusive`, else alongside the SDK's own.
fn oracle(loaded: &Loaded, exclusive: bool) -> Vec<OracleFile> {
    let defines: Vec<&str> = loaded.symbols.iter().map(String::as_str).collect();
    let refs: Vec<&Path> = if exclusive {
        loaded.refs.iter().map(PathBuf::as_path).collect()
    } else {
        Vec::new()
    };
    fcs_check_with(
        &loaded.paths,
        &defines,
        &refs,
        exclusive,
        loaded.lang_version.as_deref(),
    )
}

// ============================================================================
// The fixture project and its obligations
// ============================================================================

const SHAPES_FSI: &str = "\
module Lib.Shapes

/// A shape, from the signature.
type Shape =
    /// Round, from the signature.
    | Circle of float
    | Square of float

/// The area, from the signature.
val area: Shape -> float

/// The unit circle.
val unit: Shape
";

const SHAPES_FS: &str = "\
module Lib.Shapes

/// A shape, from the implementation.
type Shape =
    /// Round, from the implementation.
    | Circle of float
    | Square of float

/// The area, from the implementation.
let area s =
    match s with
    | Circle r -> 3.14 * r * r
    | Square a -> a * a

let unit = Circle 1.0

/// Inside its own file, the implementation's doc.
let double s = area s * 2.0
";

const MATHS_FS: &str = "\
module Lib.Maths

/// <summary>Adds one.</summary>
/// <param name=\"x\">The input.</param>
let inc (x: int) = x + 1

/// A colour.
type Colour =
    /// Red.
    | Red
    /// Green.
    | Green

/// A counter.
type Counter() =
    /// The start.
    static member Start = 0

/// Not on anything here.
// an ordinary comment
/// The constant.
let answer = 42
";

const USE_FS: &str = "\
module App.Use

open Lib

let a = Maths.inc 1
let b = Maths.Colour.Red
let c : Maths.Colour = Maths.Green
let d = Maths.Counter.Start
let e = Maths.answer
let f = Shapes.area Shapes.unit
let g = Shapes.Circle 2.0
let h : Shapes.Shape = g
";

/// One obligation: in file `file`, the occurrence ending at the end of the
/// `nth` match of the identifier `needle` (a qualified use `M.x` is graded over
/// its whole dotted span) must be graded, and agree on exactly `doc`.
struct Obligation {
    file: &'static str,
    needle: &'static str,
    nth: usize,
    doc: &'static [&'static str],
}

const OBLIGATIONS: &[Obligation] = &[
    // Cross-file values, types, cases and a static member, through the fold's
    // `Resolution::Item`.
    Obligation {
        file: "Use.fs",
        needle: "inc",
        nth: 0,
        doc: &[
            " <summary>Adds one.</summary>",
            " <param name=\"x\">The input.</param>",
        ],
    },
    Obligation {
        file: "Use.fs",
        needle: "Red",
        nth: 0,
        doc: &[" Red."],
    },
    Obligation {
        file: "Use.fs",
        needle: "Green",
        nth: 0,
        doc: &[" Green."],
    },
    Obligation {
        file: "Use.fs",
        needle: "answer",
        nth: 0,
        doc: &[" The constant."],
    },
    // Through the signature: a use in another file binds the `.fsi`'s symbol.
    Obligation {
        file: "Use.fs",
        needle: "area",
        nth: 0,
        doc: &[" The area, from the signature."],
    },
    Obligation {
        file: "Use.fs",
        needle: "Circle",
        nth: 0,
        doc: &[" Round, from the signature."],
    },
    // Inside the implementation, its own doc.
    Obligation {
        file: "Shapes.fs",
        needle: "area",
        nth: 2,
        doc: &[" The area, from the implementation."],
    },
];

/// The fixture: a signature-paired file, a plain one, and a consumer, staged
/// as a restored project the LSP's runtime path loads.
#[test]
fn fixture_project_docs_agree_with_fcs() {
    let files = [
        ("Shapes.fsi", SHAPES_FSI),
        ("Shapes.fs", SHAPES_FS),
        ("Maths.fs", MATHS_FS),
        ("Use.fs", USE_FS),
    ];
    let (mut state, uris) = runtime_project_state_files(&files);
    let project = uris[0]
        .to_file_path()
        .unwrap()
        .parent()
        .unwrap()
        .join("P.fsproj");
    let loaded = load(&project, &mut state.workspace, &mut state.semantic);
    assert_eq!(loaded.paths.len(), files.len(), "{:?}", loaded.paths);
    // The staged env holds only `System.Runtime`, so FCS reads the SDK's
    // references beside it; nothing graded here depends on an assembly.
    let fcs = oracle(&loaded, false);
    for f in &fcs {
        assert!(f.ok && !f.has_errors(), "FCS rejects the fixture: {f:?}");
    }
    let graded = grade(&loaded.ours, &fcs);
    let named: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, *t)).collect();
    let bad = failures(&graded);
    assert!(
        bad.is_empty(),
        "{}",
        bad.iter()
            .map(|g| describe(&named, g))
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!("fixture census: {:#?}", census(&graded));

    let mut unmet = Vec::new();
    for o in OBLIGATIONS {
        let file = files.iter().position(|(n, _)| *n == o.file).unwrap();
        let text = files[file].1;
        let at = text
            .match_indices(o.needle)
            .nth(o.nth)
            .unwrap_or_else(|| panic!("{} has no occurrence {} of {:?}", o.file, o.nth, o.needle))
            .0;
        let end = at + o.needle.len();
        let site = graded
            .iter()
            .find(|g| g.file == file && usize::from(g.range.end()) == end && !g.is_definition);
        let expected: Vec<String> = o.doc.iter().map(|l| l.to_string()).collect();
        // Every use in `Use.fs` is of a declaration in another file.
        let may_be_same_file = o.file != "Use.fs";
        let met = site.is_some_and(|g| {
            (may_be_same_file || g.declared_in.is_some_and(|d| d != file))
                && g.verdict == (Verdict::Agree { attached: true })
                && g.fcs_lines.as_ref() == Some(&expected)
        });
        if !met {
            unmet.push(format!(
                "{} {:?}#{}: {:?}",
                o.file,
                o.needle,
                o.nth,
                site.map(|g| (&g.verdict, &g.fcs_lines))
            ));
        }
    }
    assert!(
        unmet.is_empty(),
        "obligations not met (each must be graded as agreeing on the named doc):\n{}",
        unmet.join("\n")
    );
}

// ============================================================================
// The pinned project corpus
// ============================================================================

/// `path`'s key in the manifest: its repository's directory name and its path
/// inside it, so the key is the same wherever the corpus is materialised.
fn repo_key(path: &Path) -> String {
    let root = path
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or_else(|| panic!("{} is not inside a checkout", path.display()));
    let name = root.file_name().unwrap().to_string_lossy();
    let rel = path.strip_prefix(root).unwrap();
    format!(
        "{name}/{}",
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    )
}

/// The manifest spelling of a graded occurrence that is not an agreement.
fn entry(g: &Graded) -> Option<String> {
    let fcs = match g.fcs_lines.as_deref() {
        Some(lines) if !lines.is_empty() => "fcs-doc",
        Some(_) => "fcs-none",
        None => "fcs-unpaired",
    };
    match &g.verdict {
        Verdict::Agree { .. } => None,
        Verdict::Declined(why) => Some(format!("declined {why:?} {fcs}")),
        Verdict::Unpaired => g
            .ours
            .as_deref()
            .is_some_and(|l| !l.is_empty())
            .then(|| "unpaired-doc".to_string()),
        Verdict::OracleAmbiguous => Some("oracle-ambiguous".to_string()),
        Verdict::Diverge { .. } | Verdict::ElaborationDiverges { .. } => {
            Some("DIVERGE".to_string())
        }
    }
}

#[test]
#[ignore = "whole-project source-doc differential over restored projects; set BORZOI_PROJECT_LIST and run with --ignored under nix develop"]
fn pinned_projects_docs_agree_with_fcs() {
    let Some(list) = std::env::var_os("BORZOI_PROJECT_LIST") else {
        eprintln!("BORZOI_PROJECT_LIST unset; skipping. See the module docs.");
        return;
    };
    let projects: Vec<PathBuf> = std::env::split_paths(&list).collect();
    assert!(!projects.is_empty(), "BORZOI_PROJECT_LIST is empty");
    let mut manifest = Vec::new();
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    let mut bad_sites = Vec::new();
    let mut cross_file_docs = 0usize;
    for project in &projects {
        let mut workspace = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
        let mut sema = SemanticState::new();
        let loaded = load(project, &mut workspace, &mut sema);
        assert!(
            !loaded.refs.is_empty(),
            "{}: composed no references — restore it first",
            project.display()
        );
        let fcs = oracle(&loaded, true);
        let errors: usize = fcs
            .iter()
            .map(|f| {
                f.diagnostics
                    .iter()
                    .filter(|d| d.severity == "Error")
                    .count()
            })
            .sum();
        let graded = grade(&loaded.ours, &fcs);
        let names: Vec<String> = loaded.paths.iter().map(|p| repo_key(p)).collect();
        let named: Vec<(&str, &str)> = names
            .iter()
            .zip(&loaded.ours.texts)
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        for g in failures(&graded) {
            bad_sites.push(describe(&named, g));
        }
        for (k, v) in census(&graded) {
            *totals.entry(k).or_default() += v;
        }
        // Cross-file agreements: a use in one file graded against a doc
        // declared in another.
        let cross: Vec<&Graded> = graded
            .iter()
            .filter(|g| matches!(g.verdict, Verdict::Agree { attached: true }) && !g.is_definition)
            .filter(|g| g.declared_in.is_some_and(|d| d != g.file))
            .collect();
        cross_file_docs += cross.len();
        manifest.push(format!(
            "{} oracle-errors={errors} agree-doc={} cross-file-doc={} agree-none={}",
            repo_key(project),
            graded
                .iter()
                .filter(|g| matches!(g.verdict, Verdict::Agree { attached: true }))
                .count(),
            cross.len(),
            graded
                .iter()
                .filter(|g| matches!(g.verdict, Verdict::Agree { attached: false }))
                .count(),
        ));
        for g in &graded {
            // A decline where FCS has no doc costs nothing a reader sees, and
            // a signature-heavy project has thousands: counted per file and
            // cause (`from_counted`), not listed per site.
            if let Verdict::Declined(why) = &g.verdict
                && g.fcs_lines.as_deref().is_some_and(<[String]>::is_empty)
            {
                manifest.push(format!("{} declined {why:?} fcs-none", names[g.file]));
                continue;
            }
            if let Some(e) = entry(g) {
                let text = &loaded.ours.texts[g.file];
                manifest.push(format!(
                    "{}:{} {} {:?} {} {e}",
                    names[g.file],
                    Positions::new(text).at(usize::from(g.range.start())),
                    g.name.replace(char::is_whitespace, "_"),
                    g.def_kind,
                    if g.is_definition { "def" } else { "use" },
                ));
            }
        }
    }
    eprintln!(
        "xmldoc-projects: {} projects, {cross_file_docs} cross-file docs agreed; census {totals:#?}",
        projects.len()
    );
    assert!(
        bad_sites.is_empty(),
        "{} occurrence(s) where hover would show a doc FCS does not attach:\n{}",
        bad_sites.len(),
        bad_sites.join("\n")
    );
    let manifest = Manifest::from_counted(manifest).unwrap_or_else(|e| panic!("manifest: {e}"));
    check(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/manifests/xml_doc_source_projects.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 BORZOI_PROJECT_LIST=… nix develop -c cargo test -p borzoi --test all \
             xml_doc_source_diff::project::pinned -- --ignored"
        ),
    );
}
