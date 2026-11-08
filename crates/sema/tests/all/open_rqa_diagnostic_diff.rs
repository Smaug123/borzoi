//! FS0892 — `open` of a `[<RequireQualifiedAccess>]` module — graded against
//! FCS.
//!
//! FCS reports `tcModuleRequiresQualifiedAccess` (an error) once for every
//! module in the set an `open` resolves to that carries FSharp.Core's
//! `RequireQualifiedAccessAttribute` (`CheckDeclarations.fs`,
//! `TcOpenModuleOrNamespaceDecl`), at the range of the opened path, and opens
//! the module anyway. We report it as
//! [`SemaDiagnosticKind::OpenOfRequireQualifiedAccessModule`], once per `open`.
//!
//! The currency is the **set of ranges** each file carries an FS0892 at: FCS
//! may report one open more than once (one per RQA module it names), which
//! the set collapses. Every cell asserts certain-implies-exact — each range we
//! report, FCS reports — and pins its own verdict, so a cell the diagnostic
//! must cover cannot fall silent unnoticed and a cell FCS flags but we decline
//! is a listed gap rather than an accident:
//!
//! - [`Expect::Exact`]: FCS reports FS0892 in the cell, and our ranges equal
//!   FCS's;
//! - [`Expect::Gap`]: FCS reports it and we stay silent, for the stated reason;
//! - [`Expect::Clean`]: neither side reports it.
//!
//! Every cell joins one batched FCS project (one .NET request), isolated from
//! the others by the `{p}` prefix each cell's names carry; our side resolves
//! each cell's files on their own.
//!
//! The cells name the routes; the **generated projects** combine them. Each is
//! a few files of modules (attributed or not, `private`, `internal`, nested,
//! under a namespace or a top-level module, sometimes beside a type that
//! impersonates the attribute) and a probe file opening them by every route
//! the diagnostic decides and some it declines, plus fixture and FSharp.Core
//! paths. Two properties: everything we report, FCS reports; and every open the
//! generator knows names a module certainly carrying the attribute by a decided
//! route, we report — without the second, reporting nothing would pass. The run
//! prints a census of routes against both sides' verdicts.
//! `BORZOI_RQA_OPEN_CASES` sets the count for both the default run and its
//! ignored `deep_` twin.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::common::{
    ensure_rqa_open_fixture_built, env_usize_or, fsharp_core_bytes,
    invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectFile, SemaDiagnosticKind, SourceFile, SyntaxRecovery, qualified_names,
    resolve_project_files,
};

/// What a cell must show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// FCS reports FS0892 and we report exactly its ranges.
    Exact,
    /// FCS reports FS0892 and we decline, for the reason given.
    Gap(&'static str),
    /// Neither side reports FS0892.
    Clean,
}

/// One cell: a Compile-ordered list of files, `{p}` standing for the cell's
/// unique prefix.
struct Cell {
    label: &'static str,
    files: &'static [&'static str],
    expect: Expect,
}

