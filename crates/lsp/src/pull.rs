//! Pure assembly for the LSP 3.17 **pull** diagnostic model
//! (`textDocument/diagnostic`).
//!
//! Where [`crate::publish`] turns a document's [`FileDiagnostics`] partition
//! into the stateful per-URI *notifications* the push model needs, this module
//! turns the *same* partition into the stateless *report* the pull model
//! returns. The two share one diagnostic computation
//! (`crate::server::grouped_for_uri`) and one `#line` target resolver
//! (`crate::publish::resolve_target`); they differ only in packaging.
//!
//! ## Same-file only, by design (for now)
//!
//! A document-pull report's `related_documents[B]` is read by clients as **B's
//! complete diagnostic set**. A single generating document only knows *its own*
//! contribution to `B`, not the union across every generator (which is what the
//! push planner accumulates and publishes). Emitting one document's slice as a
//! `Full` report for `B` would let two files that both `#line`-relocate onto `B`
//! clobber each other, and would never clear `B` when a generator goes clean.
//!
//! So this first cut reports **only the requested document's own diagnostics**:
//! same-file groups, plus any `#line N "f"` group whose `f` resolves back to the
//! requested document itself (its own contribution, possibly line-shifted).
//! `#line` groups targeting *other* files are deferred to the push path — which
//! is retained and computes the correct cross-file union/clear — until pull can
//! produce that union too (see `docs/completed/pull-diagnostics-plan.md`, the deferred
//! cross-file stage). Correctness over availability: omit rather than over-claim.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use borzoi_cst::language_version::LanguageVersion;
use lsp_types::{
    Diagnostic, FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
    UnchangedDocumentDiagnosticReport, Url, WorkspaceDocumentDiagnosticReport,
    WorkspaceFullDocumentDiagnosticReport, WorkspaceUnchangedDocumentDiagnosticReport,
};

use crate::diagnostics::FileDiagnostics;
use crate::publish::resolve_target;

/// A 128-bit, deterministic `result_id` for a pulled F# source document: a hash
/// of the *exact inputs* its diagnostics are computed from — the file's
/// `source`, the active preprocessor `symbols` (its resolved
/// `DefineConstants`), and the resolved language version `lang` (the
/// feature-gate input, e.g. the `#elif` FS3350 check).
///
/// Hashing the inputs is what lets the handlers answer `Unchanged` (when this id
/// matches what the client last received) **without** lexing or parsing the
/// file. The diagnostics of a `.fs`/`.fsi`/`.fsx` file are a deterministic
/// function of exactly `(source, symbols, lang)` — the lexer and the
/// conditional-compilation-aware parser read nothing else — so identical inputs
/// guarantee identical diagnostics. Crucially, `symbols` and `lang` are the
/// values `crate::server::grouped_for_uri` itself would resolve (via
/// `Workspace::symbols_for` / `lang_version_for`), so the fast path keys on the
/// same project context the full recompute would discover from disk — including
/// a `.fsproj` that has only just appeared, without waiting for a
/// `didChangeWatchedFiles`, or one whose `<LangVersion>` alone changed.
///
/// The symbols are sorted before hashing (the set has no inherent order). Two
/// fixed-seed [`DefaultHasher`] passes (discriminated by a salt byte) give 128
/// bits, so a false `Unchanged` from a collision is ~1/2^128.
pub fn diagnostic_result_id(
    source: &str,
    symbols: &HashSet<String>,
    lang: LanguageVersion,
) -> String {
    let mut sorted: Vec<&str> = symbols.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let hash_with = |salt: u8| {
        let mut hasher = DefaultHasher::new();
        salt.hash(&mut hasher);
        sorted.hash(&mut hasher);
        source.hash(&mut hasher);
        lang.hash(&mut hasher);
        hasher.finish()
    };
    format!("{:016x}{:016x}", hash_with(0), hash_with(1))
}

