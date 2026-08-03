//! Why the LSP stopped serving project-wide features for a project, in words.
//!
//! When a `.fsproj` evaluates to something we can't trust, borzoi declines to
//! use it: `semantic::build_parses` refuses the Compile-order fold and
//! every handler degrades to single-file resolution; the project graph drops
//! reference edges. All of that is silent from the editor's side — features
//! simply stop working.
//!
//! This module is the single place that decides *both* halves of that: which
//! capabilities are declined, and what to tell the user about each. The
//! deciding consumer and the message read the same [`deferrals`] call, so they
//! cannot drift apart — which they had, entirely: a census over a 401-project
//! sample of real `.fsproj` files found three that declined the fold and *zero*
//! that produced the message, because the message read one cause channel
//! (`compile_condition_uncertainties`) that none of the three populated. See
//! `docs/fsproj-deferral-message-plan.md`.
//!
//! Two shapes carry the rest of the discipline:
//!
//! - A [`Deferral`]'s [`Causes`] distinguish *recorded* from *unrecorded* in
//!   the type, so a flag raised at a site that records nothing renders as an
//!   explicit stated absence rather than a blank explanation — and a caller can
//!   tell the two apart without reading prose.
//! - Every cause renderer is a wildcard-free `match` over the `borzoi-msbuild`
//!   cause vocabulary, so a new variant there is a compile error here rather
//!   than a cause that silently renders as nothing.
//!
//! Everything in this module is pure: the caller does the IO, the dedup and the
//! sending.

use borzoi_msbuild::{
    CompileConditionReason, CompileConditionUncertainty, CompileItemUncertaintyCause,
    CompileItemUncertaintyCauseKind, DefineConstantsUncertaintyCause, DiagnosticKind,
    DiagnosticOrigin, ImplicitImportKind, ImportFailReason, ParsedProject,
    StructuralCompileItemUncertainty,
};
use std::path::Path;

/// How many causes a single capability's explanation renders before summarising
/// the rest. A `window/showMessage` is a one-line toast in most clients, and a
/// project with a broken import chain can accumulate dozens of causes.
///
/// The residual is always *stated* ("…and 7 more") — a silent cap reads as "and
/// that was all of them", which is exactly the kind of quiet incompleteness
/// this module exists to remove. The full list goes to `tracing` at the call
/// site.
const MAX_RENDERED_CAUSES: usize = 3;

/// What the workspace knows about a `.fsproj`. Three-valued at the edge rather
/// than an `Option<&ParsedProject>` with a comment, because "the project did
/// not evaluate" is itself a reportable deferral and must not be confused with
/// "the project evaluated and is fine".
#[derive(Debug, Clone, Copy)]
pub enum ProjectEvaluation<'a> {
    /// The `.fsproj` evaluated; this is what it produced.
    Evaluated(&'a ParsedProject),
    /// The `.fsproj` did not evaluate at all — malformed XML, an unreadable
    /// file, a rejected project path. The buffer's own diagnostics say which;
    /// here we only know that nothing downstream can run.
    Failed,
}

/// A thing the LSP does for a project, which it is not doing for this one.
///
/// Only capabilities with a *live* consumer that declines appear here. Notably
/// `ParsedProject::package_references_uncertain` does not: nothing in
/// `crates/lsp` reads it, so no capability is lost and there is nothing to
/// explain. Announcing it would be a claim we cannot back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredCapability {
    /// The Compile-order fold. Without it there is no cross-file name
    /// resolution: go-to-definition, find-references, hover and
    /// workspace-symbol all fall back to single-file resolution, and nothing
    /// resolves into referenced assemblies.
    ///
    /// Declined by `semantic::build_parses` when the project failed to
    /// evaluate, when the Compile set may diverge from MSBuild
    /// (`items_uncertain`), or when the `#if` symbol set may
    /// (`define_constants_uncertain`) — under the wrong symbols we would fold
    /// the wrong branches and export the wrong bindings.
    ProjectFold,
    /// The inter-project reference edges. Without them, types defined in a
    /// referenced project do not resolve, even when this project's own fold
    /// succeeds.
    ///
    /// Declined by [`crate::workspace`]'s `references_suppressed`, which sets
    /// [`crate::project_graph::ProjectNode::references_uncertain`] and drops
    /// the edge from the runtime `AssemblyEnv`.
    ProjectReferenceEdges,
}

