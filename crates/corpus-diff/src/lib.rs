//! Empirical corpus-diff harness for project-aware name resolution.
//!
//! This crate is deliberately unpublished. It is an integration-test shell around
//! the runtime crates: load projects the way the LSP does, ask FCS for symbol uses,
//! and compare the two without letting skipped or erroring projects look like
//! proof.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use borzoi_spawn::{BoundedCommand, ChildFailure};

use borzoi::handlers::smallest_resolution_with_range;
use borzoi::project_assets::{resolve_assemblies_for_tfm, resolve_assemblies_root_only};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::{ProjectParses, SemanticState};
use borzoi::workspace::Workspace;
use borzoi_msbuild::{
    CompileConditionReason, CompileItemUncertaintyCause, CompileItemUncertaintyCauseKind,
    Diagnostic, DiagnosticKind, DiagnosticOrigin, ImportFailReason, ParsedProject, SdkVersion,
    StructuralCompileItemUncertainty, VersionSpec,
};
use borzoi_sema::{AssemblyEnv, Def, EntityHandle, OpenOpacity, Resolution, ResolvedProject};
use lsp_types::Url;
use rowan::TextRange;
use serde::{Deserialize, Serialize};

/// A project loaded through the same semantic path the LSP uses for handlers.
#[derive(Debug, Clone)]
pub struct LoadedProject {
    pub project: PathBuf,
    pub parses: ProjectParses,
    pub resolved: Arc<ResolvedProject>,
    pub assembly_env: Arc<AssemblyEnv>,
    pub project_assets: ProjectAssetsStatus,
    pub fcs_extra_refs: Vec<PathBuf>,
    pub define_constants: Vec<String>,
    pub lang_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectAssetsStatus {
    NotChecked,
    ProjectDirectoryUnavailable,
    DotnetRootUnavailable {
        path: PathBuf,
    },
    Missing {
        path: PathBuf,
    },
    Resolved {
        path: PathBuf,
        package_dlls: usize,
        framework_dlls: usize,
        project_refs: usize,
    },
    ResolutionFailed {
        path: PathBuf,
        message: String,
    },
}

impl ProjectAssetsStatus {
    fn kind(&self) -> ProjectAssetsStatusKind {
        match self {
            Self::NotChecked => ProjectAssetsStatusKind::NotChecked,
            Self::ProjectDirectoryUnavailable => {
                ProjectAssetsStatusKind::ProjectDirectoryUnavailable
            }
            Self::DotnetRootUnavailable { .. } => ProjectAssetsStatusKind::DotnetRootUnavailable,
            Self::Missing { .. } => ProjectAssetsStatusKind::Missing,
            Self::Resolved { .. } => ProjectAssetsStatusKind::Resolved,
            Self::ResolutionFailed { .. } => ProjectAssetsStatusKind::ResolutionFailed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAssetsStatusKind {
    NotChecked,
    ProjectDirectoryUnavailable,
    DotnetRootUnavailable,
    Missing,
    Resolved,
    ResolutionFailed,
}

impl ProjectAssetsStatusKind {
    /// Every variant, for callers that must emit the whole enumeration rather
    /// than only the values they observed — see [`render_generator_summary`].
    /// Keep in step with the `json_key` match below, which is exhaustive and
    /// so fails to compile when a variant is added.
    pub const ALL: [Self; 6] = [
        Self::NotChecked,
        Self::ProjectDirectoryUnavailable,
        Self::DotnetRootUnavailable,
        Self::Missing,
        Self::Resolved,
        Self::ResolutionFailed,
    ];

    fn json_key(self) -> &'static str {
        match self {
            Self::NotChecked => "not_checked",
            Self::ProjectDirectoryUnavailable => "project_directory_unavailable",
            Self::DotnetRootUnavailable => "dotnet_root_unavailable",
            Self::Missing => "missing",
            Self::Resolved => "resolved",
            Self::ResolutionFailed => "resolution_failed",
        }
    }
}

impl fmt::Display for ProjectAssetsStatusKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.json_key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadSkip {
    ProjectEvaluationFailed,
    ItemsUncertain {
        details: LoadUncertaintyDetails,
    },
    DefineConstantsUncertain {
        details: LoadUncertaintyDetails,
    },
    TooManyFiles {
        files: usize,
        max_files: NonZeroUsize,
    },
    SemanticUnavailable,
    /// The LSP declined to cache this project's reference set (a transient C#
    /// sidecar transport failure), so the oracle's references and the env the
    /// fold resolves against would come from two different resolutions.
    ReferenceSetUnstable,
}

impl fmt::Display for LoadSkip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectEvaluationFailed => f.write_str("project evaluation failed"),
            Self::ItemsUncertain { details } => {
                write!(f, "Compile items are uncertain: {details}")
            }
            Self::DefineConstantsUncertain { details } => {
                write!(f, "DefineConstants are uncertain: {details}")
            }
            Self::TooManyFiles { files, max_files } => {
                write!(f, "too many Compile items ({files} > {max_files})")
            }
            Self::SemanticUnavailable => f.write_str("LSP semantic project load returned None"),
            Self::ReferenceSetUnstable => f.write_str(
                "the reference set was not cacheable (transient C# sidecar failure), so the \
                 oracle and the fold would see different assemblies",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadUncertaintyDetails {
    causes: Vec<String>,
    compile_conditions: Vec<String>,
    diagnostics: Vec<String>,
    omitted_details: usize,
}

impl fmt::Display for LoadUncertaintyDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if !self.causes.is_empty() {
            parts.push(format!("causes: {}", self.causes.join("; ")));
        }
        if !self.compile_conditions.is_empty() {
            parts.push(format!(
                "compile conditions: {}",
                self.compile_conditions.join("; ")
            ));
        }
        if !self.diagnostics.is_empty() {
            parts.push(format!(
                "MSBuild diagnostics: {}",
                self.diagnostics.join("; ")
            ));
        }
        if self.omitted_details > 0 {
            parts.push(format!(
                "{} further detail(s) omitted",
                self.omitted_details
            ));
        }
        if parts.is_empty() {
            f.write_str("no detailed MSBuild uncertainty was captured")
        } else {
            f.write_str(&parts.join(" | "))
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadLimits {
    pub max_files: Option<NonZeroUsize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadOptions {
    pub limits: LoadLimits,
    pub build_properties: HashMap<String, String>,
}

/// Load `project` exactly through [`Workspace`] + [`SemanticState`].
pub fn load_lsp_project(project: &Path) -> Result<LoadedProject, LoadSkip> {
    load_lsp_project_with_limits(project, LoadLimits::default())
}

/// Load `project` through the LSP semantic path, refusing projects outside
/// caller-supplied corpus-runner limits before parsing or resolving sources.
pub fn load_lsp_project_with_limits(
    project: &Path,
    limits: LoadLimits,
) -> Result<LoadedProject, LoadSkip> {
    load_lsp_project_with_options(
        project,
        &LoadOptions {
            limits,
            build_properties: HashMap::new(),
        },
    )
}

/// Load `project` through the LSP semantic path under explicit corpus-runner
/// options.
pub fn load_lsp_project_with_options(
    project: &Path,
    options: &LoadOptions,
) -> Result<LoadedProject, LoadSkip> {
    let mut workspace = if options.build_properties.is_empty() {
        Workspace::new()
    } else {
        Workspace::with_env_and_extra_build_properties(
            SdkDiscoveryEnv::from_process_env(),
            options.build_properties.clone(),
        )
    };
    let mut semantic = SemanticState::new();
    let docs: HashMap<Url, String> = HashMap::new();

    let parsed = workspace
        .project(project)
        .cloned()
        .ok_or(LoadSkip::ProjectEvaluationFailed)?;
    if parsed.items_uncertain {
        return Err(LoadSkip::ItemsUncertain {
            details: items_uncertainty_details(&parsed),
        });
    }
    if parsed.define_constants_uncertain {
        return Err(LoadSkip::DefineConstantsUncertain {
            details: define_constants_uncertainty_details(&parsed),
        });
    }
    if let Some(max_files) = options.limits.max_files
        && parsed.items.len() > max_files.get()
    {
        return Err(LoadSkip::TooManyFiles {
            files: parsed.items.len(),
            max_files,
        });
    }

    let define_constants = parsed.define_constants.clone();
    let lang_version = parsed.lang_version.clone();
    let (fcs_extra_refs, project_assets) = fcs_extra_refs(project, &mut workspace, &mut semantic)?;
    let parses = semantic
        .parses_for_project(project, &mut workspace, &docs)
        .cloned()
        .ok_or(LoadSkip::SemanticUnavailable)?;
    let resolved = semantic
        .resolved_project_for(project, &mut workspace, &docs)
        .ok_or(LoadSkip::SemanticUnavailable)?;
    let dotnet_root = workspace.dotnet_root_for_project(project);
    let target_framework = workspace.served_tfm_for_project(project);
    let assembly_env = semantic.assembly_env_for_project(
        project,
        dotnet_root.as_deref(),
        &target_framework,
        &workspace,
    );

    Ok(LoadedProject {
        project: project.to_path_buf(),
        parses,
        resolved,
        assembly_env,
        project_assets,
        fcs_extra_refs,
        define_constants,
        lang_version,
    })
}

/// The reference set to hand the oracle, and what the project's assets file
/// said.
///
/// The refs are [`SemanticState::env_reference_dlls_for_project`] — not merely
/// the same *composition* the `AssemblyEnv` this run compares against is built
/// from, but the very list *that* env was built from, out of the same cache
/// entry: the assets file's package and
/// framework DLLs, each F# `<ProjectReference>`'s built output DLL, and the C#
/// sidecar's metadata DLLs. Composing a second set here from the assets file
/// alone is what kept every project with a `<ProjectReference>` out of the
/// corpus: our side resolved the referenced project's types (the env has its
/// output DLL) while the oracle, handed only packages and frameworks, answered
/// FS0039 on every use of them — 8473 errors across 113 files on
/// `WoofWare.PawPrint`'s main library, which the runner could only report as a
/// count of erroring files.
///
/// Nothing is filtered out. FSharp.Core and the framework DLLs go to FCS
/// alongside its own SDK's, which it tolerates (the diff currency is
/// `(assembly simple name, full name)`, so a duplicate under one simple name
/// cannot change a verdict), and dropping either would recreate the asymmetry
/// this exists to remove.
///
/// A project whose reference set the LSP declined to *cache* — a transient C#
/// sidecar transport failure — is refused ([`LoadSkip::ReferenceSetUnstable`]):
/// nothing then guarantees the fold below resolves against the env these refs
/// built, and a comparison across two reference sets is not evidence either way.
///
/// [`ProjectAssetsStatus`] describes the **assets file**, so its counts cover
/// only the assets-derived part of the set; a reference the composition adds (an
/// F# project ref) or omits (an unbuilt one) is not visible in them. It reads
/// that file the way the env fold does — by the served TFM where one is known,
/// which is the only way a multi-target restore resolves at all — so a status of
/// `ResolutionFailed` still means the refs above are empty for the same reason.
fn fcs_extra_refs(
    project: &Path,
    workspace: &mut Workspace,
    semantic: &mut SemanticState,
) -> Result<(Vec<PathBuf>, ProjectAssetsStatus), LoadSkip> {
    let Some(dir) = project.parent() else {
        return Ok((Vec::new(), ProjectAssetsStatus::ProjectDirectoryUnavailable));
    };
    let assets = dir.join("obj").join("project.assets.json");
    if !assets.is_file() {
        return Ok((Vec::new(), ProjectAssetsStatus::Missing { path: assets }));
    }
    let Some(dotnet_root) = workspace.dotnet_root_for_project(project) else {
        return Ok((
            Vec::new(),
            ProjectAssetsStatus::DotnetRootUnavailable { path: assets },
        ));
    };
    let target_framework = workspace.served_tfm_for_project(project);
    let (refs, retryable) = semantic.env_reference_dlls_for_project(
        project,
        Some(dotnet_root.as_path()),
        &target_framework,
        workspace,
    );
    if retryable {
        return Err(LoadSkip::ReferenceSetUnstable);
    }
    let assets_resolve = match target_framework.as_deref() {
        Some(tfm) => resolve_assemblies_for_tfm(&assets, &dotnet_root, tfm),
        None => resolve_assemblies_root_only(&assets, &dotnet_root),
    };
    let status = match assets_resolve {
        Ok(resolved) => ProjectAssetsStatus::Resolved {
            path: assets,
            package_dlls: resolved.package_dlls.len(),
            framework_dlls: resolved.framework_dlls.len(),
            project_refs: resolved.project_ref_tfms.len(),
        },
        Err(err) => ProjectAssetsStatus::ResolutionFailed {
            path: assets,
            message: err.to_string(),
        },
    };
    Ok((refs, status))
}

fn items_uncertainty_details(parsed: &ParsedProject) -> LoadUncertaintyDetails {
    let causes = parsed
        .compile_item_uncertainties
        .iter()
        .take(3)
        .map(render_compile_item_uncertainty_cause)
        .collect::<Vec<_>>();
    let compile_conditions = parsed
        .compile_condition_uncertainties
        .iter()
        .take(3)
        .map(render_compile_condition_uncertainty)
        .collect();
    let diagnostics_source: Vec<&Diagnostic> = if causes.is_empty() {
        let relevant: Vec<_> = parsed
            .diagnostics
            .iter()
            .filter(|diag| item_uncertainty_diagnostic(&diag.kind))
            .collect();
        if relevant.is_empty() {
            parsed.diagnostics.iter().collect()
        } else {
            relevant
        }
    } else {
        Vec::new()
    };
    let diagnostics = diagnostics_source
        .iter()
        .take(3)
        .map(|diag| render_msbuild_diagnostic(diag))
        .collect();
    let omitted_details = parsed.compile_item_uncertainties.len().saturating_sub(3)
        + diagnostics_source.len().saturating_sub(3)
        + parsed
            .compile_condition_uncertainties
            .len()
            .saturating_sub(3);
    LoadUncertaintyDetails {
        causes,
        compile_conditions,
        diagnostics,
        omitted_details,
    }
}

fn define_constants_uncertainty_details(parsed: &ParsedProject) -> LoadUncertaintyDetails {
    let diagnostics = parsed
        .diagnostics
        .iter()
        .filter(|diag| define_constants_uncertainty_diagnostic(&diag.kind))
        .take(3)
        .map(render_msbuild_diagnostic)
        .collect();
    let omitted_diagnostics = parsed
        .diagnostics
        .iter()
        .filter(|diag| define_constants_uncertainty_diagnostic(&diag.kind))
        .count()
        .saturating_sub(3);
    LoadUncertaintyDetails {
        causes: Vec::new(),
        compile_conditions: Vec::new(),
        diagnostics,
        omitted_details: omitted_diagnostics,
    }
}

fn item_uncertainty_diagnostic(kind: &DiagnosticKind) -> bool {
    matches!(
        kind,
        DiagnosticKind::UnresolvedImport { .. }
            | DiagnosticKind::ImportFailed { .. }
            | DiagnosticKind::UnsupportedGlob { .. }
            | DiagnosticKind::UnresolvedItemReference { .. }
            | DiagnosticKind::UnresolvedMetadataReference { .. }
            | DiagnosticKind::UnsupportedItemOperation { .. }
            | DiagnosticKind::SdkNotFound { .. }
            | DiagnosticKind::SdkVersionNotSatisfied { .. }
            | DiagnosticKind::SdkResolutionUnsupported { .. }
            | DiagnosticKind::ImplicitImportPresent { .. }
    )
}

fn define_constants_uncertainty_diagnostic(kind: &DiagnosticKind) -> bool {
    matches!(
        kind,
        DiagnosticKind::UndefinedProperty { .. }
            | DiagnosticKind::UnsupportedPropertyExpression { .. }
            | DiagnosticKind::UnresolvedItemReference { .. }
            | DiagnosticKind::UnresolvedMetadataReference { .. }
            | DiagnosticKind::UnsupportedCondition { .. }
    )
}

fn render_compile_condition_uncertainty(
    uncertainty: &borzoi_msbuild::CompileConditionUncertainty,
) -> String {
    let reason = match &uncertainty.reason {
        CompileConditionReason::UndefinedProperties(names) => {
            format!(
                "unresolved propert{} {}",
                if names.len() == 1 { "y" } else { "ies" },
                names.join(", ")
            )
        }
        CompileConditionReason::Unsupported => "unmodeled condition syntax".to_string(),
    };
    format!(
        "{} Condition=\"{}\" ({reason})",
        origin_label(&uncertainty.origin),
        uncertainty.condition,
    )
}

fn render_compile_item_uncertainty_cause(cause: &CompileItemUncertaintyCause) -> String {
    let message = match &cause.kind {
        CompileItemUncertaintyCauseKind::Diagnostic(kind) => msbuild_diagnostic_message(kind),
        CompileItemUncertaintyCauseKind::Structural(kind) => {
            structural_compile_item_uncertainty_message(kind)
        }
    };
    format!("{} {message}", origin_label(&cause.origin))
}

fn structural_compile_item_uncertainty_message(kind: &StructuralCompileItemUncertainty) -> String {
    match kind {
        StructuralCompileItemUncertainty::ProjectSdkUnsupported { sdk } => {
            format!("project SDK '{sdk}' was not evaluated and may hide default Compile items")
        }
        StructuralCompileItemUncertainty::ExplicitSdkUnsupported { sdk } => {
            format!("explicit SDK import '{sdk}' was not evaluated and may hide Compile items")
        }
        StructuralCompileItemUncertainty::SdkImportProjectUnresolved { sdk, project } => {
            format!(
                "dropped SDK import '{sdk}' Project=\"{project}\" because the Project path could not be resolved"
            )
        }
        StructuralCompileItemUncertainty::SdkImportProjectRejected { sdk, project } => {
            format!(
                "rejected SDK import '{sdk}' Project=\"{project}\" because it is not a safe SDK-relative path"
            )
        }
        StructuralCompileItemUncertainty::ImportProjectUnresolved { project } => {
            format!(
                "dropped <Import Project=\"{project}\"> because the Project path could not be resolved"
            )
        }
        StructuralCompileItemUncertainty::UnsupportedChoose => {
            "unsupported <Choose> may hide Compile items".to_string()
        }
    }
}

fn render_msbuild_diagnostic(diagnostic: &Diagnostic) -> String {
    format!(
        "{} {}",
        origin_label(&diagnostic.origin),
        msbuild_diagnostic_message(&diagnostic.kind)
    )
}

fn origin_label(origin: &DiagnosticOrigin) -> &'static str {
    match origin {
        DiagnosticOrigin::Buffer => "project",
        DiagnosticOrigin::Imported => "import",
    }
}

fn msbuild_diagnostic_message(kind: &DiagnosticKind) -> String {
    match kind {
        DiagnosticKind::UnresolvedImport { path } => {
            format!("unresolved <Import Project=\"{path}\">")
        }
        DiagnosticKind::ImportFailed { path, reason } => {
            format!(
                "failed to follow import {}: {}",
                path.display(),
                import_fail_message(reason),
            )
        }
        DiagnosticKind::UnsupportedConstruct { element } => {
            format!("unsupported MSBuild construct: <{element}>")
        }
        DiagnosticKind::UnsupportedGlob { pattern } => {
            format!("glob pattern not expanded: {pattern}")
        }
        DiagnosticKind::UndefinedProperty { name } => {
            format!("$({name}) is not defined")
        }
        DiagnosticKind::UnsupportedPropertyExpression { expression } => {
            format!("$(...) expression not understood: {expression}")
        }
        DiagnosticKind::UnresolvedItemReference { reference } => {
            format!("item reference not expanded: {reference}")
        }
        DiagnosticKind::UnresolvedMetadataReference { reference } => {
            format!("metadata reference not expanded: {reference}")
        }
        DiagnosticKind::UnsupportedCondition { condition } => {
            format!("Condition=\"{condition}\" uses unsupported syntax")
        }
        DiagnosticKind::UnsupportedItemOperation { operation } => {
            format!("item operation not supported: {operation}")
        }
        DiagnosticKind::SdkNotFound { name } => {
            format!("SDK '{name}' not found")
        }
        DiagnosticKind::SdkVersionNotSatisfied {
            name,
            spec,
            available,
        } => {
            format!(
                "SDK '{name}' has no version satisfying {} (available: {})",
                describe_sdk_spec(spec),
                describe_sdk_versions(available),
            )
        }
        DiagnosticKind::SdkResolutionUnsupported { name, reason } => {
            format!("SDK '{name}' resolution declined: {reason}")
        }
        DiagnosticKind::ImplicitImportPresent { path, kind } => {
            format!("implicit import discovered: {kind:?} at {}", path.display())
        }
    }
}

fn describe_sdk_spec(spec: &VersionSpec) -> String {
    match spec.version() {
        Some(version) => format!(
            "{version} (rollForward={:?}, allowPrerelease={})",
            spec.roll_forward(),
            spec.allow_prerelease()
        ),
        None => format!("any version (allowPrerelease={})", spec.allow_prerelease()),
    }
}

fn describe_sdk_versions(versions: &[SdkVersion]) -> String {
    if versions.is_empty() {
        return "none".to_string();
    }
    versions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn import_fail_message(reason: &ImportFailReason) -> String {
    match reason {
        ImportFailReason::NotFound => "file does not exist".to_string(),
        ImportFailReason::DepthLimit { depth } => {
            format!("import depth limit hit (depth={depth})")
        }
        ImportFailReason::MalformedXml { message } => format!("malformed XML: {message}"),
        ImportFailReason::Io { message } => format!("I/O error: {message}"),
    }
}

/// Budget for one whole-project `uses-project` type-check. Generous: it bounds
/// "this will never finish", and must not be mistaken for a performance target —
/// see [`invoke_fcs_uses_project`].
const PROJECT_TIMEOUT: Duration = Duration::from_secs(3600);

/// Budget for the `dotnet build` of `tools/fcs-dump`. A cold build restores
/// packages and compiles FCS, which is legitimately minutes: the bound is there
/// to stop a *stalled* build (blocked on a NuGet lock held by a concurrent run in
/// a sibling worktree, say) from hanging the run forever, not to police a slow
/// one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Invoke `tools/fcs-dump uses-project` for an already-loaded project.
pub fn invoke_fcs_uses_project(loaded: &LoadedProject) -> Result<String, FcsInvokeError> {
    let mut cmd = fcs_dump_command("uses-project")?;
    if !loaded.fcs_extra_refs.is_empty() {
        cmd.env(
            "BORZOI_FCS_EXTRA_REFS",
            loaded
                .fcs_extra_refs
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(";"),
        );
    }
    if !loaded.define_constants.is_empty() {
        cmd.env("BORZOI_FCS_DEFINES", loaded.define_constants.join(";"));
    }
    if let Some(lang) = loaded.lang_version.as_deref() {
        cmd.env("BORZOI_FCS_LANGVERSION", lang);
    }
    // The references we hand FCS are the **whole** set — the same
    // `SemanticState::reference_dlls_for_project` composition our own
    // `AssemblyEnv` is built from — so the oracle must not also resolve the
    // *running SDK's* framework. It is not the project's: measured on
    // `WoofWare.Incremental` (net5.0), FCS bound `System.IO.File` in the SDK's
    // `System.Runtime` while the project's own references declare it in
    // `System.IO.FileSystem`, and a resolution both sides had right was
    // recorded as a divergence in each direction. A project whose composed set
    // is *incomplete* now fails to type-check and is skipped, loudly, rather
    // than compared against a framework we never read.
    // …but only when there *is* one. A project with no assets file composes no
    // references at all, and "resolve against exactly nothing" is not a
    // reference set — FCS aborts. Our env has no assemblies there either, so
    // every imported name is a deferral rather than a comparison, and letting
    // the oracle keep its own SDK references costs the differential nothing.
    if !loaded.fcs_extra_refs.is_empty() {
        cmd.env("BORZOI_FCS_EXCLUSIVE_REFS", "1");
    }
    // The Compile order goes in on stdin; `BoundedCommand` streams it from its
    // own thread while draining both output pipes, so a project large enough to
    // fill a pipe buffer can't deadlock the round-trip (writing stdin
    // synchronously with the output pipes undrained, as this used to, is exactly
    // that bug — fine at a thousand paths, a hang at a hundred thousand), and a
    // wedged FCS is killed rather than waited on forever.
    //
    // This one invocation type-checks *every* Compile item in the project, so it
    // gets a project-scale budget rather than the driver default (which is sized
    // for a single snippet). Too tight a bound here would be worse than none: a
    // healthy but large project would be killed and recorded as skipped, quietly
    // shrinking the corpus the diff claims to cover.
    let out = BoundedCommand::new(cmd)
        .stdin_lines(loaded.parses.paths.iter().map(|p| p.display().to_string()))
        .timeout(PROJECT_TIMEOUT)
        .run()
        .map_err(FcsInvokeError::Harness)?;
    if !out.status.success() {
        return Err(FcsInvokeError::Failed {
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    String::from_utf8(out.stdout).map_err(FcsInvokeError::Utf8)
}

#[derive(Debug)]
pub enum FcsInvokeError {
    BuildFailed {
        status: std::process::ExitStatus,
        stdout: String,
        stderr: String,
    },
    /// The child could not be spawned, or outlived its deadline without
    /// answering (it was killed and reaped), or stopped reading its input — the
    /// harness itself breaking, as opposed to the oracle answering
    /// unsuccessfully.
    Harness(ChildFailure),
    Failed {
        status: std::process::ExitStatus,
        stdout: String,
        stderr: String,
    },
    Utf8(std::string::FromUtf8Error),
}

impl fmt::Display for FcsInvokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuildFailed { status, stderr, .. } => {
                write!(
                    f,
                    "dotnet build tools/fcs-dump failed with {status}: {stderr}"
                )
            }
            Self::Harness(e) => write!(f, "fcs-dump did not answer: {e}"),
            Self::Failed { status, stderr, .. } => {
                write!(f, "fcs-dump uses-project failed with {status}: {stderr}")
            }
            Self::Utf8(e) => write!(f, "fcs-dump stdout was not UTF-8: {e}"),
        }
    }
}

impl std::error::Error for FcsInvokeError {}

fn fcs_dump_command(subcommand: &str) -> Result<Command, FcsInvokeError> {
    if let Some(bin) = std::env::var_os("BORZOI_FCS_DUMP") {
        let mut c = Command::new(bin);
        c.arg(subcommand);
        return Ok(c);
    }

    let project = workspace_root().join("tools").join("fcs-dump");
    let mut build = Command::new("dotnet");
    build
        .args(["build", "-c", "Release", "--nologo"])
        .arg(&project);
    let out = BoundedCommand::new(build)
        .timeout(BUILD_TIMEOUT)
        .run()
        .map_err(FcsInvokeError::Harness)?;
    if !out.status.success() {
        return Err(FcsInvokeError::BuildFailed {
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let mut c = Command::new("dotnet");
    c.arg(project.join("bin/Release/net10.0/fcs-dump.dll"))
        .arg(subcommand);
    Ok(c)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

#[derive(Debug, Clone)]
pub struct FileUses {
    pub path: PathBuf,
    pub diagnostics: Vec<FcsDiagnostic>,
    pub uses: Vec<ProjectUse>,
}

impl FileUses {
    pub fn has_error_diagnostics(&self) -> bool {
        self.error_diagnostics().next().is_some()
    }

    /// The file's error-severity diagnostics — the single definition of what
    /// counts as an oracle error, so the predicate above and the skip reason's
    /// quotes cannot disagree about which project is comparable.
    pub fn error_diagnostics(&self) -> impl Iterator<Item = &FcsDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity.eq_ignore_ascii_case("Error"))
    }
}

#[derive(Debug, Clone)]
pub struct ProjectUse {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub is_from_definition: bool,
    pub decl: UseDecl,
    pub assembly: Option<String>,
    pub full_name: Option<String>,
    /// The entity the used symbol is declared in, named **structurally** —
    /// present for a member, field, union case or nested type. See
    /// [`DeclaringEntity`].
    pub declaring: Option<DeclaringEntity>,
    /// The used symbol's own generic-parameter count, when it is an entity.
    pub generic_arity: Option<usize>,
}

/// The declaring entity of a used symbol, as the oracle reports it rather than
/// as it renders it.
///
/// [`ProjectUse::full_name`] is a *rendering*: FCS prints the enclosing type
/// through `NicePrint`, so it arrives decorated with type arguments —
/// `Holder<_>.Value`, `ImmutableArray<(int -> string)>.Empty`,
/// `ImmutableArray<Probe.A,B>.Empty` (one argument, whose type is named
/// ``A,B``). Those arguments carry commas that are not separators and `>`s that
/// do not close the list, so nothing about the enclosing type can be recovered
/// from the string. It is read from here instead.
///
/// A **path of segments**, not a dotted name: a compiled name may itself contain
/// a dot (`[<CompiledName "Clr.Holder">]`), so splitting one would read a single
/// entity as two. Each segment is the entity's *compiled* name — the domain
/// the assembly projection's `Entity::name` is already in — with the generic-parameter
/// count ECMA-335 declares for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaringEntity {
    /// The namespace the outermost segment sits in; empty for the global one.
    pub namespace: Vec<String>,
    /// Compiled name and generic-parameter count per segment, outermost first.
    pub path: Vec<(String, usize)>,
    /// Whether the use is a **constructor**, which names its own type: FCS
    /// reports the type's display name for it, so `Dictionary<_,_>.Enumerator()`
    /// must not compose to `Dictionary.Enumerator.Enumerator`.
    pub is_constructor: bool,
}

/// Where FCS says the used symbol is declared, classified by what the
/// differential can do with it.
///
/// The distinction matters because a decl range outside the project is
/// **normal**, not a load failure: only [`Self::InProject`] can be compared
/// against our own definition site, and the rest are adjudicated by assembly
/// identity (or counted as having no oracle) instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseDecl {
    /// A declaration in one of the project's loaded Compile sources.
    InProject(DeclSite),
    /// FCS reported no declaration range at all, or one of its pseudo-file
    /// sentinels — `startup`, `unknown`, `commandLineArgs`
    /// (`FSharp.Compiler.Text.Range`). `rangeStartup` is the range of the
    /// initial type-check environment, so **every symbol imported from a
    /// referenced assembly** — a BCL namespace, an imported type — declares
    /// "at startup". FCS is saying *no source location*, not naming a file.
    Unlocated,
    /// A real file that is not one of the project's Compile items. An F#
    /// assembly carries its original source ranges in its signature data, so
    /// FSharp.Core's symbols declare at the build machine's paths
    /// (`D:\a\_work\1\s\src\fsharp\src\FSharp.Core\prim-types.fsi`);
    /// a linked file compiled into another project lands here too.
    OutsideProject(PathBuf),
}

/// FCS's pseudo-file names for a range with no source of its own
/// (`FSharp.Compiler.Text.Range`: `unknownFileName` / `startupFileName` /
/// `commandLineArgsFileName`).
const FCS_PSEUDO_FILES: [&str; 3] = ["unknown", "startup", "commandLineArgs"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclSite {
    pub file: PathBuf,
    pub start: usize,
    pub end: usize,
}

/// One Compile file the oracle reported **errors** for, with those errors.
///
/// The diagnostics ride along rather than being counted and dropped: an
/// oracle-side error means the comparison is off for the whole project, so the
/// skip reason it produces is the only account of *why* — see
/// [`fcs_error_skip_reason`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcsErrorFile {
    pub path: PathBuf,
    pub errors: Vec<FcsDiagnostic>,
}

/// How many of an erroring project's diagnostics the skip reason quotes.
/// Bounded because a project missing a reference errors on nearly every line
/// (8473 diagnostics across 113 files, measured on `WoofWare.PawPrint`) and the
/// reason is one line of a report.
const QUOTED_FCS_ERRORS: usize = 3;

/// The longest message body a quote keeps; FS0072's runs past 200 characters of
/// advice that says nothing about *this* project.
const QUOTED_FCS_MESSAGE_CHARS: usize = 110;

/// The skip reason for a project the oracle could not type-check: the counts,
/// then the leading errors with their sites, then the number of errors not
/// quoted.
///
/// One error is quoted per file before any file's second error, so a single
/// pathological file cannot crowd out the diagnostic that names the cause.
pub fn fcs_error_skip_reason(error_files: &[FcsErrorFile]) -> String {
    let total: usize = error_files.iter().map(|f| f.errors.len()).sum();
    let mut quotes = Vec::new();
    // Round-robin over files: rank 0 takes each file's first error, rank 1 its
    // second, and so on.
    let deepest = error_files
        .iter()
        .map(|f| f.errors.len())
        .max()
        .unwrap_or(0);
    'quoting: for rank in 0..deepest {
        for file in error_files {
            if let Some(diag) = file.errors.get(rank) {
                quotes.push(quote_fcs_error(&file.path, diag));
                if quotes.len() == QUOTED_FCS_ERRORS {
                    break 'quoting;
                }
            }
        }
    }
    let mut reason = format!(
        "{} files had FCS error diagnostics ({total} errors): {}",
        error_files.len(),
        quotes.join("; ")
    );
    let omitted = total.saturating_sub(quotes.len());
    if omitted > 0 {
        let _ = write!(reason, " (+{omitted} more)");
    }
    reason
}

fn quote_fcs_error(path: &Path, diag: &FcsDiagnostic) -> String {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let message = diag.message.replace('\n', " ");
    let message = match message.char_indices().nth(QUOTED_FCS_MESSAGE_CHARS) {
        Some((cut, _)) => format!("{}…", &message[..cut]),
        None => message,
    };
    format!(
        "{file}:{}:{} FS{:04} {message}",
        diag.range.start.line, diag.range.start.col, diag.error_number
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcsDiagnostic {
    pub severity: String,
    pub message: String,
    pub error_number: i32,
    pub range: FcsRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FcsRange {
    #[serde(rename = "File")]
    pub file: String,
    #[serde(rename = "Start")]
    pub start: FcsPos,
    #[serde(rename = "End")]
    pub end: FcsPos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct FcsPos {
    #[serde(rename = "Line")]
    pub line: u32,
    #[serde(rename = "Col")]
    pub col: u32,
}

#[derive(Deserialize)]
struct ProjectUsesDump {
    #[serde(rename = "Files")]
    files: Vec<RawFileUses>,
}

#[derive(Deserialize)]
struct RawFileUses {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Diagnostics", default)]
    diagnostics: Vec<RawDiagnostic>,
    #[serde(rename = "Uses")]
    uses: Vec<RawUse>,
}

#[derive(Deserialize)]
struct RawDiagnostic {
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Message")]
    message: String,
    #[serde(rename = "ErrorNumber")]
    error_number: i32,
    #[serde(rename = "Range")]
    range: FcsRange,
}

#[derive(Deserialize)]
struct RawUse {
    #[serde(rename = "SymbolName")]
    symbol_name: String,
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "IsFromDefinition")]
    is_from_definition: bool,
    #[serde(rename = "DeclRange")]
    decl_range: Option<FcsRange>,
    #[serde(rename = "Assembly", default)]
    assembly: Option<String>,
    #[serde(rename = "FullName", default)]
    full_name: Option<String>,
    #[serde(rename = "GenericArity", default)]
    generic_arity: Option<usize>,
    #[serde(rename = "DeclaringPath", default)]
    declaring_path: Option<Vec<RawDeclaringSegment>>,
    #[serde(rename = "DeclaringNamespace", default)]
    declaring_namespace: Option<String>,
    // `Option` although the oracle always emits a boolean: a `null` here would
    // fail the whole dump's parse and skip the project, and "unknown" is worth
    // no more than "not a constructor" — both decline the composition.
    #[serde(rename = "IsConstructor", default)]
    is_constructor: Option<bool>,
}

#[derive(Deserialize)]
struct RawDeclaringSegment {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Arity")]
    arity: usize,
}

/// Parse `fcs-dump uses-project` output using full path identity, not basenames.
pub fn parse_project_uses(
    json: &str,
    sources: &[(PathBuf, Arc<str>)],
) -> Result<Vec<FileUses>, ParseProjectUsesError> {
    let dump: ProjectUsesDump = serde_json::from_str(json).map_err(ParseProjectUsesError::Json)?;
    let by_path: HashMap<PathBuf, (&Path, &str)> = sources
        .iter()
        .map(|(p, src)| (path_key(p), (p.as_path(), src.as_ref())))
        .collect();
    let lookup = |fcs_path: &str| -> Option<(&Path, &str)> {
        by_path.get(&path_key(Path::new(fcs_path))).copied()
    };

    dump.files
        .into_iter()
        .map(|f| {
            let (path, src) = lookup(&f.path)
                .ok_or_else(|| ParseProjectUsesError::UnknownFile(PathBuf::from(&f.path)))?;
            let idx = LineIndex::new(src);
            let diagnostics = f
                .diagnostics
                .into_iter()
                .map(|d| FcsDiagnostic {
                    severity: d.severity,
                    message: d.message,
                    error_number: d.error_number,
                    range: d.range,
                })
                .collect();
            let uses = f
                .uses
                .into_iter()
                .map(|u| {
                    let decl = match u.decl_range {
                        None => UseDecl::Unlocated,
                        Some(d) if FCS_PSEUDO_FILES.contains(&d.file.as_str()) => {
                            UseDecl::Unlocated
                        }
                        Some(d) => match lookup(&d.file) {
                            Some((dpath, dsrc)) => {
                                let didx = LineIndex::new(dsrc);
                                UseDecl::InProject(DeclSite {
                                    file: dpath.to_path_buf(),
                                    start: didx.offset(d.start.line, d.start.col),
                                    end: didx.offset(d.end.line, d.end.col),
                                })
                            }
                            // Not a file we loaded, so its line/col cannot be
                            // turned into an offset — and must not be, since
                            // indexing our own source at them would invent a
                            // declaration site.
                            None => UseDecl::OutsideProject(PathBuf::from(&d.file)),
                        },
                    };
                    Ok(ProjectUse {
                        name: u.symbol_name,
                        start: idx.offset(u.range.start.line, u.range.start.col),
                        end: idx.offset(u.range.end.line, u.range.end.col),
                        is_from_definition: u.is_from_definition,
                        decl,
                        assembly: u.assembly,
                        full_name: u.full_name,
                        generic_arity: u.generic_arity,
                        declaring: match u.declaring_path {
                            Some(path) if !path.is_empty() => Some(DeclaringEntity {
                                // Absent is the **root** namespace, which the
                                // oracle reports as such rather than as the
                                // string `global` — a namespace can be called
                                // that, and the two are different places.
                                namespace: u
                                    .declaring_namespace
                                    .iter()
                                    .flat_map(|namespace| namespace.split('.'))
                                    .map(str::to_string)
                                    .collect(),
                                path: path
                                    .into_iter()
                                    .map(|segment| (segment.name, segment.arity))
                                    .collect(),
                                is_constructor: u.is_constructor.unwrap_or(false),
                            }),
                            Some(_) | None => None,
                        },
                    })
                })
                .collect::<Result<Vec<_>, ParseProjectUsesError>>()?;
            Ok(FileUses {
                path: path.to_path_buf(),
                diagnostics,
                uses,
            })
        })
        .collect()
}

#[derive(Debug)]
pub enum ParseProjectUsesError {
    Json(serde_json::Error),
    UnknownFile(PathBuf),
}

impl fmt::Display for ParseProjectUsesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "invalid fcs-dump uses-project JSON: {e}"),
            Self::UnknownFile(p) => write!(
                f,
                "FCS reported a file outside the loaded project: {}",
                p.display()
            ),
        }
    }
}

impl std::error::Error for ParseProjectUsesError {}

fn path_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn write_json_report_line(path: &Path, summary: &CorpusSummary) -> std::io::Result<()> {
    let line = summary
        .render_json_report_line()
        .map_err(std::io::Error::other)?;
    std::fs::write(path, line)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub files_compared: usize,
    pub uses_reported: usize,
    pub uses_considered: usize,
    pub assembly_uses_considered: usize,
    pub matches: usize,
    pub assembly_matches: usize,
    pub deferrals: usize,
    pub assembly_deferrals: usize,
    pub skipped_uses: SkippedUses,
    /// Our *defining* occurrences at ranges FCS reports nothing about. The
    /// forward direction does not grade FCS's definitions either, and silence
    /// is not a contradiction, so these are counted rather than reported as
    /// reverse divergences.
    pub unoracled_definitions: usize,
    /// Our **or-pattern alias** occurrences at ranges FCS reports nothing
    /// about ([`borzoi_sema::ResolvedFile::is_or_pattern_alias`]). An or-pattern binds one
    /// name once, so `| Ldarg _n | Ldarga _n | …` makes the second `_n` a use
    /// of the first — which FCS reports for an ordinary name but **not** for
    /// one starting with `_`. The occurrence is in binding position, where the
    /// oracle is free to say nothing, so like an unoracled definition it is
    /// counted rather than reported. Kept apart from
    /// [`unoracled_definitions`](Self::unoracled_definitions) so neither
    /// count's meaning shifts under the other.
    pub unoracled_or_pattern_aliases: usize,
    pub divergences: Vec<Divergence>,
    pub assembly_divergences: Vec<AssemblyDivergence>,
    pub reverse_divergences: Vec<ReverseDivergence>,
    pub fcs_error_files: Vec<FcsErrorFile>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedUses {
    pub definitions: usize,
    pub zero_width: usize,
    pub non_project_declarations: usize,
    /// FCS declared the symbol in a real file the project does not compile
    /// ([`UseDecl::OutsideProject`]) *and* gave no assembly identity to
    /// compare instead, so nothing can adjudicate the use.
    pub out_of_project_declarations: usize,
    pub no_oracle_declaration: usize,
}

impl SkippedUses {
    pub fn total(&self) -> usize {
        self.definitions
            + self.zero_width
            + self.non_project_declarations
            + self.out_of_project_declarations
            + self.no_oracle_declaration
    }

    fn add_assign(&mut self, other: &Self) {
        self.definitions += other.definitions;
        self.zero_width += other.zero_width;
        self.non_project_declarations += other.non_project_declarations;
        self.out_of_project_declarations += other.out_of_project_declarations;
        self.no_oracle_declaration += other.no_oracle_declaration;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusSkip {
    pub project: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAssetsObservation {
    pub project: PathBuf,
    pub status: ProjectAssetsStatus,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusSummary {
    pub build_properties: BTreeMap<String, String>,
    pub projects_discovered: usize,
    pub projects_visited: usize,
    pub comparable_projects: usize,
    pub skipped_projects: Vec<CorpusSkip>,
    pub skipped_by_reason: BTreeMap<String, usize>,
    pub project_assets: Vec<ProjectAssetsObservation>,
    pub project_assets_by_status: BTreeMap<ProjectAssetsStatusKind, usize>,
    pub project_discovery_errors: Vec<ProjectDiscoveryError>,
    pub project_discovery_errors_by_operation: BTreeMap<ProjectDiscoveryOperation, usize>,
    pub files_compared: usize,
    pub fcs_uses_reported: usize,
    pub project_uses_considered: usize,
    pub assembly_uses_considered: usize,
    pub project_matches: usize,
    pub assembly_matches: usize,
    pub project_deferrals: usize,
    pub assembly_deferrals: usize,
    pub skipped_uses: SkippedUses,
    /// Aggregate of [`Comparison::unoracled_definitions`].
    pub unoracled_definitions: usize,
    /// Aggregate of [`Comparison::unoracled_or_pattern_aliases`].
    pub unoracled_or_pattern_aliases: usize,
    pub project_divergences: usize,
    pub assembly_divergences: usize,
    pub reverse_divergences: usize,
}

impl CorpusSummary {
    pub fn new(projects_discovered: usize) -> Self {
        Self {
            projects_discovered,
            ..Self::default()
        }
    }

    pub fn new_with_build_properties(
        projects_discovered: usize,
        build_properties: &HashMap<String, String>,
    ) -> Self {
        Self {
            projects_discovered,
            build_properties: build_properties
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            ..Self::default()
        }
    }

    pub fn record_project_visited(&mut self) {
        self.projects_visited += 1;
    }

    pub fn record_skip(&mut self, project: impl Into<PathBuf>, reason: impl Into<String>) {
        let reason = reason.into();
        *self.skipped_by_reason.entry(reason.clone()).or_default() += 1;
        self.skipped_projects.push(CorpusSkip {
            project: project.into(),
            reason,
        });
    }

    pub fn record_project_assets(
        &mut self,
        project: impl Into<PathBuf>,
        status: ProjectAssetsStatus,
    ) {
        *self
            .project_assets_by_status
            .entry(status.kind())
            .or_default() += 1;
        self.project_assets.push(ProjectAssetsObservation {
            project: project.into(),
            status,
        });
    }

    pub fn record_project_discovery_errors(
        &mut self,
        errors: impl IntoIterator<Item = ProjectDiscoveryError>,
    ) {
        for error in errors {
            *self
                .project_discovery_errors_by_operation
                .entry(error.operation)
                .or_default() += 1;
            self.project_discovery_errors.push(error);
        }
    }

    pub fn record_comparison(&mut self, comparison: &Comparison) {
        self.comparable_projects += 1;
        self.files_compared += comparison.files_compared;
        self.fcs_uses_reported += comparison.uses_reported;
        self.project_uses_considered += comparison.uses_considered;
        self.assembly_uses_considered += comparison.assembly_uses_considered;
        self.project_matches += comparison.matches;
        self.assembly_matches += comparison.assembly_matches;
        self.project_deferrals += comparison.deferrals;
        self.assembly_deferrals += comparison.assembly_deferrals;
        self.skipped_uses.add_assign(&comparison.skipped_uses);
        self.project_divergences += comparison.divergences.len();
        self.assembly_divergences += comparison.assembly_divergences.len();
        self.reverse_divergences += comparison.reverse_divergences.len();
        self.unoracled_definitions += comparison.unoracled_definitions;
        self.unoracled_or_pattern_aliases += comparison.unoracled_or_pattern_aliases;
    }

    pub fn total_uses_considered(&self) -> usize {
        self.project_uses_considered + self.assembly_uses_considered
    }

    pub fn total_matches(&self) -> usize {
        self.project_matches + self.assembly_matches
    }

    pub fn total_deferrals(&self) -> usize {
        self.project_deferrals + self.assembly_deferrals
    }

    pub fn total_divergences(&self) -> usize {
        self.divergence_counts().total()
    }

    /// This run's divergences split by comparison, for
    /// [`CorpusRunnerConfig::expect_divergences`].
    pub fn divergence_counts(&self) -> DivergenceCounts {
        DivergenceCounts {
            project: self.project_divergences,
            assembly: self.assembly_divergences,
            reverse: self.reverse_divergences,
        }
    }

    pub fn skipped_projects_basis_points(&self) -> Option<u64> {
        if self.projects_visited == 0 {
            return None;
        }
        Some(ratio_basis_points(
            self.skipped_projects.len(),
            self.projects_visited,
        ))
    }

    pub fn skipped_projects_percent_string(&self) -> String {
        match self.skipped_projects_basis_points() {
            Some(points) => format_basis_points(points),
            None => "n/a".to_string(),
        }
    }

    pub fn coverage_basis_points(&self) -> Option<u64> {
        let considered = self.total_uses_considered();
        if considered == 0 {
            return None;
        }
        Some(ratio_basis_points(self.total_matches(), considered))
    }

    pub fn coverage_percent_string(&self) -> String {
        match self.coverage_basis_points() {
            Some(points) => format_basis_points(points),
            None => "n/a".to_string(),
        }
    }

    pub fn passes_soundness_gate(&self, max_divergences: usize) -> bool {
        self.comparable_projects > 0 && self.total_divergences() <= max_divergences
    }

    pub fn render_text_report(&self) -> String {
        let mut out = String::new();
        writeln!(
            out,
            "project-corpus-diff: {} discovered | {} visited | {} comparable | {} skipped | {} discovery errors",
            self.projects_discovered,
            self.projects_visited,
            self.comparable_projects,
            self.skipped_projects.len(),
            self.project_discovery_errors.len()
        )
        .expect("write String");
        writeln!(
            out,
            "project-corpus-diff skipped project rate: {}%",
            self.skipped_projects_percent_string()
        )
        .expect("write String");
        writeln!(
            out,
            "project-corpus-diff uses: {} FCS uses | {} project compared | {} assembly compared | {}% coverage",
            self.fcs_uses_reported,
            self.project_uses_considered,
            self.assembly_uses_considered,
            self.coverage_percent_string()
        )
        .expect("write String");
        writeln!(
            out,
            "project-corpus-diff matches: {} project | {} assembly | {} total",
            self.project_matches,
            self.assembly_matches,
            self.total_matches()
        )
        .expect("write String");
        writeln!(
            out,
            "project-corpus-diff deferrals: {} project | {} assembly | {} total",
            self.project_deferrals,
            self.assembly_deferrals,
            self.total_deferrals()
        )
        .expect("write String");
        writeln!(
            out,
            "project-corpus-diff divergences: {} project | {} assembly | {} reverse | {} total",
            self.project_divergences,
            self.assembly_divergences,
            self.reverse_divergences,
            self.total_divergences()
        )
        .expect("write String");
        writeln!(
            out,
            "project-corpus-diff skipped uses: {} definitions | {} zero-width | {} non-project declarations | {} out-of-project declarations | {} no-oracle declarations | {} total ({} of our own defining occurrences and {} of our or-pattern aliases unoracled)",
            self.skipped_uses.definitions,
            self.skipped_uses.zero_width,
            self.skipped_uses.non_project_declarations,
            self.skipped_uses.out_of_project_declarations,
            self.skipped_uses.no_oracle_declaration,
            self.skipped_uses.total(),
            self.unoracled_definitions,
            self.unoracled_or_pattern_aliases
        )
        .expect("write String");
        if !self.build_properties.is_empty() {
            let properties = self
                .build_properties
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            writeln!(out, "project-corpus-diff MSBuild properties: {properties}")
                .expect("write String");
        }
        if !self.skipped_by_reason.is_empty() {
            writeln!(out, "project-corpus-diff skipped projects by reason:").expect("write String");
            for (reason, count) in &self.skipped_by_reason {
                writeln!(out, "  {count}: {reason}").expect("write String");
            }
        }
        if !self.project_assets_by_status.is_empty() {
            writeln!(out, "project-corpus-diff project assets by status:").expect("write String");
            for (status, count) in &self.project_assets_by_status {
                writeln!(out, "  {count}: {status}").expect("write String");
            }
        }
        if !self.project_discovery_errors_by_operation.is_empty() {
            writeln!(out, "project-corpus-diff discovery errors by operation:")
                .expect("write String");
            for (operation, count) in &self.project_discovery_errors_by_operation {
                writeln!(out, "  {count}: {operation}").expect("write String");
            }
        }
        out
    }

    pub fn render_json_report_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(&self.json_report())?;
        line.push('\n');
        Ok(line)
    }

    fn json_report(&self) -> CorpusJsonReport<'_> {
        CorpusJsonReport {
            kind: "project_corpus_diff_summary",
            build_properties: &self.build_properties,
            projects: CorpusProjectReport {
                discovered: self.projects_discovered,
                visited: self.projects_visited,
                comparable: self.comparable_projects,
                skipped: self.skipped_projects.len(),
                skipped_basis_points: self.skipped_projects_basis_points(),
                skipped_percent: self.skipped_projects_percent_string(),
                discovery_errors: self.project_discovery_errors.len(),
            },
            uses: CorpusUsesReport {
                fcs_reported: self.fcs_uses_reported,
                project_considered: self.project_uses_considered,
                assembly_considered: self.assembly_uses_considered,
                total_considered: self.total_uses_considered(),
            },
            matches: CorpusProjectAssemblyCount {
                project: self.project_matches,
                assembly: self.assembly_matches,
                total: self.total_matches(),
            },
            deferrals: CorpusProjectAssemblyCount {
                project: self.project_deferrals,
                assembly: self.assembly_deferrals,
                total: self.total_deferrals(),
            },
            divergences: CorpusTieredCount {
                project: self.project_divergences,
                assembly: self.assembly_divergences,
                reverse: self.reverse_divergences,
                total: self.total_divergences(),
            },
            coverage: CorpusCoverageReport {
                basis_points: self.coverage_basis_points(),
                percent: self.coverage_percent_string(),
            },
            project_assets: CorpusProjectAssetsReport {
                observations: &self.project_assets,
                by_status: self
                    .project_assets_by_status
                    .iter()
                    .map(|(status, count)| (status.json_key(), *count))
                    .collect(),
            },
            skipped_uses: &self.skipped_uses,
            unoracled_definitions: self.unoracled_definitions,
            unoracled_or_pattern_aliases: self.unoracled_or_pattern_aliases,
            skipped_projects: &self.skipped_projects,
            skipped_by_reason: &self.skipped_by_reason,
            discovery_errors: &self.project_discovery_errors,
            discovery_errors_by_operation: self
                .project_discovery_errors_by_operation
                .iter()
                .map(|(operation, count)| (operation.json_key(), *count))
                .collect(),
        }
    }
}

fn ratio_basis_points(numerator: usize, denominator: usize) -> u64 {
    debug_assert!(denominator > 0);
    let numerator = numerator as u128;
    let denominator = denominator as u128;
    (((numerator * 10_000) + (denominator / 2)) / denominator) as u64
}

fn format_basis_points(points: u64) -> String {
    format!("{}.{:02}", points / 100, points % 100)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BasisPoints(u16);

impl BasisPoints {
    pub fn new(value: u16) -> Option<Self> {
        if value <= 10_000 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub fn get(self) -> u16 {
        self.0
    }

    fn percent_string(self) -> String {
        format_basis_points(self.0.into())
    }
}

impl fmt::Display for BasisPoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.percent_string())
    }
}

/// The divergence counts of a run, split by the oracle comparison that found
/// them — the currency of [`CorpusRunnerConfig::expect_divergences`].
///
/// Per category and not a single total, because the categories are independent
/// claims: a change that introduces an assembly wrong target while fixing a
/// project one moves the total by zero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DivergenceCounts {
    pub project: usize,
    pub assembly: usize,
    pub reverse: usize,
}

impl DivergenceCounts {
    pub fn total(&self) -> usize {
        self.project + self.assembly + self.reverse
    }
}

impl fmt::Display for DivergenceCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "assembly={},project={},reverse={}",
            self.assembly, self.project, self.reverse
        )
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CorpusRunnerConfig {
    /// The exact per-category divergence counts this corpus is known to
    /// produce, if the caller records them. **Two-sided**: a run that diverges
    /// more fails, and so does a run that diverges less, so a fix cannot land
    /// without bringing the recorded number down with it. A one-sided ceiling
    /// only ever ratchets in the direction nobody has to act on.
    ///
    /// Mutually exclusive with [`Self::max_divergences`]; see
    /// [`CorpusRunnerConfigError::ConflictingDivergenceRatchets`].
    pub expect_divergences: Option<DivergenceCounts>,
    pub max_divergences: usize,
    pub min_comparable_projects: Option<NonZeroUsize>,
    pub max_skipped_projects: Option<usize>,
    pub max_skipped_project_rate: Option<BasisPoints>,
    pub min_coverage: Option<BasisPoints>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusRun {
    pub summary: CorpusSummary,
    pub exhaustive: bool,
    pub divergence_details: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectCorpusRunOptions {
    pub build_properties: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusRunFailure {
    NoProjectsVisited,
    NoComparableProjects,
    ExhaustiveDiscoveryErrors {
        errors: usize,
    },
    MinComparableProjects {
        min: NonZeroUsize,
        comparable: usize,
    },
    MaxSkippedProjects {
        max: usize,
        skipped: usize,
    },
    MaxSkippedProjectRate {
        max: BasisPoints,
        actual_basis_points: u64,
        skipped: usize,
        visited: usize,
    },
    CoverageUnavailable {
        min: BasisPoints,
    },
    MinCoverage {
        min: BasisPoints,
        actual_basis_points: u64,
    },
    SoundnessGate {
        max_divergences: usize,
        divergences: usize,
    },
    DivergenceExpectation {
        expected: DivergenceCounts,
        observed: DivergenceCounts,
    },
}

impl fmt::Display for CorpusRunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProjectsVisited => write!(
                f,
                "no projects found; set BORZOI_PROJECT_CORPUS or BORZOI_PROJECT_LIST"
            ),
            Self::NoComparableProjects => write!(f, "no comparable projects"),
            Self::ExhaustiveDiscoveryErrors { errors } => {
                write!(
                    f,
                    "exhaustive project discovery had {errors} traversal error(s)"
                )
            }
            Self::MinComparableProjects { min, comparable } => write!(
                f,
                "comparable project ratchet failed ({comparable} < {min})"
            ),
            Self::MaxSkippedProjects { max, skipped } => {
                write!(f, "skipped project ratchet failed ({skipped} > {max})")
            }
            Self::MaxSkippedProjectRate {
                max,
                actual_basis_points,
                skipped,
                visited,
            } => write!(
                f,
                "skipped project rate ratchet failed ({}% > {max}; {skipped}/{visited} projects skipped)",
                format_basis_points(*actual_basis_points)
            ),
            Self::CoverageUnavailable { min } => {
                write!(f, "coverage ratchet failed (no compared uses; need {min})")
            }
            Self::MinCoverage {
                min,
                actual_basis_points,
            } => write!(
                f,
                "coverage ratchet failed ({}% < {min})",
                format_basis_points(*actual_basis_points)
            ),
            Self::SoundnessGate {
                max_divergences,
                divergences,
            } => write!(
                f,
                "project resolution divergences ({divergences} > {max_divergences})"
            ),
            Self::DivergenceExpectation { expected, observed } => {
                write!(
                    f,
                    "divergence expectation failed: expected {expected}, observed {observed}"
                )?;
                let moved = |name: &str, exp: usize, obs: usize| -> String {
                    match obs.cmp(&exp) {
                        Ordering::Greater => format!(" {name} +{}", obs - exp),
                        Ordering::Less => format!(" {name} -{}", exp - obs),
                        Ordering::Equal => String::new(),
                    }
                };
                write!(f, " —")?;
                write!(
                    f,
                    "{}",
                    moved("assembly", expected.assembly, observed.assembly)
                )?;
                write!(
                    f,
                    "{}",
                    moved("project", expected.project, observed.project)
                )?;
                write!(
                    f,
                    "{}",
                    moved("reverse", expected.reverse, observed.reverse)
                )?;
                if observed.total() < expected.total() {
                    write!(
                        f,
                        ". Some of this is a fix: lower BORZOI_PROJECT_EXPECT_DIVERGENCES to \
                         \"{observed}\" so the ratchet holds the new floor"
                    )
                } else {
                    write!(
                        f,
                        ". A raised count is a wrong target the corpus did not have before; \
                         raise the recorded count only with a reason"
                    )
                }
            }
        }
    }
}

impl std::error::Error for CorpusRunFailure {}

pub fn run_project_corpus_diff(projects: ProjectCandidates) -> CorpusRun {
    run_project_corpus_diff_with_options(projects, ProjectCorpusRunOptions::default())
}

pub fn run_project_corpus_diff_with_options(
    projects: ProjectCandidates,
    options: ProjectCorpusRunOptions,
) -> CorpusRun {
    let ProjectCandidates {
        discovered,
        exhaustive,
        max_files,
        visited,
        discovery_errors,
    } = projects;
    let mut summary =
        CorpusSummary::new_with_build_properties(discovered, &options.build_properties);
    summary.record_project_discovery_errors(discovery_errors);
    let mut divergence_details = Vec::new();
    let load_options = LoadOptions {
        limits: LoadLimits { max_files },
        build_properties: options.build_properties,
    };

    for project in visited {
        summary.record_project_visited();
        let loaded = match load_lsp_project_with_options(&project, &load_options) {
            Ok(loaded) => loaded,
            Err(reason) => {
                summary.record_skip(project, reason.to_string());
                continue;
            }
        };
        summary.record_project_assets(loaded.project.clone(), loaded.project_assets.clone());
        let json = match invoke_fcs_uses_project(&loaded) {
            Ok(json) => json,
            Err(err) => {
                summary.record_skip(loaded.project.clone(), err.to_string());
                continue;
            }
        };
        let sources: Vec<_> = loaded
            .parses
            .paths
            .iter()
            .cloned()
            .zip(loaded.parses.texts.iter().cloned())
            .collect();
        let fcs = match parse_project_uses(&json, &sources) {
            Ok(fcs) => fcs,
            Err(err) => {
                summary.record_skip(loaded.project.clone(), err.to_string());
                continue;
            }
        };
        let comparison = compare_project_uses(&loaded, &fcs);
        if !comparison.fcs_error_files.is_empty() {
            summary.record_skip(
                loaded.project.clone(),
                fcs_error_skip_reason(&comparison.fcs_error_files),
            );
            continue;
        }
        summary.record_comparison(&comparison);
        record_divergence_details(&comparison, &mut divergence_details);
    }

    CorpusRun {
        summary,
        exhaustive,
        divergence_details,
    }
}

pub fn check_project_corpus_run(
    run: &CorpusRun,
    config: CorpusRunnerConfig,
) -> Result<(), CorpusRunFailure> {
    if run.summary.projects_visited == 0 {
        return Err(CorpusRunFailure::NoProjectsVisited);
    }
    if run.summary.comparable_projects == 0 {
        return Err(CorpusRunFailure::NoComparableProjects);
    }
    if run.exhaustive && !run.summary.project_discovery_errors.is_empty() {
        return Err(CorpusRunFailure::ExhaustiveDiscoveryErrors {
            errors: run.summary.project_discovery_errors.len(),
        });
    }
    if let Some(min) = config.min_comparable_projects
        && run.summary.comparable_projects < min.get()
    {
        return Err(CorpusRunFailure::MinComparableProjects {
            min,
            comparable: run.summary.comparable_projects,
        });
    }
    if let Some(max) = config.max_skipped_projects {
        let skipped = run.summary.skipped_projects.len();
        if skipped > max {
            return Err(CorpusRunFailure::MaxSkippedProjects { max, skipped });
        }
    }
    if let Some(max) = config.max_skipped_project_rate {
        let actual_basis_points = run
            .summary
            .skipped_projects_basis_points()
            .expect("projects_visited checked above");
        if actual_basis_points > u64::from(max.get()) {
            return Err(CorpusRunFailure::MaxSkippedProjectRate {
                max,
                actual_basis_points,
                skipped: run.summary.skipped_projects.len(),
                visited: run.summary.projects_visited,
            });
        }
    }
    if let Some(min) = config.min_coverage {
        let Some(actual_basis_points) = run.summary.coverage_basis_points() else {
            return Err(CorpusRunFailure::CoverageUnavailable { min });
        };
        if actual_basis_points < u64::from(min.get()) {
            return Err(CorpusRunFailure::MinCoverage {
                min,
                actual_basis_points,
            });
        }
    }
    // A recorded expectation owns this check in both directions, because it can
    // say which category moved and which way; the one-sided ceiling only knows
    // a total. The `comparable_projects` floor `passes_soundness_gate` carries
    // is kept explicitly: a run that measured nothing must not satisfy an
    // expectation of zero by arithmetic.
    if let Some(expected) = config.expect_divergences {
        if run.summary.comparable_projects == 0 {
            return Err(CorpusRunFailure::NoComparableProjects);
        }
        let observed = run.summary.divergence_counts();
        if observed != expected {
            return Err(CorpusRunFailure::DivergenceExpectation { expected, observed });
        }
    } else if !run.summary.passes_soundness_gate(config.max_divergences) {
        return Err(CorpusRunFailure::SoundnessGate {
            max_divergences: config.max_divergences,
            divergences: run.summary.total_divergences(),
        });
    }
    Ok(())
}

pub fn render_project_corpus_run_report(run: &CorpusRun) -> String {
    let mut out = run.summary.render_text_report();
    for detail in &run.divergence_details {
        writeln!(out, "{detail}").expect("write String");
    }
    for skipped in run.summary.skipped_projects.iter().take(40) {
        writeln!(
            out,
            "skipped {}: {}",
            skipped.project.display(),
            skipped.reason
        )
        .expect("write String");
    }
    for error in run.summary.project_discovery_errors.iter().take(40) {
        writeln!(out, "project discovery error: {error}").expect("write String");
    }
    out
}

fn record_divergence_details(comparison: &Comparison, out: &mut Vec<String>) {
    for div in &comparison.divergences {
        out.push(format!(
            "divergence {}:{}..{} {} expected {}:{}..{}, got {}",
            div.file.display(),
            div.range.0,
            div.range.1,
            div.name,
            div.expected.file.display(),
            div.expected.start,
            div.expected.end,
            div.actual
        ));
    }
    for div in &comparison.assembly_divergences {
        out.push(format!(
            "assembly divergence {}:{}..{} {} expected {}:{}, got {}",
            div.file.display(),
            div.range.0,
            div.range.1,
            div.name,
            div.expected.assembly,
            div.expected.full_name,
            div.actual
        ));
    }
    for div in &comparison.reverse_divergences {
        out.push(format!(
            "reverse divergence {}:{}..{} got {} with covering FCS oracles {:?}",
            div.file.display(),
            div.range.0,
            div.range.1,
            div.actual,
            div.covering_oracles,
        ));
    }
}

#[derive(Debug, Serialize)]
struct CorpusJsonReport<'a> {
    kind: &'static str,
    build_properties: &'a BTreeMap<String, String>,
    projects: CorpusProjectReport,
    uses: CorpusUsesReport,
    matches: CorpusProjectAssemblyCount,
    deferrals: CorpusProjectAssemblyCount,
    divergences: CorpusTieredCount,
    coverage: CorpusCoverageReport,
    project_assets: CorpusProjectAssetsReport<'a>,
    skipped_uses: &'a SkippedUses,
    /// Our own defining occurrences the oracle said nothing about — reverse
    /// checks that were skipped rather than graded, so a consumer can see how
    /// much of that direction went unexercised.
    unoracled_definitions: usize,
    /// Our or-pattern aliases the oracle said nothing about — the same
    /// ungraded-reverse-check accounting, kept separate so neither count's
    /// meaning shifts under the other.
    unoracled_or_pattern_aliases: usize,
    skipped_projects: &'a [CorpusSkip],
    skipped_by_reason: &'a BTreeMap<String, usize>,
    discovery_errors: &'a [ProjectDiscoveryError],
    discovery_errors_by_operation: BTreeMap<&'static str, usize>,
}

#[derive(Debug, Serialize)]
struct CorpusProjectReport {
    discovered: usize,
    visited: usize,
    comparable: usize,
    skipped: usize,
    skipped_basis_points: Option<u64>,
    skipped_percent: String,
    discovery_errors: usize,
}

#[derive(Debug, Serialize)]
struct CorpusUsesReport {
    fcs_reported: usize,
    project_considered: usize,
    assembly_considered: usize,
    total_considered: usize,
}

#[derive(Debug, Serialize)]
struct CorpusProjectAssemblyCount {
    project: usize,
    assembly: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
struct CorpusTieredCount {
    project: usize,
    assembly: usize,
    reverse: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
struct CorpusCoverageReport {
    basis_points: Option<u64>,
    percent: String,
}

#[derive(Debug, Serialize)]
struct CorpusProjectAssetsReport<'a> {
    observations: &'a [ProjectAssetsObservation],
    by_status: BTreeMap<&'static str, usize>,
}

/// The continuous-measurements generator contract
/// (`docs/continuous-measurements.md`): the compact, durable half of the run,
/// which `borzoi-stats record` wraps in reproducibility metadata and files on
/// the `stats-data` branch.
///
/// It is a strict subset of [`CorpusJsonReport`], not a rival to it. The full
/// report keeps the worklists — which project skipped, for what reason, which
/// use went unadjudicated — and rides along as a workflow artifact. This one
/// keeps only counts, because `statistics` is a *metric namespace*: the
/// dashboard discovers a plottable metric per nested number, so every key here
/// has to mean the same thing in every run of the series. That rules out
/// arrays (which the recorder rejects outright) and equally rules out
/// open-ended string keys — `skipped_by_reason` is keyed by messages that
/// embed paths and oracle errors, so it would mint a new metric per run and
/// none of them would be comparable. Asset status is a closed enum, so it
/// stays.
#[derive(Debug, Serialize)]
struct GeneratorSummary<'a> {
    schema_version: u32,
    measurement: &'static str,
    configuration: GeneratorConfiguration<'a>,
    statistics: GeneratorStatistics,
}

/// Everything about *how* the run was configured that changes the numbers.
/// This is digested into the series key, so it must not carry anything
/// incidental — absolute checkout paths above all, which differ per CI run and
/// would put every observation in its own series of one. Which projects were
/// measured is the corpus's identity, recorded as the corpus revision.
#[derive(Debug, Serialize)]
struct GeneratorConfiguration<'a> {
    selection: GeneratorSelection,
    build_properties: &'a BTreeMap<String, String>,
}

/// How the run chose its projects — a sum, because the knobs are not shared.
/// [`project_candidates_from_settings`] applies `stride` and `limit` only when
/// walking a directory; an explicit list visits all of it. Recording them for
/// a list run would claim a knob that did nothing, and worse, would split the
/// series if a default ever moved.
#[derive(Debug, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
enum GeneratorSelection {
    None,
    List {
        max_files: Option<usize>,
    },
    Corpus {
        exhaustive: bool,
        stride: usize,
        limit: Option<usize>,
        max_files: Option<usize>,
    },
}

#[derive(Debug, Serialize)]
struct GeneratorStatistics {
    projects: CorpusProjectCounts,
    files_compared: usize,
    uses: CorpusUsesReport,
    matches: CorpusProjectAssemblyCount,
    /// The headline series: a use we resolved to nothing concrete where FCS
    /// resolved to something. Not a wrong answer, so no gate fails on it —
    /// which is exactly why it needs plotting, since a change that makes us
    /// more timid is otherwise invisible.
    deferrals: CorpusProjectAssemblyCount,
    divergences: CorpusTieredCount,
    coverage: CorpusCoverageBasisPoints,
    skipped_uses: CorpusSkippedUsesCounts,
    unoracled_definitions: usize,
    /// The sibling of [`unoracled_definitions`](Self::unoracled_definitions),
    /// plotted beside it for the same reason: both count checks the oracle
    /// declined to grade, so a change that silently moves occurrences from
    /// *graded* to *ungraded* would otherwise show up as an improvement in
    /// coverage rather than as the loss of signal it is.
    unoracled_or_pattern_aliases: usize,
    project_assets_by_status: BTreeMap<&'static str, usize>,
}

#[derive(Debug, Serialize)]
struct CorpusProjectCounts {
    discovered: usize,
    visited: usize,
    comparable: usize,
    skipped: usize,
    /// Never `Option`. See [`defined_ratio`].
    skipped_basis_points: u64,
    discovery_errors: usize,
}

#[derive(Debug, Serialize)]
struct CorpusCoverageBasisPoints {
    /// Never `Option`. See [`defined_ratio`].
    basis_points: u64,
}

/// A ratio for `statistics`, where an undefined one (an empty denominator)
/// must still be a number.
///
/// `null` is exactly as bad as an absent key here: the dashboard plots one
/// metric per nested *number*, so it ignores nulls too, skips the observation,
/// and leaves the previous point reading as "Latest" — a run that measured
/// nothing would masquerade as the last run that measured something. `0` is
/// not ambiguous in context because the denominator is emitted beside every
/// ratio (`uses.total_considered`, `projects.visited`), so "0 of 0" is
/// distinguishable from "0 of many" by anyone reading the series.
fn defined_ratio(ratio: Option<u64>) -> u64 {
    ratio.unwrap_or(0)
}

#[derive(Debug, Serialize)]
struct CorpusSkippedUsesCounts {
    definitions: usize,
    zero_width: usize,
    non_project_declarations: usize,
    out_of_project_declarations: usize,
    no_oracle_declaration: usize,
    total: usize,
}

/// The measurement name this generator files under. A path segment on the
/// `stats-data` branch, so it never changes without starting a fresh history.
pub const PROJECT_CORPUS_MEASUREMENT: &str = "project-corpus-diff";

/// Render the [continuous-measurements generator
/// contract](docs/continuous-measurements.md) for one run.
pub fn render_generator_summary(
    summary: &CorpusSummary,
    settings: &ProjectCandidateSettings,
) -> Result<String, serde_json::Error> {
    let generator = GeneratorSummary {
        schema_version: 1,
        measurement: PROJECT_CORPUS_MEASUREMENT,
        configuration: GeneratorConfiguration {
            selection: match settings.source {
                ProjectCandidateSource::None => GeneratorSelection::None,
                ProjectCandidateSource::List(_) => GeneratorSelection::List {
                    max_files: settings.max_files.map(NonZeroUsize::get),
                },
                ProjectCandidateSource::Corpus(_) => GeneratorSelection::Corpus {
                    exhaustive: settings.exhaustive,
                    stride: settings.stride.get(),
                    limit: settings.limit.map(NonZeroUsize::get),
                    max_files: settings.max_files.map(NonZeroUsize::get),
                },
            },
            build_properties: &summary.build_properties,
        },
        statistics: GeneratorStatistics {
            projects: CorpusProjectCounts {
                discovered: summary.projects_discovered,
                visited: summary.projects_visited,
                comparable: summary.comparable_projects,
                skipped: summary.skipped_projects.len(),
                skipped_basis_points: defined_ratio(summary.skipped_projects_basis_points()),
                discovery_errors: summary.project_discovery_errors.len(),
            },
            files_compared: summary.files_compared,
            uses: CorpusUsesReport {
                fcs_reported: summary.fcs_uses_reported,
                project_considered: summary.project_uses_considered,
                assembly_considered: summary.assembly_uses_considered,
                total_considered: summary.total_uses_considered(),
            },
            matches: CorpusProjectAssemblyCount {
                project: summary.project_matches,
                assembly: summary.assembly_matches,
                total: summary.total_matches(),
            },
            deferrals: CorpusProjectAssemblyCount {
                project: summary.project_deferrals,
                assembly: summary.assembly_deferrals,
                total: summary.total_deferrals(),
            },
            divergences: CorpusTieredCount {
                project: summary.project_divergences,
                assembly: summary.assembly_divergences,
                reverse: summary.reverse_divergences,
                total: summary.total_divergences(),
            },
            coverage: CorpusCoverageBasisPoints {
                basis_points: defined_ratio(summary.coverage_basis_points()),
            },
            skipped_uses: CorpusSkippedUsesCounts {
                definitions: summary.skipped_uses.definitions,
                zero_width: summary.skipped_uses.zero_width,
                non_project_declarations: summary.skipped_uses.non_project_declarations,
                out_of_project_declarations: summary.skipped_uses.out_of_project_declarations,
                no_oracle_declaration: summary.skipped_uses.no_oracle_declaration,
                total: summary.skipped_uses.total(),
            },
            unoracled_definitions: summary.unoracled_definitions,
            unoracled_or_pattern_aliases: summary.unoracled_or_pattern_aliases,
            // Every variant, including the ones that did not occur. A metric
            // that vanishes when its count reaches zero is worse than useless
            // here: the dashboard skips observations whose value is absent, so
            // a status dropping from two to none would leave the *older*,
            // nonzero point showing as the latest — a fixed problem still
            // reading as broken. A closed enumeration emitted sparsely is an
            // open one.
            project_assets_by_status: ProjectAssetsStatusKind::ALL
                .into_iter()
                .map(|status| {
                    let count = summary
                        .project_assets_by_status
                        .get(&status)
                        .copied()
                        .unwrap_or(0);
                    (status.json_key(), count)
                })
                .collect(),
        },
    };
    let mut json = serde_json::to_string_pretty(&generator)?;
    json.push('\n');
    Ok(json)
}

/// Write [`render_generator_summary`] to `path`, replacing what is there.
pub fn write_generator_summary(
    path: &Path,
    summary: &CorpusSummary,
    settings: &ProjectCandidateSettings,
) -> Result<(), GeneratorSummaryError> {
    let json =
        render_generator_summary(summary, settings).map_err(GeneratorSummaryError::Serialise)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(GeneratorSummaryError::Write)?;
    }
    std::fs::write(path, json).map_err(GeneratorSummaryError::Write)
}

