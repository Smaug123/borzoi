//! `textDocument/diagnostic` — pull-model document diagnostics.
//!
//! The request/response counterpart to the push path
//! (`textDocument/publishDiagnostics`): the client asks for one document's
//! diagnostics and gets a report back, rather than maintaining a subscription.
//! Reuses the single diagnostic computation (`crate::server::grouped_for_uri`)
//! and the pure report assembler ([`crate::pull::document_report`]); the only
//! work here is the imperative shell — reading the document's text and
//! constructing the response envelope.
//!
//! Each `Full` report carries a `result_id` ([`crate::pull::diagnostic_result_id`],
//! a hash of the source, the active symbol set, and the language version).
//! When the client echoes that id back as `previous_result_id` and none have
//! changed, we answer `Unchanged` **without** lexing or parsing the file (plan
//! Stage 3).

use lsp_types::{
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport,
    UnchangedDocumentDiagnosticReport, Url,
};

use std::path::Path;

use crate::diagnostics::FileDiagnostics;
use crate::pull::{diagnostic_result_id, document_report};
use crate::server::{State, grouped_for_uri_linked, path_extension};

/// One file's pull-diagnostic outcome, independent of the (document vs
/// workspace) report shape the caller wraps it in. Shared by
/// `textDocument/diagnostic` and `workspace/diagnostic` so they agree on any one
/// file — including the cacheability and `result_id` rules.
pub(crate) enum FileOutcome {
    /// The client's cached report for this file is still valid; echo `result_id`.
    Unchanged(String),
    /// A fresh report: the diagnostic `groups`, plus the `result_id` to stamp
    /// (present only for cacheable source files).
    Full {
        groups: Vec<FileDiagnostics>,
        result_id: Option<String>,
    },
}

/// Diagnose one file for a pull request, sharing the read / cacheability /
/// `result_id` logic. `previous_result_id` is what the client last received for
/// this URI, if anything.
///
/// `linking_project` is a `.fsproj` the caller knows enumerates this file in
/// its `<Compile>` list, refining symbol/lang-version resolution for a file
/// the ancestor walk cannot place ([`Workspace::symbols_for_linked`]'s
/// precedence rules). Only `workspace/diagnostic` has one in hand — its sweep
/// reads each file out of a just-evaluated project — so for a non-ancestor
/// linked file the workspace pull is deliberately *better-informed* than the
/// document pull, which degrades to the implicit symbol set. The two pulls
/// still agree wherever ownership resolves conclusively from ancestors.
///
/// - **Unreadable** (missing / deleted / non-`file:`) → empty `Full`, no id.
///   It clears any stale client diagnostics; crucially we do *not* substitute
///   `""` and parse it — for a `.fsproj` that would fabricate a "malformed XML".
/// - **Cacheable source** (`.fs`/`.fsi`/`.fsx`) → resolve the active symbols
///   and language version the *same* way the full recompute does
///   (`Workspace::symbols_for` / `lang_version_for`, refined by
///   `linking_project`) and key the id on `(source, symbols, lang)`, so the id
///   reflects the current project context — including a `.fsproj` that has
///   only just appeared on disk — and a fast-path `Unchanged` can never hide a
///   defines or `<LangVersion>` change the full path would have discovered.
///   Answers `Unchanged` when the id matches, else a `Full` carrying it.
/// - **`.fsproj`** (and anything else) → always `Full`, no id: its diagnostics
///   depend on inputs not captured by `(source, symbols, lang)` (referenced
///   `.csproj`/`.fsproj` existence, the SDK env), so it is recomputed on every
///   pull (cheap — one project file) rather than risk a stale `Unchanged`.
///
/// [`Workspace::symbols_for_linked`]: crate::workspace::Workspace::symbols_for_linked
pub(crate) fn diagnose_file(
    state: &mut State,
    uri: &Url,
    previous_result_id: Option<&str>,
    linking_project: Option<&Path>,
) -> FileOutcome {
    let Some(text) = document_text(state, uri) else {
        return FileOutcome::Full {
            groups: Vec::new(),
            result_id: None,
        };
    };
    if is_cacheable(uri) {
        let (symbols, lang) = match (linking_project, uri.to_file_path()) {
            // Resolve the linked owner once and reuse it for both queries:
            // `linked_owner` walks the (uncached) ancestor project chain, so
            // calling `symbols_for_linked`/`lang_version_for_linked`
            // separately would repeat that filesystem work.
            (Some(project), Ok(path)) => match state.workspace.linked_owner(&path, project) {
                Some(owner) => (
                    state.workspace.symbols_for_project(&owner),
                    state.workspace.lang_version_for_project(&owner),
                ),
                None => (
                    state.workspace.symbols_for(&path),
                    state.workspace.lang_version_for(&path),
                ),
            },
            _ => (state.symbols_for_uri(uri), state.lang_version_for_uri(uri)),
        };
        let result_id = diagnostic_result_id(&text, &symbols, lang);
        if previous_result_id == Some(result_id.as_str()) {
            return FileOutcome::Unchanged(result_id);
        }
        let groups = grouped_for_uri_linked(uri, &text, &mut state.workspace, linking_project)
            .unwrap_or_default();
        return FileOutcome::Full {
            groups,
            result_id: Some(result_id),
        };
    }
    let groups = grouped_for_uri_linked(uri, &text, &mut state.workspace, linking_project)
        .unwrap_or_default();
    FileOutcome::Full {
        groups,
        result_id: None,
    }
}