/// Assemble the full document-diagnostic report for `requested` from its
/// [`FileDiagnostics`] partition (as produced by
/// `crate::server::grouped_for_uri`).
///
/// The report's `items` are the requested document's own diagnostics: every
/// same-file group, plus any `#line` group that resolves back to `requested`.
/// `#line` groups targeting other files are dropped (deferred to the push path;
/// see the module docs). `related_documents` is therefore always `None`, and
/// `result_id` is always `None` — this first cut returns a `Full` report every
/// time (no `Unchanged` caching; see `docs/completed/pull-diagnostics-plan.md` Stage 3).
pub fn document_report(
    requested: &Url,
    groups: Vec<FileDiagnostics>,
) -> RelatedFullDocumentDiagnosticReport {
    let mut items: Vec<Diagnostic> = Vec::new();

    for group in groups {
        match group.file {
            // The document's own group.
            None => items.extend(group.diagnostics),
            // A `#line N "f"` group relocates diagnostics onto `f`. Keep it only
            // when `f` resolves back to the requested document (a self-reference
            // is part of this document's own set); a group targeting another
            // file is the push path's responsibility for now.
            Some(file) => {
                if resolve_target(requested, &file).as_ref() == Some(requested) {
                    items.extend(group.diagnostics);
                }
            }
        }
    }

    RelatedFullDocumentDiagnosticReport {
        related_documents: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id: None,
            items,
        },
    }
}

/// Assemble one **`Full`** `workspace/diagnostic` entry for `uri` from its
/// [`FileDiagnostics`] partition, stamping `result_id` (present only for
/// cacheable source files). The entry carries `uri`'s **own** diagnostics — the
/// same set [`document_report`] produces as `items` (cross-file `#line` groups
/// deferred per the module docs), so a workspace pull and a document pull agree
/// on any one file. `version: None`: the server tracks no document versions.
pub fn workspace_entry(
    uri: Url,
    groups: Vec<FileDiagnostics>,
    result_id: Option<String>,
) -> WorkspaceDocumentDiagnosticReport {
    let mut full = document_report(&uri, groups).full_document_diagnostic_report;
    full.result_id = result_id;
    WorkspaceDocumentDiagnosticReport::Full(WorkspaceFullDocumentDiagnosticReport {
        uri,
        version: None,
        full_document_diagnostic_report: full,
    })
}