#[derive(Debug)]
pub enum GeneratorSummaryError {
    Serialise(serde_json::Error),
    Write(std::io::Error),
}

impl fmt::Display for GeneratorSummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialise(error) => write!(f, "serialise generator summary: {error}"),
            Self::Write(error) => write!(f, "write generator summary: {error}"),
        }
    }
}

impl std::error::Error for GeneratorSummaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialise(error) => Some(error),
            Self::Write(error) => Some(error),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FsprojCollection {
    pub projects: Vec<PathBuf>,
    pub errors: Vec<ProjectDiscoveryError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiscoveryError {
    pub path: PathBuf,
    pub operation: ProjectDiscoveryOperation,
    pub message: String,
}

impl ProjectDiscoveryError {
    fn read_dir(path: &Path, error: std::io::Error) -> Self {
        Self::new(path, ProjectDiscoveryOperation::ReadDir, error)
    }

    fn read_entry(path: &Path, error: std::io::Error) -> Self {
        Self::new(path, ProjectDiscoveryOperation::ReadEntry, error)
    }

    fn file_type(path: &Path, error: std::io::Error) -> Self {
        Self::new(path, ProjectDiscoveryOperation::FileType, error)
    }

    fn new(path: &Path, operation: ProjectDiscoveryOperation, error: std::io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            operation,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for ProjectDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} failed: {}",
            self.operation,
            self.path.display(),
            self.message
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiscoveryOperation {
    ReadDir,
    ReadEntry,
    FileType,
}

impl fmt::Display for ProjectDiscoveryOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadDir => f.write_str("read_dir"),
            Self::ReadEntry => f.write_str("read_dir entry"),
            Self::FileType => f.write_str("file_type"),
        }
    }
}