/// Run the document-diagnostic handler. Never errors or panics: an unreadable
/// URI yields an empty `Full` report (clearing any stale client state). Answers
/// `Unchanged` when the file is cacheable and the client's `previous_result_id`
/// still matches the current `(source, symbols, lang)`.
pub fn handle(
    state: &mut State,
    params: DocumentDiagnosticParams,
) -> DocumentDiagnosticReportResult {
    let uri = &params.text_document.uri;
    match diagnose_file(state, uri, params.previous_result_id.as_deref(), None) {
        FileOutcome::Unchanged(result_id) => {
            let unchanged = RelatedUnchangedDocumentDiagnosticReport {
                related_documents: None,
                unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                    result_id,
                },
            };
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(unchanged))
        }
        FileOutcome::Full { groups, result_id } => {
            let mut report = document_report(uri, groups);
            report.full_document_diagnostic_report.result_id = result_id;
            full(report)
        }
    }
}

/// Wrap a report as a `Full` document-diagnostic response.
fn full(report: RelatedFullDocumentDiagnosticReport) -> DocumentDiagnosticReportResult {
    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(report))
}

/// Whether `uri` names an F# source file (`.fs`/`.fsi`/`.fsx`) — the only kind
/// whose diagnostics are fully captured by `(source, symbols, lang)`, and so
/// eligible for `result_id` caching.
fn is_cacheable(uri: &Url) -> bool {
    matches!(path_extension(uri).as_deref(), Some("fs" | "fsi" | "fsx"))
}

