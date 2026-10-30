//! `workspace/diagnostic` — pull-model project-wide diagnostics.
//!
//! The headline agent query: "did my change break anything *anywhere*?" Walks
//! the client's workspace roots for `.fsproj` files, enumerates each project's
//! `<Compile>` list, and returns one `Full` report per file — the project-wide
//! analogue of `textDocument/diagnostic`, reusing the same single computation
//! (`crate::server::grouped_for_uri`) and the same overlay-then-disk read.
//!
//! Like the document pull, each file's report carries that file's **own**
//! diagnostics; cross-file `#line` relocation stays with the push path (a
//! generated file's relocated diagnostics are not re-attributed here). See
//! `docs/completed/pull-diagnostics-plan.md`.
//!
//! Because the sweep reads each file out of a project it has just evaluated,
//! it knows the owner of a file *linked from outside its ancestor chain*
//! (`<Compile Include="../Shared/Foo.fs">`), which the per-file ancestor walk
//! cannot place. That project is threaded to `diagnose_file`, so such a file
//! is diagnosed under the linking project's `DefineConstants` instead of the
//! implicit symbol set — see `Workspace::symbols_for_linked` for the
//! precedence and the trustworthiness gate, and
//! `docs/workspace-index-plan.md` for why this is the sweep-local fix rather
//! than a workspace-wide ownership index.
//!
//! # Stepping
//!
//! The sweep's cost is proportional to everything under the client's roots,
//! which the server does not choose: a root one directory too high has meant
//! fourteen thousand projects and a quarter of an hour. So the sweep is a
//! [`Sweep`] value advanced one unit at a time by [`Sweep::step`] — one
//! directory read, one project evaluated, or one file diagnosed — and the
//! dispatch loop (`crate::server::run_with_fetcher`) looks at the connection
//! between units. An interactive request waits for at most one unit, a
//! `$/cancelRequest` or a newer sweep ends this one, and a hang-up or `exit`
//! abandons it. Nothing bounds the sweep's *total* run time; what is bounded is
//! how long it can hold anything else up.
//!
//! **What a report means when other messages interleave.** Which files are
//! reported, and which project each linked file is diagnosed under, come from
//! one generation of the project evaluations: a structural change mid-sweep
//! (one that invalidates them) starts the sweep over. Each item is computed
//! against the state at the moment its file is diagnosed, which is never
//! earlier than the request's arrival. An edit landing mid-sweep can
//! therefore stale an item already computed, exactly as an edit landing just
//! after a monolithic sweep's reply would; the convergence mechanism is the
//! same in both cases: the per-item `result_id` (the next pull compares it
//! against the current input) and the `workspace/diagnostic/refresh` a watched
//! change owes, which the loop holds until this sweep has replied so that it
//! cannot overtake the report it invalidates.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_types::{
    WorkspaceDiagnosticParams, WorkspaceDiagnosticReport, WorkspaceDiagnosticReportResult,
    WorkspaceDocumentDiagnosticReport,
};

use crate::handlers::diagnostic::{FileOutcome, diagnose_file};
use crate::handlers::preferred_uri;
use crate::paths::{lexically_normalize, path_dedup_key};
use crate::pull::{workspace_entry, workspace_unchanged};
use crate::server::State;
use crate::workspace::ProjectsGeneration;

/// A `workspace/diagnostic` sweep in progress. Enumerates every `.fsproj`
/// under the client's workspace roots plus the source files in their
/// `<Compile>` lists, and ends with one report per file (including clean ones,
/// so the client can clear stale diagnostics). A source file whose
/// `(source, symbols, lang)` still matches the client's `previous_result_ids`
/// entry comes back as `Unchanged`, skipping the lex/parse — the project-wide
/// payoff of `result_id` caching. With no roots configured (an unusual client,
/// or pre-`initialize`) the report is empty.
///
/// Holds paths and accumulated results only, never a borrow of [`State`], so
/// the state may change between steps (see the module docs for what that
/// means for the report).
pub struct Sweep {
    /// What the client last received for each URI, so unchanged files can
    /// answer `Unchanged` rather than being re-lexed.
    previous: HashMap<String, String>,
    /// The project-evaluation generation every fact in `phase` was read
    /// under. The file list and the ownership map are derived from project
    /// evaluations, so once the generation moves they describe a tree that no
    /// longer exists, and diagnosing the files under the new evaluations would
    /// mix the two; the sweep starts over instead.
    generation: ProjectsGeneration,
    phase: Phase,
}

/// The sweep's three passes, in order. Each pass's input is fully known before
/// it starts, so the report's order is a function of the tree alone, not of
/// how the steps interleaved with anything else.
enum Phase {
    /// Walking the roots for project files.
    Discover {
        /// Directories still to read.
        pending: Vec<PathBuf>,
        visited: HashSet<PathBuf>,
        found: Vec<PathBuf>,
    },
    /// Evaluating each project (sorted, de-duplicated) for its `<Compile>`
    /// list. See [`enumerate_project`] for `files` and `linking`.
    Enumerate {
        projects: std::vec::IntoIter<PathBuf>,
        files: Vec<PathBuf>,
        linking: HashMap<String, PathBuf>,
    },
    /// Diagnosing each file (sorted, de-duplicated).
    Diagnose {
        files: std::vec::IntoIter<PathBuf>,
        linking: HashMap<String, PathBuf>,
        /// The [`path_dedup_key`]s already reported.
        seen: HashSet<String>,
        items: Vec<WorkspaceDocumentDiagnosticReport>,
    },
}