impl ProjectDiscoveryOperation {
    fn json_key(self) -> &'static str {
        match self {
            Self::ReadDir => "read_dir",
            Self::ReadEntry => "read_entry",
            Self::FileType => "file_type",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssemblyDecl {
    pub assembly: String,
    pub full_name: String,
    /// The same declaration named from the oracle's *structural* facts rather
    /// than its rendering — `None` on our own side, and for an oracle use with
    /// no declaring entity. See [`DeclaringEntity`].
    pub structural: Option<StructuralName>,
}

/// An oracle declaration named by its declaring entity plus the used symbol's
/// own name, with no rendering in it: the path `[(Holder`1, 1)]` in namespace
/// `Probe` and the leaf `Value`, for what FCS renders `Probe.Holder<_>.Value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralName {
    /// The declaring entity, segment by segment. See [`DeclaringEntity`].
    pub declaring: DeclaringEntity,
    /// The used symbol's own display name.
    pub leaf: String,
    /// The used symbol's own generic-parameter count, when it is an entity.
    ///
    /// The declaring path names the *enclosing* entity, so a nested type's own
    /// arity is not in it: `Outer<T>.Inner<U>` and `Outer<T>.Inner<U,V>` both
    /// report the path `Outer` and the leaf `Inner`. Without this a bare
    /// nested-type use could certify the wrong one.
    pub leaf_arity: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub file: PathBuf,
    pub range: (usize, usize),
    pub name: String,
    pub expected: DeclSite,
    pub actual: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssemblyDivergence {
    pub file: PathBuf,
    pub range: (usize, usize),
    pub name: String,
    pub expected: AssemblyDecl,
    pub actual: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseDivergence {
    pub file: PathBuf,
    pub range: (usize, usize),
    pub actual: String,
    pub covering_oracles: Vec<String>,
}

/// Compare FCS project-file declarations against sema's project resolution.
///
/// This is intentionally a soundness comparator, not a completeness gate:
/// `None`/`Deferred` counts as a deferral. A concrete wrong answer is a
/// divergence. The reverse pass is stricter about over-resolution: every
/// concrete sema resolution in a comparable file must be covered by an FCS use
/// that names the same project declaration or assembly symbol. Coverage uses
/// containment rather than exact range equality because sema sometimes records a
/// segment of a long identifier while FCS reports the whole identifier path.
pub fn compare_project_uses(loaded: &LoadedProject, fcs: &[FileUses]) -> Comparison {
    let mut comparison = Comparison::default();
    let index_by_path: HashMap<PathBuf, usize> = loaded
        .parses
        .paths
        .iter()
        .enumerate()
        .map(|(idx, p)| (path_key(p), idx))
        .collect();
    let mut comparable_fcs_files = Vec::new();

    for file_uses in fcs {
        if file_uses.has_error_diagnostics() {
            comparison.fcs_error_files.push(FcsErrorFile {
                path: file_uses.path.clone(),
                errors: file_uses.error_diagnostics().cloned().collect(),
            });
            continue;
        }
        let Some(&file_idx) = index_by_path.get(&path_key(&file_uses.path)) else {
            comparison.divergences.push(Divergence {
                file: file_uses.path.clone(),
                range: (0, 0),
                name: "<file>".to_string(),
                expected: DeclSite {
                    file: file_uses.path.clone(),
                    start: 0,
                    end: 0,
                },
                actual: "FCS file not present in loaded sema project".to_string(),
            });
            continue;
        };
        comparison.files_compared += 1;
        comparison.uses_reported += file_uses.uses.len();
        comparable_fcs_files.push((file_idx, file_uses));
        let rf = loaded.resolved.file(file_idx);
        for u in &file_uses.uses {
            if u.is_from_definition {
                comparison.skipped_uses.definitions += 1;
                continue;
            }
            if u.start == u.end {
                comparison.skipped_uses.zero_width += 1;
                continue;
            }
            let range = TextRange::new(
                u32::try_from(u.start).expect("use start fits u32").into(),
                u32::try_from(u.end).expect("use end fits u32").into(),
            );
            let UseDecl::InProject(expected) = &u.decl else {
                match assembly_decl(u) {
                    Some(expected) => {
                        comparison.assembly_uses_considered += 1;
                        match rf.resolution_at(range) {
                            None | Some(Resolution::Deferred(_)) => {
                                comparison.assembly_deferrals += 1;
                            }
                            Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                                let actual = assembly_resolution_decl(&loaded.assembly_env, res);
                                if canonical_assembly(&actual.assembly)
                                    == canonical_assembly(&expected.assembly)
                                    && assembly_full_name_agrees_for(
                                        &loaded.assembly_env,
                                        res,
                                        &actual.full_name,
                                        &expected,
                                    )
                                {
                                    comparison.assembly_matches += 1;
                                } else {
                                    comparison.assembly_divergences.push(AssemblyDivergence {
                                        file: file_uses.path.clone(),
                                        range: (u.start, u.end),
                                        name: u.name.clone(),
                                        expected,
                                        actual: format!(
                                            "assembly {} full_name {}",
                                            actual.assembly, actual.full_name
                                        ),
                                    });
                                }
                            }
                            Some(other) => {
                                comparison.assembly_divergences.push(AssemblyDivergence {
                                    file: file_uses.path.clone(),
                                    range: (u.start, u.end),
                                    name: u.name.clone(),
                                    expected,
                                    actual: format!("{other:?}"),
                                })
                            }
                        }
                    }
                    // No assembly identity to compare against either. An
                    // out-of-project *file* is its own bucket: it says the
                    // symbol has a real source we simply do not hold (an F#
                    // assembly's embedded ranges, a linked file), which is
                    // worth seeing in the report rather than folding into the
                    // oracle-said-nothing count.
                    None if matches!(u.decl, UseDecl::OutsideProject(_)) => {
                        comparison.skipped_uses.out_of_project_declarations += 1;
                    }
                    None if u.assembly.is_some() || u.full_name.is_some() => {
                        comparison.skipped_uses.non_project_declarations += 1;
                    }
                    None => {
                        comparison.skipped_uses.no_oracle_declaration += 1;
                    }
                }
                continue;
            };
            comparison.uses_considered += 1;
            match rf.resolution_at(range) {
                None | Some(Resolution::Deferred(_)) => comparison.deferrals += 1,
                Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => {
                    match resolution_def(loaded, file_idx, res) {
                        Some((actual_file_idx, def))
                            if path_key(&loaded.parses.paths[actual_file_idx])
                                == path_key(&expected.file)
                                && range_pair(def.range) == (expected.start, expected.end) =>
                        {
                            comparison.matches += 1;
                        }
                        Some((actual_file_idx, def)) => {
                            comparison.divergences.push(Divergence {
                                file: file_uses.path.clone(),
                                range: (u.start, u.end),
                                name: u.name.clone(),
                                expected: expected.clone(),
                                actual: format!(
                                    "binder {:?} at {}:{}..{}",
                                    def.name,
                                    loaded.parses.paths[actual_file_idx].display(),
                                    u32::from(def.range.start()),
                                    u32::from(def.range.end())
                                ),
                            });
                        }
                        None => comparison.divergences.push(Divergence {
                            file: file_uses.path.clone(),
                            range: (u.start, u.end),
                            name: u.name.clone(),
                            expected: expected.clone(),
                            actual: format!("{res:?} (no project def)"),
                        }),
                    }
                }
                Some(other) => comparison.divergences.push(Divergence {
                    file: file_uses.path.clone(),
                    range: (u.start, u.end),
                    name: u.name.clone(),
                    expected: expected.clone(),
                    actual: format!("{other:?}"),
                }),
            }
        }
    }
    add_reverse_divergences(loaded, &comparable_fcs_files, &mut comparison);
    comparison.reverse_divergences.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.range.cmp(&b.range))
            .then(a.actual.cmp(&b.actual))
    });
    comparison
}