impl DeferredCapability {
    /// What the user has lost, phrased as an effect they can recognise rather
    /// than as the internal flag's name.
    fn consequence(self) -> &'static str {
        match self {
            DeferredCapability::ProjectFold => {
                "falling back to single-file analysis: go-to-definition, find-references and \
                 hover won't see other files in this project or its referenced assemblies"
            }
            DeferredCapability::ProjectReferenceEdges => {
                "ignoring this project's <ProjectReference> edges, so types from referenced \
                 projects won't resolve"
            }
        }
    }
}

/// Why a capability was declined.
///
/// The two arms are the distinction this repo keeps getting wrong when it is
/// left implicit: *the evaluator recorded these causes* versus *the evaluator
/// recorded none*, which is not the same as there being none. Neither renders
/// as a blank explanation, and a caller can tell them apart without inspecting
/// prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Causes {
    /// At least one recorded cause, deduped, first-seen order. Non-empty: the
    /// only constructor collapses the empty case to [`Causes::Unrecorded`].
    Recorded(Vec<String>),
    /// The flag was raised at a site that records no cause. Every
    /// `project_references_uncertain` site in `borzoi-msbuild` is one; the
    /// Compile and define axes always record (pinned by
    /// `compile_uncertainty_always_records_a_cause` and
    /// `define_uncertainty_and_its_causes_agree` there).
    Unrecorded,
}

impl Causes {
    fn from_rendered(causes: impl IntoIterator<Item = String>) -> Self {
        let mut deduped: Vec<String> = Vec::new();
        for cause in causes {
            if !deduped.contains(&cause) {
                deduped.push(cause);
            }
        }
        if deduped.is_empty() {
            Causes::Unrecorded
        } else {
            Causes::Recorded(deduped)
        }
    }

    /// The recorded phrases, or an empty slice when there are none. Reading
    /// this as "there was nothing wrong" is the mistake the enum exists to
    /// prevent — match on the variant instead.
    pub fn recorded(&self) -> &[String] {
        match self {
            Causes::Recorded(causes) => causes,
            Causes::Unrecorded => &[],
        }
    }

    /// The "why" clause, capped at [`MAX_RENDERED_CAUSES`] with the residual
    /// stated. A silent cap reads as "and that was all of them".
    fn render(&self) -> String {
        match self {
            Causes::Unrecorded => "why: no specific cause was recorded".to_string(),
            Causes::Recorded(causes) => {
                let shown = causes.len().min(MAX_RENDERED_CAUSES);
                let mut text = format!("why: {}", causes[..shown].join("; "));
                if causes.len() > shown {
                    text.push_str(&format!(" (and {} more)", causes.len() - shown));
                }
                text
            }
        }
    }
}

/// One declined capability together with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferral {
    capability: DeferredCapability,
    causes: Causes,
}

impl Deferral {
    fn new(capability: DeferredCapability, causes: impl IntoIterator<Item = String>) -> Self {
        Self {
            capability,
            causes: Causes::from_rendered(causes),
        }
    }

    pub fn capability(&self) -> DeferredCapability {
        self.capability
    }

    pub fn causes(&self) -> &Causes {
        &self.causes
    }
}

/// Every capability the LSP declines for this project, with its causes. Empty
/// exactly when the project is fully usable.
///
/// This is the predicate the deciding consumers call, so a project that defers
/// is a project that has something to report, by construction rather than by
/// discipline.
pub fn deferrals(eval: ProjectEvaluation<'_>) -> Vec<Deferral> {
    let parsed = match eval {
        ProjectEvaluation::Failed => {
            return vec![Deferral::new(
                DeferredCapability::ProjectFold,
                [
                    "the project file could not be evaluated at all (see its own diagnostics)"
                        .to_string(),
                ],
            )];
        }
        ProjectEvaluation::Evaluated(parsed) => parsed,
    };

    let mut out = Vec::new();

    if parsed.items_uncertain || parsed.define_constants_uncertain {
        // Both axes feed the same decline, so they share one explanation. The
        // condition uncertainties come first: they name the gated Compile item,
        // which is the most actionable thing we can say.
        let causes = parsed
            .compile_condition_uncertainties
            .iter()
            .map(render_condition_uncertainty)
            .chain(
                parsed
                    .compile_item_uncertainties
                    .iter()
                    .map(render_compile_cause),
            )
            .chain(
                parsed
                    .define_constants_uncertainties
                    .iter()
                    .map(render_define_cause),
            );
        out.push(Deferral::new(DeferredCapability::ProjectFold, causes));
    }

    if parsed.project_references_uncertain {
        // This axis has no cause channel of its own in `borzoi-msbuild` — it is
        // raised at a dozen sites, none of which record one. The best we can do
        // is borrow the Compile axis's *structural* causes: an import or SDK we
        // couldn't follow is a genuine reason the reference list can't be
        // trusted (it could have carried `<ProjectReference>` mutations), and
        // one of those sites raises both axes together.
        //
        // Sound but possibly incomplete: if the reference axis was additionally
        // raised by an item-pass site of its own (a `Remove`, an unevaluable
        // Include) while a structural Compile cause also exists, we show the
        // structural one and not the item-pass one. Every phrase shown is true;
        // the list may be short. Giving the axis its own cause channel in
        // `borzoi-msbuild`, as the Compile and define axes have, is what would
        // close that — see `docs/fsproj-deferral-message-plan.md`.
        //
        // With no structural cause at all, `Causes::Unrecorded` states the
        // absence rather than inventing one.
        let causes = parsed
            .compile_item_uncertainties
            .iter()
            .filter(|cause| is_structural(&cause.kind))
            .map(render_compile_cause);
        out.push(Deferral::new(
            DeferredCapability::ProjectReferenceEdges,
            causes,
        ));
    }

    out
}

