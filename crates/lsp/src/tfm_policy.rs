//! Which target framework the LSP serves a project under, and the vocabulary
//! for seeding it (fsproj 3.3c, `docs/completed/fsproj-tfm-selection-plan.md` E1/E2/E7).
//!
//! The policy is **first-declared**, and it is a *decision*, not an effect:
//! [`tfm_choice`] reads a first-pass [`ParsedProject`] and returns which TFM to
//! serve and whether serving it needs a second, seeded evaluation. The caller
//! performs that evaluation itself.
//!
//! That split exists because two surfaces must reach the same answer from
//! different inputs. [`crate::workspace`] evaluates the project **from disk**
//! and caches the result; [`crate::fsproj_diagnostics`] evaluates the open
//! **buffer**, which may be unsaved and so may declare different TFMs than the
//! file on disk. Sharing the parse would be wrong; sharing the decision is
//! exactly right, and having one function makes E5's coherence invariant —
//! parse and env agree on the served TFM — mechanically checkable rather than
//! a convention.

use std::collections::HashMap;

use borzoi_msbuild::{ParsedProject, target_frameworks};

/// The served-TFM decision for one project, as computed from its first
/// (unseeded, or caller-seeded) evaluation.
///
/// Each variant is one numbered case of the policy documented on
/// [`tfm_choice`]. Only [`TfmChoice::Reseed`] asks the caller to evaluate a
/// second time; every other variant is answerable from the pass already in
/// hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TfmChoice {
    /// Case 1 — the caller pinned `TargetFramework` as a build global. `None`
    /// when that global is empty, which is an explicit "no TFM" (the outer
    /// dispatch build), not an absent choice.
    CallerOwned(Option<String>),
    /// Case 2 — the project body wrote a non-empty `<TargetFramework>`. Pass 1
    /// already evaluated under it.
    BodyPinned(String),
    /// Case 3 — first-declared of a multi-targeted project. The caller must
    /// re-evaluate with this value seeded as a read-only global (what an
    /// MSBuild inner build does) for the served answer.
    Reseed(String),
    /// Case 3, declined — `TargetFramework`'s provenance is untrusted, so no
    /// seed can be validated. Serve pass 1 and let each read flip its own
    /// `*_uncertain` flag.
    Untrusted,
    /// Case 4 — the project declares no TFM anywhere.
    NoneDeclared,
}

impl TfmChoice {
    /// The TFM this project is served under, assuming the caller honours a
    /// [`TfmChoice::Reseed`]. This is the value
    /// [`crate::workspace::Workspace::target_framework_for_project`] publishes,
    /// so it is the currency the coherence invariant (E5) is stated in.
    pub(crate) fn served(&self) -> Option<&str> {
        match self {
            TfmChoice::CallerOwned(tfm) => tfm.as_deref(),
            TfmChoice::BodyPinned(tfm) | TfmChoice::Reseed(tfm) => Some(tfm),
            TfmChoice::Untrusted | TfmChoice::NoneDeclared => None,
        }
    }

    /// The TFM to seed a second evaluation with, or `None` when pass 1 already
    /// is the served evaluation. Single-targeted and body-pinned projects
    /// therefore pay no extra parse.
    pub(crate) fn reseed(&self) -> Option<&str> {
        match self {
            TfmChoice::Reseed(tfm) => Some(tfm),
            TfmChoice::CallerOwned(_)
            | TfmChoice::BodyPinned(_)
            | TfmChoice::Untrusted
            | TfmChoice::NoneDeclared => None,
        }
    }
}