fn assembly_decl(use_: &ProjectUse) -> Option<AssemblyDecl> {
    match (&use_.assembly, &use_.full_name) {
        (Some(assembly), Some(full_name)) => Some(AssemblyDecl {
            assembly: assembly.clone(),
            full_name: full_name.clone(),
            structural: use_.declaring.as_ref().map(|declaring| StructuralName {
                declaring: declaring.clone(),
                leaf: use_.name.clone(),
                leaf_arity: use_.generic_arity,
            }),
        }),
        (Some(_), None) | (None, Some(_)) | (None, None) => None,
    }
}

fn assembly_resolution_decl(env: &AssemblyEnv, res: Resolution) -> AssemblyDecl {
    match res {
        Resolution::Entity(handle) => {
            let entity = env.entity(handle);
            AssemblyDecl {
                assembly: entity.assembly.name.clone(),
                full_name: env.entity_full_name(handle),
                structural: None,
            }
        }
        Resolution::Member { parent, idx } => {
            let entity = env.entity(parent);
            AssemblyDecl {
                assembly: entity.assembly.name.clone(),
                full_name: format!(
                    "{}.{}",
                    env.entity_full_name(parent),
                    env.member_display_name(parent, idx)
                ),
                structural: None,
            }
        }
        Resolution::Local(_)
        | Resolution::Item(_)
        | Resolution::Deferred(_)
        | Resolution::Unresolved => unreachable!("only assembly resolutions have assembly decls"),
    }
}

fn add_reverse_divergences(
    loaded: &LoadedProject,
    fcs_files: &[(usize, &FileUses)],
    comparison: &mut Comparison,
) {
    for (file_idx, file_uses) in fcs_files {
        let rf = loaded.resolved.file(*file_idx);
        let mut resolutions: Vec<_> = rf.resolutions().iter().collect();
        resolutions.sort_by_key(|(range, _)| range_pair(**range));
        for (range, &res) in resolutions {
            if !is_concrete_resolution(res) {
                continue;
            }
            let (start, end) = range_pair(*range);
            if file_uses
                .uses
                .iter()
                .any(|u| fcs_use_confirms_resolution(loaded, *file_idx, u, start, end, res))
            {
                continue;
            }
            let covering_oracles: Vec<String> = file_uses
                .uses
                .iter()
                .filter(|u| fcs_use_covers_range(u, start, end))
                .map(fcs_oracle_summary)
                .collect();
            // The oracle did not speak *about this range*, so it cannot be
            // contradicting us. That is routine wherever the occurrence sits in
            // *binding* position, which is the one place FCS is free to say
            // nothing:
            //
            // - a *defining* occurrence — the forward direction does not grade
            //   FCS's definitions either;
            // - an *or-pattern alias* — `| Ldarg _n | Ldarga _n | …` binds one
            //   `_n`, and FCS reports the later alternatives for an ordinary
            //   name but not for one starting with `_`.
            //
            // "Spoke about this range" is an *exact* span match, not the
            // enclosure `covering_oracles` reports: FCS synthesises an `_arg1`
            // over the whole of a non-simple lambda parameter, so in
            // `fun (A _n | B _n) -> _n` every occurrence inside the pattern is
            // enclosed by a use of an unrelated symbol. Treating that as speech
            // would report a correct answer as a divergence.
            //
            // Count each so the silence stays visible.
            let spoke_here = file_uses
                .uses
                .iter()
                .any(|u| u.start == start && u.end == end);
            if !spoke_here {
                if is_defining_occurrence(loaded, *file_idx, res, start, end) {
                    comparison.unoracled_definitions += 1;
                    continue;
                }
                if rf.is_or_pattern_alias(*range) {
                    comparison.unoracled_or_pattern_aliases += 1;
                    continue;
                }
            }
            comparison.reverse_divergences.push(ReverseDivergence {
                file: loaded.parses.paths[*file_idx].clone(),
                range: (start, end),
                actual: resolution_summary(loaded, *file_idx, res),
                covering_oracles,
            });
        }
    }
}

/// Whether the resolution at `start..end` is the binder's **own** declaration
/// site — its definition range is the range itself.
fn is_defining_occurrence(
    loaded: &LoadedProject,
    file_idx: usize,
    res: Resolution,
    start: usize,
    end: usize,
) -> bool {
    matches!(res, Resolution::Local(_) | Resolution::Item(_))
        && resolution_def(loaded, file_idx, res).is_some_and(|(def_file_idx, def)| {
            def_file_idx == file_idx && range_pair(def.range) == (start, end)
        })
}

fn is_concrete_resolution(res: Resolution) -> bool {
    matches!(
        res,
        Resolution::Local(_)
            | Resolution::Item(_)
            | Resolution::Entity(_)
            | Resolution::Member { .. }
    )
}

fn fcs_use_confirms_resolution(
    loaded: &LoadedProject,
    file_idx: usize,
    use_: &ProjectUse,
    start: usize,
    end: usize,
    res: Resolution,
) -> bool {
    if !fcs_use_covers_range(use_, start, end) {
        return false;
    }
    match res {
        Resolution::Local(_) | Resolution::Item(_) => {
            let UseDecl::InProject(expected) = &use_.decl else {
                return false;
            };
            resolution_def(loaded, file_idx, res).is_some_and(|(actual_file_idx, def)| {
                path_key(&loaded.parses.paths[actual_file_idx]) == path_key(&expected.file)
                    && range_pair(def.range) == (expected.start, expected.end)
            })
        }
        Resolution::Entity(_) | Resolution::Member { .. } => {
            assembly_decl(use_).is_some_and(|expected| {
                assembly_resolution_confirms_decl(&loaded.assembly_env, res, &expected)
            })
        }
        Resolution::Deferred(_) | Resolution::Unresolved => false,
    }
}

/// Canonicalise a core-BCL assembly name to its **ref-pack facade**, so a
/// comparison holds regardless of how the `fcs-dump` driving it was deployed.
///
/// A type-forwarded corelib type (`System.String`, `System.Object`, …) is
/// *defined* in `System.Private.CoreLib.dll` and *surfaced* through the
/// `System.Runtime.dll` facade. A framework-dependent `fcs-dump` resolves the
/// SDK ref pack and reports `System.Runtime`; the self-contained publish CI
/// ships resolves the implementation framework and reports
/// `System.Private.CoreLib`. Our side reads the ref-pack assemblies from the
/// project's assets and so always reports the facade. Both name the same
/// logical entity, and the full name still has to agree exactly, so a genuine
/// divergence is still caught rather than absorbed. (The sema harness
/// `resolve_qualifier_precedence_diff` canonicalises identically.)
fn canonical_assembly(name: &str) -> &str {
    match name {
        "System.Private.CoreLib" => "System.Runtime",
        other => other,
    }
}

/// Whether our rendering of an imported symbol's full name agrees with FCS's.
///
/// Compared modulo the **double-backtick quoting** FCS applies to identifiers
/// that need it — ``Microsoft.FSharp.Core.Operators.``not```. Only the
/// delimiter pairs are removed, never a lone backtick: a quoted identifier may
/// contain one (`lex.fsl` closes the quote on a *doubled* backtick only), so
/// ``a`b`` and ``ab`` name different members and must not collapse together.
///
/// Nothing else is normalised, deliberately. FCS reports an F# *module*'s
/// `FullName` as the bare display name (`Seq`), which cannot witness which
/// symbol was bound; rather than accept a leaf-only match here, `fcs-dump`
/// qualifies such a name from the entity's own `AccessPath` before it reaches
/// us, so this comparison stays exact.
fn assembly_full_name_agrees(actual: &str, expected: &str) -> bool {
    let unquote = |s: &str| s.replace("``", "");
    unquote(actual) == unquote(expected)
}

/// [`assembly_full_name_agrees`] against the oracle's rendered name **or** the
/// structural one our own resolution certifies — see [`certified_expected`].
///
/// An extra accepted name, never a substituted one: the rendered name is what
/// most uses agree on already, and the two are not interchangeable. FCS names a
/// constructor use by its *type* (`System.ArgumentOutOfRangeException`) while
/// its declaring entity and display name compose to
/// `System.ArgumentOutOfRangeException.ArgumentOutOfRangeException`, so
/// substituting would turn agreement into a divergence.
fn assembly_full_name_agrees_for(
    env: &AssemblyEnv,
    res: Resolution,
    actual: &str,
    expected: &AssemblyDecl,
) -> bool {
    assembly_full_name_agrees(actual, &expected.full_name)
        || certified_expected(env, res, expected)
            .is_some_and(|certified| assembly_full_name_agrees(actual, &certified))
}

/// The oracle declaration named the way **we** name it, or `None` when our own
/// resolution does not certify it — leaving the oracle's rendered name to be
/// compared exactly as it arrived.
///
/// FCS's full name for a member is a **rendering**: the enclosing type is
/// printed through `NicePrint`, so it arrives decorated —
/// `MethodReturnType<_>.Returns`, `ImmutableArray<(int -> string)>.Empty`,
/// `ImmutableArray<Probe.A,B>.Empty` (one argument, of the type ``A,B``). Our
/// own full names carry no arity at all, so a *correct* resolution scored as a
/// divergence in both directions — 518 of the 530 measured on
/// `WoofWare.PawPrint`'s main library.
///
/// Nothing about that decoration is parsed. The enclosing entity arrives
/// structurally instead ([`DeclaringEntity`]), and is matched against the
/// enclosing chain we resolved by [`chain_position`]; what comes back is *our*
/// name for the entity that certified, so the two sides' spelling domains never
/// have to be reconciled at the comparison.
///
/// A constructor names its own type, so it certifies the entity alone; every
/// other symbol certifies the entity plus its own name.
fn certified_expected(
    env: &AssemblyEnv,
    res: Resolution,
    expected: &AssemblyDecl,
) -> Option<String> {
    let structural = expected.structural.as_ref()?;
    let chain = match res {
        Resolution::Entity(handle) => env.enclosing_chain(handle),
        Resolution::Member { parent, .. } => env.enclosing_chain(parent),
        Resolution::Local(_)
        | Resolution::Item(_)
        | Resolution::Deferred(_)
        | Resolution::Unresolved => return None,
    };
    // The path names the *enclosing* entity, so for a use that is itself an
    // entity the leaf's own arity is the only thing separating
    // `Outer<T>.Inner<U>` from `Outer<T>.Inner<U,V>`.
    if let (Resolution::Entity(handle), Some(arity)) = (res, structural.leaf_arity)
        && !structural.declaring.is_constructor
        && env.entity(handle).generic_parameters.len() != arity
    {
        return None;
    }
    let position = chain_position(env, &chain, &structural.declaring)?;
    let named = env.entity_full_name(chain[position]);
    Some(if structural.declaring.is_constructor {
        named
    } else {
        format!("{named}.{}", structural.leaf)
    })
}

/// How far along `chain` the oracle's declaring path reaches, or `None` when it
/// names something our resolution did not.
///
/// Matched in **one** domain: each segment's compiled name against
/// the assembly projection's `Entity::name`, which is the same name with ECMA-335's
/// arity mangling stripped — so the suffix is dropped here too, and the arity it
/// encoded is compared as the count the oracle reports beside it. Matching
/// either that or our *source* spelling would not be injective: with
/// `[<CompiledName "C">] type A<'T>` beside `[<CompiledName "A">] type B<'T>`,
/// the oracle's `A` for a member of `B` would also match `A`'s source name.
///
/// The arity comparison is what keeps `Holder<'T>` apart from `Holder<'T,'U>`,
/// and a companion module — never generic — out of a generic entity's place.
/// The namespace pins the path to a place rather than to a shape, and is the
/// *root sentinel* rather than the string `global`, since a namespace can be
/// called that.
///
/// **Known limit.** Two entities whose compiled names differ only by an arity
/// suffix that one of them spells explicitly — `[<CompiledName "C">] type A<'T>`
/// beside `[<CompiledName "C`1">] type B<'T>` — are indistinguishable here,
/// because the assembly projection stores `C` for both: `Entity::name` has the
/// suffix stripped and the arity moved to `generic_parameters`, so the
/// distinction is gone before this comparison sees it. Closing it means giving
/// the projection a name that remembers its mangling, not tightening this
/// function; it is filed rather than worked around.
fn chain_position(
    env: &AssemblyEnv,
    chain: &[EntityHandle],
    declaring: &DeclaringEntity,
) -> Option<usize> {
    let position = declaring.path.len().checked_sub(1)?;
    if position >= chain.len() || env.entity(*chain.first()?).namespace != declaring.namespace {
        return None;
    }
    let mut enclosing_parameters = 0usize;
    for (index, (name, arity)) in declaring.path.iter().enumerate() {
        let entity = env.entity(chain[index]);
        if entity.generic_parameters.len() != *arity {
            return None;
        }
        // The projection strips ECMA-335's arity mangling; strip it here only
        // when the suffix *is* this segment's arity delta, so that a compiled
        // name which merely looks mangled — `[<CompiledName "C`1">] type A`
        // beside `[<CompiledName "C">] type B`, both non-generic — is compared
        // whole and cannot certify against the other.
        let delta = arity.checked_sub(enclosing_parameters)?;
        enclosing_parameters = *arity;
        let spelling = match name.rsplit_once('`') {
            Some((head, suffix)) if suffix == delta.to_string() => head,
            Some(_) | None => name.as_str(),
        };
        if spelling != entity.name {
            return None;
        }
    }
    (!env.is_module(chain[position])).then_some(position)
}

fn assembly_resolution_confirms_decl(
    env: &AssemblyEnv,
    res: Resolution,
    expected: &AssemblyDecl,
) -> bool {
    let actual = assembly_resolution_decl(env, res);
    if canonical_assembly(&actual.assembly) != canonical_assembly(&expected.assembly) {
        return false;
    }
    // The oracle's rendered name and the structural one it certifies are both
    // accepted, and neither replaces the other (see
    // [`assembly_full_name_agrees_for`]). The prefix rule — FCS naming a member
    // *below* the entity we resolved — applies to each in turn, since the
    // rendering is exactly where the arity decoration would break it.
    let mut candidates = vec![expected.full_name.clone()];
    candidates.extend(certified_expected(env, res, expected));
    candidates.iter().any(|expected_full| match res {
        Resolution::Entity(_) => {
            assembly_full_name_agrees(&actual.full_name, expected_full)
                || expected_full
                    .strip_prefix(&actual.full_name)
                    .is_some_and(|tail| tail.starts_with('.'))
        }
        Resolution::Member { .. } => assembly_full_name_agrees(&actual.full_name, expected_full),
        Resolution::Local(_)
        | Resolution::Item(_)
        | Resolution::Deferred(_)
        | Resolution::Unresolved => false,
    })
}

fn fcs_use_covers_range(use_: &ProjectUse, start: usize, end: usize) -> bool {
    use_.start != use_.end && use_.start <= start && end <= use_.end
}

fn fcs_oracle_summary(use_: &ProjectUse) -> String {
    match &use_.decl {
        UseDecl::InProject(decl) => {
            return format!(
                "project {}:{}..{}",
                decl.file.display(),
                decl.start,
                decl.end
            );
        }
        UseDecl::OutsideProject(file) if assembly_decl(use_).is_none() => {
            return format!("declared outside the project at {}", file.display());
        }
        UseDecl::OutsideProject(_) | UseDecl::Unlocated => {}
    }
    if let Some(decl) = assembly_decl(use_) {
        return format!("assembly {} full_name {}", decl.assembly, decl.full_name);
    }
    match (&use_.assembly, &use_.full_name) {
        (Some(assembly), None) => format!("partial assembly {assembly} without full_name"),
        (None, Some(full_name)) => format!("partial full_name {full_name} without assembly"),
        (Some(assembly), Some(full_name)) => {
            format!("assembly {assembly} full_name {full_name}")
        }
        (None, None) => "no oracle declaration".to_string(),
    }
}

