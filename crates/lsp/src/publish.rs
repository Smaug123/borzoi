//! Pure planning for `textDocument/publishDiagnostics` across files.
//!
//! `publishDiagnostics` is per-URI and *stateful*: the client shows the last
//! set the server published for each URI, so every URI we ever squiggle must
//! later be cleared explicitly. When a `#line N "f"` directive relocates a
//! diagnostic onto another file `f` (Stage 4 of
//! `docs/completed/line-directive-remap-plan.md`), that diagnostic must be published
//! under `f`'s URI — and erased there when the generating document stops
//! producing it.
//!
//! [`PublishState`] turns the per-file [`FileDiagnostics`] partition
//! ([`crate::diagnostics::grouped_diagnostics`]) into the exact list of
//! notifications to send, keeping enough state to clear targets that drop out
//! and to publish the *union* when several documents target one file. It is a
//! pure state machine: given the changed document's URI and its groups, it
//! returns the notifications and updates its own bookkeeping — no IO, no
//! connection — so the binary's only job is to send what it returns.
//!
//! ## The unified contribution model
//!
//! Every document `G` *contributes* a set of diagnostics to each *target*
//! URI: its same-file diagnostics are its contribution to `T = G` itself, and
//! each `#line N "f"` group is its contribution to `T = resolve(f)`. A
//! publish for any URI `U` is then the union, over all documents, of their
//! contribution to `U`. This single rule covers same-file publishing,
//! clearing on edit/close, cross-file relocation, and the case where a file
//! is simultaneously a generator and another document's target.

use std::collections::HashMap;
use std::path::Path;

use lsp_types::{Diagnostic, PublishDiagnosticsParams, Url};

use crate::diagnostics::FileDiagnostics;
use crate::paths::lexically_normalize;

/// Per-document cross-file bookkeeping. For each generating document, the
/// diagnostics it currently contributes to each target URI (including itself,
/// under its own URI, for the same-file group). Lets [`plan`] clear targets a
/// document drops and publish the union when several documents share a target.
///
/// [`plan`]: PublishState::plan
#[derive(Debug, Default)]
pub struct PublishState {
    /// `generating URI -> (target URI -> that document's diagnostics for it)`.
    /// A document with no diagnostics at all holds no entry; a target a
    /// document contributes nothing to holds no key.
    contributions: HashMap<Url, HashMap<Url, Vec<Diagnostic>>>,
}

impl PublishState {
    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Plan the publishes for `changed`, whose diagnostics partition into
    /// `groups` (the same-file group `file: None`, plus one cross-file group
    /// per `#line N "f"`). Returns every notification to send: the changed
    /// document's own set first, then each affected target's recomputed union,
    /// including empty sets that *clear* targets `changed` no longer feeds.
    ///
    /// Cross-file file strings are resolved per
    /// `docs/completed/line-directive-remap-plan.md` Q1 (absolute as-is; relative
    /// against `changed`'s directory; unresolvable ⇒ dropped with a warning).
    pub fn plan(
        &mut self,
        changed: &Url,
        groups: Vec<FileDiagnostics>,
    ) -> Vec<PublishDiagnosticsParams> {
        let mut new_contrib: HashMap<Url, Vec<Diagnostic>> = HashMap::new();
        for group in groups {
            match group.file {
                None => {
                    // Same-file: `changed`'s contribution to itself. Skip when
                    // empty so a clean document holds no entry (its own URI is
                    // still published below via `changed` always being
                    // affected).
                    if !group.diagnostics.is_empty() {
                        new_contrib
                            .entry(changed.clone())
                            .or_default()
                            .extend(group.diagnostics);
                    }
                }
                Some(file) => match resolve_target(changed, &file) {
                    Some(target) => new_contrib
                        .entry(target)
                        .or_default()
                        .extend(group.diagnostics),
                    None => crate::log_warn!(
                        "dropping cross-file #line diagnostics: cannot resolve target",
                        file = file,
                        document = changed
                    ),
                },
            }
        }

        let old = self.contributions.remove(changed).unwrap_or_default();
        // Targets to republish: everything `changed` newly or previously fed,
        // minus its own URI (always republished first, below).
        let targets = sorted_targets(new_contrib.keys().chain(old.keys()), changed);

        if !new_contrib.is_empty() {
            self.contributions.insert(changed.clone(), new_contrib);
        }

        let mut out = Vec::with_capacity(targets.len() + 1);
        out.push(self.publish_for(changed));
        out.extend(targets.iter().map(|t| self.publish_for(t)));
        out
    }