/// The outcome of one [`Sweep::step`].
pub enum Step {
    /// More work remains.
    Pending(Sweep),
    Done(WorkspaceDiagnosticReportResult),
}

impl Sweep {
    /// Start a sweep over the roots `state` holds now; a root change later in
    /// the sweep does not move it.
    pub fn new(state: &State, params: WorkspaceDiagnosticParams) -> Self {
        Self::start(
            state,
            params
                .previous_result_ids
                .into_iter()
                .map(|p| (p.uri.as_str().to_owned(), p.value))
                .collect(),
        )
    }

    fn start(state: &State, previous: HashMap<String, String>) -> Self {
        Sweep {
            previous,
            generation: state.workspace.projects_generation(),
            phase: Phase::Discover {
                pending: state.workspace_roots().to_vec(),
                visited: HashSet::new(),
                found: Vec::new(),
            },
        }
    }

    /// Do one unit of work: read one directory, evaluate one project, or
    /// diagnose one file. A step that finds its pass exhausted does only the
    /// in-memory hand-off to the next pass, and one that finds the project
    /// evaluations invalidated since the sweep began only starts it over.
    ///
    /// Starting over has no bound of its own: a client that keeps reporting
    /// structural changes keeps the sweep from finishing, exactly as it would
    /// keep a client that re-pulls on every refresh from settling.
    pub fn step(self, state: &mut State) -> Step {
        if state.workspace.projects_generation() != self.generation {
            return Step::Pending(Sweep::start(state, self.previous));
        }
        let Sweep {
            previous,
            generation,
            phase,
        } = self;
        let phase = match phase {
            Phase::Discover {
                mut pending,
                mut visited,
                mut found,
            } => match pending.pop() {
                Some(dir) => {
                    if visited.insert(dir.clone()) {
                        discover_in(&dir, &mut pending, &mut found);
                    }
                    Phase::Discover {
                        pending,
                        visited,
                        found,
                    }
                }
                None => {
                    found.sort();
                    found.dedup();
                    Phase::Enumerate {
                        projects: found.into_iter(),
                        files: Vec::new(),
                        linking: HashMap::new(),
                    }
                }
            },
            Phase::Enumerate {
                mut projects,
                mut files,
                mut linking,
            } => match projects.next() {
                Some(project) => {
                    enumerate_project(state, &project, &mut files, &mut linking);
                    Phase::Enumerate {
                        projects,
                        files,
                        linking,
                    }
                }
                None => {
                    // Sorted for a deterministic report order. Exact-duplicate
                    // paths are collapsed here; the diagnose pass additionally
                    // de-dups case-insensitively (per platform) on the path key.
                    files.sort();
                    files.dedup();
                    Phase::Diagnose {
                        files: files.into_iter(),
                        linking,
                        seen: HashSet::new(),
                        items: Vec::new(),
                    }
                }
            },
            Phase::Diagnose {
                mut files,
                linking,
                mut seen,
                mut items,
            } => match files.next() {
                Some(path) => {
                    items.extend(diagnose(state, &previous, &linking, &mut seen, &path));
                    Phase::Diagnose {
                        files,
                        linking,
                        seen,
                        items,
                    }
                }
                None => {
                    return Step::Done(WorkspaceDiagnosticReportResult::Report(
                        WorkspaceDiagnosticReport { items },
                    ));
                }
            },
        };
        Step::Pending(Sweep {
            previous,
            generation,
            phase,
        })
    }
}

/// The report entry for one enumerated file, or `None` when it is a duplicate
/// of a file already reported or has no URI.
fn diagnose(
    state: &mut State,
    previous: &HashMap<String, String>,
    linking: &HashMap<String, PathBuf>,
    seen: &mut HashSet<String>,
    path: &Path,
) -> Option<WorkspaceDocumentDiagnosticReport> {
    // One report per physical file: a file linked into several projects (or
    // spelled with different case on a case-insensitive platform) is reported
    // once. Dedup on the platform-aware path key *before* building a URI, so
    // two case-variant spellings can't slip through as two differently-cased
    // `file://` URIs.
    let dedup_key = path_dedup_key(path);
    if !seen.insert(dedup_key.clone()) {
        return None;
    }
    let uri = preferred_uri(path, &state.docs)?;
    let prev = previous.get(uri.as_str()).map(String::as_str);
    // `diagnose_file` is the same per-file logic `textDocument/diagnostic`
    // uses, so the two pulls agree on any one file (cacheability, the
    // result-id rule, and the unreadable→empty-`Full` rule) — except that this
    // sweep passes the project it enumerated the file from, so a non-ancestor
    // linked file gets that project's defines where the document pull, which
    // has no such hint, degrades to the implicit set (see `diagnose_file`).
    let linked = linking.get(&dedup_key).map(PathBuf::as_path);
    Some(match diagnose_file(state, &uri, prev, linked) {
        FileOutcome::Unchanged(result_id) => workspace_unchanged(uri, result_id),
        FileOutcome::Full { groups, result_id } => workspace_entry(uri, groups, result_id),
    })
}