const CELLS: &[Cell] = &[
    // ---- the module is declared earlier in the same file ----
    Cell {
        label: "same-file / anonymous root",
        files: &["[<RequireQualifiedAccess>]\nmodule {p}M =\n    let x = 1\n\nopen {p}M\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / anonymous root, long attribute spelling",
        files: &[
            "[<RequireQualifiedAccessAttribute>]\nmodule {p}M =\n    let x = 1\n\nopen {p}M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / attribute among others in one list",
        files: &[
            "[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix); RequireQualifiedAccess>]\nmodule {p}M =\n    let x = 1\n\nopen {p}M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / anonymous root, nested, by its dotted path",
        files: &[
            "module {p}A =\n    [<RequireQualifiedAccess>]\n    module M =\n        let x = 1\n\nopen {p}A.M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / anonymous root, opened inside a later module",
        files: &[
            "[<RequireQualifiedAccess>]\nmodule {p}M =\n    let x = 1\n\nmodule {p}U =\n    open {p}M\n    let y = 2\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / dotted path through the namespace being checked",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nmodule User =\n    open {p}N.M\n    let y = 2\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / dotted path through the module being checked",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nopen {p}Top.M\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / an earlier namespace block, global-rooted",
        files: &[
            "namespace {p}A\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nnamespace {p}B\n\nmodule U =\n    open global.{p}A.M\n    let y = 2\n",
        ],
        expect: Expect::Gap(
            "an earlier block of the same file, reached from the root, is not decided",
        ),
    },
    Cell {
        label: "same-file / an earlier block of the same namespace",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nnamespace {p}N\n\nmodule U =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / an earlier block of an enclosing namespace",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nnamespace {p}N.Inner\n\nmodule U =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / plain module",
        files: &["module {p}M =\n    let x = 1\n\nopen {p}M\n"],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / named module header",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nopen M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / namespace, opened from a sibling module",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nmodule User =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / nested, opened inside its parent",
        files: &[
            "module {p}Top\n\nmodule A =\n    [<RequireQualifiedAccess>]\n    module M =\n        let x = 1\n\n    open M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / nested, opened by its dotted path after the parent",
        files: &[
            "module {p}Top\n\nmodule A =\n    [<RequireQualifiedAccess>]\n    module M =\n        let x = 1\n\nopen A.M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / nested, reached through an open of its parent",
        files: &[
            "module {p}Top\n\nmodule A =\n    [<RequireQualifiedAccess>]\n    module M =\n        let x = 1\n\nopen A\nopen M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / only the parent is RQA",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\nmodule A =\n    module M =\n        let x = 1\n\nopen A.M\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / global-rooted open of the namespace being checked",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nmodule User =\n    open global.{p}N.M\n    let y = 2\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / private module",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\nmodule private M =\n    let x = 1\n\nopen M\n",
        ],
        expect: Expect::Gap("a private module's accessibility at the open is not modelled"),
    },
    Cell {
        label: "same-file / attribute impersonated by a project type",
        files: &[
            "module {p}Top\n\ntype RequireQualifiedAccessAttribute() =\n    inherit System.Attribute()\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nopen M\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / attribute through an abbreviation",
        files: &[
            "module {p}Top\n\ntype Rqa = RequireQualifiedAccessAttribute\n\n[<Rqa>]\nmodule M =\n    let x = 1\n\nopen M\n",
        ],
        expect: Expect::Gap("an attribute written through an abbreviation is not followed"),
    },
    Cell {
        label: "same-file / rec module",
        files: &[
            "module rec {p}Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nmodule User =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Gap("opens inside a rec scope are not decided"),
    },
    Cell {
        label: "same-file / module abbreviation of an RQA module",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nmodule L = M\nopen L\n",
        ],
        expect: Expect::Gap("an open through a module abbreviation is not decided"),
    },
    Cell {
        label: "same-file / open type of an RQA union",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\ntype U =\n    | A\n    | B\n\nopen type U\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "same-file / #nowarn 892",
        files: &[
            "#nowarn \"892\"\nmodule {p}Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nopen M\n",
        ],
        expect: Expect::Exact,
    },
    // ---- the module is declared in an earlier file ----
    Cell {
        label: "cross-file / fully qualified",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N.M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / through an open of its namespace",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N\nopen M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / from inside the same namespace",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "namespace {p}N\n\nmodule P =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / a top-level module header",
        files: &[
            "[<RequireQualifiedAccess>]\nmodule {p}N.M\n\nlet x = 1\n",
            "module {p}P\n\nopen {p}N.M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / nested in a module",
        files: &[
            "module {p}N.Host\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N.Host\nopen M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / global-rooted",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen global.{p}N.M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / a top-level module header, from inside its namespace",
        files: &[
            "[<RequireQualifiedAccess>]\nmodule {p}N.M\n\nlet x = 1\n",
            "namespace {p}N\n\nmodule P =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / the attribute on an internal module",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule internal M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N.M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "cross-file / namespace global",
        files: &[
            "namespace global\n\n[<RequireQualifiedAccess>]\nmodule {p}M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}M\n",
        ],
        expect: Expect::Exact,
    },
    Cell {
        label: "same-file / namespace global",
        files: &[
            "namespace global\n\n[<RequireQualifiedAccess>]\nmodule {p}M =\n    let x = 1\n\nmodule {p}U =\n    open {p}M\n    let y = 2\n",
        ],
        expect: Expect::Gap("a `namespace global` block's own modules are not decided"),
    },
    Cell {
        label: "cross-file / nested in a private top-level module",
        files: &[
            "module private {p}N.Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N.Top.M\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "cross-file / private module",
        files: &[
            "namespace {p}N\n\n[<RequireQualifiedAccess>]\nmodule private M =\n    let x = 1\n",
            "namespace {p}N\n\nmodule P =\n    open M\n    let y = 2\n",
        ],
        expect: Expect::Gap("a private module's accessibility at the open is not modelled"),
    },
    Cell {
        label: "cross-file / attribute impersonated in the declaring file",
        files: &[
            "namespace {p}N\n\ntype RequireQualifiedAccessAttribute() =\n    inherit System.Attribute()\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N.M\n",
        ],
        expect: Expect::Clean,
    },
    Cell {
        label: "cross-file / plain module",
        files: &[
            "namespace {p}N\n\nmodule M =\n    let x = 1\n",
            "module {p}P\n\nopen {p}N.M\n",
        ],
        expect: Expect::Clean,
    },
    // ---- the module is in a referenced assembly ----
    Cell {
        label: "assembly / fully qualified",
        files: &["module {p}P\n\nopen RqaAsm.Strict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / plain module",
        files: &["module {p}P\n\nopen RqaAsm.Loose\n"],
        expect: Expect::Clean,
    },
    Cell {
        label: "assembly / through an open of its namespace",
        files: &["module {p}P\n\nopen RqaAsm\nopen Strict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / nested, fully qualified",
        files: &["module {p}P\n\nopen RqaAsm.Outer.InnerStrict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / nested, through an open of its parent",
        files: &["module {p}P\n\nopen RqaAsm.Outer\nopen InnerStrict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / suffixed companion module",
        files: &["module {p}P\n\nopen RqaAsm.Bag\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / child of an auto-open module, by its short name",
        files: &["module {p}P\n\nopen RqaAsm\nopen ViaAuto\n"],
        expect: Expect::Gap(
            "a path reached through an auto-open module's prefix may name a module FCS does not",
        ),
    },
    Cell {
        label: "assembly / child of an auto-open module, fully qualified",
        files: &["module {p}P\n\nopen RqaAsm.AutoHost.ViaAuto\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / internal module",
        files: &["module {p}P\n\nopen RqaAsm.HiddenStrict\n"],
        expect: Expect::Clean,
    },
    Cell {
        label: "assembly / root-namespace module",
        files: &["module {p}P\n\nopen RqaRootStrict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / global-rooted",
        files: &["module {p}P\n\nopen global.RqaAsm.Strict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / deeper namespace",
        files: &["module {p}P\n\nopen RqaAsm.Deep.DeepStrict\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / from inside its namespace",
        files: &["namespace RqaAsm\n\nmodule {p}U =\n    open Strict\n    let y = 2\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "assembly / from a top-level module in its namespace",
        files: &["module RqaAsm.{p}Top\n\nopen Strict\n"],
        expect: Expect::Exact,
    },
    // ---- FSharp.Core ----
    Cell {
        label: "fsharp-core / open List",
        files: &["module {p}P\n\nopen List\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "fsharp-core / open Seq",
        files: &["module {p}P\n\nopen Seq\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "fsharp-core / open Option, which is not RQA",
        files: &["module {p}P\n\nopen Option\n"],
        expect: Expect::Clean,
    },
    Cell {
        label: "fsharp-core / fully qualified List",
        files: &["module {p}P\n\nopen Microsoft.FSharp.Collections.List\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "fsharp-core / FSharp.Collections.List",
        files: &["module {p}P\n\nopen FSharp.Collections.List\n"],
        expect: Expect::Exact,
    },
    Cell {
        label: "fsharp-core / open Checked",
        files: &["module {p}P\n\nopen Checked\n"],
        expect: Expect::Clean,
    },
    Cell {
        label: "fsharp-core / open Operators",
        files: &["module {p}P\n\nopen Operators\n"],
        expect: Expect::Clean,
    },
    // ---- the file does not parse ----
    Cell {
        label: "parse error elsewhere in the file",
        files: &[
            "module {p}Top\n\n[<RequireQualifiedAccess>]\nmodule M =\n    let x = 1\n\nopen M\n\nlet broken = (1 +\n",
        ],
        expect: Expect::Gap("a file that failed to parse is not decided"),
    },
    // ---- last, because it declares a project module at an assembly module's
    //      path, which every later file of the batch would see ----
    Cell {
        label: "assembly / a project module at an assembly module's path does not reach its children",
        files: &["module RqaAsm.Outer\n\nopen InnerStrict\n"],
        expect: Expect::Clean,
    },
];

/// The env our side resolves against: FSharp.Core (which every FCS check
/// references) and the fixture.
fn env() -> AssemblyEnv {
    let fixture = std::fs::read(ensure_rqa_open_fixture_built()).expect("read rqa-open fixture");
    let core = Ecma335Assembly::parse(fsharp_core_bytes()).expect("parse FSharp.Core");
    let fx = Ecma335Assembly::parse(&fixture).expect("parse rqa-open fixture");
    AssemblyEnv::from_views(&[core, fx]).expect("build AssemblyEnv")
}

/// Our FS0892 ranges, per file of one project, resolving exactly its files.
fn our_ranges(env: &AssemblyEnv, files: &[(PathBuf, String)]) -> Vec<BTreeSet<(usize, usize)>> {
    let mut sources = Vec::new();
    let mut recoveries = Vec::new();
    for (_, src) in files {
        let parsed = parse(src);
        recoveries.push(SyntaxRecovery::of(&parsed));
        sources.push(SourceFile::Impl(
            ImplFile::cast(parsed.root).expect("impl file"),
        ));
    }
    let paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
    let qnofs = qualified_names(&sources, &paths);
    let project: Vec<ProjectFile> = sources
        .into_iter()
        .zip(qnofs)
        .zip(recoveries)
        .map(|((s, q), r)| ProjectFile::new(s, q, r))
        .collect();
    let resolved = resolve_project_files(&project, env);
    resolved
        .files()
        .iter()
        .map(|f| {
            f.diagnostics()
                .iter()
                .filter(|d| d.kind == SemaDiagnosticKind::OpenOfRequireQualifiedAccessModule)
                .map(|d| (usize::from(d.range.start()), usize::from(d.range.end())))
                .collect()
        })
        .collect()
}

/// One project, graded: its files, and per file the FS0892 ranges each side
/// reports.
struct Graded {
    files: Vec<(PathBuf, String)>,
    ours: Vec<BTreeSet<(usize, usize)>>,
    fcs: Vec<BTreeSet<(usize, usize)>>,
}

/// How many projects share one FCS request.
const PROJECTS_PER_REQUEST: usize = 100;

/// Grade `projects` (each a Compile-ordered list of sources): FCS checks them
/// batched into shared projects, isolated by the names each carries, and we
/// resolve each on its own.
fn grade(env: &AssemblyEnv, projects: &[Vec<String>]) -> Vec<Graded> {
    let fixture = ensure_rqa_open_fixture_built();
    let mut out = Vec::new();
    for chunk in projects.chunks(PROJECTS_PER_REQUEST) {
        let built: Vec<Vec<(PathBuf, String)>> = chunk
            .iter()
            .map(|files| {
                files
                    .iter()
                    .map(|src| (temp_fs_file("rqa_open", src), src.clone()))
                    .collect()
            })
            .collect();
        let sources: Vec<(PathBuf, String)> = built.iter().flatten().cloned().collect();
        let paths: Vec<&Path> = sources.iter().map(|(p, _)| p.as_path()).collect();
        let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
        let fcs_files = parse_fcs_uses_project(&json, &sources);
        for (p, _) in &sources {
            let _ = std::fs::remove_file(p);
        }
        for files in built {
            let fcs = files
                .iter()
                .map(|(path, _)| {
                    let fu = fcs_files
                        .iter()
                        .find(|f| f.path.file_name() == path.file_name())
                        .unwrap_or_else(|| panic!("no FCS result for {path:?}"));
                    fu.diagnostics
                        .as_ref()
                        .expect("the uses-project dump carries Diagnostics")
                        .iter()
                        .filter(|d| d.error_number == 892)
                        .map(|d| (d.start, d.end))
                        .collect()
                })
                .collect();
            let ours = our_ranges(env, &files);
            out.push(Graded { files, ours, fcs });
        }
    }
    out
}

#[test]
fn fs0892_cells_match_fcs() {
    let env = env();
    let projects: Vec<Vec<String>> = CELLS
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let prefix = format!("Rqa{i}");
            cell.files
                .iter()
                .map(|f| f.replace("{p}", &prefix))
                .collect()
        })
        .collect();
    // One request: the last cell must follow every other.
    assert!(CELLS.len() <= PROJECTS_PER_REQUEST);
    let graded = grade(&env, &projects);

    let mut failures = Vec::new();
    for (cell, g) in CELLS.iter().zip(&graded) {
        let Graded { files, ours, fcs } = g;
        let render = || {
            let srcs: Vec<String> = files.iter().map(|(_, s)| s.replace('\n', "\\n")).collect();
            format!("    files: {srcs:?}\n    FCS:  {fcs:?}\n    ours: {ours:?}")
        };
        let unconfirmed = ours.iter().zip(fcs).any(|(o, f)| !o.is_subset(f));
        if unconfirmed {
            failures.push(format!(
                "  {} [WRONG: we report an FS0892 FCS does not]\n{}",
                cell.label,
                render()
            ));
            continue;
        }
        let fcs_flags = fcs.iter().any(|f| !f.is_empty());
        let we_flag = ours.iter().any(|o| !o.is_empty());
        let holds = match cell.expect {
            Expect::Exact => fcs_flags && ours == fcs,
            Expect::Gap(_) => fcs_flags && !we_flag,
            Expect::Clean => !fcs_flags && !we_flag,
        };
        if !holds {
            failures.push(format!(
                "  {} [expected {:?}]\n{}",
                cell.label,
                cell.expect,
                render()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} FS0892 cells disagree with their pin:\n{}",
        failures.len(),
        CELLS.len(),
        failures.join("\n")
    );
}

// ============================================================================
// Generated projects
// ============================================================================

/// How a generated `open` reaches its target — the census dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Route {
    /// The module's full path, from the root.
    Root,
    /// The full path behind `global.`.
    Global,
    /// A generated namespace, opened to shorten the next open.
    Namespace,
    /// `open <namespace>` then the rest of the path.
    ThroughNamespace,
    /// `open <parent module>` then the child's name.
    ThroughParent,
    /// The path relative to the namespace the probe sits in.
    EnclosingNamespace,
    /// A module declared earlier in the probe file, by its path relative to
    /// the probe's container.
    SameFile,
    /// A module declared earlier in the probe file, by its full path through
    /// the block still being checked, or behind `global.`.
    SameFileRooted,
    /// A path into FSharp.Core or the fixture.
    Assembly,
    /// A path naming nothing.
    Nothing,
}

/// One generated `open`: where its path sits in the probe file, how it got
/// there, and whether the diagnostic is owed.
struct GenOpen {
    range: (usize, usize),
    route: Route,
    /// The generator knows the open names a module certainly carrying the
    /// attribute by a route the diagnostic decides, so we must report it —
    /// what keeps the subset property from holding because we report nothing.
    must_report: bool,
}

/// One generated project: its files in Compile order (the probe last) and
/// the probe's opens.
struct GenCase {
    files: Vec<String>,
    opens: Vec<GenOpen>,
}

/// A module the generator declared, by full path.
#[derive(Clone)]
struct GenModule {
    path: Vec<String>,
    /// It carries the attribute, is reachable from every file, and its
    /// declaring file is free of anything that could impersonate the
    /// attribute: the diagnostic must hold it RQA.
    certain: bool,
}

/// A tape of choices.
struct Tape {
    nums: Vec<u32>,
    pos: usize,
}

impl Tape {
    fn pick(&mut self, n: usize) -> usize {
        let v = self.nums[self.pos % self.nums.len()];
        self.pos += 1;
        v as usize % n
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.pick(one_in) == 0
    }
}

/// Fixture and FSharp.Core paths an `open` may name, and whether FS0892 is
/// owed for each (every one names its module from the root or through an
/// implicitly opened FSharp.Core namespace).
const ASSEMBLY_OPENS: &[(&str, bool)] = &[
    ("RqaAsm.Strict", true),
    ("global.RqaAsm.Strict", true),
    ("RqaAsm.Loose", false),
    ("RqaAsm.Outer.InnerStrict", true),
    ("RqaAsm.Bag", true),
    ("RqaRootStrict", true),
    ("RqaAsm", false),
    ("Strict", false),
    ("InnerStrict", false),
    ("ViaAuto", false),
    ("List", true),
    ("Seq", true),
    ("Checked", false),
];

/// The attribute text a generated module carries, and whether it is the
/// certain RQA spelling.
fn gen_attribute(t: &mut Tape) -> (&'static str, bool) {
    match t.pick(6) {
        0 | 1 => ("[<RequireQualifiedAccess>]\n", true),
        2 => ("[<RequireQualifiedAccessAttribute>]\n", true),
        3 => ("[<AutoOpen>]\n", false),
        _ => ("", false),
    }
}

/// Declare 1–3 modules (each maybe with one child) at `indent` under
/// `container`, appending them to `out` and `mods`.
fn gen_modules(
    t: &mut Tape,
    out: &mut String,
    indent: &str,
    container: &[String],
    trusted: bool,
    counter: &mut usize,
    mods: &mut Vec<GenModule>,
) {
    for _ in 0..=t.pick(3) {
        let (attr, rqa) = gen_attribute(t);
        let access = ["", "", "internal ", "private "][t.pick(4)];
        let name = format!("M{counter}");
        *counter += 1;
        if !attr.is_empty() {
            out.push_str(&format!("{indent}{attr}"));
        }
        out.push_str(&format!("{indent}module {access}{name} =\n"));
        out.push_str(&format!("{indent}    let v{counter} = 1\n"));
        let mut path = container.to_vec();
        path.push(name);
        let reachable = access != "private ";
        mods.push(GenModule {
            path: path.clone(),
            certain: rqa && reachable && trusted,
        });
        if t.chance(3) {
            let (cattr, crqa) = gen_attribute(t);
            let caccess = ["", "private "][t.pick(4).min(1)];
            let cname = format!("C{counter}");
            *counter += 1;
            let inner = format!("{indent}    ");
            if !cattr.is_empty() {
                out.push_str(&format!("{inner}{cattr}"));
            }
            out.push_str(&format!("{inner}module {caccess}{cname} =\n"));
            out.push_str(&format!("{inner}    let v{counter} = 2\n"));
            let mut cpath = path;
            cpath.push(cname);
            mods.push(GenModule {
                path: cpath,
                certain: crqa && reachable && caccess.is_empty() && trusted,
            });
        }
        out.push('\n');
    }
}

/// Generate one project from `seed`, its names under the root namespace
/// `G<seed>` so that many cases share one FCS project.
fn gen_case(seed: u32) -> GenCase {
    let mut t = Tape {
        nums: super::common::generator::seed_tape(seed, 512),
        pos: 0,
    };
    let g = format!("G{seed}");
    let mut counter = 0usize;
    let mut mods: Vec<GenModule> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    // Once a file declares a type spelled like the attribute, no written
    // attribute in it or after it is trusted.
    let mut poisoned = false;

    for j in 0..t.pick(3) {
        let ns = format!("N{}", j % 2);
        let mut src = String::new();
        let impersonate = t.chance(10);
        poisoned |= impersonate;
        let trusted = !poisoned;
        match t.pick(3) {
            0 => {
                src.push_str(&format!("namespace {g}.{ns}\n\n"));
                if impersonate {
                    src.push_str(
                        "type RequireQualifiedAccessAttribute() =\n    inherit System.Attribute()\n\n",
                    );
                }
                let container = vec![g.clone(), ns.clone()];
                gen_modules(
                    &mut t,
                    &mut src,
                    "",
                    &container,
                    trusted,
                    &mut counter,
                    &mut mods,
                );
            }
            k => {
                // A top-level module header, itself RQA for `k == 2`.
                let top = format!("T{j}");
                if k == 2 {
                    src.push_str("[<RequireQualifiedAccess>]\n");
                }
                src.push_str(&format!("module {g}.{ns}.{top}\n\n"));
                if impersonate {
                    src.push_str(
                        "type RequireQualifiedAccessAttribute() =\n    inherit System.Attribute()\n\n",
                    );
                }
                let container = vec![g.clone(), ns.clone(), top];
                mods.push(GenModule {
                    path: container.clone(),
                    certain: k == 2 && trusted,
                });
                gen_modules(
                    &mut t,
                    &mut src,
                    "",
                    &container,
                    trusted,
                    &mut counter,
                    &mut mods,
                );
            }
        }
        files.push(src);
    }

    // The probe.
    let mut src = String::new();
    let mut opens: Vec<GenOpen> = Vec::new();
    let mut local: Vec<GenModule> = Vec::new();
    // Same-file declarations precede the opens, in the container the header
    // makes: the anonymous root's implicit file module (whose paths the opens
    // write as declared), the probe's own top-level module, or the namespace
    // the probe module sits in.
    let with_local = t.chance(2);
    let (indent, enclosing, local_container): (&str, Option<Vec<String>>, Vec<String>) =
        match t.pick(3) {
            0 => {
                if with_local {
                    gen_modules(
                        &mut t,
                        &mut src,
                        "",
                        &[],
                        !poisoned,
                        &mut counter,
                        &mut local,
                    );
                }
                ("", None, Vec::new())
            }
            1 => {
                src.push_str(&format!("module {g}.Probe\n\n"));
                let container = vec![g.clone(), "Probe".to_string()];
                if with_local {
                    gen_modules(
                        &mut t,
                        &mut src,
                        "",
                        &container,
                        !poisoned,
                        &mut counter,
                        &mut local,
                    );
                }
                ("", None, container)
            }
            _ => {
                src.push_str(&format!("namespace {g}.N0\n\n"));
                let container = vec![g.clone(), "N0".to_string()];
                if with_local {
                    gen_modules(
                        &mut t,
                        &mut src,
                        "",
                        &container,
                        !poisoned,
                        &mut counter,
                        &mut local,
                    );
                }
                src.push_str("module Probe =\n");
                ("    ", Some(container.clone()), container)
            }
        };
    let mut push_open = |src: &mut String, path: &str, route: Route, must: bool| {
        src.push_str(indent);
        src.push_str("open ");
        let start = src.len();
        src.push_str(path);
        opens.push(GenOpen {
            range: (start, src.len()),
            route,
            must_report: must,
        });
        src.push('\n');
    };
    for _ in 0..=t.pick(4) {
        match t.pick(9) {
            0 | 1 if !mods.is_empty() => {
                let m = mods[t.pick(mods.len())].clone();
                let (path, route) = if t.chance(4) {
                    (format!("global.{}", m.path.join(".")), Route::Global)
                } else {
                    (m.path.join("."), Route::Root)
                };
                push_open(&mut src, &path, route, m.certain);
            }
            2 if !mods.is_empty() => {
                let m = mods[t.pick(mods.len())].clone();
                let (ns, rest) = m.path.split_at(2);
                push_open(&mut src, &ns.join("."), Route::Namespace, false);
                push_open(
                    &mut src,
                    &rest.join("."),
                    Route::ThroughNamespace,
                    m.certain,
                );
            }
            3 if mods.iter().any(|m| m.path.len() > 3) => {
                let deep: Vec<&GenModule> = mods.iter().filter(|m| m.path.len() > 3).collect();
                let m = deep[t.pick(deep.len())].clone();
                let (parent, child) = m.path.split_at(m.path.len() - 1);
                // The parent open is itself graded, as a `Root` open of
                // whatever the parent is.
                let parent_certain = mods.iter().any(|p| p.path == parent && p.certain);
                push_open(&mut src, &parent.join("."), Route::Root, parent_certain);
                push_open(&mut src, &child.join("."), Route::ThroughParent, m.certain);
            }
            4 if enclosing.is_some() => {
                let ns = enclosing.as_ref().expect("guarded");
                let under: Vec<&GenModule> =
                    mods.iter().filter(|m| m.path.starts_with(ns)).collect();
                if let Some(&m) = under.get(t.pick(under.len().max(1))) {
                    let rest = m.path[ns.len()..].join(".");
                    push_open(&mut src, &rest, Route::EnclosingNamespace, m.certain);
                }
            }
            5 | 8 if !local.is_empty() => {
                let m = local[t.pick(local.len())].clone();
                let relative = m.path[local_container.len()..].join(".");
                match t.pick(3) {
                    0 if !local_container.is_empty() => {
                        // Through the name of the block still being checked,
                        // which FCS does not resolve to this file's module.
                        push_open(&mut src, &m.path.join("."), Route::SameFileRooted, false);
                    }
                    1 => {
                        // `global.` never reaches this file's module.
                        push_open(
                            &mut src,
                            &format!("global.{}", m.path.join(".")),
                            Route::SameFileRooted,
                            false,
                        );
                    }
                    _ => push_open(&mut src, &relative, Route::SameFile, m.certain),
                }
            }
            6 => {
                let (path, must) = ASSEMBLY_OPENS[t.pick(ASSEMBLY_OPENS.len())];
                push_open(&mut src, path, Route::Assembly, must);
            }
            _ => push_open(&mut src, &format!("{g}.Nowhere"), Route::Nothing, false),
        }
    }
    src.push_str(indent);
    src.push_str("let probeDone = 0\n");
    files.push(src);
    GenCase { files, opens }
}

/// Generated projects against FCS, `cases` of them from `first_seed`: every
/// FS0892 we report, FCS reports (in every file), and every one the generator
/// says is owed, we report. Prints the census of routes against both sides'
/// verdicts.
fn check_generated(first_seed: u32, cases: usize) {
    let env = env();
    let generated: Vec<GenCase> = (0..cases)
        .map(|i| gen_case(first_seed.wrapping_add(i as u32)))
        .collect();
    let graded = grade(
        &env,
        &generated
            .iter()
            .map(|c| c.files.clone())
            .collect::<Vec<_>>(),
    );

    let mut failures = Vec::new();
    // (route, FCS reports, we report) → count
    let mut census: std::collections::BTreeMap<(Route, bool, bool), usize> =
        std::collections::BTreeMap::new();
    for (case, g) in generated.iter().zip(&graded) {
        for (_, src) in &g.files {
            let errors = parse(src).errors;
            assert!(
                errors.is_empty(),
                "generated file fails to parse: {errors:?}\n{src}"
            );
        }
        let dump = || {
            g.files
                .iter()
                .map(|(_, s)| format!("    ----\n{s}"))
                .collect::<String>()
        };
        for (i, (o, f)) in g.ours.iter().zip(&g.fcs).enumerate() {
            if !o.is_subset(f) {
                failures.push(format!(
                    "  file {i}: we report {:?}, FCS {:?}\n{}",
                    o.difference(f).collect::<Vec<_>>(),
                    f,
                    dump()
                ));
            }
        }
        let probe_ours = g.ours.last().expect("a probe file");
        let probe_fcs = g.fcs.last().expect("a probe file");
        for open in &case.opens {
            let ours = probe_ours.contains(&open.range);
            let fcs = probe_fcs.contains(&open.range);
            *census.entry((open.route, fcs, ours)).or_default() += 1;
            if open.must_report && !ours {
                failures.push(format!(
                    "  owed FS0892 at {:?} ({:?}) not reported (FCS: {fcs})\n{}",
                    open.range,
                    open.route,
                    dump()
                ));
            }
        }
    }
    eprintln!("FS0892 census (route, FCS reports, we report): count");
    for ((route, fcs, ours), n) in &census {
        eprintln!("  {route:?} fcs={fcs} ours={ours}: {n}");
    }
    assert!(
        failures.is_empty(),
        "{} FS0892 failures over {cases} generated projects:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn fs0892_generated_projects_match_fcs() {
    check_generated(1, env_usize_or("BORZOI_RQA_OPEN_CASES", 100));
}

#[test]
#[ignore = "deep generated sweep against FCS; BORZOI_RQA_OPEN_CASES overrides the count"]
fn deep_fs0892_generated_projects_match_fcs() {
    check_generated(10_000, env_usize_or("BORZOI_RQA_OPEN_CASES", 600));
}