fn resolution_summary(loaded: &LoadedProject, file_idx: usize, res: Resolution) -> String {
    match res {
        Resolution::Local(_) | Resolution::Item(_) => match resolution_def(loaded, file_idx, res) {
            Some((actual_file_idx, def)) => format!(
                "project {:?} at {}:{}..{}",
                def.name,
                loaded.parses.paths[actual_file_idx].display(),
                u32::from(def.range.start()),
                u32::from(def.range.end())
            ),
            None => format!("{res:?} (no project def)"),
        },
        Resolution::Entity(_) | Resolution::Member { .. } => {
            let actual = assembly_resolution_decl(&loaded.assembly_env, res);
            format!(
                "assembly {} full_name {}",
                actual.assembly, actual.full_name
            )
        }
        Resolution::Deferred(_) | Resolution::Unresolved => format!("{res:?}"),
    }
}

/// One `open` declaration in an explained file, lifted from the sema
/// [`ResolutionTrace`](borzoi_sema::ResolutionTrace) with the byte range
/// projected to `(start, end)`. A per-open **fact** — its range and opacity —
/// with no relevance verdict attached (see [`TokenExplanation`] for why the tool
/// leaves scope correlation to the reader).
#[derive(Debug, Clone)]
pub struct ExplainedOpen {
    /// The `open …` declaration's `(start, end)` byte range.
    pub range: (usize, usize),
    /// The opened path, `idText`-normalised (the type's path for `open type`).
    pub path: Vec<String>,
    /// Whether this is an `open type …`.
    pub is_type: bool,
    /// Which opaque-open flags this open flipped (see [`OpenOpacity`]).
    pub opacity: OpenOpacity,
}

/// The resolution-explain result for one token (see [`explain_token`]): its
/// resolution and every `open` in the file with its opacity, so a human can see
/// *why* a name deferred — the `open TypeEquality` poisoning a bare
/// `List.replicate` investigation, as a reusable query rather than a manual dig.
///
/// **It states facts, not a relevance verdict.** It reports the token's
/// resolution and each open's *per-open* opacity + range; it does *not* claim
/// which open gated *this* token. The [`ResolutionTrace`](borzoi_sema::ResolutionTrace)
/// is a per-open record, and several deferral causes are *per-token* — they
/// depend on the token, not on any one open, so a per-open trace cannot attribute
/// them:
/// - a member/qualified TAIL (`value.Member`) defers pending inference regardless
///   of any open (a head-vs-tail distinction the trace lacks);
/// - an attribute (`[<Attr>]`) whose in-file type precedes *any* later open
///   defers to that open — every open advances the open frontier, so this is not
///   a property of one open;
/// - an open's lexical scope is a *block*, not an offset prefix — the resolver
///   resets open-state at every top-level block / sibling boundary, so an earlier
///   open by offset may be out of scope entirely.
///
/// So the reader — who has the file — correlates the perturbing opens (with their
/// line ranges) against the token; the tool supplies the candidates and the
/// caveats, not the conclusion. It never labels an open harmless (`clean`),
/// because an all-false open can still take part in a per-token deferral.
#[derive(Debug, Clone)]
pub struct TokenExplanation {
    /// The occurrence `(start, end)` the resolution was recorded at, or `None`
    /// when nothing resolved at this byte.
    pub token_range: Option<(usize, usize)>,
    /// The source text of [`token_range`](Self::token_range) (empty when `None`).
    pub token_text: String,
    /// The resolution at the token, if one was recorded.
    pub resolution: Option<Resolution>,
    /// A human rendering of [`resolution`](Self::resolution) — a project def
    /// site, an assembly full name, or a `Deferred(..)` / not-found note.
    pub resolution_summary: String,
    /// Every `open` in the file, in source order, with its opacity.
    pub opens: Vec<ExplainedOpen>,
}

impl TokenExplanation {
    /// Every `open` in the file that **perturbs resolution** through a modeled
    /// *per-open* mechanism (see [`OpenOpacity::perturbs_resolution`]), in source
    /// order — the candidate culprits a human locates against the token by their
    /// ranges. A per-open fact, not a per-token verdict: an open with no modeled
    /// perturbation can still participate in a *per-token* deferral (an attribute
    /// whose in-file type precedes any later open; a member tail) the per-open
    /// trace cannot attribute. See the type docs.
    pub fn perturbing_opens(&self) -> Vec<&ExplainedOpen> {
        self.opens
            .iter()
            .filter(|o| o.opacity.perturbs_resolution())
            .collect()
    }

    /// A human-readable multi-line report — the CLI dump.
    pub fn render(&self) -> String {
        let mut out = String::new();
        match self.token_range {
            Some((s, e)) => {
                let _ = writeln!(out, "token {:?} @ {s}..{e}", self.token_text);
            }
            None => {
                let _ = writeln!(out, "(no resolution recorded at this position)");
            }
        }
        let _ = writeln!(out, "  resolution: {}", self.resolution_summary);
        if self.opens.is_empty() {
            let _ = writeln!(out, "  opens: (none)");
        } else {
            let _ = writeln!(out, "  opens (source order):");
            for o in &self.opens {
                let kind = if o.is_type { "open type" } else { "open" };
                let effect = if o.opacity.perturbs_resolution() {
                    let mut flags = Vec::new();
                    if o.opacity.opaque_value {
                        flags.push("opaque_value");
                    }
                    if o.opacity.opaque_dotted {
                        flags.push("opaque_dotted");
                    }
                    if o.opacity.unmodelled {
                        flags.push("unmodelled");
                    }
                    if o.opacity.staled_earlier {
                        flags.push("staled_earlier");
                    }
                    if o.opacity.imported_deferred {
                        flags.push("imported_deferred");
                    }
                    if o.opacity.added_reading {
                        flags.push("added_reading");
                    }
                    format!("PERTURBS [{}]", flags.join(", "))
                } else {
                    // Never "clean" — that would claim harmlessness the per-open
                    // trace cannot prove (an all-false open can still cause a
                    // per-token deferral). It triggered none of the modeled ones.
                    "(no modeled per-open effect)".to_string()
                };
                let _ = writeln!(
                    out,
                    "    {kind} {} @ {}..{} — {effect}",
                    o.path.join("."),
                    o.range.0,
                    o.range.1,
                );
            }
        }
        // For any deferred token, spell out what the per-open view can and cannot
        // say — including the per-token deferral causes it does NOT attribute, so
        // the reader is never misled by a "no modeled effect" open. Fires even
        // when no open perturbs per-open, and even when the file has no opens at
        // all (a bare member tail).
        if matches!(self.resolution, Some(Resolution::Deferred(_))) {
            let perturbing = self.perturbing_opens();
            let mut note = String::from("  note: token is Deferred. ");
            if perturbing.is_empty() {
                note.push_str("No open triggers a modeled per-open perturbation. ");
            } else {
                let list: Vec<String> = perturbing
                    .iter()
                    .map(|o| format!("{} @ {}..{}", o.path.join("."), o.range.0, o.range.1))
                    .collect();
                note.push_str(&format!(
                    "{} open(s) trigger a modeled per-open perturbation [{}]; if the token is a \
                     dotted HEAD (e.g. `List` in `List.replicate`) lexically after one in the SAME \
                     block/enclosing module, deleting that open may let it resolve. ",
                    perturbing.len(),
                    list.join(", "),
                ));
            }
            note.push_str(
                "This per-open view does NOT attribute per-token deferrals — correlate manually: \
                 a member/qualified TAIL (`value.Member`) defers pending inference regardless of \
                 any open; an attribute (`[<Attr>]`) whose in-file type precedes ANY later open \
                 defers to that open; and an open's scope is its block, not an offset prefix (the \
                 resolver resets open-state at block boundaries).",
            );
            let _ = writeln!(out, "{note}");
        }
        out
    }
}

/// Explain the token at byte offset `byte` in file `file_idx` of `loaded`: its
/// resolution and the file's opaque-`open` trace, so a human can see why a name
/// deferred (the resolution-explain mechanism). A pure query over the
/// already-resolved project — no refetch, no effects.
pub fn explain_token(loaded: &LoadedProject, file_idx: usize, byte: usize) -> TokenExplanation {
    let file = loaded.resolved.file(file_idx);
    let text = &loaded.parses.texts[file_idx];
    let (token_range, token_text, resolution, resolution_summary) =
        match smallest_resolution_with_range(file, byte) {
            Some((range, res)) => {
                let (s, e) = range_pair(range);
                (
                    Some((s, e)),
                    text.get(s..e).unwrap_or("").to_string(),
                    Some(res),
                    resolution_summary(loaded, file_idx, res),
                )
            }
            None => (
                None,
                String::new(),
                None,
                "(no resolution recorded here)".to_string(),
            ),
        };
    let opens = file
        .resolution_trace()
        .opens
        .iter()
        .map(|o| ExplainedOpen {
            range: range_pair(o.range),
            path: o.path.clone(),
            is_type: o.is_type,
            opacity: o.opacity,
        })
        .collect();
    TokenExplanation {
        token_range,
        token_text,
        resolution,
        resolution_summary,
        opens,
    }
}

fn resolution_def(
    loaded: &LoadedProject,
    file_idx: usize,
    res: Resolution,
) -> Option<(usize, &Def)> {
    loaded
        .resolved
        .file(file_idx)
        .resolved_def(res)
        .map(|def| (file_idx, def))
        .or_else(|| loaded.resolved.item_def(res))
}

fn range_pair(range: TextRange) -> (usize, usize) {
    (
        u32::from(range.start()) as usize,
        u32::from(range.end()) as usize,
    )
}

struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(source: &'a str) -> Self {
        let mut starts = vec![0, 0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { source, starts }
    }

    fn offset(&self, line: u32, col: u32) -> usize {
        let line = line as usize;
        let col = col as usize;
        if line >= self.starts.len() {
            return self.source.len();
        }
        let base = self.starts[line];
        let line_end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());

        let mut units = 0usize;
        let mut byte_pos = base;
        for ch in self.source[base..line_end].chars() {
            if units >= col {
                break;
            }
            let next_units = units + ch.len_utf16();
            if next_units > col {
                break;
            }
            units = next_units;
            byte_pos += ch.len_utf8();
        }
        byte_pos.min(self.source.len())
    }
}

/// Recursively collect `.fsproj` candidates for the ignored corpus runner.
pub fn collect_fsprojs(root: &Path) -> Vec<PathBuf> {
    collect_fsprojs_with_diagnostics(root).projects
}

/// Recursively collect `.fsproj` candidates and every traversal error observed.
pub fn collect_fsprojs_with_diagnostics(root: &Path) -> FsprojCollection {
    let mut collection = FsprojCollection::default();
    collect_fsprojs_into(root, &mut collection);
    collection.projects.sort();
    collection.errors.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.operation.cmp(&b.operation))
            .then(a.message.cmp(&b.message))
    });
    collection
}

/// Projects selected for a project-corpus runner invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCandidates {
    pub discovered: usize,
    pub exhaustive: bool,
    pub max_files: Option<NonZeroUsize>,
    pub visited: Vec<PathBuf>,
    pub discovery_errors: Vec<ProjectDiscoveryError>,
}

/// Parse the current `BORZOI_PROJECT_*` environment and select projects.
pub fn project_candidates_from_env() -> Result<ProjectCandidates, ProjectCandidateSettingsError> {
    ProjectCandidateSettings::from_env().map(project_candidates_from_settings)
}

/// Select projects from already-parsed corpus runner settings.
pub fn project_candidates_from_settings(settings: ProjectCandidateSettings) -> ProjectCandidates {
    match settings.source {
        ProjectCandidateSource::None => ProjectCandidates {
            discovered: 0,
            exhaustive: settings.exhaustive,
            max_files: settings.max_files,
            visited: Vec::new(),
            discovery_errors: Vec::new(),
        },
        ProjectCandidateSource::List(projects) => ProjectCandidates {
            discovered: projects.len(),
            exhaustive: settings.exhaustive,
            max_files: settings.max_files,
            visited: projects,
            discovery_errors: Vec::new(),
        },
        ProjectCandidateSource::Corpus(root) => {
            let collection = collect_fsprojs_with_diagnostics(&root);
            let discovered = collection.projects.len();
            let visited = collection
                .projects
                .into_iter()
                .step_by(settings.stride.get())
                .take(settings.limit.map(NonZeroUsize::get).unwrap_or(usize::MAX))
                .collect();
            ProjectCandidates {
                discovered,
                exhaustive: settings.exhaustive,
                max_files: settings.max_files,
                visited,
                discovery_errors: collection.errors,
            }
        }
    }
}

/// Parsed project-corpus runner settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCandidateSettings {
    pub source: ProjectCandidateSource,
    pub exhaustive: bool,
    pub stride: NonZeroUsize,
    pub limit: Option<NonZeroUsize>,
    pub max_files: Option<NonZeroUsize>,
}

impl ProjectCandidateSettings {
    pub fn from_env() -> Result<Self, ProjectCandidateSettingsError> {
        Self::from_raw_env(ProjectCandidateRawEnv::current())
    }