/// Pick the target framework to serve `pass1`'s project under (fsproj 3.3c,
/// plan E1/E2). `extras` is the caller's build-global bag, exactly as handed
/// to the evaluation that produced `pass1`.
///
/// Policy: first-declared. Precisely, the chosen TFM is
///
/// 1. the caller-seeded `TargetFramework` global when present (any casing,
///    any value — `None` when it's empty). The caller owns the choice: an
///    empty read-only global is an explicit "no TFM", and re-seeding would
///    both override that input and trip the evaluator's case-insensitive
///    duplicate-key validation, failing the whole evaluation;
/// 2. else the body-written `<TargetFramework>` when non-empty. MSBuild's
///    outer/inner gate is `'$(TargetFrameworks)' != '' and
///    '$(TargetFramework)' == ''`, so a non-empty singular is a single-target
///    build even when the plural is also set. Pass 1 already evaluated under
///    it — no second pass;
/// 3. else the **first** [`target_frameworks`] entry, under which the project
///    is re-evaluated with `TargetFramework` seeded as a read-only global —
///    exactly what an MSBuild inner build does — so `$(TargetFramework)`-gated
///    defines and Compile items become evaluable instead of flipping the
///    `*_uncertain` flags. NOT taken when pass 1's `TargetFramework` is
///    [`tfm_untrusted`] (an unpinned empty singular alongside the plural): the
///    real build may be a *single* build under a value we can't see, so
///    seeding would evaluate the gated defines/items cleanly under a choice
///    [`crate::workspace::Workspace::target_framework_for_project`] declines to
///    serve — pairing first-declared parses with whatever the env's no-TFM
///    fallback selects (the E5 incoherence). Keeping pass 1 lets every read of
///    the unpinned `TargetFramework` flip its own `*_uncertain` flag instead;
/// 4. else [`TfmChoice::NoneDeclared`] (no TFM declared anywhere): keep pass 1.
///
/// The one extra parse in case 3 happens once per multi-targeted project.
pub(crate) fn tfm_choice(pass1: &ParsedProject, extras: &HashMap<String, String>) -> TfmChoice {
    if let Some(value) = lookup_property_ci(extras, "TargetFramework") {
        let trimmed = value.trim();
        return TfmChoice::CallerOwned((!trimmed.is_empty()).then(|| trimmed.to_string()));
    }
    if let Some(body) = body_target_framework(pass1) {
        return TfmChoice::BodyPinned(body);
    }
    if tfm_untrusted(pass1) {
        return TfmChoice::Untrusted;
    }
    match target_frameworks(pass1).first() {
        Some(first) => TfmChoice::Reseed(first.clone()),
        None => TfmChoice::NoneDeclared,
    }
}

/// Whether a document can be served as an inner build at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReseedOutcome {
    /// The seed is read-only to this document, so an evaluation seeded with it
    /// *is* the inner build it names.
    AsSeeded,
    /// The document opts `TargetFramework` out of global protection, so it may
    /// overwrite the seed mid-evaluation and the seeded pass must be discarded
    /// — see [`reseed_outcome`]. Deliberately carries no value: there is no
    /// single TFM such a pass runs under.
    Overridable,
}

/// Whether an evaluation seeded with `TargetFramework` is the inner build it
/// was seeded for (fsproj 3.3c, plan E7).
///
/// A seeded global is read-only to the document — unless the document opts the
/// name out with `<Project TreatAsLocalProperty="…">`, after which a project
/// write beats the seed. **That opt-out is the signal**, and it is a syntactic
/// property of the document
/// ([`ParsedProject::locally_overridable_properties`]), stated the same way
/// however the write is spelled and whether or not it fires.
///
/// The tempting alternative — look for a `TargetFramework` entry in the
/// evaluated property table — is unsound in the `absent-vs-unread` way. An
/// override whose value the evaluator refuses (`@(Tfms)`, `%(Meta)`) is
/// performed by MSBuild but drops the name from our table entirely (probed),
/// so absence proves "no write" and "a write we could not evaluate" alike. The
/// document's own declaration has no such gap.
///
/// **An overridable document cannot be served as an inner build at all**, so
/// the caller discards the seeded pass and keeps the outer one. MSBuild
/// evaluates in document order: a `$(TargetFramework)`-gated `<PropertyGroup>`
/// above the override has already contributed the **seed's** defines and items
/// while the table ends at the override's value. The pass is a mixture of two
/// builds, and any single TFM reported for it would pair one build's defines
/// with the other's assets — precisely the incoherence E5 exists to prevent.
///
/// Recovering a usable answer would mean modelling per-property TFM provenance
/// through the whole evaluation. That is not worth building for a document that
/// overwrites its own inner-build seed: zero projects in the pinned F# corpus or
/// the local NuGet cache opt `TargetFramework` out (they opt out `RepoRoot`,
/// `OutDir`, `WasmNativeWorkload`, `RestoreAdditionalProjectSources`), so this
/// declines nothing anyone has been observed to write.
pub(crate) fn reseed_outcome(pass2: &ParsedProject) -> ReseedOutcome {
    // Lowercased in the evaluator, to match MSBuild's OrdinalIgnoreCase
    // property-name comparison.
    if pass2
        .locally_overridable_properties
        .contains("targetframework")
    {
        return ReseedOutcome::Overridable;
    }
    // An entry we could not resolve to a name: MSBuild expands `$(…)` in the
    // attribute and honours the result (probed, dotnet 10.0.301), while the
    // field records the raw text. Absence of `targetframework` above therefore
    // does not prove the seed is protected — the same absent-vs-unread gap the
    // property table had — so an unreadable entry declines like any other
    // opt-out. Over-conservative when it expands to a different name, and only
    // ever in the declining direction.
    if pass2
        .locally_overridable_properties
        .iter()
        .any(|name| name.contains("$("))
    {
        return ReseedOutcome::Overridable;
    }
    ReseedOutcome::AsSeeded
}

