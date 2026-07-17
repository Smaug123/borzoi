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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lsp_types::{
    WorkspaceDiagnosticParams, WorkspaceDiagnosticReport, WorkspaceDiagnosticReportResult,
    WorkspaceDocumentDiagnosticReport,
};

use crate::handlers::diagnostic::{FileOutcome, diagnose_file};
use crate::handlers::preferred_uri;
use crate::paths::{lexically_normalize, path_dedup_key};
use crate::pull::{workspace_entry, workspace_unchanged};
use crate::server::State;

/// Run the workspace-diagnostic handler. Enumerates every `.fsproj` under the
/// client's workspace roots plus the source files in their `<Compile>` lists,
/// and returns one report per file (including clean ones, so the client can
/// clear stale diagnostics). A source file whose `(source, symbols, lang)`
/// still matches the client's `previous_result_ids` entry comes back as
/// `Unchanged`, skipping the lex/parse — the project-wide payoff of
/// `result_id` caching.
/// With no roots configured (an unusual client, or pre-`initialize`) the report
/// is empty.
pub fn handle(
    state: &mut State,
    params: WorkspaceDiagnosticParams,
) -> WorkspaceDiagnosticReportResult {
    let roots = state.workspace_roots().to_vec();
    let projects = discover_fsprojs(&roots);
    let (files, linking) = enumerate_files(state, &projects);

    // What the client last received for each URI, so unchanged files can answer
    // `Unchanged` rather than being re-lexed. Borrows `params`, which is
    // disjoint from the `&mut state` the loop uses.
    let previous: HashMap<&str, &str> = params
        .previous_result_ids
        .iter()
        .map(|p| (p.uri.as_str(), p.value.as_str()))
        .collect();

    let mut items: Vec<WorkspaceDocumentDiagnosticReport> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for path in files {
        // One report per physical file: a file linked into several projects (or
        // spelled with different case on a case-insensitive platform) is
        // reported once. Dedup on the platform-aware path key *before* building a
        // URI, so two case-variant spellings can't slip through as two
        // differently-cased `file://` URIs.
        let dedup_key = path_dedup_key(&path);
        if !seen.insert(dedup_key.clone()) {
            continue;
        }
        let Some(uri) = preferred_uri(&path, &state.docs) else {
            continue;
        };
        let prev = previous.get(uri.as_str()).copied();
        // `diagnose_file` is the same per-file logic `textDocument/diagnostic`
        // uses, so the two pulls agree on any one file (cacheability, the
        // result-id rule, and the unreadable→empty-`Full` rule) — except that
        // this sweep passes the project it enumerated the file from, so a
        // non-ancestor linked file gets that project's defines where the
        // document pull, which has no such hint, degrades to the implicit set
        // (see `diagnose_file`).
        let entry = match diagnose_file(
            state,
            &uri,
            prev,
            linking.get(&dedup_key).map(PathBuf::as_path),
        ) {
            FileOutcome::Unchanged(result_id) => workspace_unchanged(uri, result_id),
            FileOutcome::Full { groups, result_id } => workspace_entry(uri, groups, result_id),
        };
        items.push(entry);
    }

    WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items })
}

/// The files to diagnose: each discovered `.fsproj` itself, plus every
/// `<Compile>` item across all of them, lexically normalised and sorted for a
/// deterministic report order. Exact-duplicate paths are collapsed here; the
/// caller additionally de-dups case-insensitively (per platform) on the path
/// key, so this only needs to guarantee a stable order.
///
/// The second return value maps each enumerated source file — keyed by
/// [`path_dedup_key`], the same platform-aware key the report dedup uses, so
/// case-variant spellings of one physical file share a single owner — to the
/// project it was read out of, for the linked-file symbol refinement
/// (`Workspace::symbols_for_linked`). Only a project whose `<Compile>` list is
/// trustworthy (`!items_uncertain`) is recorded — an uncertain list may retain
/// an item MSBuild would have removed, so it proves nothing about ownership
/// (the same gate `Workspace::membership` applies; the consumer re-checks, so
/// this is a filter, not the soundness boundary). When several such projects
/// list one file, the first in the (sorted) project order wins,
/// deterministically — regardless of which spelling survives as the report
/// path.
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

/// Recursively find every `*.fsproj` under `roots`, skipping build-output and
/// VCS directories and never following symlinks (so the walk cannot cycle).
/// Sorted and de-duplicated for deterministic enumeration.
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
            // `file_type` reports a symlink *as* a symlink (not its target), so
            // both `is_dir()` and `is_file()` are false for links — we never
            // follow them, which also means the walk cannot loop.
            if file_type.is_dir() {
                let skip = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_skipped_dir);
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
            (full.uri.path().rsplit('/').next() == Some(file_name))
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
                full.uri.path().rsplit('/').next() == Some("Shared.fs")
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
            (uri.path().rsplit('/').next() == Some(file_name)).then_some(kind)
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
            (f.uri.path().rsplit('/').next() == Some("App.fsproj"))
                .then(|| f.full_document_diagnostic_report.result_id.clone())
        });
        assert_eq!(fsproj_id, Some(None), "fsproj must carry no result_id");

        // Even echoing every id, the fsproj stays Full (always recomputed).
        let second = handle(&mut state, params_with(previous_ids(&first)));
        assert_eq!(entry_kind(&second, "App.fsproj"), Some("full"));
    }
}