    pub fn from_raw_env(
        raw: ProjectCandidateRawEnv,
    ) -> Result<Self, ProjectCandidateSettingsError> {
        let source = match (raw.project_list, raw.project_corpus) {
            (Some(_), Some(_)) => return Err(ProjectCandidateSettingsError::MultipleSources),
            (Some(list), None) => {
                ProjectCandidateSource::List(std::env::split_paths(&list).collect())
            }
            (None, Some(root)) => ProjectCandidateSource::Corpus(PathBuf::from(root)),
            (None, None) => ProjectCandidateSource::None,
        };
        let exhaustive = parse_exhaustive(raw.exhaustive)?;
        let explicit_stride = parse_nonzero("BORZOI_PROJECT_STRIDE", raw.stride)?;
        let stride = explicit_stride.unwrap_or_else(|| {
            if exhaustive {
                NonZeroUsize::new(1).expect("1 is non-zero")
            } else {
                NonZeroUsize::new(13).expect("13 is non-zero")
            }
        });
        let limit = parse_nonzero("BORZOI_PROJECT_LIMIT", raw.limit)?;
        let max_files = parse_nonzero("BORZOI_PROJECT_MAX_FILES", raw.max_files)?;

        if exhaustive {
            if explicit_stride.is_some_and(|s| s.get() != 1) {
                return Err(ProjectCandidateSettingsError::ExhaustiveStride { stride });
            }
            if let Some(limit) = limit {
                return Err(ProjectCandidateSettingsError::ExhaustiveLimit { limit });
            }
            if let Some(max_files) = max_files {
                return Err(ProjectCandidateSettingsError::ExhaustiveMaxFiles { max_files });
            }
        }

        Ok(Self {
            source,
            exhaustive,
            stride,
            limit,
            max_files,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCandidateSource {
    None,
    List(Vec<PathBuf>),
    Corpus(PathBuf),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectCandidateRawEnv {
    pub project_list: Option<OsString>,
    pub project_corpus: Option<OsString>,
    pub exhaustive: Option<OsString>,
    pub stride: Option<OsString>,
    pub limit: Option<OsString>,
    pub max_files: Option<OsString>,
}

impl ProjectCandidateRawEnv {
    pub fn current() -> Self {
        Self {
            project_list: std::env::var_os("BORZOI_PROJECT_LIST"),
            project_corpus: std::env::var_os("BORZOI_PROJECT_CORPUS"),
            exhaustive: std::env::var_os("BORZOI_PROJECT_EXHAUSTIVE"),
            stride: std::env::var_os("BORZOI_PROJECT_STRIDE"),
            limit: std::env::var_os("BORZOI_PROJECT_LIMIT"),
            max_files: std::env::var_os("BORZOI_PROJECT_MAX_FILES"),
        }
    }
}

/// Parse corpus-runner project-load options from the environment.
pub type ProjectCorpusRunOptionsResult =
    Result<ProjectCorpusRunOptions, ProjectCorpusRunOptionsError>;

pub fn project_corpus_run_options_from_env() -> ProjectCorpusRunOptionsResult {
    ProjectCorpusRunOptions::from_raw_env(ProjectCorpusRunOptionsRawEnv::current())
}

impl ProjectCorpusRunOptions {
    pub fn from_env() -> ProjectCorpusRunOptionsResult {
        project_corpus_run_options_from_env()
    }

    pub fn from_raw_env(
        raw: ProjectCorpusRunOptionsRawEnv,
    ) -> Result<Self, ProjectCorpusRunOptionsError> {
        Ok(Self {
            build_properties: parse_msbuild_properties(
                "BORZOI_PROJECT_MSBUILD_PROPERTIES",
                raw.msbuild_properties,
            )?,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectCorpusRunOptionsRawEnv {
    pub msbuild_properties: Option<OsString>,
}

impl ProjectCorpusRunOptionsRawEnv {
    pub fn current() -> Self {
        Self {
            msbuild_properties: std::env::var_os("BORZOI_PROJECT_MSBUILD_PROPERTIES"),
        }
    }
}

/// Parse the current `BORZOI_PROJECT_*` ratchet environment.
pub fn corpus_runner_config_from_env() -> Result<CorpusRunnerConfig, CorpusRunnerConfigError> {
    CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv::current())
}

impl CorpusRunnerConfig {
    pub fn from_env() -> Result<Self, CorpusRunnerConfigError> {
        corpus_runner_config_from_env()
    }

    pub fn from_raw_env(raw: CorpusRunnerRawEnv) -> Result<Self, CorpusRunnerConfigError> {
        let expect_divergences = parse_divergence_expectation(raw.expect_divergences)?;
        if expect_divergences.is_some() && raw.max_divergences.is_some() {
            return Err(CorpusRunnerConfigError::ConflictingDivergenceRatchets);
        }
        Ok(Self {
            expect_divergences,
            max_divergences: parse_runner_usize(
                "BORZOI_PROJECT_MAX_DIVERGENCES",
                raw.max_divergences,
            )?
            .unwrap_or(0),
            min_comparable_projects: parse_runner_nonzero(
                "BORZOI_PROJECT_MIN_COMPARABLE",
                raw.min_comparable_projects,
            )?,
            max_skipped_projects: parse_runner_usize(
                "BORZOI_PROJECT_MAX_SKIPPED",
                raw.max_skipped_projects,
            )?,
            max_skipped_project_rate: parse_runner_basis_points(
                "BORZOI_PROJECT_MAX_SKIPPED_BPS",
                raw.max_skipped_project_rate,
            )?,
            min_coverage: parse_runner_basis_points(
                "BORZOI_PROJECT_MIN_COVERAGE_BPS",
                raw.min_coverage,
            )?,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorpusRunnerRawEnv {
    pub expect_divergences: Option<OsString>,
    pub max_divergences: Option<OsString>,
    pub min_comparable_projects: Option<OsString>,
    pub max_skipped_projects: Option<OsString>,
    pub max_skipped_project_rate: Option<OsString>,
    pub min_coverage: Option<OsString>,
}

impl CorpusRunnerRawEnv {
    pub fn current() -> Self {
        Self {
            expect_divergences: std::env::var_os("BORZOI_PROJECT_EXPECT_DIVERGENCES"),
            max_divergences: std::env::var_os("BORZOI_PROJECT_MAX_DIVERGENCES"),
            min_comparable_projects: std::env::var_os("BORZOI_PROJECT_MIN_COMPARABLE"),
            max_skipped_projects: std::env::var_os("BORZOI_PROJECT_MAX_SKIPPED"),
            max_skipped_project_rate: std::env::var_os("BORZOI_PROJECT_MAX_SKIPPED_BPS"),
            min_coverage: std::env::var_os("BORZOI_PROJECT_MIN_COVERAGE_BPS"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusRunnerConfigError {
    /// `BORZOI_PROJECT_EXPECT_DIVERGENCES` and `BORZOI_PROJECT_MAX_DIVERGENCES`
    /// were both set. They are two incompatible readings of the same quantity —
    /// a two-sided expectation and a one-sided ceiling — so a precedence rule
    /// would silently discard whichever the caller meant.
    ConflictingDivergenceRatchets,
    InvalidDivergenceExpectation {
        value: String,
        reason: &'static str,
    },
    InvalidUsize {
        key: &'static str,
        value: String,
    },
    InvalidNonZeroUsize {
        key: &'static str,
        value: String,
    },
    InvalidBasisPoints {
        key: &'static str,
        value: String,
    },
}

impl fmt::Display for CorpusRunnerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingDivergenceRatchets => write!(
                f,
                "set BORZOI_PROJECT_EXPECT_DIVERGENCES or BORZOI_PROJECT_MAX_DIVERGENCES, not both"
            ),
            Self::InvalidDivergenceExpectation { value, reason } => write!(
                f,
                "BORZOI_PROJECT_EXPECT_DIVERGENCES must be \"assembly=<n>,project=<n>,reverse=<n>\"                  ({reason}); got {value:?}"
            ),
            Self::InvalidUsize { key, value } => {
                write!(f, "{key} must be a non-negative integer; got {value:?}")
            }
            Self::InvalidNonZeroUsize { key, value } => {
                write!(f, "{key} must be a positive integer; got {value:?}")
            }
            Self::InvalidBasisPoints { key, value } => {
                write!(
                    f,
                    "{key} must be an integer number of basis points from 0 to 10000; got {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for CorpusRunnerConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCorpusRunOptionsError {
    InvalidMsbuildProperty {
        key: &'static str,
        entry: String,
    },
    DuplicateMsbuildProperty {
        key: &'static str,
        first: String,
        second: String,
    },
}

impl fmt::Display for ProjectCorpusRunOptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMsbuildProperty { key, entry } => write!(
                f,
                "{key} entries must be semicolon-separated Name=Value pairs with non-empty names; got {entry:?}"
            ),
            Self::DuplicateMsbuildProperty { key, first, second } => write!(
                f,
                "{key} contains duplicate MSBuild property names {first:?} and {second:?} (MSBuild property names compare OrdinalIgnoreCase)"
            ),
        }
    }
}

impl std::error::Error for ProjectCorpusRunOptionsError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCandidateSettingsError {
    MultipleSources,
    InvalidFlag { key: &'static str, value: String },
    InvalidNonZeroUsize { key: &'static str, value: String },
    ExhaustiveStride { stride: NonZeroUsize },
    ExhaustiveLimit { limit: NonZeroUsize },
    ExhaustiveMaxFiles { max_files: NonZeroUsize },
}

impl fmt::Display for ProjectCandidateSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleSources => write!(
                f,
                "set only one of BORZOI_PROJECT_LIST or BORZOI_PROJECT_CORPUS"
            ),
            Self::InvalidFlag { key, value } => {
                write!(f, "{key} must be unset, 0, or 1; got {value:?}")
            }
            Self::InvalidNonZeroUsize { key, value } => {
                write!(f, "{key} must be a positive integer; got {value:?}")
            }
            Self::ExhaustiveStride { stride } => {
                write!(
                    f,
                    "BORZOI_PROJECT_EXHAUSTIVE=1 requires stride 1; got {stride}"
                )
            }
            Self::ExhaustiveLimit { limit } => {
                write!(
                    f,
                    "BORZOI_PROJECT_EXHAUSTIVE=1 must not set BORZOI_PROJECT_LIMIT; got {limit}"
                )
            }
            Self::ExhaustiveMaxFiles { max_files } => {
                write!(
                    f,
                    "BORZOI_PROJECT_EXHAUSTIVE=1 must not set BORZOI_PROJECT_MAX_FILES; got {max_files}"
                )
            }
        }
    }
}

impl std::error::Error for ProjectCandidateSettingsError {}

/// Parse `BORZOI_PROJECT_EXPECT_DIVERGENCES`, spelled
/// `assembly=<n>,project=<n>,reverse=<n>` in any order.
///
/// All three categories are required and no category may repeat: the value is a
/// *record* of what the corpus produces, and a spelling that silently defaults a
/// category would record a claim nobody wrote. An unknown key is an error for
/// the same reason — a typo would otherwise leave the category it meant to pin
/// at its default.
fn parse_divergence_expectation(
    raw: Option<OsString>,
) -> Result<Option<DivergenceCounts>, CorpusRunnerConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let text = raw.to_string_lossy().into_owned();
    let invalid = |reason: &'static str| CorpusRunnerConfigError::InvalidDivergenceExpectation {
        value: text.clone(),
        reason,
    };
    let (mut project, mut assembly, mut reverse) = (None, None, None);
    for field in text.split(',') {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| invalid("each field is <category>=<count>"))?;
        let count: usize = value
            .parse()
            .map_err(|_| invalid("each count is a non-negative integer"))?;
        let slot = match key.trim() {
            "project" => &mut project,
            "assembly" => &mut assembly,
            "reverse" => &mut reverse,
            _ => return Err(invalid("categories are assembly, project and reverse")),
        };
        if slot.replace(count).is_some() {
            return Err(invalid("each category appears exactly once"));
        }
    }
    match (project, assembly, reverse) {
        (Some(project), Some(assembly), Some(reverse)) => Ok(Some(DivergenceCounts {
            project,
            assembly,
            reverse,
        })),
        _ => Err(invalid("all three categories are required")),
    }
}

fn parse_runner_usize(
    key: &'static str,
    value: Option<OsString>,
) -> Result<Option<usize>, CorpusRunnerConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    value
        .parse::<usize>()
        .map(Some)
        .map_err(|_| CorpusRunnerConfigError::InvalidUsize {
            key,
            value: value.to_string(),
        })
}

fn parse_runner_nonzero(
    key: &'static str,
    value: Option<OsString>,
) -> Result<Option<NonZeroUsize>, CorpusRunnerConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    value.parse::<NonZeroUsize>().map(Some).map_err(|_| {
        CorpusRunnerConfigError::InvalidNonZeroUsize {
            key,
            value: value.to_string(),
        }
    })
}

fn parse_runner_basis_points(
    key: &'static str,
    value: Option<OsString>,
) -> Result<Option<BasisPoints>, CorpusRunnerConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    let points = value
        .parse::<u16>()
        .map_err(|_| CorpusRunnerConfigError::InvalidBasisPoints {
            key,
            value: value.to_string(),
        })?;
    BasisPoints::new(points)
        .ok_or_else(|| CorpusRunnerConfigError::InvalidBasisPoints {
            key,
            value: value.to_string(),
        })
        .map(Some)
}

fn parse_msbuild_properties(
    key: &'static str,
    value: Option<OsString>,
) -> Result<HashMap<String, String>, ProjectCorpusRunOptionsError> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let value = value.to_string_lossy();
    let mut properties = HashMap::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    for raw_entry in value.split(';') {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((name, property_value)) = entry.split_once('=') else {
            return Err(ProjectCorpusRunOptionsError::InvalidMsbuildProperty {
                key,
                entry: entry.to_string(),
            });
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectCorpusRunOptionsError::InvalidMsbuildProperty {
                key,
                entry: entry.to_string(),
            });
        }
        let lower = name.to_ascii_lowercase();
        if let Some(first) = seen.insert(lower, name.to_string()) {
            return Err(ProjectCorpusRunOptionsError::DuplicateMsbuildProperty {
                key,
                first,
                second: name.to_string(),
            });
        }
        properties.insert(name.to_string(), property_value.trim().to_string());
    }
    Ok(properties)
}

fn parse_exhaustive(value: Option<OsString>) -> Result<bool, ProjectCandidateSettingsError> {
    let Some(value) = value else {
        return Ok(false);
    };
    let value = value.to_string_lossy();
    match value.as_ref() {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(ProjectCandidateSettingsError::InvalidFlag {
            key: "BORZOI_PROJECT_EXHAUSTIVE",
            value: other.to_string(),
        }),
    }
}

fn parse_nonzero(
    key: &'static str,
    value: Option<OsString>,
) -> Result<Option<NonZeroUsize>, ProjectCandidateSettingsError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    value.parse::<NonZeroUsize>().map(Some).map_err(|_| {
        ProjectCandidateSettingsError::InvalidNonZeroUsize {
            key,
            value: value.to_string(),
        }
    })
}

fn collect_fsprojs_into(dir: &Path, collection: &mut FsprojCollection) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            collection
                .errors
                .push(ProjectDiscoveryError::read_dir(dir, error));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                collection
                    .errors
                    .push(ProjectDiscoveryError::read_entry(dir, error));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                collection
                    .errors
                    .push(ProjectDiscoveryError::file_type(&path, error));
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                continue;
            }
            collect_fsprojs_into(&path, collection);
        } else if path.extension().and_then(|s| s.to_str()) == Some("fsproj") {
            collection.projects.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_assembly::EntityKind;
    use proptest::prelude::*;
    use std::ffi::OsString;

    fn bps(value: u16) -> BasisPoints {
        BasisPoints::new(value).expect("test basis points are in range")
    }

    fn write_fixture(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(path, text).expect("write fixture file");
    }

    #[test]
    fn parser_matches_duplicate_basenames_by_full_path() {
        let root = PathBuf::from("/tmp/corpus_diff_parse");
        let a_path = root.join("A").join("Program.fs");
        let b_path = root.join("B").join("Program.fs");
        let a_src: Arc<str> = Arc::from("module A\nlet x = 1\n");
        let b_src: Arc<str> = Arc::from("module B\nlet y = A.x\n");
        let json = format!(
            r#"{{
  "Files": [
    {{
      "Path": "{}",
      "Diagnostics": [],
      "Uses": []
    }},
    {{
      "Path": "{}",
      "Diagnostics": [],
      "Uses": [
        {{
          "SymbolName": "x",
          "Range": {{ "File": "{}", "Start": {{ "Line": 2, "Col": 10 }}, "End": {{ "Line": 2, "Col": 11 }} }},
          "IsFromDefinition": false,
          "DeclRange": {{ "File": "{}", "Start": {{ "Line": 2, "Col": 4 }}, "End": {{ "Line": 2, "Col": 5 }} }},
          "Assembly": null,
          "FullName": null
        }}
      ]
    }}
  ]
}}"#,
            a_path.display(),
            b_path.display(),
            b_path.display(),
            a_path.display(),
        );
        let parsed = parse_project_uses(&json, &[(a_path.clone(), a_src), (b_path.clone(), b_src)])
            .expect("parse project uses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].path, b_path);
        assert_eq!(
            parsed[1].uses[0].decl,
            UseDecl::InProject(DeclSite {
                file: a_path,
                start: 13,
                end: 14,
            })
        );
    }

    /// One `uses-project` file entry with a single use whose `DeclRange`
    /// names `decl_file`.
    fn one_use_json(path: &Path, decl_file: &str) -> String {
        format!(
            r#"{{
  "Files": [
    {{
      "Path": "{}",
      "Diagnostics": [],
      "Uses": [
        {{
          "SymbolName": "x",
          "Range": {{ "File": "{}", "Start": {{ "Line": 3, "Col": 8 }}, "End": {{ "Line": 3, "Col": 9 }} }},
          "IsFromDefinition": false,
          "DeclRange": {{ "File": "{}", "Start": {{ "Line": 2, "Col": 4 }}, "End": {{ "Line": 2, "Col": 5 }} }},
          "Assembly": null,
          "FullName": null
        }}
      ]
    }}
  ]
}}"#,
            path.display(),
            path.display(),
            decl_file.replace('\\', "\\\\"),
        )
    }

    /// A framework-dependent `fcs-dump` names a forwarded corelib type by its
    /// ref-pack facade, a self-contained one by the implementation assembly;
    /// our side always reads the facade. Same entity either way — but nothing
    /// else is folded together.
    #[test]
    fn corelib_facade_and_implementation_name_the_same_assembly() {
        assert_eq!(
            canonical_assembly("System.Private.CoreLib"),
            canonical_assembly("System.Runtime")
        );
        assert_ne!(
            canonical_assembly("System.Runtime"),
            canonical_assembly("System.Collections")
        );
    }

    /// FCS escapes identifiers that need it; that says nothing about which
    /// symbol was bound. Everything else must agree exactly — including the
    /// qualification, which `fcs-dump` supplies for the names FCS reports
    /// bare, and any *interior* backtick, which is part of the identifier.
    #[test]
    fn assembly_full_names_agree_modulo_backtick_quoting_only() {
        assert!(assembly_full_name_agrees(
            "Microsoft.FSharp.Core.Operators.not",
            "Microsoft.FSharp.Core.Operators.``not``"
        ));
        // A double-backtick-quoted identifier may contain a single backtick
        // (`lex.fsl`: only a doubled one closes the quote), so ``a`b`` and
        // ``ab`` are *different* members and must not collapse together.
        assert!(assembly_full_name_agrees("M.a`b", "M.``a`b``"));
        assert!(!assembly_full_name_agrees("M.a`b", "M.``ab``"));
        assert!(!assembly_full_name_agrees("M.ab", "M.``a`b``"));
        assert!(!assembly_full_name_agrees(
            "Microsoft.FSharp.Collections.Seq",
            "Seq"
        ));
        assert!(!assembly_full_name_agrees(
            "Microsoft.FSharp.Collections.Seq",
            "Microsoft.FSharp.Collections.List"
        ));
    }

    /// One `Demo.Holder` name held three ways at once — the generic type, a
    /// same-named non-generic type, and the companion module — plus a
    /// two-parameter `Demo.Pair` and a `Demo.Outer<'a>.Inner`. This is the
    /// candidate set an oracle declaration has to be certified *against*: every
    /// refusal the certification makes has a witness here.
    fn marker_fixture_env() -> AssemblyEnv {
        let generic_holder = fixture_entity("Holder", EntityKind::Class, 1, &["Empty"]);
        let plain_holder = fixture_entity("Holder", EntityKind::Class, 0, &["Empty"]);
        let module_holder = fixture_entity("Holder", EntityKind::Module, 0, &["Empty"]);
        let pair = fixture_entity("Pair", EntityKind::Class, 2, &["Empty"]);
        let mut outer = fixture_entity("Outer", EntityKind::Class, 1, &[]);
        // A nested type re-declares its encloser's type parameters in ECMA-335,
        // so `Inner` carries arity 1 without spelling one of its own.
        outer.nested_types = vec![fixture_entity("Inner", EntityKind::Class, 1, &["Empty"])];
        AssemblyEnv::from_entities(vec![
            generic_holder,
            plain_holder,
            module_holder,
            pair,
            outer,
        ])
    }

    fn fixture_entity(
        name: &str,
        kind: EntityKind,
        arity: usize,
        fields: &[&str],
    ) -> borzoi_assembly::Entity {
        borzoi_assembly::Entity {
            assembly: borzoi_assembly::AssemblyIdentity {
                name: "Demo".to_string(),
                version: borzoi_assembly::Version {
                    major: 1,
                    minor: 0,
                    build: 0,
                    revision: 0,
                },
                public_key_token: None,
            },
            namespace: vec!["Demo".to_string()],
            name: name.to_string(),
            kind,
            access: borzoi_assembly::Access::Public,
            is_sealed: false,
            generic_parameters: (0..arity).map(type_parameter).collect(),
            base_type: None,
            interfaces: vec![],
            members: fields.iter().map(|f| static_field(f)).collect(),
            skipped_members: vec![],
            method_def_tokens: vec![],
            nested_types: vec![],
            is_readonly: false,
            is_byref_like: false,
            is_struct: false,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: vec![],
            source_name: None,
            extension_member_names: vec![],
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            custom_attrs: vec![],
            abbreviation_target: None,
            definition_range: None,
        }
    }

    fn type_parameter(index: usize) -> borzoi_assembly::TypeParameter {
        borzoi_assembly::TypeParameter {
            name: format!("T{index}"),
            variance: borzoi_assembly::Variance::Invariant,
            reference_type_constraint: false,
            value_type_constraint: false,
            default_constructor_constraint: false,
            is_unmanaged: false,
            allows_ref_struct: false,
            nullability: borzoi_assembly::Nullability::Oblivious,
            type_constraints: vec![],
        }
    }

    fn static_field(name: &str) -> borzoi_assembly::Member {
        borzoi_assembly::Member::Field(borzoi_assembly::Field {
            name: name.to_string(),
            access: borzoi_assembly::Access::Public,
            ty: borzoi_assembly::TypeRef::Primitive(borzoi_assembly::Primitive::I4),
            is_static: true,
            is_init_only: false,
            is_volatile: false,
            is_literal: false,
            is_required: false,
            compiler_feature_required: vec![],
            nullability: borzoi_assembly::Nullability::Oblivious,
            custom_attrs: vec![],
        })
    }

    /// The handle for `Demo.<name>` at `arity`, as a module or as a type.
    fn fixture_handle(env: &AssemblyEnv, name: &str, arity: usize, module: bool) -> EntityHandle {
        env.public_entities_named(&["Demo".to_string()], name)
            .into_iter()
            .find(|h| {
                env.entity(*h).generic_parameters.len() == arity && env.is_module(*h) == module
            })
            .unwrap_or_else(|| panic!("Demo.{name} at arity {arity} (module: {module})"))
    }

    /// An oracle declaration in assembly `Demo`: the name FCS *rendered*, plus
    /// the structural path it reported beside it. `path` is
    /// `(compiled name, arity)` per segment, outermost first.
    fn oracle_decl(
        rendered: &str,
        path: &[(&str, usize)],
        leaf: &str,
        is_constructor: bool,
    ) -> AssemblyDecl {
        AssemblyDecl {
            assembly: "Demo".to_string(),
            full_name: rendered.to_string(),
            structural: Some(StructuralName {
                declaring: DeclaringEntity {
                    namespace: vec!["Demo".to_string()],
                    path: path
                        .iter()
                        .map(|(name, arity)| ((*name).to_string(), *arity))
                        .collect(),
                    is_constructor,
                },
                leaf: leaf.to_string(),
                leaf_arity: None,
            }),
        }
    }

    /// A `Resolution::Member` naming `member` on `Demo.<name>`, with the name we
    /// would render for it.
    fn fixture_member(
        env: &AssemblyEnv,
        name: &str,
        arity: usize,
        module: bool,
        member: &str,
    ) -> (Resolution, String) {
        let parent = fixture_handle(env, name, arity, module);
        let idx = env
            .member(parent, member)
            .unwrap_or_else(|| panic!("member {member} on Demo.{name}"));
        let res = Resolution::Member { parent, idx };
        let decl = assembly_resolution_decl(env, res);
        (res, decl.full_name)
    }

    /// The oracle's rendering is never read: whatever type arguments FCS printed
    /// — underscore typars, an instantiation with dots and commas in it, a
    /// function type whose `->` looks like a closing bracket — the declaration
    /// compared is the structural one.
    #[test]
    fn the_rendering_is_ignored_and_the_structural_name_compared() {
        let env = marker_fixture_env();
        let (res, ours) = fixture_member(&env, "Holder", 1, false, "Empty");
        assert_eq!(ours, "Demo.Holder.Empty");
        for rendered in [
            "Demo.Holder<_>.Empty",
            "Demo.Holder<Demo.Thing>.Empty",
            "Demo.Holder<(Microsoft.FSharp.Core.int -> Microsoft.FSharp.Core.string)>.Empty",
            "Demo.Holder<Probe.A,B>.Empty",
        ] {
            let expected = oracle_decl(rendered, &[("Holder`1", 1)], "Empty", false);
            assert_eq!(
                certified_expected(&env, res, &expected).as_deref(),
                Some("Demo.Holder.Empty"),
                "{rendered}"
            );
            assert!(assembly_full_name_agrees_for(&env, res, &ours, &expected));
        }
    }

    /// `ImmutableArray<Probe.A,B>` is *one* argument whose type is named
    /// ``A,B``, and `Holder<(int -> string)>` closes no list at its first `>`.
    /// Reading an arity out of either rendering gets it wrong — 2 and 0 — which
    /// is why the arity comes from the oracle instead. A same-named type of the
    /// wrong arity must not certify.
    #[test]
    fn an_arity_that_only_the_rendering_supports_certifies_nothing() {
        let env = marker_fixture_env();
        let (pair_res, _) = fixture_member(&env, "Pair", 2, false, "Empty");
        // The comma-bearing rendering *looks* like two arguments, and `Pair`
        // does take two — but the oracle says the declaring entity has one.
        assert_eq!(
            certified_expected(
                &env,
                pair_res,
                &oracle_decl(
                    "Demo.Pair<Probe.A,B>.Empty",
                    &[("Holder`1", 1)],
                    "Empty",
                    false
                )
            ),
            None
        );
        // And the right arity on the right entity does certify.
        assert_eq!(
            certified_expected(
                &env,
                pair_res,
                &oracle_decl("Demo.Pair<_,_>.Empty", &[("Pair`2", 2)], "Empty", false)
            )
            .as_deref(),
            Some("Demo.Pair.Empty")
        );
        // A same-named entity of a different arity is a different declaration.
        assert_eq!(
            certified_expected(
                &env,
                pair_res,
                &oracle_decl("Demo.Pair<_>.Empty", &[("Pair`1", 1)], "Empty", false)
            ),
            None
        );
    }

    /// The pin the task asks for: a companion module's member stays a
    /// divergence against a generic declaring entity. A module has no type
    /// parameters, so it can never certify one — which is why the certification
    /// is done against our resolution rather than against a string.
    #[test]
    fn a_module_never_certifies_a_generic_declaring_entity() {
        let env = marker_fixture_env();
        let (module_res, module_ours) = fixture_member(&env, "Holder", 0, true, "Empty");
        assert_eq!(module_ours, "Demo.Holder.Empty");
        assert_eq!(
            certified_expected(
                &env,
                module_res,
                &oracle_decl("Demo.Holder<_>.Empty", &[("Holder`1", 1)], "Empty", false)
            ),
            None
        );
        // Nor does a same-named *type* of the wrong arity — `ImmutableArray`
        // and `ImmutableArray<'T>` are both real, and only one has `Empty`.
        let (plain_res, plain_ours) = fixture_member(&env, "Holder", 0, false, "Empty");
        assert_eq!(plain_ours, "Demo.Holder.Empty");
        assert_eq!(
            certified_expected(
                &env,
                plain_res,
                &oracle_decl("Demo.Holder<_>.Empty", &[("Holder`1", 1)], "Empty", false)
            ),
            None
        );
    }

    /// An *encloser* of the entity we resolved certifies too — the shape a union
    /// case takes, since a case with a field is a type nested in its union, so
    /// FCS declares the case in the union while we resolve the carrier below it.
    #[test]
    fn an_enclosing_entity_certifies_its_own_declaration() {
        let env = marker_fixture_env();
        let outer = fixture_handle(&env, "Outer", 1, false);
        let inner = env
            .children(outer)
            .iter()
            .copied()
            .find(|h| env.entity(*h).name == "Inner")
            .expect("Demo.Outer.Inner");
        let res = Resolution::Entity(inner);
        let ours = assembly_resolution_decl(&env, res).full_name;
        assert_eq!(ours, "Demo.Outer.Inner");
        assert!(assembly_resolution_confirms_decl(
            &env,
            res,
            &oracle_decl(
                "Demo.Outer<_>.Inner",
                // ECMA mangles the *delta*: `Inner` declares none of its own,
                // so it carries no suffix while its arity is the encloser's.
                &[("Outer`1", 1), ("Inner", 1)],
                "Inner",
                true
            )
        ));
        // The encloser's arity is its own, and a contradicting one certifies
        // nothing.
        assert!(!assembly_resolution_confirms_decl(
            &env,
            res,
            &oracle_decl(
                "Demo.Outer<_,_>.Inner",
                &[("Outer`2", 2), ("Inner", 2)],
                "Inner",
                true
            )
        ));
    }

    /// A declaring entity our resolution knows nothing about certifies nothing,
    /// however plausible its name.
    #[test]
    fn a_declaring_entity_outside_the_resolved_chain_is_refused() {
        let env = marker_fixture_env();
        let (res, _) = fixture_member(&env, "Holder", 1, false, "Empty");
        assert_eq!(
            certified_expected(
                &env,
                res,
                &oracle_decl("Demo.Other<_>.Empty", &[("Other`1", 1)], "Empty", false)
            ),
            None
        );
        // A namespace is not an entity we resolved either.
        assert_eq!(
            certified_expected(
                &env,
                res,
                &oracle_decl(
                    "Demo<_>.Holder.Empty",
                    &[("Holder", 1), ("Extra", 1)],
                    "Empty",
                    false
                )
            ),
            None
        );
    }

    /// The certified name is an *extra* accepted name, never a substituted one.
    /// FCS names a **constructor** use by its type — `System.Reflection.AssemblyName`
    /// — while its declaring entity and display name compose to
    /// `…AssemblyName.AssemblyName`; substituting turned 8 agreeing sites on
    /// `WoofWare.PawPrint.Domain` into divergences.
    #[test]
    fn a_certified_name_never_replaces_the_rendered_one() {
        let env = marker_fixture_env();
        let handle = fixture_handle(&env, "Holder", 1, false);
        let res = Resolution::Entity(handle);
        let ours = assembly_resolution_decl(&env, res).full_name;
        assert_eq!(ours, "Demo.Holder");
        let ctor = oracle_decl("Demo.Holder", &[("Holder`1", 1)], "Holder", true);
        // The composed name is not the one FCS gave, and would not match.
        assert_eq!(
            certified_expected(&env, res, &ctor).as_deref(),
            Some("Demo.Holder")
        );
        assert!(assembly_full_name_agrees_for(&env, res, &ours, &ctor));
        assert!(assembly_resolution_confirms_decl(&env, res, &ctor));
    }

    /// A **constructor of a nested type** is where composing declaring-plus-leaf
    /// gets the wrong name: FCS's declaring entity is the type itself, so
    /// `Dictionary`2.Enumerator` + `Enumerator` composes
    /// `Dictionary.Enumerator.Enumerator`. Its *rendering* carries ECMA arity
    /// mangling rather than a decoration, though, so it aligns with our chain
    /// end to end and names the type — measured from FCS on
    /// `System.Collections.Generic.Dictionary<int, string>.Enumerator()`.
    #[test]
    fn a_nested_constructors_rendering_names_its_type() {
        let mut outer = fixture_entity("Outer", EntityKind::Class, 2, &[]);
        outer.nested_types = vec![fixture_entity("Inner", EntityKind::Class, 2, &[])];
        let env = AssemblyEnv::from_entities(vec![outer]);
        let outer_handle = fixture_handle(&env, "Outer", 2, false);
        let inner = env
            .children(outer_handle)
            .iter()
            .copied()
            .find(|h| env.entity(*h).name == "Inner")
            .expect("Demo.Outer.Inner");
        let res = Resolution::Entity(inner);
        let ours = assembly_resolution_decl(&env, res).full_name;
        assert_eq!(ours, "Demo.Outer.Inner");
        let ctor = AssemblyDecl {
            assembly: "Demo".to_string(),
            full_name: "Demo.Outer`2.Inner".to_string(),
            structural: Some(StructuralName {
                declaring: DeclaringEntity {
                    namespace: vec!["Demo".to_string()],
                    path: vec![("Outer`2".to_string(), 2), ("Inner".to_string(), 2)],
                    is_constructor: true,
                },
                leaf: "Inner".to_string(),
                leaf_arity: None,
            }),
        };
        assert_eq!(
            certified_expected(&env, res, &ctor).as_deref(),
            Some("Demo.Outer.Inner")
        );
        assert!(assembly_full_name_agrees_for(&env, res, &ours, &ctor));
    }

    /// The oracle's structural names are in the **compiled** domain and ours in
    /// the source one: a `[<CompiledName "ClrHolder">] type SourceHolder<'T>`
    /// declares its cases in `Renamed.ClrHolder` (measured), while
    /// `entity_full_name` says `Renamed.SourceHolder`. Both spellings name the
    /// same entity, so both certify — and what comes back is *our* name.
    #[test]
    fn a_compiled_name_still_names_the_entity_we_resolved() {
        let mut renamed = fixture_entity("ClrHolder", EntityKind::Union, 1, &["Case"]);
        renamed.source_name = Some("SourceHolder".to_string());
        let env = AssemblyEnv::from_entities(vec![renamed]);
        // The name index keys on the source spelling, which is how a consumer
        // finds it; the compiled one is what the oracle reports.
        let parent = fixture_handle(&env, "SourceHolder", 1, false);
        let idx = env.member(parent, "Case").expect("SourceHolder.Case");
        let res = Resolution::Member { parent, idx };
        let ours = assembly_resolution_decl(&env, res).full_name;
        assert_eq!(ours, "Demo.SourceHolder.Case");
        let case = AssemblyDecl {
            assembly: "Demo".to_string(),
            full_name: "Demo.SourceHolder<_>.Case".to_string(),
            structural: Some(StructuralName {
                declaring: DeclaringEntity {
                    namespace: vec!["Demo".to_string()],
                    path: vec![("ClrHolder".to_string(), 1)],
                    is_constructor: false,
                },
                leaf: "Case".to_string(),
                leaf_arity: None,
            }),
        };
        assert_eq!(
            certified_expected(&env, res, &case).as_deref(),
            Some("Demo.SourceHolder.Case")
        );
        assert!(assembly_full_name_agrees_for(&env, res, &ours, &case));
    }

    /// A nested type's arity mangling is a **delta** per segment, so
    /// ``Outer`1.Inner`1`` and ``Outer`2.Inner`` are different declarations that
    /// both total two parameters. Dropping the suffixes would collapse them and
    /// let a wrong resolution certify; each segment's running total is checked
    /// against the chain entity it names instead.
    #[test]
    fn nested_arity_is_checked_per_segment_not_in_total() {
        let mut outer = fixture_entity("Outer", EntityKind::Class, 1, &[]);
        outer.nested_types = vec![fixture_entity("Inner", EntityKind::Class, 2, &["Empty"])];
        let env = AssemblyEnv::from_entities(vec![outer]);
        let outer_handle = fixture_handle(&env, "Outer", 1, false);
        let inner = env
            .children(outer_handle)
            .iter()
            .copied()
            .find(|h| env.entity(*h).name == "Inner")
            .expect("Demo.Outer.Inner");
        let idx = env.member(inner, "Empty").expect("Inner.Empty");
        let res = Resolution::Member { parent: inner, idx };
        // Ours is `Outer`1.Inner`1`: one parameter on the encloser, two in total.
        assert_eq!(
            certified_expected(
                &env,
                res,
                &oracle_decl(
                    "Demo.Outer<_>.Inner<_>.Empty",
                    &[("Outer`1", 1), ("Inner`1", 2)],
                    "Empty",
                    false
                )
            )
            .as_deref(),
            Some("Demo.Outer.Inner.Empty")
        );
        // `Outer`2.Inner` has the same total and the same segments, and is a
        // different declaration.
        assert_eq!(
            certified_expected(
                &env,
                res,
                &oracle_decl(
                    "Demo.Outer<_,_>.Inner.Empty",
                    &[("Outer`2", 2), ("Inner", 2)],
                    "Empty",
                    false
                )
            ),
            None
        );
    }

    /// A nested type's *own* arity is not in the declaring path, which names its
    /// encloser: `Outer<T>.Inner<U>` and `Outer<T>.Inner<U,V>` both report the
    /// path `Outer` and the leaf `Inner`. The used symbol's own arity, which the
    /// oracle reports for an entity, is what tells them apart.
    #[test]
    fn a_nested_types_own_arity_separates_two_same_named_ones() {
        let mut outer = fixture_entity("Outer", EntityKind::Class, 1, &[]);
        outer.nested_types = vec![fixture_entity("Inner", EntityKind::Class, 2, &[])];
        let env = AssemblyEnv::from_entities(vec![outer]);
        let outer_handle = fixture_handle(&env, "Outer", 1, false);
        let inner = env
            .children(outer_handle)
            .iter()
            .copied()
            .find(|h| env.entity(*h).name == "Inner")
            .expect("Demo.Outer.Inner");
        let res = Resolution::Entity(inner);
        let nested = |leaf_arity: usize| AssemblyDecl {
            assembly: "Demo".to_string(),
            full_name: "Demo.Outer<_>.Inner".to_string(),
            structural: Some(StructuralName {
                declaring: DeclaringEntity {
                    namespace: vec!["Demo".to_string()],
                    path: vec![("Outer`1".to_string(), 1)],
                    is_constructor: false,
                },
                leaf: "Inner".to_string(),
                leaf_arity: Some(leaf_arity),
            }),
        };
        // Ours declares one of its own on top of the encloser's: two in total.
        assert_eq!(
            certified_expected(&env, res, &nested(2)).as_deref(),
            Some("Demo.Outer.Inner")
        );
        assert_eq!(certified_expected(&env, res, &nested(3)), None);
    }

    /// A compiled name that merely *looks* mangled is compared whole. With
    /// `[<CompiledName "C`1">] type A` beside `[<CompiledName "C">] type B`,
    /// both non-generic, the projection stores `C` for both; stripping the
    /// suffix unconditionally would let a resolution to one certify against the
    /// other. The suffix is only dropped when it is that segment's arity delta.
    #[test]
    fn a_suffix_that_is_not_the_arity_delta_is_part_of_the_name() {
        let env =
            AssemblyEnv::from_entities(vec![fixture_entity("C", EntityKind::Class, 0, &["X"])]);
        let parent = fixture_handle(&env, "C", 0, false);
        let idx = env.member(parent, "X").expect("C.X");
        let res = Resolution::Member { parent, idx };
        let ours = assembly_resolution_decl(&env, res).full_name;
        assert_eq!(ours, "Demo.C.X");
        // The oracle names a *different* declaration whose compiled name happens
        // to end in a backtick and a digit.
        assert_eq!(
            certified_expected(
                &env,
                res,
                &oracle_decl("Demo.C.X", &[("C`1", 0)], "X", false)
            ),
            None
        );
        // The same suffix on a generic entity is the mangling, and does strip.
        let generic =
            AssemblyEnv::from_entities(vec![fixture_entity("C", EntityKind::Class, 1, &["X"])]);
        let parent = fixture_handle(&generic, "C", 1, false);
        let idx = generic.member(parent, "X").expect("C.X");
        let res = Resolution::Member { parent, idx };
        assert_eq!(
            certified_expected(
                &generic,
                res,
                &oracle_decl("Demo.C<_>.X", &[("C`1", 1)], "X", false)
            )
            .as_deref(),
            Some("Demo.C.X")
        );
    }

    /// A use with no declaring entity — a bare type, or an oracle that reported
    /// none — is compared exactly as it arrived.
    #[test]
    fn a_use_without_a_declaring_entity_is_compared_as_given() {
        let env = marker_fixture_env();
        let (res, ours) = fixture_member(&env, "Holder", 1, false, "Empty");
        let bare = AssemblyDecl {
            assembly: "Demo".to_string(),
            full_name: "Demo.Holder.Empty".to_string(),
            structural: None,
        };
        assert_eq!(certified_expected(&env, res, &bare), None);
        assert!(assembly_full_name_agrees_for(&env, res, &ours, &bare));
    }

    /// One declaration in the generated candidate set: `Demo.<name>.<member>`
    /// held by an entity of `arity` parameters, as a module or a type.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Decl {
        name: char,
        arity: usize,
        module: bool,
        member: char,
    }

    fn decl_strategy() -> impl Strategy<Value = Decl> {
        ("[A-C]", 0_usize..3, any::<bool>(), "[x-y]").prop_map(|(name, arity, module, member)| {
            Decl {
                name: name.chars().next().expect("one-character name"),
                // A module is never generic, so the pair (module, arity > 0) is
                // not a state the oracle can report.
                arity: if module { 0 } else { arity },
                module,
                member: member.chars().next().expect("one-character member"),
            }
        })
    }

    /// How FCS *renders* `decl`'s enclosing type. Deliberately adversarial —
    /// arguments whose commas are not separators and whose `>`s close nothing —
    /// because the comparison must not depend on this string at all.
    fn fcs_rendering(decl: &Decl, instantiate: bool) -> String {
        if decl.arity == 0 {
            return format!("Demo.{}.{}", decl.name, decl.member);
        }
        let args: Vec<&str> = if instantiate {
            [
                "Probe.A,B",
                "(Microsoft.FSharp.Core.int -> Demo.C)",
                "A.B<C,D>",
            ]
            .into_iter()
            .take(decl.arity)
            .collect()
        } else {
            std::iter::repeat_n("_", decl.arity).collect()
        };
        format!("Demo.{}<{}>.{}", decl.name, args.join(","), decl.member)
    }

    proptest! {
        /// The certification's whole job is to accept a *rendering* difference
        /// without ever equating two different declarations. Stated as a
        /// reference formula over the generated candidate set: an oracle
        /// declaration agrees with our resolution exactly when both name the
        /// same `Demo.<name>.<member>` **and** our entity is a non-module of the
        /// declaring entity's arity. The rendering varies freely underneath and
        /// changes nothing.
        #[test]
        fn certification_never_equates_distinct_declarations(
            ours in decl_strategy(),
            theirs in decl_strategy(),
            instantiate in any::<bool>(),
        ) {
            let entity = fixture_entity(
                &ours.name.to_string(),
                if ours.module { EntityKind::Module } else { EntityKind::Class },
                ours.arity,
                &[&ours.member.to_string()],
            );
            let env = AssemblyEnv::from_entities(vec![entity]);
            let parent = fixture_handle(&env, &ours.name.to_string(), ours.arity, ours.module);
            let idx = env.member(parent, &ours.member.to_string()).expect("planted member");
            let res = Resolution::Member { parent, idx };
            let our_name = assembly_resolution_decl(&env, res).full_name;

            let expected = AssemblyDecl {
                assembly: "Demo".to_string(),
                full_name: fcs_rendering(&theirs, instantiate),
                structural: Some(StructuralName {
                    declaring: DeclaringEntity {
                        namespace: vec!["Demo".to_string()],
                        path: vec![(theirs.name.to_string(), theirs.arity)],
                        is_constructor: false,
                    },
                    leaf: theirs.member.to_string(),
                    leaf_arity: None,
                }),
            };
            let same_declaration = ours.name == theirs.name && ours.member == theirs.member;
            // A *non-generic* declaring entity renders exactly the name we
            // render, so those agree on the name alone — a rendered name cannot
            // tell an arity-0 type's member from its companion module's, and
            // this change does not pretend otherwise (task #39). Where the
            // oracle's entity is generic the rendering can never match, so
            // agreement is the certification's alone: our entity must be a
            // non-module of that arity.
            let want = same_declaration
                && (theirs.arity == 0 || (ours.arity == theirs.arity && !ours.module));
            prop_assert_eq!(
                assembly_full_name_agrees_for(&env, res, &our_name, &expected),
                want,
                "ours {:?} vs oracle {:?}",
                ours,
                expected
            );
        }
    }

    /// FCS's `rangeStartup` sentinel (`range.fs`: `startupFileName = "startup"`)
    /// is the range of the initial type-check environment, so every symbol
    /// imported from a referenced assembly — a BCL namespace, a type — declares
    /// "at startup". It is FCS saying *no source location*, not a file we
    /// failed to load, so it parses as [`UseDecl::Unlocated`] and the use is
    /// adjudicated by assembly identity instead.
    #[test]
    fn a_startup_decl_range_is_unlocated() {
        let path = PathBuf::from("/tmp/corpus_diff_parse_startup/Program.fs");
        let src: Arc<str> = Arc::from("module A\nlet x = 1\nlet y = x\n");
        let parsed = parse_project_uses(&one_use_json(&path, "startup"), &[(path, src)])
            .expect("a startup decl range is not a load failure");
        assert_eq!(parsed[0].uses[0].decl, UseDecl::Unlocated);
    }

    /// An F# assembly carries its *original* source ranges in its signature
    /// data, so FSharp.Core's symbols declare at the build machine's paths.
    /// Those are real files, just not ours: the use keeps the path (so the
    /// report can name it) and is adjudicated by assembly identity.
    #[test]
    fn a_decl_range_outside_the_compile_set_keeps_its_path() {
        let path = PathBuf::from("/tmp/corpus_diff_parse_outside/Program.fs");
        let src: Arc<str> = Arc::from("module A\nlet x = 1\nlet y = x\n");
        let foreign = concat!(
            r"D:\a\_work\1\s\src\fsharp\src\FSharp.Core\",
            "prim-types.fsi"
        );
        let parsed = parse_project_uses(&one_use_json(&path, foreign), &[(path, src)])
            .expect("an out-of-project decl file is not a load failure");
        assert_eq!(
            parsed[0].uses[0].decl,
            UseDecl::OutsideProject(PathBuf::from(foreign))
        );
    }

    #[test]
    fn parser_keeps_an_unknown_decl_file_out_of_the_project_comparison() {
        let root = PathBuf::from("/tmp/corpus_diff_parse_unknown_decl");
        let path = root.join("Program.fs");
        let src: Arc<str> = Arc::from("module A\nlet x = 1\nlet y = x\n");
        let unknown = root.join("Other.fs");
        let json = format!(
            r#"{{
  "Files": [
    {{
      "Path": "{}",
      "Diagnostics": [],
      "Uses": [
        {{
          "SymbolName": "x",
          "Range": {{ "File": "{}", "Start": {{ "Line": 3, "Col": 8 }}, "End": {{ "Line": 3, "Col": 9 }} }},
          "IsFromDefinition": false,
          "DeclRange": {{ "File": "{}", "Start": {{ "Line": 2, "Col": 4 }}, "End": {{ "Line": 2, "Col": 5 }} }},
          "Assembly": null,
          "FullName": null
        }}
      ]
    }}
  ]
}}"#,
            path.display(),
            path.display(),
            unknown.display(),
        );
        let parsed = parse_project_uses(&json, &[(path, src)]).expect("parse project uses");
        assert_eq!(
            parsed[0].uses[0].decl,
            UseDecl::OutsideProject(unknown),
            "an unloadable decl file must not be mistaken for an in-project declaration"
        );
    }

    #[test]
    fn fsproj_collection_reports_missing_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing");

        let collection = collect_fsprojs_with_diagnostics(&missing);

        assert_eq!(collection.projects, Vec::<PathBuf>::new());
        assert_eq!(collection.errors.len(), 1);
        assert_eq!(collection.errors[0].path, missing);
        assert_eq!(
            collection.errors[0].operation,
            ProjectDiscoveryOperation::ReadDir
        );
        assert!(!collection.errors[0].message.is_empty());
    }

    #[test]
    fn project_candidate_settings_rejects_ambiguous_source_env() {
        let err = ProjectCandidateSettings::from_raw_env(ProjectCandidateRawEnv {
            project_list: Some(OsString::from("/tmp/A.fsproj")),
            project_corpus: Some(OsString::from("/tmp/corpus")),
            ..ProjectCandidateRawEnv::default()
        })
        .expect_err("two project sources should be rejected");

        assert_eq!(err, ProjectCandidateSettingsError::MultipleSources);
    }

    #[test]
    fn project_candidate_settings_rejects_invalid_numeric_env() {
        let err = ProjectCandidateSettings::from_raw_env(ProjectCandidateRawEnv {
            project_corpus: Some(OsString::from("/tmp/corpus")),
            stride: Some(OsString::from("0")),
            ..ProjectCandidateRawEnv::default()
        })
        .expect_err("zero stride should be rejected");

        assert_eq!(
            err,
            ProjectCandidateSettingsError::InvalidNonZeroUsize {
                key: "BORZOI_PROJECT_STRIDE",
                value: "0".to_string(),
            }
        );
    }

    #[test]
    fn project_candidate_settings_rejects_exhaustive_limiters() {
        let stride_err = ProjectCandidateSettings::from_raw_env(ProjectCandidateRawEnv {
            project_corpus: Some(OsString::from("/tmp/corpus")),
            exhaustive: Some(OsString::from("1")),
            stride: Some(OsString::from("2")),
            ..ProjectCandidateRawEnv::default()
        })
        .expect_err("exhaustive stride should be rejected");
        assert_eq!(
            stride_err,
            ProjectCandidateSettingsError::ExhaustiveStride {
                stride: NonZeroUsize::new(2).expect("non-zero"),
            }
        );

        let limit_err = ProjectCandidateSettings::from_raw_env(ProjectCandidateRawEnv {
            project_corpus: Some(OsString::from("/tmp/corpus")),
            exhaustive: Some(OsString::from("1")),
            limit: Some(OsString::from("1")),
            ..ProjectCandidateRawEnv::default()
        })
        .expect_err("exhaustive limit should be rejected");
        assert_eq!(
            limit_err,
            ProjectCandidateSettingsError::ExhaustiveLimit {
                limit: NonZeroUsize::new(1).expect("non-zero"),
            }
        );

        let max_files_err = ProjectCandidateSettings::from_raw_env(ProjectCandidateRawEnv {
            project_corpus: Some(OsString::from("/tmp/corpus")),
            exhaustive: Some(OsString::from("1")),
            max_files: Some(OsString::from("1")),
            ..ProjectCandidateRawEnv::default()
        })
        .expect_err("exhaustive max files should be rejected");
        assert_eq!(
            max_files_err,
            ProjectCandidateSettingsError::ExhaustiveMaxFiles {
                max_files: NonZeroUsize::new(1).expect("non-zero"),
            }
        );
    }

    #[test]
    fn project_candidates_apply_stride_limit_and_preserve_max_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in ["A", "B", "C", "D", "E"] {
            write_fixture(
                &tmp.path().join(name).join(format!("{name}.fsproj")),
                "<Project />",
            );
        }

        let candidates = project_candidates_from_settings(ProjectCandidateSettings {
            source: ProjectCandidateSource::Corpus(tmp.path().to_path_buf()),
            exhaustive: false,
            stride: NonZeroUsize::new(2).expect("non-zero"),
            limit: NonZeroUsize::new(2),
            max_files: NonZeroUsize::new(3),
        });

        assert_eq!(candidates.discovered, 5);
        assert_eq!(candidates.visited.len(), 2);
        assert!(candidates.visited[0].ends_with("A.fsproj"));
        assert!(candidates.visited[1].ends_with("C.fsproj"));
        assert_eq!(candidates.max_files, NonZeroUsize::new(3));
    }

    #[test]
    fn project_candidate_settings_accepts_explicit_list_without_corpus_walk() {
        let project_a = PathBuf::from("A.fsproj");
        let project_b = PathBuf::from("B.fsproj");
        let list = std::env::join_paths([&project_a, &project_b]).expect("paths join");
        let settings = ProjectCandidateSettings::from_raw_env(ProjectCandidateRawEnv {
            project_list: Some(list),
            ..ProjectCandidateRawEnv::default()
        })
        .expect("list settings are valid");

        assert_eq!(
            settings.source,
            ProjectCandidateSource::List(vec![project_a, project_b])
        );
        assert!(!settings.exhaustive);
    }

    #[test]
    fn corpus_runner_config_parses_ratchets() {
        let config = CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
            max_divergences: Some(OsString::from("2")),
            min_comparable_projects: Some(OsString::from("3")),
            max_skipped_projects: Some(OsString::from("4")),
            max_skipped_project_rate: Some(OsString::from("2500")),
            min_coverage: Some(OsString::from("9000")),
            expect_divergences: None,
        })
        .expect("runner config is valid");

        assert_eq!(config.max_divergences, 2);
        assert_eq!(config.min_comparable_projects, NonZeroUsize::new(3));
        assert_eq!(config.max_skipped_projects, Some(4));
        assert_eq!(config.max_skipped_project_rate, Some(bps(2500)));
        assert_eq!(config.min_coverage, Some(bps(9000)));
    }

    #[test]
    fn project_corpus_run_options_parse_msbuild_properties() {
        let options = ProjectCorpusRunOptions::from_raw_env(ProjectCorpusRunOptionsRawEnv {
            msbuild_properties: Some(OsString::from(
                "DISABLE_ARCADE=true; Configuration = Release ; Empty=",
            )),
        })
        .expect("runner options are valid");

        assert_eq!(
            options.build_properties,
            HashMap::from([
                ("DISABLE_ARCADE".to_string(), "true".to_string()),
                ("Configuration".to_string(), "Release".to_string()),
                ("Empty".to_string(), "".to_string()),
            ])
        );
    }

    #[test]
    fn project_corpus_run_options_reject_invalid_msbuild_properties() {
        assert_eq!(
            ProjectCorpusRunOptions::from_raw_env(ProjectCorpusRunOptionsRawEnv {
                msbuild_properties: Some(OsString::from("DISABLE_ARCADE")),
            }),
            Err(ProjectCorpusRunOptionsError::InvalidMsbuildProperty {
                key: "BORZOI_PROJECT_MSBUILD_PROPERTIES",
                entry: "DISABLE_ARCADE".to_string(),
            })
        );
        assert_eq!(
            ProjectCorpusRunOptions::from_raw_env(ProjectCorpusRunOptionsRawEnv {
                msbuild_properties: Some(OsString::from("Name=1; name=2")),
            }),
            Err(ProjectCorpusRunOptionsError::DuplicateMsbuildProperty {
                key: "BORZOI_PROJECT_MSBUILD_PROPERTIES",
                first: "Name".to_string(),
                second: "name".to_string(),
            })
        );
    }

    #[test]
    fn corpus_runner_config_rejects_invalid_ratchets() {
        assert_eq!(
            CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
                min_comparable_projects: Some(OsString::from("0")),
                ..CorpusRunnerRawEnv::default()
            }),
            Err(CorpusRunnerConfigError::InvalidNonZeroUsize {
                key: "BORZOI_PROJECT_MIN_COMPARABLE",
                value: "0".to_string(),
            })
        );
        assert_eq!(
            CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
                max_skipped_project_rate: Some(OsString::from("10001")),
                ..CorpusRunnerRawEnv::default()
            }),
            Err(CorpusRunnerConfigError::InvalidBasisPoints {
                key: "BORZOI_PROJECT_MAX_SKIPPED_BPS",
                value: "10001".to_string(),
            })
        );
        assert_eq!(
            CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
                max_divergences: Some(OsString::from("not-a-number")),
                ..CorpusRunnerRawEnv::default()
            }),
            Err(CorpusRunnerConfigError::InvalidUsize {
                key: "BORZOI_PROJECT_MAX_DIVERGENCES",
                value: "not-a-number".to_string(),
            })
        );
    }

    #[test]
    fn corpus_summary_aggregates_counts_and_skip_reasons() {
        let mut summary = CorpusSummary::new(3);
        summary.record_project_visited();
        summary.record_project_visited();
        summary.record_project_visited();
        summary.record_skip("/tmp/SkippedA.fsproj", "project evaluation failed");
        summary.record_skip("/tmp/SkippedB.fsproj", "project evaluation failed");
        summary.record_project_assets(
            "/tmp/MissingAssets.fsproj",
            ProjectAssetsStatus::Missing {
                path: PathBuf::from("/tmp/obj/project.assets.json"),
            },
        );
        summary.record_project_discovery_errors(vec![ProjectDiscoveryError {
            path: PathBuf::from("/tmp/unreadable"),
            operation: ProjectDiscoveryOperation::ReadDir,
            message: "permission denied".to_string(),
        }]);

        let expected = DeclSite {
            file: PathBuf::from("/tmp/A.fs"),
            start: 10,
            end: 11,
        };
        let comparison = Comparison {
            files_compared: 2,
            uses_reported: 8,
            uses_considered: 4,
            assembly_uses_considered: 2,
            matches: 3,
            assembly_matches: 1,
            deferrals: 1,
            assembly_deferrals: 1,
            skipped_uses: SkippedUses {
                definitions: 2,
                zero_width: 1,
                non_project_declarations: 3,
                out_of_project_declarations: 0,
                no_oracle_declaration: 4,
            },
            unoracled_definitions: 0,
            unoracled_or_pattern_aliases: 0,
            divergences: vec![Divergence {
                file: PathBuf::from("/tmp/B.fs"),
                range: (20, 21),
                name: "x".to_string(),
                expected: expected.clone(),
                actual: "Deferred".to_string(),
            }],
            assembly_divergences: vec![AssemblyDivergence {
                file: PathBuf::from("/tmp/B.fs"),
                range: (30, 35),
                name: "Value".to_string(),
                expected: AssemblyDecl {
                    assembly: "Synthetic.Assembly".to_string(),
                    full_name: "Demo.Widget.Value".to_string(),
                    structural: None,
                },
                actual: "assembly Synthetic.Assembly full_name Demo.Widget.Other".to_string(),
            }],
            reverse_divergences: vec![ReverseDivergence {
                file: PathBuf::from("/tmp/B.fs"),
                range: (40, 41),
                actual: "project \"x\" at /tmp/A.fs:10..11".to_string(),
                covering_oracles: vec!["no oracle declaration".to_string()],
            }],
            fcs_error_files: Vec::new(),
        };

        summary.record_comparison(&comparison);

        assert_eq!(summary.projects_discovered, 3);
        assert_eq!(summary.projects_visited, 3);
        assert_eq!(summary.comparable_projects, 1);
        assert_eq!(summary.skipped_projects.len(), 2);
        assert_eq!(summary.project_assets.len(), 1);
        assert_eq!(
            summary
                .project_assets_by_status
                .get(&ProjectAssetsStatusKind::Missing),
            Some(&1)
        );
        assert_eq!(summary.project_discovery_errors.len(), 1);
        assert_eq!(
            summary
                .project_discovery_errors_by_operation
                .get(&ProjectDiscoveryOperation::ReadDir),
            Some(&1)
        );
        assert_eq!(
            summary.skipped_by_reason.get("project evaluation failed"),
            Some(&2)
        );
        assert_eq!(summary.files_compared, 2);
        assert_eq!(summary.fcs_uses_reported, 8);
        assert_eq!(summary.total_uses_considered(), 6);
        assert_eq!(summary.total_matches(), 4);
        assert_eq!(summary.total_deferrals(), 2);
        assert_eq!(summary.total_divergences(), 3);
        assert_eq!(summary.skipped_uses.total(), 10);
        assert_eq!(summary.coverage_percent_string(), "66.67");
        assert_eq!(summary.skipped_projects_percent_string(), "66.67");

        let report = summary.render_text_report();
        assert!(
            report.contains(
                "3 discovered | 3 visited | 1 comparable | 2 skipped | 1 discovery errors"
            )
        );
        assert!(report.contains("project-corpus-diff skipped project rate: 66.67%"));
        assert!(report.contains("1 project | 1 assembly | 1 reverse | 3 total"));
        assert!(report.contains("project-corpus-diff discovery errors by operation:"));
        assert!(report.contains("1: read_dir"));
        assert!(report.contains("project-corpus-diff project assets by status:"));
        assert!(report.contains("1: missing"));

        let json = summary
            .render_json_report_line()
            .expect("summary serializes as JSON");
        assert!(json.ends_with('\n'));
        let report: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(report["kind"], "project_corpus_diff_summary");
        assert_eq!(report["build_properties"], serde_json::json!({}));
        assert_eq!(report["projects"]["discovered"], 3);
        assert_eq!(report["projects"]["visited"], 3);
        assert_eq!(report["projects"]["comparable"], 1);
        assert_eq!(report["projects"]["skipped"], 2);
        assert_eq!(report["projects"]["skipped_basis_points"], 6667);
        assert_eq!(report["projects"]["skipped_percent"], "66.67");
        assert_eq!(report["projects"]["discovery_errors"], 1);
        assert_eq!(report["project_assets"]["by_status"]["missing"], 1);
        assert_eq!(
            report["project_assets"]["observations"][0]["project"],
            "/tmp/MissingAssets.fsproj"
        );
        assert_eq!(
            report["project_assets"]["observations"][0]["status"]["kind"],
            "missing"
        );
        assert_eq!(report["uses"]["fcs_reported"], 8);
        assert_eq!(report["uses"]["total_considered"], 6);
        assert_eq!(report["matches"]["total"], 4);
        assert!(report["matches"].get("reverse").is_none());
        assert_eq!(report["deferrals"]["total"], 2);
        assert_eq!(report["divergences"]["total"], 3);
        assert_eq!(report["coverage"]["basis_points"], 6667);
        assert_eq!(report["coverage"]["percent"], "66.67");
        assert_eq!(
            report["skipped_projects"][0]["project"],
            "/tmp/SkippedA.fsproj"
        );
        assert_eq!(
            report["discovery_errors_by_operation"]["read_dir"],
            serde_json::json!(1)
        );
    }

    #[test]
    fn corpus_summary_reports_build_properties() {
        let summary = CorpusSummary::new_with_build_properties(
            1,
            &HashMap::from([
                ("DISABLE_ARCADE".to_string(), "true".to_string()),
                ("Configuration".to_string(), "Release".to_string()),
            ]),
        );

        let text = summary.render_text_report();
        assert!(text.contains(
            "project-corpus-diff MSBuild properties: Configuration=Release; DISABLE_ARCADE=true"
        ));

        let json = summary
            .render_json_report_line()
            .expect("summary serializes as JSON");
        let report: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(report["build_properties"]["Configuration"], "Release");
        assert_eq!(report["build_properties"]["DISABLE_ARCADE"], "true");
    }

    #[test]
    fn corpus_summary_soundness_gate_requires_comparable_project() {
        let mut summary = CorpusSummary::new(1);
        assert_eq!(summary.coverage_percent_string(), "n/a");
        assert!(!summary.passes_soundness_gate(0));

        summary.record_comparison(&Comparison::default());
        assert!(summary.passes_soundness_gate(0));

        summary.record_comparison(&Comparison {
            reverse_divergences: vec![ReverseDivergence {
                file: PathBuf::from("/tmp/B.fs"),
                range: (5, 6),
                actual: "project \"x\" at /tmp/A.fs:1..2".to_string(),
                covering_oracles: Vec::new(),
            }],
            ..Comparison::default()
        });

        assert!(!summary.passes_soundness_gate(0));
        assert!(summary.passes_soundness_gate(1));
    }

    /// A summary carrying `project`/`assembly`/`reverse` divergences, for the
    /// expectation tests below.
    fn summary_with_divergences(counts: DivergenceCounts) -> CorpusSummary {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison {
            divergences: (0..counts.project)
                .map(|i| Divergence {
                    file: PathBuf::from("/tmp/B.fs"),
                    range: (i, i + 1),
                    name: "x".to_string(),
                    expected: DeclSite {
                        file: PathBuf::from("/tmp/A.fs"),
                        start: 1,
                        end: 2,
                    },
                    actual: "Unresolved".to_string(),
                })
                .collect(),
            assembly_divergences: (0..counts.assembly)
                .map(|i| AssemblyDivergence {
                    file: PathBuf::from("/tmp/B.fs"),
                    range: (i, i + 1),
                    name: "y".to_string(),
                    expected: AssemblyDecl {
                        assembly: "Lib".to_string(),
                        full_name: "Lib.T".to_string(),
                        structural: None,
                    },
                    actual: "Other.T".to_string(),
                })
                .collect(),
            reverse_divergences: (0..counts.reverse)
                .map(|i| ReverseDivergence {
                    file: PathBuf::from("/tmp/B.fs"),
                    range: (i, i + 1),
                    actual: "project \"x\" at /tmp/A.fs:1..2".to_string(),
                    covering_oracles: Vec::new(),
                })
                .collect(),
            ..Comparison::default()
        });
        summary
    }

    fn run_with_divergences(counts: DivergenceCounts) -> CorpusRun {
        CorpusRun {
            summary: summary_with_divergences(counts),
            exhaustive: false,
            divergence_details: Vec::new(),
        }
    }

    const RECORDED: DivergenceCounts = DivergenceCounts {
        project: 1,
        assembly: 16,
        reverse: 16,
    };

    fn expecting(counts: DivergenceCounts) -> CorpusRunnerConfig {
        CorpusRunnerConfig {
            expect_divergences: Some(counts),
            ..CorpusRunnerConfig::default()
        }
    }

    #[test]
    fn a_divergence_expectation_passes_only_on_the_exact_counts() {
        assert_eq!(
            check_project_corpus_run(&run_with_divergences(RECORDED), expecting(RECORDED)),
            Ok(())
        );
    }

    /// The regression direction — what a `#204`-shaped change does.
    #[test]
    fn a_divergence_expectation_fails_when_a_category_regresses() {
        let observed = DivergenceCounts {
            assembly: 17,
            ..RECORDED
        };
        assert_eq!(
            check_project_corpus_run(&run_with_divergences(observed), expecting(RECORDED)),
            Err(CorpusRunFailure::DivergenceExpectation {
                expected: RECORDED,
                observed,
            })
        );
    }

    /// The other side of the ratchet: fixing a divergence fails until the
    /// recorded count comes down with it. Without this the ceiling never
    /// descends and the gate decays into a rubber stamp.
    #[test]
    fn a_divergence_expectation_fails_when_a_category_improves() {
        let observed = DivergenceCounts {
            assembly: 15,
            ..RECORDED
        };
        assert_eq!(
            check_project_corpus_run(&run_with_divergences(observed), expecting(RECORDED)),
            Err(CorpusRunFailure::DivergenceExpectation {
                expected: RECORDED,
                observed,
            })
        );
    }

    /// Why the expectation is per-category rather than a single total: a change
    /// that introduces an assembly wrong target while fixing a project one
    /// leaves the total untouched, and a total-only ratchet cannot see it.
    #[test]
    fn a_divergence_expectation_sees_a_trade_that_keeps_the_total() {
        let observed = DivergenceCounts {
            project: 0,
            assembly: 17,
            reverse: 16,
        };
        assert_eq!(observed.total(), RECORDED.total());
        assert_eq!(
            check_project_corpus_run(&run_with_divergences(observed), expecting(RECORDED)),
            Err(CorpusRunFailure::DivergenceExpectation {
                expected: RECORDED,
                observed,
            })
        );
    }

    /// An expectation is still a ceiling: a run that measured nothing at all
    /// must not satisfy it by accident.
    #[test]
    fn a_divergence_expectation_still_requires_a_comparable_project() {
        let empty = CorpusRun {
            summary: CorpusSummary::new(1),
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert!(check_project_corpus_run(&empty, expecting(RECORDED)).is_err());
    }

    #[test]
    fn a_divergence_expectation_parses_its_three_categories_in_any_order() {
        let parsed = CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
            expect_divergences: Some(OsString::from("reverse=16,assembly=16,project=1")),
            ..CorpusRunnerRawEnv::default()
        })
        .expect("parses");
        assert_eq!(parsed.expect_divergences, Some(RECORDED));
    }

    #[test]
    fn a_divergence_expectation_rejects_a_malformed_spelling() {
        for bad in [
            "assembly=16,project=1",                        // reverse missing
            "assembly=16,project=1,reverse=16,x=0",         // unknown category
            "assembly=16,assembly=16,project=1,reverse=16", // duplicate
            "assembly=16 project=1 reverse=16",             // wrong separator
            "assembly=-1,project=1,reverse=16",             // not a count
        ] {
            let parsed = CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
                expect_divergences: Some(OsString::from(bad)),
                ..CorpusRunnerRawEnv::default()
            });
            assert!(parsed.is_err(), "{bad:?} must not parse");
        }
    }

    /// The two knobs say the same thing in incompatible ways — one-sided
    /// ceiling versus two-sided expectation — so setting both is a
    /// configuration error rather than a silent precedence rule.
    #[test]
    fn a_divergence_expectation_conflicts_with_a_max_divergences_ceiling() {
        let parsed = CorpusRunnerConfig::from_raw_env(CorpusRunnerRawEnv {
            expect_divergences: Some(OsString::from("assembly=16,project=1,reverse=16")),
            max_divergences: Some(OsString::from("33")),
            ..CorpusRunnerRawEnv::default()
        });
        assert_eq!(
            parsed,
            Err(CorpusRunnerConfigError::ConflictingDivergenceRatchets)
        );
    }

    #[test]
    fn project_corpus_run_gate_reports_runner_failures() {
        let config = CorpusRunnerConfig::default();
        let empty = CorpusRun {
            summary: CorpusSummary::new(0),
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(&empty, config),
            Err(CorpusRunFailure::NoProjectsVisited)
        );

        let mut no_comparable_summary = CorpusSummary::new(1);
        no_comparable_summary.record_project_visited();
        let no_comparable = CorpusRun {
            summary: no_comparable_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(&no_comparable, config),
            Err(CorpusRunFailure::NoComparableProjects)
        );

        let mut discovery_summary = CorpusSummary::new(1);
        discovery_summary.record_project_visited();
        discovery_summary.record_comparison(&Comparison::default());
        discovery_summary.record_project_discovery_errors(vec![ProjectDiscoveryError {
            path: PathBuf::from("/tmp/unreadable"),
            operation: ProjectDiscoveryOperation::ReadDir,
            message: "permission denied".to_string(),
        }]);
        let discovery = CorpusRun {
            summary: discovery_summary,
            exhaustive: true,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(&discovery, config),
            Err(CorpusRunFailure::ExhaustiveDiscoveryErrors { errors: 1 })
        );

        let mut divergent_summary = CorpusSummary::new(1);
        divergent_summary.record_project_visited();
        divergent_summary.record_comparison(&Comparison {
            divergences: vec![Divergence {
                file: PathBuf::from("/tmp/B.fs"),
                range: (5, 6),
                name: "x".to_string(),
                expected: DeclSite {
                    file: PathBuf::from("/tmp/A.fs"),
                    start: 1,
                    end: 2,
                },
                actual: "Unresolved".to_string(),
            }],
            ..Comparison::default()
        });
        let divergent = CorpusRun {
            summary: divergent_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(&divergent, config),
            Err(CorpusRunFailure::SoundnessGate {
                max_divergences: 0,
                divergences: 1,
            })
        );
    }

    #[test]
    fn project_corpus_run_gate_reports_ratchet_failures() {
        let mut min_comparable_summary = CorpusSummary::new(2);
        min_comparable_summary.record_project_visited();
        min_comparable_summary.record_project_visited();
        min_comparable_summary.record_comparison(&Comparison {
            uses_considered: 1,
            matches: 1,
            ..Comparison::default()
        });
        min_comparable_summary.record_comparison(&Comparison {
            uses_considered: 1,
            matches: 1,
            ..Comparison::default()
        });
        let min_comparable = CorpusRun {
            summary: min_comparable_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(
                &min_comparable,
                CorpusRunnerConfig {
                    min_comparable_projects: NonZeroUsize::new(3),
                    ..CorpusRunnerConfig::default()
                },
            ),
            Err(CorpusRunFailure::MinComparableProjects {
                min: NonZeroUsize::new(3).expect("non-zero"),
                comparable: 2,
            })
        );

        let mut max_skipped_summary = CorpusSummary::new(3);
        max_skipped_summary.record_project_visited();
        max_skipped_summary.record_project_visited();
        max_skipped_summary.record_project_visited();
        max_skipped_summary.record_skip("/tmp/SkippedA.fsproj", "project evaluation failed");
        max_skipped_summary.record_skip("/tmp/SkippedB.fsproj", "project evaluation failed");
        max_skipped_summary.record_comparison(&Comparison {
            uses_considered: 1,
            matches: 1,
            ..Comparison::default()
        });
        let max_skipped = CorpusRun {
            summary: max_skipped_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(
                &max_skipped,
                CorpusRunnerConfig {
                    max_skipped_projects: Some(1),
                    ..CorpusRunnerConfig::default()
                },
            ),
            Err(CorpusRunFailure::MaxSkippedProjects { max: 1, skipped: 2 })
        );

        let mut max_skipped_rate_summary = CorpusSummary::new(2);
        max_skipped_rate_summary.record_project_visited();
        max_skipped_rate_summary.record_project_visited();
        max_skipped_rate_summary.record_skip("/tmp/Skipped.fsproj", "project evaluation failed");
        max_skipped_rate_summary.record_comparison(&Comparison {
            uses_considered: 1,
            matches: 1,
            ..Comparison::default()
        });
        let max_skipped_rate = CorpusRun {
            summary: max_skipped_rate_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(
                &max_skipped_rate,
                CorpusRunnerConfig {
                    max_skipped_project_rate: Some(bps(4_999)),
                    ..CorpusRunnerConfig::default()
                },
            ),
            Err(CorpusRunFailure::MaxSkippedProjectRate {
                max: bps(4_999),
                actual_basis_points: 5_000,
                skipped: 1,
                visited: 2,
            })
        );

        let mut coverage_unavailable_summary = CorpusSummary::new(1);
        coverage_unavailable_summary.record_project_visited();
        coverage_unavailable_summary.record_comparison(&Comparison::default());
        let coverage_unavailable = CorpusRun {
            summary: coverage_unavailable_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(
                &coverage_unavailable,
                CorpusRunnerConfig {
                    min_coverage: Some(bps(1)),
                    ..CorpusRunnerConfig::default()
                },
            ),
            Err(CorpusRunFailure::CoverageUnavailable { min: bps(1) })
        );

        let mut min_coverage_summary = CorpusSummary::new(1);
        min_coverage_summary.record_project_visited();
        min_coverage_summary.record_comparison(&Comparison {
            uses_considered: 4,
            matches: 3,
            ..Comparison::default()
        });
        let min_coverage = CorpusRun {
            summary: min_coverage_summary,
            exhaustive: false,
            divergence_details: Vec::new(),
        };
        assert_eq!(
            check_project_corpus_run(
                &min_coverage,
                CorpusRunnerConfig {
                    min_coverage: Some(bps(8_000)),
                    ..CorpusRunnerConfig::default()
                },
            ),
            Err(CorpusRunFailure::MinCoverage {
                min: bps(8_000),
                actual_basis_points: 7_500,
            })
        );
    }

    #[test]
    fn project_corpus_run_report_includes_diagnostics() {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_skip("/tmp/Skipped.fsproj", "project evaluation failed");
        summary.record_project_discovery_errors(vec![ProjectDiscoveryError {
            path: PathBuf::from("/tmp/unreadable"),
            operation: ProjectDiscoveryOperation::ReadDir,
            message: "permission denied".to_string(),
        }]);
        let run = CorpusRun {
            summary,
            exhaustive: false,
            divergence_details: vec![
                "divergence /tmp/B.fs:5..6 x expected /tmp/A.fs:1..2, got Unresolved".to_string(),
            ],
        };

        let report = render_project_corpus_run_report(&run);

        assert!(report.contains("1 discovered | 1 visited | 0 comparable | 1 skipped"));
        assert!(report.contains("divergence /tmp/B.fs:5..6 x expected"));
        assert!(report.contains("skipped /tmp/Skipped.fsproj: project evaluation failed"));
        assert!(report.contains("project discovery error: read_dir /tmp/unreadable"));
    }

    #[test]
    fn json_report_writer_writes_one_summary_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report_path = tmp.path().join("summary.jsonl");
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison::default());

        write_json_report_line(&report_path, &summary).expect("write report");

        let text = std::fs::read_to_string(report_path).expect("read report");
        assert_eq!(text.lines().count(), 1);
        let report: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(report["kind"], "project_corpus_diff_summary");
        assert_eq!(report["projects"]["discovered"], 1);
        assert_eq!(report["projects"]["visited"], 1);
        assert_eq!(report["projects"]["comparable"], 1);
    }

    /// Every counter the text report shows must reach the JSONL too — machine
    /// consumers ratchet on it, and a silently-absent field reads as "this
    /// never happens" (codex review).
    #[test]
    fn the_json_report_carries_the_unoracled_occurrence_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report_path = tmp.path().join("summary.jsonl");
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison {
            unoracled_definitions: 7,
            unoracled_or_pattern_aliases: 3,
            ..Comparison::default()
        });

        write_json_report_line(&report_path, &summary).expect("write report");

        let text = std::fs::read_to_string(report_path).expect("read report");
        let report: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(report["unoracled_definitions"], 7);
        assert_eq!(report["unoracled_or_pattern_aliases"], 3);
    }

    /// The generator summary is what `borzoi-stats record` publishes, so a
    /// counter that reaches the JSONL but not `statistics` is invisible in the
    /// continuous measurements — the series simply never exists, which reads as
    /// "this never happens" rather than "nobody plotted it" (codex review).
    #[test]
    fn the_generator_statistics_carry_the_unoracled_occurrence_counts() {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison {
            unoracled_definitions: 7,
            unoracled_or_pattern_aliases: 3,
            ..Comparison::default()
        });

        let rendered = render_generator_summary(&summary, &generator_settings()).expect("render");
        let json: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

        assert_eq!(json["statistics"]["unoracled_definitions"], 7);
        assert_eq!(json["statistics"]["unoracled_or_pattern_aliases"], 3);
    }

    fn generator_settings() -> ProjectCandidateSettings {
        ProjectCandidateSettings {
            source: ProjectCandidateSource::List(vec![PathBuf::from("/checkout/A.fsproj")]),
            exhaustive: false,
            stride: NonZeroUsize::new(1).expect("non-zero"),
            limit: None,
            max_files: None,
        }
    }

    /// The contract is only worth having if the recorder accepts it, so check
    /// it with the recorder — not with a restatement of its rules here, which
    /// would drift the moment `borzoi-stats` tightened one.
    #[test]
    fn the_generator_summary_is_accepted_by_the_stats_recorder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let summary_path = tmp.path().join("summary.json");
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison::default());

        write_generator_summary(&summary_path, &summary, &generator_settings())
            .expect("write generator summary");

        let recorded = borzoi_stats::record_observation(&borzoi_stats::RecordInput {
            summary: summary_path,
            history: tmp.path().join("history"),
            repository: "Smaug123/borzoi".into(),
            commit: "0".repeat(40),
            measured_at: "2026-07-25T10:00:00Z".into(),
            run_id: 1,
            run_number: 1,
            run_attempt: 1,
            corpus_source: "Smaug123/borzoi-project-corpus".into(),
            corpus_revision: "1".repeat(40),
            flake_lock_hash: "a".repeat(64),
        })
        .expect("the stats recorder accepts our generator summary");
        assert!(
            recorded
                .to_string_lossy()
                .contains(PROJECT_CORPUS_MEASUREMENT),
            "{recorded:?}"
        );
    }

    /// Two runs that measured the same thing must land in the same series, and
    /// the checkout path is the trap: in CI it is a fresh temp directory every
    /// run, so leaking it into the configuration would file every observation
    /// under a series of one and no trend would ever appear.
    #[test]
    fn the_configuration_records_the_knobs_but_no_checkout_path() {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison::default());

        let here = render_generator_summary(&summary, &generator_settings()).expect("render");
        let mut elsewhere_settings = generator_settings();
        elsewhere_settings.source =
            ProjectCandidateSource::List(vec![PathBuf::from("/other/run/B.fsproj")]);
        let elsewhere = render_generator_summary(&summary, &elsewhere_settings).expect("render");
        assert_eq!(here, elsewhere);
        assert!(!here.contains("/checkout/"), "{here}");

        let json: serde_json::Value = serde_json::from_str(&here).expect("valid JSON");
        assert_eq!(json["measurement"], PROJECT_CORPUS_MEASUREMENT);
        assert_eq!(json["configuration"]["selection"]["source"], "list");
    }

    /// The configuration is digested into the series key, so it must record
    /// the knobs that *acted* and no others. `stride` and `limit` act only on
    /// a directory walk — [`project_candidates_from_settings`] visits an
    /// explicit list whole — so recording them for a list run would claim an
    /// influence they never had and would split the series if a default moved.
    #[test]
    fn the_configuration_records_only_the_knobs_that_selection_actually_applied() {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison::default());
        let render = |settings: &ProjectCandidateSettings| -> serde_json::Value {
            serde_json::from_str(&render_generator_summary(&summary, settings).expect("render"))
                .expect("valid JSON")
        };

        // The pinned corpus is an explicit list, and stride is *13* by default
        // even there, so this is the live case rather than a hypothetical.
        let mut listed = generator_settings();
        listed.stride = NonZeroUsize::new(13).expect("non-zero");
        listed.limit = NonZeroUsize::new(4);
        let selection = render(&listed)["configuration"]["selection"].clone();
        assert_eq!(selection["source"], "list");
        assert_eq!(selection["stride"], serde_json::Value::Null);
        assert_eq!(selection["limit"], serde_json::Value::Null);

        // Walking a directory, both knobs select, so both are identity.
        let corpus = |stride: usize, limit: Option<usize>| ProjectCandidateSettings {
            source: ProjectCandidateSource::Corpus(PathBuf::from("/corpus")),
            exhaustive: false,
            stride: NonZeroUsize::new(stride).expect("non-zero"),
            limit: limit.and_then(NonZeroUsize::new),
            max_files: None,
        };
        let walked = render(&corpus(13, None));
        assert_eq!(walked["configuration"]["selection"]["source"], "corpus");
        assert_eq!(walked["configuration"]["selection"]["stride"], 13);
        assert_ne!(walked, render(&corpus(1, None)));
        assert_ne!(walked, render(&corpus(13, Some(4))));

        // A list and a walk are never the same series, whatever the knobs.
        assert_ne!(render(&listed), walked);
    }

    /// Every leaf of `statistics` must be a number, on every run, whatever the
    /// input. A `null` is exactly as invisible to the dashboard as a missing
    /// key: it plots one metric per nested *number*, so either way the
    /// observation is skipped and the previous point still reads as "Latest" —
    /// a run that measured nothing masquerading as the last one that did.
    ///
    /// This walks the whole rendered tree rather than naming the fields it
    /// knows about, because the failure it guards against is precisely a field
    /// nobody thought to name: the sparse-map version of this bug was fixed
    /// one release earlier while two `Option` ratios went on serialising as
    /// `null`.
    #[test]
    fn no_statistic_is_ever_null_however_empty_the_run() {
        fn assert_all_numbers(value: &serde_json::Value, path: &str) {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, child) in fields {
                        assert_all_numbers(child, &format!("{path}.{key}"));
                    }
                }
                serde_json::Value::Number(_) => {}
                other => panic!(
                    "statistics{path} is {other}, not a number — the dashboard \
                     skips the observation and the previous value reads as latest"
                ),
            }
        }

        // The degenerate run: comparable, but with nothing to divide by, which
        // is what sends every ratio down its `None` branch.
        let mut empty = CorpusSummary::new(0);
        empty.record_comparison(&Comparison::default());
        assert_eq!(empty.coverage_basis_points(), None);
        assert_eq!(empty.skipped_projects_basis_points(), None);

        for summary in [&empty, &{
            let mut populated = CorpusSummary::new(1);
            populated.record_project_visited();
            populated.record_comparison(&Comparison {
                uses_considered: 3,
                matches: 1,
                ..Comparison::default()
            });
            populated
        }] {
            let json: serde_json::Value = serde_json::from_str(
                &render_generator_summary(summary, &generator_settings()).expect("render"),
            )
            .expect("valid JSON");
            assert_all_numbers(&json["statistics"], "");
        }
    }

    /// A metric that disappears when its count reaches zero reads, on the
    /// dashboard, as if it had never improved: absent values are filtered out,
    /// so the newest observation is skipped and an older nonzero point still
    /// shows as "Latest". Every variant of a closed enumeration is therefore
    /// emitted on every run, whether or not it occurred.
    #[test]
    fn every_asset_status_is_a_metric_even_at_zero() {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison::default());
        summary.record_project_assets(PathBuf::from("/A.fsproj"), ProjectAssetsStatus::NotChecked);

        let json: serde_json::Value = serde_json::from_str(
            &render_generator_summary(&summary, &generator_settings()).expect("render"),
        )
        .expect("valid JSON");
        let by_status = &json["statistics"]["project_assets_by_status"];

        for status in ProjectAssetsStatusKind::ALL {
            assert!(
                by_status[status.json_key()].is_number(),
                "{} is absent, so a run where it drops to zero would be skipped \
                 and the previous nonzero value would still read as the latest",
                status.json_key()
            );
        }
        assert_eq!(by_status["not_checked"], 1);
        assert_eq!(by_status["resolved"], 0);
        assert_eq!(
            by_status.as_object().expect("object").len(),
            ProjectAssetsStatusKind::ALL.len()
        );
    }

    /// The point of the series: deferrals are counted, never gated, so this is
    /// the only place a rise in them shows up.
    #[test]
    fn the_generator_statistics_carry_deferrals_and_the_counts_that_scale_them() {
        let mut summary = CorpusSummary::new(1);
        summary.record_project_visited();
        summary.record_comparison(&Comparison {
            deferrals: 11,
            assembly_deferrals: 5,
            matches: 3,
            assembly_matches: 1,
            uses_considered: 14,
            assembly_uses_considered: 6,
            ..Comparison::default()
        });

        let json: serde_json::Value = serde_json::from_str(
            &render_generator_summary(&summary, &generator_settings()).unwrap(),
        )
        .expect("valid JSON");
        let statistics = &json["statistics"];
        assert_eq!(statistics["deferrals"]["project"], 11);
        assert_eq!(statistics["deferrals"]["assembly"], 5);
        assert_eq!(statistics["deferrals"]["total"], 16);
        // A deferral count only means something against the population it was
        // drawn from, so the denominators travel with it.
        assert_eq!(statistics["uses"]["total_considered"], 20);
        assert_eq!(statistics["matches"]["total"], 4);
        assert_eq!(statistics["coverage"]["basis_points"], 2000);
        assert_eq!(statistics["projects"]["comparable"], 1);
    }
}
