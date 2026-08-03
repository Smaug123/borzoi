//! Why the LSP stopped serving project-wide features for a project, in words.
//!
//! When a `.fsproj` evaluates to something we can't trust, borzoi declines to
//! use it: `semantic::build_parses` refuses the Compile-order fold and
//! every handler degrades to single-file resolution; the project graph drops
//! reference edges. All of that is silent from the editor's side — features
//! simply stop working.
//!
//! This module is the single place that decides *both* halves of that: which
//! capabilities are declined, and what to tell the user about each — for the
//! declines it covers ([`DeferredCapability`] documents the one known coverage
//! limit). The deciding consumers and the message read the same [`deferrals`]
//! call, so they cannot drift apart — which they had, entirely: a census over a 401-project
//! sample of real `.fsproj` files found three that declined the fold and *zero*
//! that produced the message, because the message read one cause channel
//! (`compile_condition_uncertainties`) that none of the three populated. See
//! `docs/fsproj-deferral-message-plan.md`.
//!
//! For that to mean anything the inputs must be wide enough to hold every fact
//! a decline reads: [`ProjectEvaluation`] carries `not_an_inner_build` (which no
//! evaluator flag records, yet which drops reference edges), and
//! [`FoldRefusal`] carries the exits `semantic::build_parses` reaches only after
//! reading the sources. A narrower input is how a decline escapes the explainer.
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
use std::path::{Path, PathBuf};

/// How many causes a single capability's explanation renders before summarising
/// the rest. A `window/showMessage` is a one-line toast in most clients, and a
/// project with a broken import chain can accumulate dozens of causes.
///
/// The residual is always *stated* ("…and 7 more") — a silent cap reads as "and
/// that was all of them", which is exactly the kind of quiet incompleteness
/// this module exists to remove. The full list goes to `tracing` at the call
/// site.
const MAX_RENDERED_CAUSES: usize = 3;

/// What the workspace knows about a `.fsproj` — **everything** the LSP's
/// decline decisions read, so that a decline can always be explained from it.
///
/// Three-valued at the edge rather than an `Option<&ParsedProject>` with a
/// comment, because "the project did not evaluate" is itself a reportable
/// deferral and must not be confused with "the project evaluated and is fine".
///
/// The `Evaluated` arm carries more than the `ParsedProject` because more than
/// the `ParsedProject` decides: [`Self::drops_reference_edges`] also turns on
/// `not_an_inner_build`, which nothing in the evaluation flags. A narrower input
/// here is what lets a capability be declined by a fact the explainer cannot
/// see — build the value from
/// [`crate::workspace::Workspace::project_evaluation`], never by hand.
#[derive(Debug, Clone, Copy)]
pub enum ProjectEvaluation<'a> {
    /// The `.fsproj` evaluated.
    Evaluated {
        /// What the evaluator produced.
        parsed: &'a ParsedProject,
        /// We are serving the **outer dispatch** build of a multi-targeted
        /// project, whose `<ProjectReference>` list is not the real build's
        /// under any TFM. Nothing in `parsed` flags this: a multi-targeted
        /// document never writes the singular `TargetFramework`, so it decides
        /// `'$(TargetFramework)' == ''` perfectly cleanly. See
        /// `workspace::EvaluatedProject::not_an_inner_build`.
        not_an_inner_build: bool,
    },
    /// The `.fsproj` did not evaluate at all — malformed XML, an unreadable
    /// file, a rejected project path. The buffer's own diagnostics say which;
    /// here we only know that nothing downstream can run.
    Failed,
}

impl ProjectEvaluation<'_> {
    /// Whether the inter-project reference edge set is dropped for this project
    /// — the predicate `workspace::references_suppressed` applies (modulo its
    /// walk-purpose gate, which is about *why the caller is walking*, not about
    /// the project).
    ///
    /// Shared rather than restated, because a walk that drops edges on a
    /// condition the message doesn't know about is exactly the silence this
    /// module exists to remove.
    pub fn drops_reference_edges(&self) -> bool {
        match self {
            // A failed evaluation has no reference list to drop; its whole-project
            // deferral covers it.
            ProjectEvaluation::Failed => false,
            ProjectEvaluation::Evaluated {
                parsed,
                not_an_inner_build,
            } => parsed.project_references_uncertain || *not_an_inner_build,
        }
    }
}

