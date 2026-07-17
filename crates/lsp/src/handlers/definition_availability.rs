//! Explaining *why* go-to-definition finds nothing at a cursor.
//!
//! [`crate::handlers::definition`] is deliberately silent on every failure mode
//! (D5: say nothing rather than guess), so a jump that lands on a name we can't
//! resolve yields a bare `null`. That's honest but unhelpful: an agent can't
//! tell an unmodelled `AutoOpen` import from a genuine typo from a click on
//! whitespace. This module turns the *resolution outcome already computed by
//! sema* into an inspectable [`DefinitionUnavailable`] value — a data
//! description of the reason, per "data descriptions over behavioural
//! abstractions" — that a surface (hover today; a structured extension or
//! `window/logMessage` later) can render without re-deriving anything.
//!
//! The reasons mirror sema's [`Resolution`] taxonomy one-for-one, so the
//! classifier is a total, side-effect-free function of the resolution at the
//! cursor. It never claims "unavailable" for a *navigable* resolution
//! ([`Resolution::Local`] / [`Resolution::Item`] / [`Resolution::Entity`] /
//! [`Resolution::Member`]) — those are what go-to-definition *can* answer
//! (modulo the referenced-assembly PDB read, which is a separate downstream
//! concern this layer intentionally does not predict).

use borzoi_cst::syntax::{SyntaxKind, SyntaxNode};
use borzoi_sema::{DeferredReason, Resolution, ResolvedFile};
use rowan::{TextRange, TextSize};

use super::{smallest_resolution_at, smallest_resolution_with_range};

/// Why go-to-definition produced no navigable location under the cursor. Each
/// variant is a distinct, honest reason we declined — the closed set mirrors
/// sema's [`Resolution`] failure taxonomy, plus the one case sema records
/// nothing for (an identifier with no occurrence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// A single-segment name bound by no in-file scope
    /// ([`DeferredReason::UnboundName`]). It may come from an `open`ed /
    /// `AutoOpen`'d module or a referenced assembly we don't model yet — the
    /// "unmodelled import" case, not necessarily a typo.
    UnboundName,
    /// The member / qualified tail of a dotted path (`a.B`, `M.N.x`)
    /// ([`DeferredReason::QualifiedAccess`]): resolving the segments after the
    /// first needs the receiver's inferred type or a module / assembly
    /// environment we don't have here.
    QualifiedAccess,
    /// A type-position name where a shadow is *possible* but unpinnable — an
    /// opaque `open` could supply a type of this name
    /// ([`DeferredReason::ShadowableType`]) — so we decline rather than guess.
    ShadowableType,
    /// The name resolves to nothing in any scope or import we model
    /// ([`Resolution::Unresolved`]). Sema reserves this for Phase 4 and does
    /// not produce it yet; carried so the mapping is total the day it does.
    Unresolved,
    /// The cursor is on an identifier the resolver recorded *no* occurrence for
    /// — a resolver-coverage gap, not a definite error in the user's code.
    /// Distinct so a surface can hedge it (or route it to telemetry) rather
    /// than present it as a confident diagnosis.
    UntrackedName,
}

/// Why go-to-definition is unavailable at a cursor: the [`UnavailableReason`],
/// plus whether resolution ran in single-file fallback (no project context).
/// The `degraded_single_file` flag is *context*, not a separate reason — an
/// `UnboundName` in degraded mode may really be a cross-file symbol the missing
/// project would have supplied, so the flag colours the explanation without
/// changing which resolution arm fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefinitionUnavailable {
    pub reason: UnavailableReason,
    pub degraded_single_file: bool,
}

impl DefinitionUnavailable {
    /// A markdown explanation for a hover (or any client that renders markdown).
    /// A `**No definition available**` header, the reason sentence, then — in
    /// degraded mode — a note that project context was missing.
    pub fn explain(&self) -> String {
        let mut body = format!("**No definition available**\n\n{}", self.reason.sentence());
        if self.degraded_single_file {
            body.push_str("\n\n");
            body.push_str(DEGRADED_NOTE);
        }
        body
    }
}