/// Assemble one **`Unchanged`** `workspace/diagnostic` entry: the client's
/// cached report for `uri` is still valid, so we only echo `result_id`.
/// `version: None`, as for [`workspace_entry`].
pub fn workspace_unchanged(uri: Url, result_id: String) -> WorkspaceDocumentDiagnosticReport {
    WorkspaceDocumentDiagnosticReport::Unchanged(WorkspaceUnchangedDocumentDiagnosticReport {
        uri,
        version: None,
        unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport { result_id },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{DiagnosticSeverity, Range};

    use crate::publish::PublishState;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn diag(message: &str) -> Diagnostic {
        Diagnostic {
            range: Range::default(),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("borzoi".to_string()),
            message: message.to_string(),
            ..Default::default()
        }
    }

    fn group(file: Option<&str>, messages: &[&str]) -> FileDiagnostics {
        FileDiagnostics {
            file: file.map(str::to_string),
            diagnostics: messages.iter().map(|m| diag(m)).collect(),
        }
    }

    /// The report's own (`items`) messages.
    fn item_messages(report: &RelatedFullDocumentDiagnosticReport) -> Vec<&str> {
        report
            .full_document_diagnostic_report
            .items
            .iter()
            .map(|d| d.message.as_str())
            .collect()
    }

    #[test]
    fn same_file_group_becomes_items() {
        let doc = url("file:///proj/Gen.fs");
        let report = document_report(&doc, vec![group(None, &["boom"])]);
        assert_eq!(item_messages(&report), ["boom"]);
        assert!(report.related_documents.is_none());
    }

    #[test]
    fn clean_document_is_empty_full_report() {
        let doc = url("file:///proj/Gen.fs");
        let report = document_report(&doc, vec![group(None, &[])]);
        assert!(item_messages(&report).is_empty());
        assert!(report.related_documents.is_none());
    }

    /// A `#line` group targeting *another* file is omitted from the report (its
    /// diagnostics belong to the push path's cross-file union, not to this
    /// document's own pull report).
    #[test]
    fn cross_file_group_is_deferred_not_reported() {
        let doc = url("file:///proj/Gen.fs");
        let report = document_report(
            &doc,
            vec![group(None, &["own"]), group(Some("Lexer.fsl"), &["cross"])],
        );
        assert_eq!(item_messages(&report), ["own"]);
        assert!(
            report.related_documents.is_none(),
            "cross-file groups must not be over-claimed as a related Full report"
        );
    }

    /// Several distinct cross-file targets are all deferred, leaving only the
    /// document's own (here empty) set.
    #[test]
    fn all_cross_file_targets_are_deferred() {
        let doc = url("file:///proj/Gen.fs");
        let report = document_report(
            &doc,
            vec![
                group(Some("A.fsl"), &["a"]),
                group(Some("sub/B.fsl"), &["b"]),
            ],
        );
        assert!(item_messages(&report).is_empty());
        assert!(report.related_documents.is_none());
    }

    /// A `#line` directive that resolves back to the document's own URI is part
    /// of its own set and merges into `items` (mirrors the push path's
    /// self-referential merge).
    #[test]
    fn self_referential_directive_merges_into_items() {
        let doc = url("file:///proj/Gen.fs");
        let report = document_report(
            &doc,
            vec![group(None, &["same"]), group(Some("Gen.fs"), &["loop"])],
        );
        assert_eq!(item_messages(&report), ["same", "loop"]);
        assert!(report.related_documents.is_none());
    }

    // --- D9: pull ≡ push on the requested document's own set ----------------

    /// The `items` this module produces for `A` must equal what
    /// [`PublishState::plan`] publishes onto `A`'s own URI for the same groups —
    /// both represent "`A`'s own diagnostics". (Pull defers `A`'s *cross-file*
    /// contributions to push, so only the own-URI set is compared.) This pins
    /// the stateless pull assembler to the trusted push planner.
    fn assert_pull_eq_push_own(doc: &Url, groups: Vec<FileDiagnostics>) {
        // Push: the publish addressed to `doc`'s own URI (always present, and
        // first, in a fresh single-document plan).
        let mut push = PublishState::new();
        let params = push.plan(doc, groups.clone());
        let push_own: Vec<String> = params
            .iter()
            .find(|p| &p.uri == doc)
            .map(|p| p.diagnostics.iter().map(|d| d.message.clone()).collect())
            .unwrap_or_default();

        // Pull: the report's own items.
        let report = document_report(doc, groups);
        let pull_own: Vec<String> = report
            .full_document_diagnostic_report
            .items
            .iter()
            .map(|d| d.message.clone())
            .collect();

        assert_eq!(pull_own, push_own, "pull/push disagree on {doc}'s own set");
    }

    #[test]
    fn pull_eq_push_same_file_only() {
        assert_pull_eq_push_own(&url("file:///proj/Gen.fs"), vec![group(None, &["boom"])]);
    }

    #[test]
    fn pull_eq_push_ignores_cross_file() {
        assert_pull_eq_push_own(
            &url("file:///proj/Gen.fs"),
            vec![
                group(None, &["own"]),
                group(Some("Lexer.fsl"), &["x", "y"]),
                group(Some("../Other.fsl"), &["z"]),
            ],
        );
    }

    #[test]
    fn pull_eq_push_self_referential() {
        assert_pull_eq_push_own(
            &url("file:///proj/Gen.fs"),
            vec![group(None, &["same"]), group(Some("Gen.fs"), &["loop"])],
        );
    }

    #[test]
    fn pull_eq_push_clean() {
        assert_pull_eq_push_own(&url("file:///proj/Gen.fs"), vec![group(None, &[])]);
    }

    // --- workspace_entry ----------------------------------------------------

    /// A workspace entry is a `Full` report tagged with the file's URI, no
    /// version, carrying that file's own diagnostics and the given `result_id`.
    #[test]
    fn workspace_entry_is_full_tagged_with_uri() {
        let uri = url("file:///proj/Bad.fs");
        let entry = workspace_entry(
            uri.clone(),
            vec![group(None, &["boom"])],
            Some("abc123".to_string()),
        );
        match entry {
            WorkspaceDocumentDiagnosticReport::Full(full) => {
                assert_eq!(full.uri, uri);
                assert_eq!(full.version, None);
                assert_eq!(
                    full.full_document_diagnostic_report.result_id.as_deref(),
                    Some("abc123")
                );
                let msgs: Vec<&str> = full
                    .full_document_diagnostic_report
                    .items
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect();
                assert_eq!(msgs, ["boom"]);
            }
            other => panic!("expected a Full workspace entry, got {other:?}"),
        }
    }

    /// A clean file still yields a `Full` entry (empty items), so a client can
    /// tell "checked, clean" from "not checked" and clear stale diagnostics.
    #[test]
    fn workspace_entry_clean_file_is_empty_full() {
        let uri = url("file:///proj/Lib.fs");
        let entry = workspace_entry(uri.clone(), vec![group(None, &[])], None);
        match entry {
            WorkspaceDocumentDiagnosticReport::Full(full) => {
                assert_eq!(full.uri, uri);
                assert!(full.full_document_diagnostic_report.items.is_empty());
                assert!(full.full_document_diagnostic_report.result_id.is_none());
            }
            other => panic!("expected a Full workspace entry, got {other:?}"),
        }
    }

    /// A workspace `Unchanged` entry tags the URI and echoes only the result_id.
    #[test]
    fn workspace_unchanged_echoes_result_id() {
        let uri = url("file:///proj/Lib.fs");
        let entry = workspace_unchanged(uri.clone(), "abc123".to_string());
        match entry {
            WorkspaceDocumentDiagnosticReport::Unchanged(unchanged) => {
                assert_eq!(unchanged.uri, uri);
                assert_eq!(unchanged.version, None);
                assert_eq!(
                    unchanged.unchanged_document_diagnostic_report.result_id,
                    "abc123"
                );
            }
            other => panic!("expected an Unchanged workspace entry, got {other:?}"),
        }
    }

    /// Pin the wire shape: the workspace `Unchanged` entry serialises with the
    /// `"unchanged"` discriminator and the flattened `resultId` / `uri`.
    #[test]
    fn workspace_unchanged_serialises_with_kind() {
        let entry = workspace_unchanged(url("file:///proj/Lib.fs"), "abc123".to_string());
        let json = serde_json::to_value(&entry).expect("serialise");
        assert_eq!(json["kind"], "unchanged", "{json}");
        assert_eq!(json["resultId"], "abc123", "{json}");
        assert_eq!(json["uri"], "file:///proj/Lib.fs", "{json}");
    }

    // --- diagnostic_result_id ----------------------------------------------

    fn syms(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    const LANG: LanguageVersion = LanguageVersion::DEFAULT;

    #[test]
    fn result_id_is_deterministic() {
        assert_eq!(
            diagnostic_result_id("let x = 1\n", &syms(&["FOO"]), LANG),
            diagnostic_result_id("let x = 1\n", &syms(&["FOO"]), LANG),
        );
        // Symbol-set order doesn't matter (the set has none).
        assert_eq!(
            diagnostic_result_id("let x = 1\n", &syms(&["A", "B"]), LANG),
            diagnostic_result_id("let x = 1\n", &syms(&["B", "A"]), LANG),
        );
        // 128 bits → 32 hex chars.
        assert_eq!(diagnostic_result_id("", &syms(&[]), LANG).len(), 32);
    }

    #[test]
    fn result_id_changes_with_source() {
        assert_ne!(
            diagnostic_result_id("let x = 1\n", &syms(&["FOO"]), LANG),
            diagnostic_result_id("let x = 2\n", &syms(&["FOO"]), LANG),
        );
    }

    #[test]
    fn result_id_changes_with_symbols() {
        // The load-bearing property: a change to the active defines (a different
        // project context) changes the id even for identical source, so it can't
        // be falsely cached.
        assert_ne!(
            diagnostic_result_id("let x = 1\n", &syms(&["FOO"]), LANG),
            diagnostic_result_id("let x = 1\n", &syms(&["BAR"]), LANG),
        );
    }

    #[test]
    fn result_id_changes_with_lang_version() {
        // Same load-bearing property for the language version: it gates
        // feature diagnostics (`#elif` FS3350), so a project changing only
        // `<LangVersion>` must invalidate the cached report.
        assert_ne!(
            diagnostic_result_id("let x = 1\n", &syms(&["FOO"]), LanguageVersion::V7_0),
            diagnostic_result_id("let x = 1\n", &syms(&["FOO"]), LanguageVersion::V8_0),
        );
    }
}