/// A thing the LSP does for a project, which it is not doing for this one.
///
/// Only capabilities with a *live* consumer that declines appear here. Notably
/// `ParsedProject::package_references_uncertain` does not: nothing in
/// `crates/lsp` reads it, so no capability is lost and there is nothing to
/// explain. Announcing it would be a claim we cannot back.
///
/// # Known coverage limit: graph-level reference suppression
///
/// [`Self::ProjectReferenceEdges`] is reported from the *entry project's own*
/// evaluation. The compile-closure graph walk suppresses edges per node, on
/// facts this input does not carry:
///
/// - a **later target framework** of a multi-targeted project (the walk
///   evaluates additional/seeded TFMs; a clean first TFM hides an uncertain
///   second one), and
/// - a **transitive** node — if an open project A references B and *B's*
///   reference list is untrustworthy, A's `AssemblyEnv` loses C while A itself
///   is clean.
///
/// Both are under-reporting, never mis-reporting: nothing false is said, but a
/// user can lose a reference edge without being told. Closing them needs the
/// graph's own per-node verdict, which means running
/// [`crate::workspace::Workspace::project_graph`] — a deliberately *off-cache*
/// multi-project walk (it must not pin the project memo) — as an input to
/// reporting. That is a new axis with a real cost, not a fix to this one, and
/// is tracked in `docs/fsproj-deferral-message-plan.md`.
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

/// Which stage of the pipeline decided a decline.
///
/// [`DeferredCapability::ProjectFold`] is reachable from both, and telling them
/// apart is load-bearing for [`reconcile`]: an evaluation-caused decline is
/// re-decidable from the evaluation alone at any moment, while a fold-caused
/// one is only knowable by folding. Carrying the wrong kind forward across an
/// unknown fold leaves a *recovered* project still marked as broken, so
/// reintroducing the same problem is deduped away as "already reported".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineStage {
    /// Decided by the `.fsproj` evaluation — always knowable.
    Evaluation,
    /// Decided by the Compile-order fold — knowable only once it has run.
    Fold,
}

/// One declined capability together with why, and which stage decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferral {
    capability: DeferredCapability,
    stage: DeclineStage,
    causes: Causes,
}

impl Deferral {
    fn new(
        capability: DeferredCapability,
        stage: DeclineStage,
        causes: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            capability,
            stage,
            causes: Causes::from_rendered(causes),
        }
    }

    pub fn capability(&self) -> DeferredCapability {
        self.capability
    }

    pub fn stage(&self) -> DeclineStage {
        self.stage
    }

    pub fn causes(&self) -> &Causes {
        &self.causes
    }
}

/// Why the Compile-order fold refused **after** the `.fsproj` evaluation was
/// accepted — a second decline stage, reached only by reading and parsing the
/// Compile items themselves.
///
/// `semantic::build_parses` returns one of these instead of a bare `None`, so
/// that a refusal it decides cannot be a refusal nobody can explain. The
/// evaluation-caused arm exists so that *every* exit is a value: it carries no
/// detail because [`deferrals`] already has the evaluation and explains it
/// better.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldRefusal {
    /// The `.fsproj` evaluation itself is untrustworthy or absent. Explained by
    /// [`deferrals`] from the evaluation; adds nothing here.
    ProjectEvaluation,
    /// A Compile item could not be read from either the editor buffer or disk.
    /// The fold hard-fails on a hole rather than skipping the file: sema's fold
    /// is order-sensitive, so a silent gap can bind a later reference to the
    /// wrong entity — a *wrong* answer, not merely a missing one.
    UnreadableCompileItem { file: PathBuf },
    /// The CST parser panicked on a Compile item.
    ParserPanic { file: PathBuf },
    /// A Compile item parses to a differently *shaped* tree either side of the
    /// F# 8 strict-indentation boundary, and the project's `LangVersion`
    /// provenance can't say which side the real build uses.
    LanguageVersionShape { file: PathBuf },
    /// A Compile item parsed to a root node that is neither an implementation
    /// nor a signature file — an internal invariant break, like a panic.
    UnexpectedParseRoot { file: PathBuf },
}