/// Whether the Compile-order fold is declined — the exact question
/// `semantic::build_parses` asks. Defined in terms of [`deferrals`] so
/// that a project which declines always has a message, and vice versa.
pub fn defers_project_fold(eval: ProjectEvaluation<'_>) -> bool {
    deferrals(eval)
        .iter()
        .any(|d| d.capability == DeferredCapability::ProjectFold)
}

/// The editor message for a project's deferrals, or `None` when there are none.
///
/// Pure: the caller owns the dedup-per-session and the sending.
pub fn deferral_message(project: &Path, deferrals: &[Deferral]) -> Option<String> {
    if deferrals.is_empty() {
        return None;
    }
    let name = project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.display().to_string());

    let body = deferrals
        .iter()
        .map(|deferral| {
            format!(
                "{} — {}.",
                deferral.capability.consequence(),
                deferral.causes.render()
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("{name}: {body}"))
}

/// Whether a Compile cause is a *structural* drop — a construct whose content
/// never entered the walk, and so could equally have carried
/// `<ProjectReference>` items. Used to borrow the Compile axis's causes for the
/// reference axis.
///
/// The diagnostic half delegates to
/// [`borzoi_msbuild::DiagnosticKind::hides_unseen_content`], the evaluator's own
/// definition of the class, rather than restating it — a local `matches!` here
/// silently stops matching the evaluator the moment a variant joins the class.
fn is_structural(kind: &CompileItemUncertaintyCauseKind) -> bool {
    match kind {
        CompileItemUncertaintyCauseKind::Structural(_) => true,
        CompileItemUncertaintyCauseKind::Diagnostic(kind) => kind.hides_unseen_content(),
    }
}

/// Append the origin to a cause phrase. `Buffer` reads naturally without it —
/// the message already names the project — while `Imported` is worth saying,
/// because the user will not find the construct in the file they opened.
fn with_origin(text: String, origin: &DiagnosticOrigin) -> String {
    match origin {
        DiagnosticOrigin::Buffer => text,
        DiagnosticOrigin::Imported => format!("{text}, in an imported file"),
    }
}

fn render_condition_uncertainty(u: &CompileConditionUncertainty) -> String {
    let text = match &u.reason {
        CompileConditionReason::UndefinedProperties(names) => format!(
            "a Compile item gated on `{}`, which reads undefined propert{} {}",
            u.condition,
            if names.len() == 1 { "y" } else { "ies" },
            names
                .iter()
                .map(|n| format!("$({n})"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CompileConditionReason::Unsupported => format!(
            "a Compile item gated on a condition I can't evaluate: `{}`",
            u.condition
        ),
    };
    with_origin(text, &u.origin)
}

fn render_compile_cause(cause: &CompileItemUncertaintyCause) -> String {
    let text = match &cause.kind {
        CompileItemUncertaintyCauseKind::Diagnostic(kind) => render_diagnostic_kind(kind),
        CompileItemUncertaintyCauseKind::Structural(structural) => render_structural(structural),
    };
    with_origin(text, &cause.origin)
}

fn render_define_cause(cause: &DefineConstantsUncertaintyCause) -> String {
    with_origin(
        format!(
            "the `#if` symbols may be wrong: {}",
            render_diagnostic_kind(&cause.kind)
        ),
        &cause.origin,
    )
}

fn render_structural(structural: &StructuralCompileItemUncertainty) -> String {
    match structural {
        StructuralCompileItemUncertainty::ProjectSdkUnsupported { sdk } => format!(
            "the project's SDK `{sdk}` couldn't be resolved, so its default items are unknown"
        ),
        StructuralCompileItemUncertainty::ExplicitSdkUnsupported { sdk } => {
            format!("an `<Import Sdk=\"{sdk}\">` whose SDK couldn't be resolved")
        }
        StructuralCompileItemUncertainty::SdkImportProjectUnresolved { sdk, project } => format!(
            "an `<Import Sdk=\"{sdk}\" Project=\"{project}\">` whose Project path didn't resolve"
        ),
        StructuralCompileItemUncertainty::SdkImportProjectRejected { sdk, project } => format!(
            "an `<Import Sdk=\"{sdk}\" Project=\"{project}\">` naming a path outside the SDK root"
        ),
        StructuralCompileItemUncertainty::ImportProjectUnresolved { project } => {
            format!("an `<Import Project=\"{project}\">` I couldn't resolve to one definite file")
        }
        StructuralCompileItemUncertainty::UnsupportedChoose => {
            "a `<Choose>` block, which I don't descend".to_string()
        }
    }
}

/// Render one evaluator diagnostic as a cause phrase.
///
/// Wildcard-free on purpose: a new [`DiagnosticKind`] in `borzoi-msbuild` must
/// fail to compile here rather than reach a user as an unexplained silence.
fn render_diagnostic_kind(kind: &DiagnosticKind) -> String {
    match kind {
        DiagnosticKind::UnresolvedImport { path } => {
            format!("an `<Import Project=\"{path}\">` that was never followed")
        }
        DiagnosticKind::ImportFailed { path, reason } => format!(
            "an `<Import>` of {} that couldn't be read ({})",
            path.display(),
            render_import_fail(reason)
        ),
        DiagnosticKind::UnsupportedConstruct { element } => {
            format!("a `<{element}>` element I don't evaluate")
        }
        DiagnosticKind::UnsupportedGlob { pattern } => {
            format!("a wildcard include `{pattern}` I don't expand")
        }
        DiagnosticKind::UndefinedProperty { name } => {
            format!("`$({name})` isn't defined anywhere I can see")
        }
        DiagnosticKind::UnsupportedPropertyExpression { expression } => {
            format!("a property expression I can't evaluate: `{expression}`")
        }
        DiagnosticKind::UnresolvedItemReference { reference } => {
            format!("an item reference I can't expand: `{reference}`")
        }
        DiagnosticKind::UnresolvedMetadataReference { reference } => {
            format!("an item-metadata reference I can't expand: `{reference}`")
        }
        DiagnosticKind::UnsupportedCondition { condition } => {
            format!("a condition I can't evaluate: `{condition}`")
        }
        DiagnosticKind::UnsupportedItemOperation { operation } => {
            format!("an item operation I don't apply: `{operation}`")
        }
        DiagnosticKind::SdkNotFound { name } => {
            format!("the SDK `{name}` isn't installed where I can find it")
        }
        DiagnosticKind::SdkVersionNotSatisfied {
            name,
            spec: _,
            available,
        } => format!(
            "no installed `{name}` satisfies the version constraint in scope (found: {})",
            if available.is_empty() {
                "none".to_string()
            } else {
                available
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        DiagnosticKind::SdkResolutionUnsupported { name, reason } => {
            format!("the SDK `{name}` resolves in a way I can't reproduce exactly ({reason})")
        }
        DiagnosticKind::ImplicitImportPresent { path, kind } => format!(
            "a {} at {} that I don't follow",
            match kind {
                ImplicitImportKind::DirectoryBuildProps => "Directory.Build.props",
                ImplicitImportKind::DirectoryBuildTargets => "Directory.Build.targets",
                ImplicitImportKind::DirectoryPackagesProps => "Directory.Packages.props",
            },
            path.display()
        ),
    }
}

/// Wildcard-free for the same reason as [`render_diagnostic_kind`].
fn render_import_fail(reason: &ImportFailReason) -> String {
    match reason {
        ImportFailReason::NotFound => "no such file".to_string(),
        ImportFailReason::DepthLimit { depth } => {
            format!("imports nested more than {depth} deep")
        }
        ImportFailReason::MalformedXml { message } => format!("malformed XML: {message}"),
        ImportFailReason::Io { message } => message.clone(),
    }
}

#[cfg(test)]
mod tests;
