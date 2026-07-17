//! `workspace/symbol` — project-wide symbol search.
//!
//! Folds the same top-level export extraction `textDocument/documentSymbol`
//! uses ([`crate::handlers::file_export_symbols`]) over every file of every
//! project that owns a currently-open source buffer, then keeps the symbols
//! whose name contains the request's `query` (case-insensitive; an empty query
//! matches everything). The result is always the flat [`SymbolInformation`]
//! shape — we always have a concrete `Location`, so the lazy `WorkspaceSymbol`
//! shape buys nothing.
//!
//! **Scope: the projects you're working in.** The search set is the owning
//! projects of the open documents, not an eager whole-workspace crawl — the
//! server discovers projects lazily (there's no workspace-folder scan or
//! `didChangeWatchedFiles` wiring yet), so opening *one* file in a project
//! makes *all* of that project's top-level symbols searchable, and the set
//! grows as more files are opened. A whole-workspace file→project index would
//! broaden this to the whole workspace, but it was explored and **shelved**
//! (can't be made both sound and useful with the current msbuild diagnostics —
//! see `docs/workspace-index-plan.md`), so this open-buffers scope stands.
//!
//! **Degradation (D5: under-resolve, never wrong, never panic).** A buffer whose
//! project can't be folded — orphan (no `.fsproj`), partial evaluation, or a
//! signature-bearing project `parses_for_project` refuses — falls back to
//! single-file extraction from its own buffer text, exactly as the references
//! handler degrades. The CST parser runs under `catch_unwind`, so a buffer that
//! panics the parser contributes nothing rather than crashing the server.

use std::collections::HashSet;
use std::path::PathBuf;

use borzoi_cst::syntax::{AstNode, ImplFile};
use lsp_types::{SymbolInformation, Url, WorkspaceSymbolParams, WorkspaceSymbolResponse};

use crate::cst_panic_safe::parse_with_symbols;
use crate::handlers::{file_export_symbols, symbol_information};
use crate::paths::{lexically_normalize, paths_equal};
use crate::server::State;

/// An open source buffer the search considers. `path` is `None` for non-`file:`
/// buffers (unsaved `untitled:` / in-memory docs): they can't name an owning
/// project, but their own symbols are still searchable via the single-file pass.
struct OpenSource {
    uri: Url,
    path: Option<PathBuf>,
    text: String,
}