    /// Plan the publishes when `changed` is closed: it contributes nothing, so
    /// every target it fed is recomputed (clearing where it was the only
    /// contributor) and its own URI is cleared. Mirrors the LSP convention of
    /// clearing a closed document's diagnostics, extended across the files it
    /// had relocated diagnostics onto.
    pub fn plan_close(&mut self, changed: &Url) -> Vec<PublishDiagnosticsParams> {
        let old = self.contributions.remove(changed).unwrap_or_default();
        let targets = sorted_targets(old.keys(), changed);
        let mut out = Vec::with_capacity(targets.len() + 1);
        out.push(self.publish_for(changed));
        out.extend(targets.iter().map(|t| self.publish_for(t)));
        out
    }

    /// The notification for `target`: the union, over all documents, of their
    /// current contribution to it. An empty union clears the URI.
    fn publish_for(&self, target: &Url) -> PublishDiagnosticsParams {
        PublishDiagnosticsParams {
            uri: target.clone(),
            diagnostics: self.union_for(target),
            version: None,
        }
    }

    /// The union of every document's contribution to `target`, concatenated in
    /// a deterministic (sorted-by-URI) generator order.
    fn union_for(&self, target: &Url) -> Vec<Diagnostic> {
        let mut generators: Vec<&Url> = self.contributions.keys().collect();
        generators.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut out = Vec::new();
        for g in generators {
            if let Some(diags) = self.contributions[g].get(target) {
                out.extend(diags.iter().cloned());
            }
        }
        out
    }
}

/// The affected targets other than `changed`, sorted by URI string and
/// deduplicated, so the planned output is deterministic.
fn sorted_targets<'a>(keys: impl Iterator<Item = &'a Url>, changed: &Url) -> Vec<Url> {
    let mut targets: Vec<Url> = keys.filter(|k| *k != changed).cloned().collect();
    targets.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    targets.dedup();
    targets
}