/// Add `project` and its `<Compile>` items, lexically normalised, to `files`.
///
/// `linking` maps each enumerated source file — keyed by [`path_dedup_key`],
/// the same platform-aware key the report dedup uses, so case-variant
/// spellings of one physical file share a single owner — to the project it was
/// read out of, for the linked-file symbol refinement
/// (`Workspace::symbols_for_linked`). Only a project whose `<Compile>` list is
/// trustworthy (`!items_uncertain`) is recorded — an uncertain list may retain
/// an item MSBuild would have removed, so it proves nothing about ownership
/// (the same gate `Workspace::membership` applies; the consumer re-checks, so
/// this is a filter, not the soundness boundary). When several such projects
/// list one file, the first to be enumerated wins; the enumerate pass visits
/// projects in sorted order, so that is deterministic — regardless of which
/// spelling survives as the report path.
fn enumerate_project(
    state: &mut State,
    project: &Path,
    files: &mut Vec<PathBuf>,
    linking: &mut HashMap<String, PathBuf>,
) {
    files.push(lexically_normalize(project));
    if let Some(parsed) = state.workspace.project(project) {
        let trustworthy = !parsed.items_uncertain;
        for item in &parsed.items {
            let file = lexically_normalize(&item.include);
            if trustworthy {
                linking
                    .entry(path_dedup_key(&file))
                    .or_insert_with(|| project.to_path_buf());
            }
            files.push(file);
        }
    }
}

/// Read one directory of the walk for `*.fsproj` files: project files go to
/// `found`, subdirectories worth walking to `pending`. Build-output and VCS
/// directories are skipped, and symlinks are never followed (so the walk
/// cannot cycle).
fn discover_in(dir: &Path, pending: &mut Vec<PathBuf>, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        // `file_type` reports a symlink *as* a symlink (not its target), so
        // both `is_dir()` and `is_file()` are false for links — we never follow
        // them, which also means the walk cannot loop.
        if file_type.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_skipped_dir);
            if !skip {
                pending.push(path);
            }
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("fsproj"))
        {
            found.push(path);
        }
    }
}