impl FoldRefusal {
    /// The cause phrase, or `None` for the arm the evaluation already explains.
    fn cause(&self) -> Option<String> {
        match self {
            FoldRefusal::ProjectEvaluation => None,
            FoldRefusal::UnreadableCompileItem { file } => Some(format!(
                "the Compile item {} can't be read from the editor or from disk",
                file.display()
            )),
            FoldRefusal::ParserPanic { file } => Some(format!(
                "parsing the Compile item {} hit a bug in borzoi's parser",
                file.display()
            )),
            FoldRefusal::LanguageVersionShape { file } => Some(format!(
                "{} parses differently either side of the F# 8 indentation rule, and this \
                 project's LangVersion isn't pinned anywhere I can trust",
                file.display()
            )),
            FoldRefusal::UnexpectedParseRoot { file } => Some(format!(
                "the Compile item {} parsed to something borzoi didn't expect (a bug)",
                file.display()
            )),
        }
    }
}

/// What the Compile-order fold has most recently done for a project.
///
/// Three-valued because "no fold has run since the inputs changed" is not
/// "the fold succeeded". Collapsing them either re-reports a stale refusal
/// against a project that has since been fixed, or silently claims a project
/// folds when nothing has tried — the absent-versus-unread confusion this
/// codebase keeps paying for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldOutcome<'a> {
    /// The most recent fold succeeded.
    Succeeded,
    /// The most recent fold refused, for this reason.
    Refused(&'a FoldRefusal),
    /// No fold has run since this project's inputs last changed. Nothing is
    /// claimed either way — see [`deferrals`], which declines to speak rather
    /// than guess.
    Unknown,
}

/// Every capability the LSP declines for this project, with its causes. Empty
/// exactly when the project is fully usable.
///
/// This is the predicate the deciding consumers call, so a project that defers
/// is a project that has something to report, by construction rather than by
/// discipline.
///
/// `fold` is what the Compile-order fold most recently did. It can only *add* a
/// deferral: the fold never succeeds on an evaluation this function already
/// declines. [`FoldOutcome::Unknown`] adds nothing — see
/// [`fold_verdict_known`] for why the *caller* must also decline to clear a
/// previous message on it.
pub fn deferrals(eval: ProjectEvaluation<'_>, fold: FoldOutcome<'_>) -> Vec<Deferral> {
    let mut out = Vec::new();

    let (parsed, fold_declined_on_evaluation) = match eval {
        ProjectEvaluation::Failed => {
            return vec![Deferral::new(
                DeferredCapability::ProjectFold,
                DeclineStage::Evaluation,
                [
                    "the project file could not be evaluated at all (see its own diagnostics)"
                        .to_string(),
                ],
            )];
        }
        ProjectEvaluation::Evaluated { parsed, .. } => (
            parsed,
            parsed.items_uncertain || parsed.define_constants_uncertain,
        ),
    };

    if fold_declined_on_evaluation {
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
        out.push(Deferral::new(
            DeferredCapability::ProjectFold,
            DeclineStage::Evaluation,
            causes,
        ));
    } else if let Some(cause) = match fold {
        FoldOutcome::Refused(refusal) => refusal.cause(),
        FoldOutcome::Succeeded | FoldOutcome::Unknown => None,
    } {
        // The evaluation was fine but the fold still refused, on a fact only
        // reading the sources could reveal. Same lost capability, so the same
        // deferral — the user does not care which stage declined.
        out.push(Deferral::new(
            DeferredCapability::ProjectFold,
            DeclineStage::Fold,
            [cause],
        ));
    }

    if eval.drops_reference_edges() {
        // This axis has no cause channel of its own in `borzoi-msbuild` — it is
        // raised at a dozen sites, none of which record one, and
        // `not_an_inner_build` is not an evaluator flag at all. The best we can
        // do is borrow the Compile axis's *structural* causes: an import or SDK
        // we couldn't follow is a genuine reason the reference list can't be
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
            .filter(|cause| explains_dropped_references(&cause.kind))
            .map(render_compile_cause);
        out.push(Deferral::new(
            DeferredCapability::ProjectReferenceEdges,
            DeclineStage::Evaluation,
            causes,
        ));
    }

    out
}