/// Resolve a `#line` directive's verbatim file string to a URI, anchored on
/// the generating document. An absolute path is used as-is; a relative path is
/// joined onto `generating`'s parent directory (which requires `generating` to
/// be a `file:` URL — the only anchor we have). The result is lexically
/// normalized so it matches the URI the client uses for the opened source.
/// Returns `None` when the string cannot be resolved (e.g. a relative path
/// under a non-`file:` document), so the caller can drop those diagnostics
/// rather than publish them to a guessed location. Mirrors FCS taking the
/// filename verbatim while leaving LSP-specific URI resolution to us
/// (`docs/completed/line-directive-remap-plan.md` Q1).
///
/// `pub(crate)` so the pull-diagnostic path ([`crate::pull`]) resolves
/// `#line` cross-file targets identically to the push path — one definition,
/// no drift.
pub(crate) fn resolve_target(generating: &Url, file: &str) -> Option<Url> {
    let raw = Path::new(file);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        generating.to_file_path().ok()?.parent()?.join(raw)
    };
    Url::from_file_path(lexically_normalize(&joined)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{DiagnosticSeverity, Range};

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

    /// The messages a planned notification carries, for terse assertions.
    fn messages(params: &PublishDiagnosticsParams) -> Vec<&str> {
        params
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect()
    }

    /// Find the single planned notification for `uri`.
    fn for_uri<'a>(
        params: &'a [PublishDiagnosticsParams],
        uri: &Url,
    ) -> &'a PublishDiagnosticsParams {
        let matches: Vec<_> = params.iter().filter(|p| p.uri == *uri).collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one publish for {uri}: {params:#?}"
        );
        matches[0]
    }

    // --- path resolution ----------------------------------------------------

    #[test]
    fn resolve_relative_joins_generating_directory() {
        let doc = url("file:///proj/src/Gen.fs");
        assert_eq!(
            resolve_target(&doc, "Lexer.fsl"),
            Some(url("file:///proj/src/Lexer.fsl"))
        );
        assert_eq!(
            resolve_target(&doc, "sub/Lexer.fsl"),
            Some(url("file:///proj/src/sub/Lexer.fsl"))
        );
    }

    #[test]
    fn resolve_absolute_used_as_is() {
        let doc = url("file:///proj/src/Gen.fs");
        assert_eq!(
            resolve_target(&doc, "/other/Lexer.fsl"),
            Some(url("file:///other/Lexer.fsl"))
        );
    }

    /// `..` and `.` segments are lexically normalized, so a directive from a
    /// generated file in `obj/` (the common fslex layout) resolves to the same
    /// URI the client uses for the real source — otherwise diagnostics would
    /// land on a phantom `file:///proj/obj/../Lexer.fsl` resource and never
    /// union or clear with the opened target.
    #[test]
    fn resolve_normalizes_parent_segments() {
        let doc = url("file:///proj/obj/Gen.fs");
        assert_eq!(
            resolve_target(&doc, "../Lexer.fsl"),
            Some(url("file:///proj/Lexer.fsl"))
        );
        assert_eq!(
            resolve_target(&doc, "./sub/../Lexer.fsl"),
            Some(url("file:///proj/obj/Lexer.fsl"))
        );
        // Absolute targets are normalized too.
        assert_eq!(
            resolve_target(&doc, "/a/b/../c/Lexer.fsl"),
            Some(url("file:///a/c/Lexer.fsl"))
        );
    }

    #[test]
    fn resolve_relative_under_non_file_uri_fails() {
        // No filesystem anchor for a relative path under an unsaved buffer.
        assert_eq!(
            resolve_target(&url("untitled:Untitled-1"), "Lexer.fsl"),
            None
        );
    }

    // --- plan ---------------------------------------------------------------

    /// Regression: a document with only same-file diagnostics plans exactly
    /// one notification — its own URI — identical to the pre-Stage-4 server.
    #[test]
    fn plan_same_file_only_is_single_publish() {
        let mut state = PublishState::new();
        let doc = url("file:///proj/Gen.fs");
        let params = state.plan(&doc, vec![group(None, &["boom"])]);
        assert_eq!(params.len(), 1, "{params:#?}");
        assert_eq!(params[0].uri, doc);
        assert_eq!(messages(&params[0]), ["boom"]);
    }

    /// A clean document still plans an (empty) publish for its own URI, so any
    /// previously published diagnostics are cleared.
    #[test]
    fn plan_clean_document_clears_own_uri() {
        let mut state = PublishState::new();
        let doc = url("file:///proj/Gen.fs");
        let params = state.plan(&doc, vec![group(None, &[])]);
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].uri, doc);
        assert!(params[0].diagnostics.is_empty());
    }

    /// Fan-out: a cross-file directive yields two notifications — the document's
    /// own (same-file) set, then the resolved target's set.
    #[test]
    fn plan_fans_out_to_cross_file_target() {
        let mut state = PublishState::new();
        let doc = url("file:///proj/Gen.fs");
        let params = state.plan(
            &doc,
            vec![group(None, &["own"]), group(Some("Lexer.fsl"), &["cross"])],
        );
        let target = url("file:///proj/Lexer.fsl");
        assert_eq!(params.len(), 2, "{params:#?}");
        assert_eq!(params[0].uri, doc, "own URI is published first");
        assert_eq!(messages(&params[0]), ["own"]);
        assert_eq!(messages(for_uri(&params, &target)), ["cross"]);
    }

    /// Clearing: once the cross-file error is gone, the next plan republishes
    /// the target with an empty set.
    #[test]
    fn plan_clears_dropped_cross_file_target() {
        let mut state = PublishState::new();
        let doc = url("file:///proj/Gen.fs");
        let target = url("file:///proj/Lexer.fsl");
        state.plan(
            &doc,
            vec![group(None, &[]), group(Some("Lexer.fsl"), &["cross"])],
        );
        let params = state.plan(&doc, vec![group(None, &[])]);
        // own URI (empty) + the cleared target (empty).
        assert!(for_uri(&params, &doc).diagnostics.is_empty());
        assert!(
            for_uri(&params, &target).diagnostics.is_empty(),
            "target should be cleared: {params:#?}"
        );
    }

    /// Closing a document clears its own URI and every target it had fed.
    #[test]
    fn plan_close_clears_own_and_targets() {
        let mut state = PublishState::new();
        let doc = url("file:///proj/Gen.fs");
        let target = url("file:///proj/Lexer.fsl");
        state.plan(
            &doc,
            vec![group(None, &["own"]), group(Some("Lexer.fsl"), &["cross"])],
        );
        let params = state.plan_close(&doc);
        assert!(for_uri(&params, &doc).diagnostics.is_empty());
        assert!(for_uri(&params, &target).diagnostics.is_empty());
    }

    /// Union: two documents targeting one file publish the union of their
    /// contributions; dropping one leaves the other's diagnostics in place.
    #[test]
    fn plan_unions_shared_target_across_documents() {
        let mut state = PublishState::new();
        let g1 = url("file:///proj/G1.fs");
        let g2 = url("file:///proj/G2.fs");
        let target = url("file:///proj/Lexer.fsl");

        state.plan(&g1, vec![group(Some("Lexer.fsl"), &["from-g1"])]);
        let after_g2 = state.plan(&g2, vec![group(Some("Lexer.fsl"), &["from-g2"])]);
        // Sorted generator order (G1 before G2) makes the union deterministic.
        assert_eq!(
            messages(for_uri(&after_g2, &target)),
            ["from-g1", "from-g2"],
            "{after_g2:#?}"
        );

        // G1 stops contributing: only G2's diagnostic remains on the target.
        let after_g1_clean = state.plan(&g1, vec![group(None, &[])]);
        assert_eq!(messages(for_uri(&after_g1_clean, &target)), ["from-g2"]);
    }

    /// Different path spellings of one target (a `../`-relative directive from a
    /// generated file in `obj/` and a plain-relative directive from the project
    /// root) normalize to the same URI and therefore union — they must not land
    /// on separate phantom resources.
    #[test]
    fn plan_unions_target_across_path_spellings() {
        let mut state = PublishState::new();
        let from_obj = url("file:///proj/obj/Gen.fs");
        let from_root = url("file:///proj/Root.fs");
        let target = url("file:///proj/Lexer.fsl");

        state.plan(&from_obj, vec![group(Some("../Lexer.fsl"), &["via-obj"])]);
        let after_root = state.plan(&from_root, vec![group(Some("Lexer.fsl"), &["via-root"])]);
        // Both spellings resolve to the one target and union; the order is the
        // sorted generator order (`…/Root.fs` < `…/obj/Gen.fs`).
        assert_eq!(
            messages(for_uri(&after_root, &target)),
            ["via-root", "via-obj"],
            "spellings must union on the canonical target: {after_root:#?}"
        );
    }

    /// A relative cross-file directive under a non-`file:` document is dropped
    /// (no anchor); the same-file set is unaffected.
    #[test]
    fn plan_drops_unresolvable_cross_file_keeps_same_file() {
        let mut state = PublishState::new();
        let doc = url("untitled:Untitled-1");
        let params = state.plan(
            &doc,
            vec![group(None, &["own"]), group(Some("Lexer.fsl"), &["cross"])],
        );
        assert_eq!(params.len(), 1, "cross-file dropped: {params:#?}");
        assert_eq!(params[0].uri, doc);
        assert_eq!(messages(&params[0]), ["own"]);
    }

    /// A directive pointing at the document's own URI merges with the same-file
    /// group rather than producing a second, conflicting publish for that URI.
    #[test]
    fn plan_self_referential_directive_merges_into_own_publish() {
        let mut state = PublishState::new();
        let doc = url("file:///proj/Gen.fs");
        let params = state.plan(
            &doc,
            vec![group(None, &["same"]), group(Some("Gen.fs"), &["loop"])],
        );
        assert_eq!(params.len(), 1, "{params:#?}");
        assert_eq!(params[0].uri, doc);
        assert_eq!(messages(&params[0]), ["same", "loop"]);
    }
}