/// The project's own non-empty `<TargetFramework>` write, trimmed.
///
/// Distinct from [`target_frameworks`], which folds the plural in: this is
/// specifically "did the body pin a single TFM", MSBuild's inner-build signal.
pub(crate) fn body_target_framework(parsed: &ParsedProject) -> Option<String> {
    lookup_property_ci(&parsed.properties, "TargetFramework")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// Whether the project's `TargetFramework` is too weakly established to seed a
/// second evaluation with, or to publish as a node's known TFM: its write sits
/// behind a gate the evaluator could not pin, or its value still holds a
/// `$(...)` we did not expand.
///
/// Deliberately the singular only: an outer-gated PLURAL (the arcade idiom,
/// `<TargetFrameworks Condition="'$(TargetFramework)' == ''">`) is unpinned by
/// construction — its gate reads the then-undefined `TargetFramework` — yet the
/// TFM-invariant intersection consumes the declared list without trusting any
/// single branch, so distrusting it here would break the idiom for nothing. The
/// body singular, by contrast, is consumed as an authoritative
/// `NodeTfm::Known`.
pub(crate) fn tfm_untrusted(parsed: &ParsedProject) -> bool {
    parsed.property_provenance_untrusted("TargetFramework")
        || body_target_framework(parsed)
            .as_deref()
            .is_some_and(|v| v.contains("$("))
}

/// Whether the caller's extra build properties pin a **non-empty**
/// `TargetFramework` global — the caller then owns the TFM choice, and its
/// value needs no provenance (globals out-rank body writes). An EMPTY
/// caller-supplied `TargetFramework` is the outer (dispatch) build, not a
/// TFM choice — the SDK's inner-build gate is exactly
/// `'$(TargetFramework)' == ''` — so it does not count as ownership and
/// falls through to the normal declared-TFM classification (in
/// `resolve_node_uncached`, reading it as ownership would classify a
/// multi-targeted node `NoneDeclared` and let the output locator fold a
/// lone stale variant). Shared between `resolve_node_uncached` and
/// [`crate::workspace::Workspace::target_framework_for_project`] so the
/// graph-node and entry-side provenance gates cannot drift.
pub(crate) fn caller_owns_target_framework(
    extra_build_properties: &HashMap<String, String>,
) -> bool {
    extra_build_properties
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("TargetFramework") && !v.trim().is_empty())
}

/// Seed `TargetFramework` as a build global for an inner-build (per-TFM)
/// evaluation, **replacing** any case-insensitively equal existing key.
/// MSBuild global-property names compare OrdinalIgnoreCase and the
/// evaluator's input validation rejects case-insensitive duplicates, so a
/// caller-supplied differently-cased key (e.g. an explicitly empty
/// `targetframework`, which deliberately falls through to per-TFM
/// evaluation) must be displaced, not joined — a duplicate fails the whole
/// branch evaluation and even TFM-invariant edges would vanish.
pub(crate) fn seed_target_framework_global(map: &mut HashMap<String, String>, tfm: &str) {
    map.retain(|k, _| !k.eq_ignore_ascii_case("TargetFramework"));
    map.insert("TargetFramework".to_string(), tfm.to_string());
}

/// Case-insensitive property lookup (MSBuild property names compare
/// OrdinalIgnoreCase; both build-global bags and [`ParsedProject::properties`]
/// preserve the source spelling).
pub(crate) fn lookup_property_ci<'a>(
    map: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}