/// Run the workspace-symbol handler. Always returns `Some` (an empty list when
/// nothing matches), so a stale client symbol panel clears rather than sticking
/// with the previous answer.
pub fn handle(state: &mut State, params: WorkspaceSymbolParams) -> Option<WorkspaceSymbolResponse> {
    // Unicode lowercase (not ASCII-only): F# identifiers can be non-ASCII, so a
    // `CAFÉ` query must fold to match `café`.
    let query = params.query.to_lowercase();

    // Snapshot the open source buffers up front (clone), releasing the borrow
    // on `state.docs` so `state` can be taken mutably below.
    let open_sources: Vec<OpenSource> = state
        .docs
        .iter()
        .filter(|(uri, _)| is_source_uri(uri))
        .map(|(uri, text)| OpenSource {
            uri: uri.clone(),
            path: uri.to_file_path().ok(),
            text: text.clone(),
        })
        .collect();

    // Search set: the owning project of each file-backed open buffer, deduped by
    // path *equality* (not byte-identical `PathBuf`, since `owning_project` spells
    // the path the way the opening file was) so a project is folded at most once.
    // This is a cost optimisation; the symbol-identity dedup below is what keeps
    // the *results* correct.
    let mut projects: Vec<PathBuf> = Vec::new();
    for src in &open_sources {
        if let Some(path) = &src.path
            && let Some(project) = state.workspace.owning_project(path)
            && !projects
                .iter()
                .any(|p| paths_equal(&lexically_normalize(p), &lexically_normalize(&project)))
        {
            projects.push(project);
        }
    }

    let mut out: Vec<SymbolInformation> = Vec::new();
    // Dedup by **symbol identity** (name + location), not by file path. A file
    // shared by several projects is listed once, while genuinely distinct
    // symbols are all kept — crucially including the different `#if` branches a
    // shared file exposes under each project's `DefineConstants` (those differ
    // in range, so a path-level skip would wrongly drop the later project's
    // branch symbol).
    let mut seen: HashSet<SymbolKey> = HashSet::new();
    // Normalised paths a *successful* project fold already covered. Project
    // folds are authoritative — each file is parsed under its compiling
    // project's defines — so the single-file fallback must skip these. Otherwise
    // an open linked file gets reparsed under its *owning*-project (or default)
    // defines, which can activate an `#if` branch no project compiles and emit a
    // phantom symbol `seen` won't catch (different range).
    let mut covered: Vec<PathBuf> = Vec::new();

    // 1. Whole-project symbols for every project we can fold.
    for project in &projects {
        // Clone the parses out of `self` inside a tight scope: `parses_for_project`
        // borrows `semantic`/`workspace`/`docs` mutably, and we need `state.docs`
        // again below for `preferred_uri`. Cloning `ProjectParses` is cheap
        // (rowan green nodes are Arc'd; `paths`/`texts` are small).
        let parses = {
            let State {
                semantic,
                workspace,
                docs,
                ..
            } = &mut *state;
            match semantic.parses_for_project(project, workspace, docs) {
                Some(p) => p.clone(),
                None => continue,
            }
        };
        for i in 0..parses.len() {
            // Normalise the include before building the URI: the msbuild parser
            // leaves `..` in `<Compile>` paths (`A/../Shared.fs`), so a linked
            // file reached from two projects would otherwise yield two distinct
            // URI spellings — defeating the identity dedup and emitting an ugly
            // `…/A/../Shared.fs` Location. `preferred_uri` still prefers an open
            // buffer's own URI when one is path-equal.
            let path = lexically_normalize(&parses.paths[i]);
            if let Some(uri) = crate::handlers::preferred_uri(&path, &state.docs) {
                collect_matching(
                    &mut out,
                    &mut seen,
                    &query,
                    parses.texts[i].as_ref(),
                    &parses.files[i],
                    &uri,
                );
            }
            covered.push(path);
        }
    }

    // 2. Single-file fallback for open buffers no project fold covered (orphans,
    //    partial / signature projects, unsaved buffers). A covered buffer is
    //    skipped: its project fold already emitted it under the right defines.
    for src in &open_sources {
        if let Some(path) = &src.path {
            let norm = lexically_normalize(path);
            if covered.iter().any(|c| paths_equal(c, &norm)) {
                continue;
            }
        }
        let symbols = state.symbols_for_uri(&src.uri);
        let lang = state.lang_version_for_uri(&src.uri);
        let Some(parse) = parse_with_symbols(&src.text, &symbols, lang) else {
            continue;
        };
        let Some(file) = ImplFile::cast(parse.root) else {
            continue;
        };
        collect_matching(&mut out, &mut seen, &query, &src.text, &file, &src.uri);
    }

    Some(WorkspaceSymbolResponse::Flat(out))
}

/// A symbol's identity for cross-source dedup: its name, the URI it lives in,
/// and its range (start/end line + UTF-16 character). Two entries equal under
/// this key are the same declaration surfaced twice; genuinely distinct
/// symbols (including different `#if` branches at different offsets) differ.
type SymbolKey = (String, String, u32, u32, u32, u32);

/// Append every export of `file` whose name matches `lower_query` (a
/// case-insensitive substring; empty matches all) and that `seen` hasn't
/// already recorded, anchored in `uri`. Matching folds Unicode case so
/// non-ASCII identifiers compare case-insensitively.
fn collect_matching(
    out: &mut Vec<SymbolInformation>,
    seen: &mut HashSet<SymbolKey>,
    lower_query: &str,
    text: &str,
    file: &ImplFile,
    uri: &Url,
) {
    for (name, kind, range) in file_export_symbols(text, file) {
        if !lower_query.is_empty() && !name.to_lowercase().contains(lower_query) {
            continue;
        }
        let key = (
            name.clone(),
            uri.as_str().to_string(),
            range.start.line,
            range.start.character,
            range.end.line,
            range.end.character,
        );
        if seen.insert(key) {
            out.push(symbol_information(uri, name, kind, range));
        }
    }
}

/// Whether `uri`'s path names an F# source file (`.fs` / `.fsi` / `.fsx`),
/// case-insensitively. Works on the URI path directly so non-`file:` buffers
/// (in-memory / unsaved) are classified too — mirrors `server::path_extension`.
fn is_source_uri(uri: &Url) -> bool {
    let last = uri.path().rsplit('/').next().unwrap_or("");
    std::path::Path::new(last)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "fs" | "fsi" | "fsx"))
}