/// Whether the Compile-order fold's verdict is knowable from current state.
///
/// `false` exactly when the evaluation is clean and the fold outcome is
/// [`FoldOutcome::Unknown`]: nothing has folded since the inputs changed, so we
/// can say neither that the fold is declined nor that it is fine.
///
/// The asymmetry is deliberate. An evaluation-level decline is knowable without
/// folding, so it settles the question on its own; only the *silence* is
/// uninformative.
pub fn fold_verdict_known(eval: ProjectEvaluation<'_>, fold: FoldOutcome<'_>) -> bool {
    !matches!(fold, FoldOutcome::Unknown) || evaluation_declines_project_fold(eval)
}

/// This refresh's account of a project, with any claim the current state cannot
/// make carried over from `previous` rather than silently dropped.
///
/// Only [`DeferredCapability::ProjectFold`] is ever unknowable — every other
/// capability is decided by the evaluation alone, which is always in hand. So a
/// project that has not folded still *publishes* its known losses (a dropped
/// `<ProjectReference>` edge set is a fact about the evaluation), while its
/// previously-reported fold verdict stands until a fold settles it.
///
/// Handling this per capability rather than per project is what stops two
/// independent facts being conflated: skipping the whole report on an unknown
/// fold hid known reference-edge losses until an unrelated request happened to
/// fold the project, and let an evaluation-level *recovery* go unrecorded, so
/// reintroducing the same problem was deduped away as "already reported".
pub fn reconcile(
    fresh: Vec<Deferral>,
    previous: &[Deferral],
    eval: ProjectEvaluation<'_>,
    fold: FoldOutcome<'_>,
) -> Vec<Deferral> {
    if fold_verdict_known(eval, fold) {
        return fresh;
    }
    debug_assert!(
        !fresh
            .iter()
            .any(|d| d.capability() == DeferredCapability::ProjectFold),
        "an unknowable fold cannot have produced a ProjectFold deferral"
    );
    let mut out = fresh;
    if let Some(prev) = previous.iter().find(|d| {
        // Only a *fold-stage* verdict is unknowable and therefore carried. An
        // evaluation-caused one was just recomputed from the same evaluation
        // that is in hand — its absence now is a recovery, and dropping it is
        // the point.
        d.capability() == DeferredCapability::ProjectFold && d.stage() == DeclineStage::Fold
    }) {
        // Fold deferrals lead, as they do in `deferrals`: the lost capability is
        // the broader one.
        out.insert(0, prev.clone());
    }
    out
}

/// Whether the *evaluation* alone declines the Compile-order fold — the gate
/// `semantic::build_parses` applies before it reads any source. Defined in terms
/// of [`deferrals`] so that a project which declines here always has a message,
/// and vice versa.
///
/// A `false` here is not "the fold will succeed": the fold has later exits of
/// its own, which it reports as a [`FoldRefusal`] and which [`deferrals`] folds
/// into the same capability.
pub fn evaluation_declines_project_fold(eval: ProjectEvaluation<'_>) -> bool {
    deferrals(eval, FoldOutcome::Unknown)
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

/// Whether a Compile cause is *also* evidence about the `<ProjectReference>`
/// list — a construct whose content never entered the walk, and which could
/// therefore have carried reference mutations we never saw. Used to borrow the
/// Compile axis's causes for the reference axis, which records none of its own.
///
/// Both halves delegate to `borzoi-msbuild`'s own definitions
/// ([`borzoi_msbuild::DiagnosticKind::hides_unseen_content`],
/// [`borzoi_msbuild::StructuralCompileItemUncertainty::hides_project_references`])
/// rather than restating them. Restating is not merely fragile here, it is
/// wrong: a local "every structural cause counts" would include
/// `UnsupportedChoose`, which the evaluator deliberately exempts because it
/// scans a `<Choose>`'s branches for reference mutations itself. A Compile-only
/// `<Choose>` alongside an unrelated `<ProjectReference Remove>` would then be
/// named as the reason for a drop it did not cause — a confidently wrong
/// explanation, which is worse than the stated absence it displaced.
fn explains_dropped_references(kind: &CompileItemUncertaintyCauseKind) -> bool {
    match kind {
        CompileItemUncertaintyCauseKind::Structural(structural) => {
            structural.hides_project_references()
        }
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