impl UnavailableReason {
    /// The one-sentence explanation of this reason (no header, no degraded
    /// note). Kept `&'static str` so it's trivially testable and allocation-free.
    fn sentence(self) -> &'static str {
        match self {
            UnavailableReason::UnboundName => {
                "This name isn't bound by any definition, `open`, or `AutoOpen` this analyzer \
                 models yet — it may come from an imported module or referenced assembly whose \
                 contents aren't resolved here."
            }
            UnavailableReason::QualifiedAccess => {
                "This is a qualified or member access (`a.B`) whose target needs the receiver's \
                 inferred type, or a module / assembly environment the analyzer hasn't resolved \
                 yet."
            }
            UnavailableReason::ShadowableType => {
                "This type name could be shadowed by an `open` the analyzer can't see through, so \
                 it declines to guess which type it refers to."
            }
            UnavailableReason::Unresolved => {
                "This name resolves to nothing in any scope or import the analyzer models."
            }
            UnavailableReason::UntrackedName => {
                "The analyzer recorded no resolution for this identifier — a coverage gap rather \
                 than a definite error in your code."
            }
        }
    }
}

/// The note appended in single-file fallback: the file wasn't analysed as part
/// of an evaluated project, so anything cross-file or cross-assembly is out of
/// reach regardless of its own resolvability.
const DEGRADED_NOTE: &str = "_Analyzed without project context (its `.fsproj` didn't evaluate, or \
                             the file isn't one of the project's compile items), so cross-file and \
                             referenced-assembly symbols can't be resolved here._";

/// Classify why go-to-definition finds nothing at `byte` in `file`. `None` when
/// there *is* something to navigate to (a resolution of a navigable kind) or
/// when the cursor is on nothing name-like (whitespace, a keyword, punctuation)
/// — in both cases there is no honest explanation to give. `degraded_single_file`
/// is threaded through unchanged: it reflects *how* `file` was resolved, which
/// the caller knows and the classifier does not.
pub fn classify(
    file: &ResolvedFile,
    root: &SyntaxNode,
    byte: usize,
    degraded_single_file: bool,
) -> Option<DefinitionUnavailable> {
    let reason = match smallest_resolution_at(file, byte) {
        // Navigable: go-to-definition can answer (source-IO caveats aside).
        Some(
            Resolution::Local(_)
            | Resolution::Item(_)
            | Resolution::Entity(_)
            | Resolution::Member { .. },
        ) => return None,
        Some(Resolution::Deferred(DeferredReason::UnboundName)) => UnavailableReason::UnboundName,
        Some(Resolution::Deferred(DeferredReason::QualifiedAccess)) => {
            UnavailableReason::QualifiedAccess
        }
        Some(Resolution::Deferred(DeferredReason::ShadowableType)) => {
            UnavailableReason::ShadowableType
        }
        Some(Resolution::Unresolved) => UnavailableReason::Unresolved,
        // No recorded occurrence: only an *identifier* under the cursor is worth
        // explaining (a coverage gap). Whitespace / keywords / punctuation get
        // nothing.
        None => {
            if identifier_token_range(root, byte).is_some() {
                UnavailableReason::UntrackedName
            } else {
                return None;
            }
        }
    };
    Some(DefinitionUnavailable {
        reason,
        degraded_single_file,
    })
}

/// Where to anchor the explanation tooltip: the resolved-occurrence range if the
/// cursor sits on one, else the identifier token's range. `Some` whenever
/// [`classify`] returns `Some` (a resolution carries a range; an
/// [`UnavailableReason::UntrackedName`] came from an identifier token).
pub fn explanation_range(file: &ResolvedFile, root: &SyntaxNode, byte: usize) -> Option<TextRange> {
    if let Some((range, _)) = smallest_resolution_with_range(file, byte) {
        return Some(range);
    }
    identifier_token_range(root, byte)
}