/// Directory names not worth walking for `.fsproj` files: build outputs and
/// VCS / tooling metadata. Keeps the scan cheap and avoids surfacing generated
/// project files under `obj/`.
fn is_skipped_dir(name: &str) -> bool {
    matches!(name, "bin" | "obj" | ".git" | ".direnv" | "node_modules")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Diagnostic, PartialResultParams, WorkDoneProgressParams};
    use std::fs;
    use tempfile::TempDir;

    use crate::server::State;

    /// Run a sweep to completion, as the dispatch loop does when nothing
    /// interleaves.
    fn handle(
        state: &mut State,
        params: WorkspaceDiagnosticParams,
    ) -> WorkspaceDiagnosticReportResult {
        let mut sweep = Sweep::new(state, params);
        loop {
            match sweep.step(state) {
                Step::Pending(next) => sweep = next,
                Step::Done(report) => return report,
            }
        }
    }

    /// The discover pass alone, run to completion.
    fn discover_fsprojs(roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut pending = roots.to_vec();
        let mut visited = HashSet::new();
        let mut found = Vec::new();
        while let Some(dir) = pending.pop() {
            if visited.insert(dir.clone()) {
                discover_in(&dir, &mut pending, &mut found);
            }
        }
        found.sort();
        found.dedup();
        found
    }

    fn params() -> WorkspaceDiagnosticParams {
        WorkspaceDiagnosticParams {
            identifier: None,
            previous_result_ids: Vec::new(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        }
    }

    /// An SDK-less project listing `includes` (literal `<Compile>` paths).
    fn fsproj(includes: &[&str]) -> String {
        let items: String = includes
            .iter()
            .map(|i| format!("<Compile Include=\"{i}\" />"))
            .collect();
        format!("<Project><ItemGroup>{items}</ItemGroup></Project>")
    }

    /// The `Full` report items for the file named `file_name`, or `None` if no
    /// entry for it is present.
    fn full_items<'a>(
        result: &'a WorkspaceDiagnosticReportResult,
        file_name: &str,
    ) -> Option<&'a [Diagnostic]> {
        let WorkspaceDiagnosticReportResult::Report(report) = result else {
            return None;
        };
        report.items.iter().find_map(|item| {
            let WorkspaceDocumentDiagnosticReport::Full(full) = item else {
                return None;
            };
            (full.uri.path().as_str().rsplit('/').next() == Some(file_name))
                .then_some(full.full_document_diagnostic_report.items.as_slice())
        })
    }

    #[test]
    fn no_roots_yields_empty_report() {
        let mut state = State::default();
        let result = handle(&mut state, params());
        match result {
            WorkspaceDiagnosticReportResult::Report(r) => assert!(r.items.is_empty()),
            other => panic!("expected an (empty) Report, got {other:?}"),
        }
    }

    #[test]
    fn skips_build_output_dirs() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("App.fsproj"), fsproj(&[])).unwrap();
        fs::create_dir(tmp.path().join("obj")).unwrap();
        fs::write(tmp.path().join("obj").join("Generated.fsproj"), fsproj(&[])).unwrap();

        let found = discover_fsprojs(&[tmp.path().to_path_buf()]);
        let names: Vec<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert_eq!(names, ["App.fsproj"], "obj/ must be skipped: {found:?}");
    }

    #[test]
    fn finds_nested_fsproj() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("src").join("Lib");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("Lib.fsproj"), fsproj(&[])).unwrap();

        let found = discover_fsprojs(&[tmp.path().to_path_buf()]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].ends_with("src/Lib/Lib.fsproj"));
    }

    #[test]
    fn reports_per_compile_file_and_the_project() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("App.fsproj"), fsproj(&["Lib.fs", "Bad.fs"])).unwrap();
        fs::write(tmp.path().join("Lib.fs"), "let x = 1\n").unwrap();
        // An orphan `#endif` is a structural directive error (symbol-independent).
        fs::write(tmp.path().join("Bad.fs"), "#endif\n").unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        assert!(
            !full_items(&result, "Bad.fs")
                .expect("a report for Bad.fs")
                .is_empty(),
            "the broken file must carry a diagnostic: {result:?}"
        );
        assert!(
            full_items(&result, "Lib.fs")
                .expect("a report for Lib.fs")
                .is_empty(),
            "the clean file is reported as an empty Full: {result:?}"
        );
        assert!(
            full_items(&result, "App.fsproj").is_some(),
            "the project file itself is reported: {result:?}"
        );
    }

    /// A `<Compile>` item whose file doesn't exist on disk is still reported, as
    /// an *empty* `Full` — so a client clears any stale diagnostics for it
    /// rather than keeping them when the file is deleted.
    #[test]
    fn deleted_compile_file_reported_as_empty_full() {
        let tmp = TempDir::new().unwrap();
        // `Gone.fs` is listed but never written.
        fs::write(tmp.path().join("App.fsproj"), fsproj(&["Gone.fs"])).unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        assert!(
            full_items(&result, "Gone.fs")
                .expect("a (clearing) report for the missing file")
                .is_empty(),
            "a deleted Compile file must get an empty Full, not be omitted: {result:?}"
        );
    }

    #[test]
    fn linked_file_reported_once() {
        let tmp = TempDir::new().unwrap();
        // Two projects in the same directory both compile Shared.fs.
        fs::write(tmp.path().join("A.fsproj"), fsproj(&["Shared.fs"])).unwrap();
        fs::write(tmp.path().join("B.fsproj"), fsproj(&["Shared.fs"])).unwrap();
        fs::write(tmp.path().join("Shared.fs"), "let x = 1\n").unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        let WorkspaceDiagnosticReportResult::Report(report) = &result else {
            panic!("expected a Report");
        };
        let shared = report
            .items
            .iter()
            .filter(|item| {
                let WorkspaceDocumentDiagnosticReport::Full(full) = item else {
                    return false;
                };
                full.uri.path().as_str().rsplit('/').next() == Some("Shared.fs")
            })
            .count();
        assert_eq!(shared, 1, "a linked file is reported once: {result:?}");
    }

    /// A `.fs` source whose active branch parses cleanly only when `FOO` is
    /// defined: with the right `DefineConstants` the report is empty; with the
    /// implicit symbol set the `#else` branch's dangling `let x =` errors.
    const NEEDS_FOO: &str = "#if FOO\nlet x = 1\n#else\nlet x =\n#endif\n";

    /// A file linked from *outside* its ancestor chain (`../Shared/Foo.fs`)
    /// must be diagnosed under the linking project's `DefineConstants`: the
    /// sweep enumerated it from that very project's `<Compile>` list, so the
    /// owner is in hand even though the ancestor walk cannot find it.
    #[test]
    fn non_ancestor_linked_file_uses_linking_projects_defines() {
        let tmp = TempDir::new().unwrap();
        let shared = tmp.path().join("Shared");
        let proj_a = tmp.path().join("ProjA");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&proj_a).unwrap();
        fs::write(shared.join("Foo.fs"), NEEDS_FOO).unwrap();
        fs::write(
            proj_a.join("ProjA.fsproj"),
            "<Project>\
               <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>\
               <ItemGroup><Compile Include=\"../Shared/Foo.fs\" /></ItemGroup>\
             </Project>",
        )
        .unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        assert_eq!(
            full_items(&result, "Foo.fs").expect("a report for Foo.fs"),
            &[] as &[Diagnostic],
            "the linked file must be diagnosed under ProjA's FOO define: {result:?}"
        );
    }

    /// The linking project's defines apply only when its `<Compile>` list is
    /// trustworthy. Here a skipped item-affecting construct (a `<Compile
    /// Remove>` behind an unevaluable property-function condition) flips
    /// `items_uncertain`, so the listed item proves nothing — the file keeps
    /// the implicit symbol set, and its `#else` branch errors.
    #[test]
    fn items_uncertain_linking_project_does_not_donate_defines() {
        let tmp = TempDir::new().unwrap();
        let shared = tmp.path().join("Shared");
        let proj_a = tmp.path().join("ProjA");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&proj_a).unwrap();
        fs::write(shared.join("Foo.fs"), NEEDS_FOO).unwrap();
        fs::write(
            proj_a.join("ProjA.fsproj"),
            "<Project>\
               <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>\
               <ItemGroup>\
                 <Compile Include=\"../Shared/Foo.fs\" />\
                 <Compile Remove=\"Nothing.fs\" Condition=\"$([System.String]::IsNullOrEmpty(''))\" />\
               </ItemGroup>\
             </Project>",
        )
        .unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        assert!(
            !full_items(&result, "Foo.fs")
                .expect("a report for Foo.fs")
                .is_empty(),
            "an items_uncertain project must not donate its defines: {result:?}"
        );
    }

    /// A *conclusive ancestor* owner still wins over the linking project, so
    /// the workspace pull agrees with `textDocument/diagnostic` wherever the
    /// ancestor walk already resolves ownership: `Shared/Shared.fsproj`
    /// defines `FOO` and lists `Foo.fs`; `ProjA` also links it but defines
    /// nothing, and must not strip the define.
    #[test]
    fn conclusive_ancestor_owner_beats_linking_project() {
        let tmp = TempDir::new().unwrap();
        let shared = tmp.path().join("Shared");
        let proj_a = tmp.path().join("ProjA");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&proj_a).unwrap();
        fs::write(shared.join("Foo.fs"), NEEDS_FOO).unwrap();
        fs::write(
            shared.join("Shared.fsproj"),
            "<Project>\
               <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>\
               <ItemGroup><Compile Include=\"Foo.fs\" /></ItemGroup>\
             </Project>",
        )
        .unwrap();
        // Sorts before Shared.fsproj in project enumeration order, so a
        // naive first-enumerator-wins rule would pick it.
        fs::write(
            proj_a.join("ProjA.fsproj"),
            "<Project>\
               <ItemGroup><Compile Include=\"../Shared/Foo.fs\" /></ItemGroup>\
             </Project>",
        )
        .unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        assert_eq!(
            full_items(&result, "Foo.fs").expect("a report for Foo.fs"),
            &[] as &[Diagnostic],
            "the ancestor owner's FOO define must govern: {result:?}"
        );
    }

    /// On a case-insensitive filesystem, two projects linking one physical
    /// file under different spellings must pick the linked owner the same way
    /// the report dedup collapses the spellings: first project in sorted
    /// order. Here `ProjA` (first) links `foo.fs` and defines `FOO`; `ProjB`
    /// links `Foo.fs`, whose spelling sorts first and so survives as the
    /// report path. An owner map keyed by exact path would hand the surviving
    /// spelling to `ProjB` (no defines); keyed by the platform dedup key,
    /// `ProjA` wins and the file is clean. Gated to case-insensitive
    /// platforms; on Linux the spellings are genuinely distinct files.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn case_variant_linked_owner_follows_project_order() {
        let tmp = TempDir::new().unwrap();
        let shared = tmp.path().join("Shared");
        let proj_a = tmp.path().join("ProjA");
        let proj_b = tmp.path().join("ProjB");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&proj_a).unwrap();
        fs::create_dir_all(&proj_b).unwrap();
        fs::write(shared.join("foo.fs"), NEEDS_FOO).unwrap();
        fs::write(
            proj_a.join("ProjA.fsproj"),
            "<Project>\
               <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>\
               <ItemGroup><Compile Include=\"../Shared/foo.fs\" /></ItemGroup>\
             </Project>",
        )
        .unwrap();
        fs::write(
            proj_b.join("ProjB.fsproj"),
            "<Project>\
               <ItemGroup><Compile Include=\"../Shared/Foo.fs\" /></ItemGroup>\
             </Project>",
        )
        .unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        let WorkspaceDiagnosticReportResult::Report(report) = &result else {
            panic!("expected a Report");
        };
        let items: Vec<_> = report
            .items
            .iter()
            .filter_map(|item| {
                let WorkspaceDocumentDiagnosticReport::Full(full) = item else {
                    return None;
                };
                full.uri
                    .path()
                    .as_str()
                    .rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case("foo.fs"))
                    .then_some(full.full_document_diagnostic_report.items.as_slice())
            })
            .collect();
        assert_eq!(
            items.len(),
            1,
            "one report for the physical file: {result:?}"
        );
        assert_eq!(
            items[0],
            &[] as &[Diagnostic],
            "ProjA (first in project order) must donate FOO: {result:?}"
        );
    }

    /// On a case-insensitive filesystem, two projects spelling the same physical
    /// file with different case yield a single report — deduped on the
    /// platform-aware path key, not the raw URI casing. Gated to
    /// case-insensitive platforms; on Linux the two spellings are genuinely
    /// distinct files.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn case_variant_spellings_reported_once() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("A.fsproj"), fsproj(&["Lib.fs"])).unwrap();
        fs::write(tmp.path().join("B.fsproj"), fsproj(&["lib.fs"])).unwrap();
        fs::write(tmp.path().join("Lib.fs"), "let x = 1\n").unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let result = handle(&mut state, params());

        let WorkspaceDiagnosticReportResult::Report(report) = &result else {
            panic!("expected a Report");
        };
        let count = report
            .items
            .iter()
            .filter(|item| {
                let WorkspaceDocumentDiagnosticReport::Full(full) = item else {
                    return false;
                };
                full.uri
                    .path()
                    .as_str()
                    .rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case("lib.fs"))
            })
            .count();
        assert_eq!(
            count, 1,
            "case-variant spellings collapse to one: {result:?}"
        );
    }

    // --- result_id caching (Stage 3, Part B) -------------------------------

    fn params_with(
        previous_result_ids: Vec<lsp_types::PreviousResultId>,
    ) -> WorkspaceDiagnosticParams {
        WorkspaceDiagnosticParams {
            previous_result_ids,
            ..params()
        }
    }

    /// `(uri, result_id)` pairs the client would echo back: every `Full` entry
    /// that carries an id (i.e. the cacheable source files).
    fn previous_ids(result: &WorkspaceDiagnosticReportResult) -> Vec<lsp_types::PreviousResultId> {
        let WorkspaceDiagnosticReportResult::Report(report) = result else {
            return Vec::new();
        };
        report
            .items
            .iter()
            .filter_map(|item| {
                let WorkspaceDocumentDiagnosticReport::Full(full) = item else {
                    return None;
                };
                let value = full.full_document_diagnostic_report.result_id.clone()?;
                Some(lsp_types::PreviousResultId {
                    uri: full.uri.clone(),
                    value,
                })
            })
            .collect()
    }

    /// `"full"` / `"unchanged"` for the entry named `file_name`, or `None`.
    fn entry_kind(
        result: &WorkspaceDiagnosticReportResult,
        file_name: &str,
    ) -> Option<&'static str> {
        let WorkspaceDiagnosticReportResult::Report(report) = result else {
            return None;
        };
        report.items.iter().find_map(|item| {
            let (uri, kind) = match item {
                WorkspaceDocumentDiagnosticReport::Full(f) => (&f.uri, "full"),
                WorkspaceDocumentDiagnosticReport::Unchanged(u) => (&u.uri, "unchanged"),
            };
            (uri.path().as_str().rsplit('/').next() == Some(file_name)).then_some(kind)
        })
    }

    #[test]
    fn unchanged_files_come_back_unchanged() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("App.fsproj"), fsproj(&["Lib.fs", "Bad.fs"])).unwrap();
        fs::write(tmp.path().join("Lib.fs"), "let x = 1\n").unwrap();
        fs::write(tmp.path().join("Bad.fs"), "#endif\n").unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);

        let first = handle(&mut state, params());
        assert_eq!(entry_kind(&first, "Lib.fs"), Some("full"));
        assert_eq!(entry_kind(&first, "Bad.fs"), Some("full"));

        // Echo the ids back: nothing changed, so the source files are Unchanged.
        let second = handle(&mut state, params_with(previous_ids(&first)));
        assert_eq!(
            entry_kind(&second, "Lib.fs"),
            Some("unchanged"),
            "{second:?}"
        );
        assert_eq!(
            entry_kind(&second, "Bad.fs"),
            Some("unchanged"),
            "{second:?}"
        );
    }

    #[test]
    fn changed_file_is_full_others_unchanged() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("App.fsproj"), fsproj(&["Lib.fs", "Bad.fs"])).unwrap();
        fs::write(tmp.path().join("Lib.fs"), "let x = 1\n").unwrap();
        fs::write(tmp.path().join("Bad.fs"), "let y = 1\n").unwrap();

        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
        let first = handle(&mut state, params());

        // Edit one file on disk.
        fs::write(tmp.path().join("Bad.fs"), "#endif\n").unwrap();

        let second = handle(&mut state, params_with(previous_ids(&first)));
        assert_eq!(
            entry_kind(&second, "Bad.fs"),
            Some("full"),
            "the edited file recomputes: {second:?}"
        );
        assert!(
            !full_items(&second, "Bad.fs")
                .expect("Bad.fs report")
                .is_empty(),
            "and now carries its diagnostic: {second:?}"
        );
        assert_eq!(
            entry_kind(&second, "Lib.fs"),
            Some("unchanged"),
            "the unchanged file is cached: {second:?}"
        );
    }

    #[test]
    fn fsproj_entry_is_never_cached() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("App.fsproj"), fsproj(&[])).unwrap();
        let mut state = State::default();
        state.set_workspace_roots(vec![tmp.path().to_path_buf()]);

        let first = handle(&mut state, params());
        // The `.fsproj` entry is `Full` with no result_id (so it's not echoable).
        let WorkspaceDiagnosticReportResult::Report(report) = &first else {
            panic!("expected a Report");
        };
        let fsproj_id = report.items.iter().find_map(|item| {
            let WorkspaceDocumentDiagnosticReport::Full(f) = item else {
                return None;
            };
            (f.uri.path().as_str().rsplit('/').next() == Some("App.fsproj"))
                .then(|| f.full_document_diagnostic_report.result_id.clone())
        });
        assert_eq!(fsproj_id, Some(None), "fsproj must carry no result_id");

        // Even echoing every id, the fsproj stays Full (always recomputed).
        let second = handle(&mut state, params_with(previous_ids(&first)));
        assert_eq!(entry_kind(&second, "App.fsproj"), Some("full"));
    }

    /// The sweep as one synchronous pass, the shape it had before it was
    /// stepped: the oracle that splitting it into steps moved nothing.
    mod reference {
        use super::*;

        pub fn handle(
            state: &mut State,
            params: WorkspaceDiagnosticParams,
        ) -> WorkspaceDiagnosticReportResult {
            let roots = state.workspace_roots().to_vec();
            let projects = discover_fsprojs(&roots);
            let (files, linking) = enumerate_files(state, &projects);
            let previous: HashMap<&str, &str> = params
                .previous_result_ids
                .iter()
                .map(|p| (p.uri.as_str(), p.value.as_str()))
                .collect();
            let mut items: Vec<WorkspaceDocumentDiagnosticReport> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for path in files {
                let dedup_key = path_dedup_key(&path);
                if !seen.insert(dedup_key.clone()) {
                    continue;
                }
                let Some(uri) = preferred_uri(&path, &state.docs) else {
                    continue;
                };
                let prev = previous.get(uri.as_str()).copied();
                let entry = match diagnose_file(
                    state,
                    &uri,
                    prev,
                    linking.get(&dedup_key).map(PathBuf::as_path),
                ) {
                    FileOutcome::Unchanged(result_id) => workspace_unchanged(uri, result_id),
                    FileOutcome::Full { groups, result_id } => {
                        workspace_entry(uri, groups, result_id)
                    }
                };
                items.push(entry);
            }
            WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items })
        }

        fn enumerate_files(
            state: &mut State,
            projects: &[PathBuf],
        ) -> (Vec<PathBuf>, HashMap<String, PathBuf>) {
            let mut files: Vec<PathBuf> = Vec::new();
            let mut linking: HashMap<String, PathBuf> = HashMap::new();
            for proj in projects {
                files.push(lexically_normalize(proj));
                if let Some(parsed) = state.workspace.project(proj) {
                    let trustworthy = !parsed.items_uncertain;
                    for item in &parsed.items {
                        let file = lexically_normalize(&item.include);
                        if trustworthy {
                            linking
                                .entry(path_dedup_key(&file))
                                .or_insert_with(|| proj.clone());
                        }
                        files.push(file);
                    }
                }
            }
            files.sort();
            files.dedup();
            (files, linking)
        }

        fn discover_fsprojs(roots: &[PathBuf]) -> Vec<PathBuf> {
            let mut out: Vec<PathBuf> = Vec::new();
            let mut stack: Vec<PathBuf> = roots.to_vec();
            let mut visited: HashSet<PathBuf> = HashSet::new();
            while let Some(dir) = stack.pop() {
                if !visited.insert(dir.clone()) {
                    continue;
                }
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    let path = entry.path();
                    if file_type.is_dir() {
                        let skip = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                            matches!(n, "bin" | "obj" | ".git" | ".direnv" | "node_modules")
                        });
                        if !skip {
                            stack.push(path);
                        }
                    } else if file_type.is_file()
                        && path
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("fsproj"))
                    {
                        out.push(path);
                    }
                }
            }
            out.sort();
            out.dedup();
            out
        }
    }

    /// Directories a generated tree may use, relative to the root. `bin` and
    /// `obj` exercise the skip list.
    const TREE_DIRS: &[&str] = &["", "A", "C", "A/B", "a", "Shared", "bin", "obj/X"];
    /// The [`TREE_DIRS`] a project may sit in. Kept to a few, siblings among
    /// them (`A`, `C`), so that two projects linking one file from outside
    /// its ancestor chain — the case where the first-listed owner donates the
    /// defines — is common rather than a rare draw.
    const TREE_PROJECT_DIRS: &[usize] = &[0, 1, 2, 3, 6];
    /// `Shared`, in [`TREE_DIRS`], and `../Shared/X.fs`, in [`TREE_INCLUDES`]:
    /// the generator leans on both for the same reason.
    const SHARED_DIR: usize = 5;
    const SHARED_LINK: usize = 4;
    /// `<Compile Include>` spellings a generated project may list: siblings,
    /// a case variant, a subdirectory, links out of the project's directory,
    /// and a file that is never written.
    const TREE_INCLUDES: &[&str] = &[
        "X.fs",
        "x.fs",
        "Y.fs",
        "B/Y.fs",
        "../Shared/X.fs",
        "../X.fs",
        "Missing.fs",
    ];
    const TREE_SOURCES: &[&str] = &["X.fs", "x.fs", "Y.fs"];
    /// Clean, structurally broken, and define-dependent source text.
    const TREE_TEXTS: &[&str] = &["let x = 1\n", "#endif\n", NEEDS_FOO];

    #[derive(Debug, Clone)]
    struct TreeProject {
        dir: usize,
        name: &'static str,
        defines_foo: bool,
        includes: Vec<usize>,
    }

    #[derive(Debug, Clone)]
    struct Tree {
        projects: Vec<TreeProject>,
        /// `(dir, name, text)`.
        sources: Vec<(usize, usize, usize)>,
    }

    fn tree_strategy() -> impl proptest::strategy::Strategy<Value = Tree> {
        use proptest::prelude::*;
        let project = (
            prop::sample::select(TREE_PROJECT_DIRS),
            prop::sample::select(vec!["P", "Q", "p"]),
            any::<bool>(),
            prop::collection::vec(
                prop_oneof![1 => Just(SHARED_LINK), 2 => 0..TREE_INCLUDES.len()],
                0..4,
            ),
        )
            .prop_map(|(dir, name, defines_foo, includes)| TreeProject {
                dir,
                name,
                defines_foo,
                includes,
            });
        let source = (
            prop_oneof![1 => Just(SHARED_DIR), 2 => 0..TREE_DIRS.len()],
            0..TREE_SOURCES.len(),
            0..TREE_TEXTS.len(),
        );
        (
            prop::collection::vec(project, 0..5),
            prop::collection::vec(source, 0..6),
        )
            .prop_map(|(projects, sources)| Tree { projects, sources })
    }

    fn write_tree(root: &std::path::Path, tree: &Tree) {
        for dir in TREE_DIRS {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for &(dir, name, text) in &tree.sources {
            fs::write(
                root.join(TREE_DIRS[dir]).join(TREE_SOURCES[name]),
                TREE_TEXTS[text],
            )
            .unwrap();
        }
        for p in &tree.projects {
            let defines = if p.defines_foo {
                "<PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>"
            } else {
                ""
            };
            let items: String = p
                .includes
                .iter()
                .map(|&i| format!("<Compile Include=\"{}\" />", TREE_INCLUDES[i]))
                .collect();
            fs::write(
                root.join(TREE_DIRS[p.dir])
                    .join(format!("{}.fsproj", p.name)),
                format!("<Project>{defines}<ItemGroup>{items}</ItemGroup></Project>"),
            )
            .unwrap();
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        /// Stepping the sweep reports exactly what the single synchronous pass
        /// does, on a cold sweep and on the warm follow-up that echoes the
        /// first one's result ids.
        #[test]
        fn stepped_sweep_matches_the_synchronous_pass(tree in tree_strategy()) {
            let tmp = TempDir::new().unwrap();
            write_tree(tmp.path(), &tree);
            let mut stepped = State::default();
            stepped.set_workspace_roots(vec![tmp.path().to_path_buf()]);
            let mut synchronous = State::default();
            synchronous.set_workspace_roots(vec![tmp.path().to_path_buf()]);

            let cold = handle(&mut stepped, params());
            proptest::prop_assert_eq!(&cold, &reference::handle(&mut synchronous, params()));
            let warm = handle(&mut stepped, params_with(previous_ids(&cold)));
            proptest::prop_assert_eq!(
                &warm,
                &reference::handle(&mut synchronous, params_with(previous_ids(&cold)))
            );
        }

        /// A structural change landing at any step of a sweep yields the report
        /// a sweep started after it would: the sweep never combines a file
        /// list or an ownership map read from one set of project evaluations
        /// with diagnoses run under another.
        #[test]
        fn a_structural_change_mid_sweep_reports_the_new_tree(
            before in tree_strategy(),
            after in tree_strategy(),
            at in 0usize..48,
        ) {
            let tmp = TempDir::new().unwrap();
            write_tree(tmp.path(), &before);
            let mut state = State::default();
            state.set_workspace_roots(vec![tmp.path().to_path_buf()]);
            let mut sweep = Sweep::new(&state, params());
            for _ in 0..at {
                match sweep.step(&mut state) {
                    Step::Pending(next) => sweep = next,
                    // Finished before the change: nothing was in flight.
                    Step::Done(_) => return Ok(()),
                }
            }

            fs::remove_dir_all(tmp.path()).unwrap();
            fs::create_dir(tmp.path()).unwrap();
            write_tree(tmp.path(), &after);
            let changed = tmp.path().join("P.fsproj");
            state.apply_watched_changes(&[lsp_types::FileEvent {
                uri: <lsp_types::Uri as crate::uri::UriExt>::from_file_path(&changed).unwrap(),
                typ: lsp_types::FileChangeType::CHANGED,
            }]);
            let report = loop {
                match sweep.step(&mut state) {
                    Step::Pending(next) => sweep = next,
                    Step::Done(report) => break report,
                }
            };

            let mut fresh = State::default();
            fresh.set_workspace_roots(vec![tmp.path().to_path_buf()]);
            proptest::prop_assert_eq!(report, handle(&mut fresh, params()));
        }
    }
}