/// A document's current text: the open-buffer overlay if the client has it
/// open, else the on-disk contents (D5). The disk fallback lets an agent write
/// a file and immediately pull its diagnostics without an intervening
/// `didOpen`, and is what `workspace/diagnostic` will rely on for the (mostly
/// unopened) files it enumerates. Returns `None` for a non-`file:` URI that
/// isn't open, or an unreadable path.
pub(crate) fn document_text(state: &State, uri: &Url) -> Option<String> {
    if let Some(text) = state.docs.get(uri) {
        return Some(text.clone());
    }
    let path = uri.to_file_path().ok()?;
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        FileChangeType, FileEvent, PartialResultParams, TextDocumentIdentifier,
        WorkDoneProgressParams,
    };

    use crate::server::State;

    fn params_with(uri: Url, previous_result_id: Option<String>) -> DocumentDiagnosticParams {
        DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri },
            identifier: None,
            previous_result_id,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        }
    }

    fn params(uri: Url) -> DocumentDiagnosticParams {
        params_with(uri, None)
    }

    /// The report's own `items`, asserting a `Full` report came back.
    fn items(result: &DocumentDiagnosticReportResult) -> &[lsp_types::Diagnostic] {
        match result {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
                &full.full_document_diagnostic_report.items
            }
            other => panic!("expected a Full report, got {other:?}"),
        }
    }

    /// The `result_id` of a `Full` report (asserting it's `Full` and carries one).
    fn full_result_id(result: &DocumentDiagnosticReportResult) -> String {
        match result {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => full
                .full_document_diagnostic_report
                .result_id
                .clone()
                .expect("a Full report carries a result_id"),
            other => panic!("expected a Full report, got {other:?}"),
        }
    }

    fn is_unchanged(result: &DocumentDiagnosticReportResult) -> bool {
        matches!(
            result,
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(_))
        )
    }

    /// An orphan `#endif` is a structural directive error reported regardless of
    /// the active symbol set, so it needs no owning project — ideal for
    /// exercising the wiring.
    const UNMATCHED_ENDIF: &str = "#endif\n";

    #[test]
    fn reports_open_buffer_diagnostic() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/Foo.fs").unwrap();
        state.docs.insert(uri.clone(), UNMATCHED_ENDIF.to_string());

        let result = handle(&mut state, params(uri));
        assert!(
            !items(&result).is_empty(),
            "an unmatched #endif must surface as a diagnostic: {result:?}"
        );
    }

    #[test]
    fn clean_open_buffer_is_empty_report() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/Foo.fs").unwrap();
        state.docs.insert(uri.clone(), "let x = 1\n".to_string());

        let result = handle(&mut state, params(uri));
        assert!(items(&result).is_empty(), "clean file: {result:?}");
    }

    /// D5: a file present only on disk (never `didOpen`ed) is still diagnosed —
    /// the agent-writes-then-pulls workflow.
    #[test]
    fn reads_unopened_file_from_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("OnDisk.fs");
        std::fs::write(&path, UNMATCHED_ENDIF).unwrap();
        let uri = Url::from_file_path(&path).unwrap();

        let mut state = State::default();
        // Deliberately *not* inserted into `state.docs`.
        let result = handle(&mut state, params(uri));
        assert!(
            !items(&result).is_empty(),
            "an on-disk file must be read and diagnosed: {result:?}"
        );
    }

    #[test]
    fn missing_file_is_empty_report() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/DoesNotExist.fs").unwrap();
        let result = handle(&mut state, params(uri));
        assert!(items(&result).is_empty());
    }

    // --- result_id caching (Stage 3) ---------------------------------------

    #[test]
    fn unchanged_when_result_id_is_echoed() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/Foo.fs").unwrap();
        state.docs.insert(uri.clone(), "let x = 1\n".to_string());

        let id = full_result_id(&handle(&mut state, params(uri.clone())));
        let second = handle(&mut state, params_with(uri, Some(id)));
        assert!(
            is_unchanged(&second),
            "echoing the id back must yield Unchanged: {second:?}"
        );
    }

    #[test]
    fn full_when_previous_result_id_is_stale() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/Foo.fs").unwrap();
        state.docs.insert(uri.clone(), "let x = 1\n".to_string());

        // A non-matching previous id forces a fresh `Full` (with the real id).
        let result = handle(&mut state, params_with(uri, Some("deadbeef".to_string())));
        assert!(!is_unchanged(&result), "{result:?}");
        let _ = full_result_id(&result);
    }

    #[test]
    fn result_id_changes_when_source_changes() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/Foo.fs").unwrap();
        state.docs.insert(uri.clone(), "let x = 1\n".to_string());
        let id1 = full_result_id(&handle(&mut state, params(uri.clone())));

        // Edit the overlay; echoing the old id must not be cached.
        state.docs.insert(uri.clone(), "let x = 2\n".to_string());
        let again = handle(&mut state, params_with(uri, Some(id1.clone())));
        assert!(
            !is_unchanged(&again),
            "an edit must not be cached: {again:?}"
        );
        assert_ne!(full_result_id(&again), id1);
    }

    /// An SDK-less `.fsproj` defining `define` and compiling `A.fs`.
    fn fsproj_define(define: &str) -> String {
        format!(
            "<Project><PropertyGroup><DefineConstants>{define}</DefineConstants></PropertyGroup>\
             <ItemGroup><Compile Include=\"A.fs\" /></ItemGroup></Project>"
        )
    }

    /// The load-bearing test: changing the owning project's `DefineConstants` on
    /// disk (then invalidating via a watched change) changes the resolved
    /// symbols, so the same source is no longer cached.
    #[test]
    fn project_defines_change_busts_the_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proj = tmp.path().join("App.fsproj");
        let file = tmp.path().join("A.fs");
        std::fs::write(&proj, fsproj_define("FOO")).unwrap();
        std::fs::write(&file, "let x = 1\n").unwrap();
        let uri = Url::from_file_path(&file).unwrap();

        let mut state = State::default();
        let id = full_result_id(&handle(&mut state, params(uri.clone())));
        // Unchanged while nothing has changed.
        assert!(is_unchanged(&handle(
            &mut state,
            params_with(uri.clone(), Some(id.clone()))
        )));

        // Change the defines on disk and deliver the watched change, so the next
        // resolve re-evaluates the project and sees BAR instead of FOO.
        std::fs::write(&proj, fsproj_define("BAR")).unwrap();
        state.apply_watched_changes(&[FileEvent {
            uri: Url::from_file_path(&proj).unwrap(),
            typ: FileChangeType::CHANGED,
        }]);

        let after = handle(&mut state, params_with(uri, Some(id)));
        assert!(
            !is_unchanged(&after),
            "a defines change must bust the cache: {after:?}"
        );
    }

    /// The reviewer's case: a `.fsproj` *appearing* on disk changes the resolved
    /// symbols on the very next pull — even with no `didChangeWatchedFiles` — so
    /// the fast path must not return a stale `Unchanged`. (Keying the id on the
    /// resolved symbols, which the full path also discovers from disk, is what
    /// makes this correct.)
    #[test]
    fn lazily_discovered_project_busts_the_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("A.fs");
        // The active branch — and so the diagnostics — depend on FOO.
        std::fs::write(&file, "#if FOO\nlet x = 1\n#endif\n").unwrap();
        let uri = Url::from_file_path(&file).unwrap();

        let mut state = State::default();
        // First pull: no project on disk, so FOO is undefined.
        let id = full_result_id(&handle(&mut state, params(uri.clone())));
        assert!(is_unchanged(&handle(
            &mut state,
            params_with(uri.clone(), Some(id.clone()))
        )));

        // A project defining FOO and compiling A.fs appears — no notification.
        std::fs::write(tmp.path().join("App.fsproj"), fsproj_define("FOO")).unwrap();

        let after = handle(&mut state, params_with(uri, Some(id)));
        assert!(
            !is_unchanged(&after),
            "a newly-appeared project must bust the cache: {after:?}"
        );
    }

    /// `.fsproj` diagnostics depend on inputs the epoch doesn't track, so they
    /// are never cached: every pull is a `Full` with no `result_id` to echo.
    #[test]
    fn fsproj_is_never_cached() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/App.fsproj").unwrap();
        state
            .docs
            .insert(uri.clone(), "<Project this is broken".to_string());

        let result = handle(&mut state, params(uri));
        match &result {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
                assert!(
                    !full.full_document_diagnostic_report.items.is_empty(),
                    "the broken fsproj should still report: {full:?}"
                );
                assert!(
                    full.full_document_diagnostic_report.result_id.is_none(),
                    "fsproj reports must carry no result_id (uncacheable): {full:?}"
                );
            }
            other => panic!("expected a Full report, got {other:?}"),
        }
    }

    /// Regression: an unreadable `.fsproj` must clear (empty `Full`), not get the
    /// fsproj XML parser run on a fabricated empty string (which would emit a
    /// bogus "malformed XML").
    #[test]
    fn unreadable_fsproj_is_empty_not_malformed() {
        let mut state = State::default();
        let uri = Url::parse("file:///proj/Missing.fsproj").unwrap();
        let result = handle(&mut state, params(uri));
        assert!(
            items(&result).is_empty(),
            "an unreadable fsproj must clear, not fabricate an error: {result:?}"
        );
    }
}