/// The [`TextRange`] of an `IDENT_TOK` touching `byte`, if any. Containment is
/// inclusive at both ends (matching [`smallest_resolution_at`]'s rule), so a
/// cursor at a token boundary still finds an adjacent identifier.
fn identifier_token_range(root: &SyntaxNode, byte: usize) -> Option<TextRange> {
    let offset = TextSize::try_from(byte).ok()?;
    if offset > root.text_range().end() {
        return None;
    }
    root.token_at_offset(offset)
        .find(|token| token.kind() == SyntaxKind::IDENT_TOK)
        .map(|token| token.text_range())
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_cst::syntax::{AstNode, ImplFile};
    use borzoi_sema::{AssemblyEnv, ProjectItems, resolve_file};
    use proptest::prelude::*;

    /// Parse + single-file-resolve `src`, returning the resolved file and its
    /// syntax root (owned, so it outlives the parse).
    fn resolve(src: &str) -> (ResolvedFile, SyntaxNode) {
        let parse = borzoi_cst::parser::parse(src);
        let file = ImplFile::cast(parse.root).expect("source parses as an impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
        (resolved, file.syntax().clone())
    }

    /// Every reason the classifier reports across all byte offsets of `src`
    /// (single-file / non-degraded), in offset order.
    fn reasons_in(src: &str) -> Vec<UnavailableReason> {
        let (file, root) = resolve(src);
        (0..=src.len())
            .filter_map(|byte| classify(&file, &root, byte, false).map(|u| u.reason))
            .collect()
    }

    // A spread of small F# sources exercising the taxonomy + offset edges. The
    // properties below hold for *every* one at *every* offset, so this doubles
    // as a fuzz corpus without a bespoke F# generator.
    const SNIPPETS: &[&str] = &[
        "let x = 1\nlet y = x\n",
        "module M\nlet f a = a + b\n",
        "module M\nlet g = System.Console.WriteLine\n",
        "let value = 42\nlet other = value\n",
        "module M\nlet rec loop n = loop (n - 1)\n",
        "namespace N\nmodule M =\n    let z = unknownName\n",
        "module M\ntype T = { A : int }\nlet t = { A = 1 }\n",
        "let _ = nowhere\n",
        "module M\nopen System\nlet c = Console\n",
        "let add a b = a + b\nlet r = add 1 2\n",
    ];

    // ---- taxonomy mapping -------------------------------------------------

    proptest! {
        /// The classifier's output is exactly determined by the resolution at
        /// the cursor: navigable → no explanation; each `Deferred` reason → its
        /// twin; no occurrence → `UntrackedName` iff an identifier is there.
        /// Swept over all offsets (incl. past-the-end and mid-token), this also
        /// pins offset-robustness (no panics) and the boundary rule.
        #[test]
        fn classify_tracks_the_resolution_taxonomy(
            snippet in prop::sample::select(SNIPPETS),
            pick in any::<prop::sample::Index>(),
        ) {
            let (file, root) = resolve(snippet);
            let byte = pick.index(snippet.len() + 1);
            let got = classify(&file, &root, byte, false).map(|u| u.reason);
            let expected = match smallest_resolution_at(&file, byte) {
                Some(
                    Resolution::Local(_)
                    | Resolution::Item(_)
                    | Resolution::Entity(_)
                    | Resolution::Member { .. },
                ) => None,
                Some(Resolution::Deferred(DeferredReason::UnboundName)) => {
                    Some(UnavailableReason::UnboundName)
                }
                Some(Resolution::Deferred(DeferredReason::QualifiedAccess)) => {
                    Some(UnavailableReason::QualifiedAccess)
                }
                Some(Resolution::Deferred(DeferredReason::ShadowableType)) => {
                    Some(UnavailableReason::ShadowableType)
                }
                Some(Resolution::Unresolved) => Some(UnavailableReason::Unresolved),
                None => identifier_token_range(&root, byte)
                    .map(|_| UnavailableReason::UntrackedName),
            };
            prop_assert_eq!(got, expected);
        }

        /// The `degraded_single_file` flag is pure context: flipping it changes
        /// only that field, never the reason or whether an explanation is given.
        #[test]
        fn degraded_flag_is_orthogonal_to_the_reason(
            snippet in prop::sample::select(SNIPPETS),
            pick in any::<prop::sample::Index>(),
        ) {
            let (file, root) = resolve(snippet);
            let byte = pick.index(snippet.len() + 1);
            let plain = classify(&file, &root, byte, false);
            let degraded = classify(&file, &root, byte, true);
            prop_assert_eq!(plain.map(|u| u.reason), degraded.map(|u| u.reason));
            prop_assert_eq!(plain.is_some(), degraded.is_some());
            prop_assert_eq!(plain.map(|u| u.degraded_single_file), plain.map(|_| false));
            prop_assert_eq!(degraded.map(|u| u.degraded_single_file), degraded.map(|_| true));
        }

        /// Agreement with the single-file go-to-definition handler: a position it
        /// can locate ([`ResolvedFile::resolved_def`] is `Some`) is never
        /// explained-away, and a `Deferred`/`Unresolved` position is always
        /// explained and never located. (`Entity`/`Member` can't arise under the
        /// default, assembly-less env, so the two outcomes partition cleanly.)
        #[test]
        fn agrees_with_single_file_definition(
            snippet in prop::sample::select(SNIPPETS),
            pick in any::<prop::sample::Index>(),
        ) {
            let (file, root) = resolve(snippet);
            let byte = pick.index(snippet.len() + 1);
            if let Some(res) = smallest_resolution_at(&file, byte) {
                let locatable = file.resolved_def(res).is_some();
                let explained = classify(&file, &root, byte, false).is_some();
                if locatable {
                    prop_assert!(!explained, "a locatable definition must not be explained-away");
                }
                if matches!(res, Resolution::Deferred(_) | Resolution::Unresolved) {
                    prop_assert!(explained && !locatable);
                }
            }
        }
    }

    // ---- reachability of each surfaced reason -----------------------------

    #[test]
    fn unbound_name_is_reached_and_labelled() {
        // `b` in `a + b` is bound by nothing → Deferred(UnboundName).
        assert!(
            reasons_in("module M\nlet f a = a + b\n").contains(&UnavailableReason::UnboundName)
        );
    }

    #[test]
    fn qualified_access_is_reached_and_labelled() {
        // With no assemblies loaded, the tail of `System.Console.WriteLine`
        // defers as QualifiedAccess.
        assert!(
            reasons_in("module M\nlet g = System.Console.WriteLine\n")
                .contains(&UnavailableReason::QualifiedAccess)
        );
    }

    #[test]
    fn untracked_name_is_reached_and_labelled() {
        // A record *field label* is an identifier the resolver records no
        // name-use for, so hovering it is a coverage gap, not a resolvable name.
        assert!(
            reasons_in("module M\ntype T = { A : int }\nlet t = { A = 1 }\n")
                .contains(&UnavailableReason::UntrackedName)
        );
    }

    #[test]
    fn navigable_names_are_never_explained() {
        // `x`'s binder and its use both navigate, so no offset of this file is
        // labelled with the navigable-name reasons we don't emit — every
        // explanation, if any, is for the trailing newline region (none).
        let reasons = reasons_in("let x = 1\nlet y = x\n");
        // The binder `x`, the use `x`, and `y` all resolve; the only names here
        // are those, so nothing is explained.
        assert!(
            !reasons.contains(&UnavailableReason::UnboundName),
            "resolvable names must not be reported unbound: {reasons:?}"
        );
    }

    // ---- rendering --------------------------------------------------------

    #[test]
    fn explain_has_header_reason_and_no_degraded_note_when_in_project() {
        let u = DefinitionUnavailable {
            reason: UnavailableReason::UnboundName,
            degraded_single_file: false,
        };
        let body = u.explain();
        assert!(body.starts_with("**No definition available**"), "{body}");
        assert!(body.contains("AutoOpen"), "{body}");
        assert!(!body.contains("without project context"), "{body}");
    }

    #[test]
    fn explain_adds_the_degraded_note_in_single_file_mode() {
        let u = DefinitionUnavailable {
            reason: UnavailableReason::UnboundName,
            degraded_single_file: true,
        };
        assert!(u.explain().contains("without project context"));
    }

    #[test]
    fn every_reason_renders_a_distinct_nonempty_sentence() {
        let reasons = [
            UnavailableReason::UnboundName,
            UnavailableReason::QualifiedAccess,
            UnavailableReason::ShadowableType,
            UnavailableReason::Unresolved,
            UnavailableReason::UntrackedName,
        ];
        let sentences: Vec<&str> = reasons.iter().map(|r| r.sentence()).collect();
        for (i, a) in sentences.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &sentences[i + 1..] {
                assert_ne!(a, b, "reason sentences must be distinct");
            }
        }
    }

    #[test]
    fn whitespace_and_keywords_are_not_explained() {
        // Column 3 of `let x = 1` is the space after `let` — no identifier
        // touches it, so there is nothing to explain.
        let (file, root) = resolve("let x = 1\n");
        assert!(classify(&file, &root, 3, true).is_none());
    }

    #[test]
    fn past_end_offset_does_not_panic() {
        let (file, root) = resolve("let x = 1\n");
        // Far past the buffer: no token, no resolution, no explanation.
        assert!(classify(&file, &root, 10_000, false).is_none());
    }
}
