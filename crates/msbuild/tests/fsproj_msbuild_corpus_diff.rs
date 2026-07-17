//! Broad MSBuild oracle over real `.fsproj` corpora.
//!
//! The small `fsproj_msbuild_diff` integration test pins a handful of known
//! F# compiler projects. This ignored runner is the wider empirical harness:
//! it recursively discovers real `.fsproj` files, evaluates each one through
//! this crate, asks `dotnet msbuild -getItem/-getProperty` for the same facts,
//! and fails on any unexcused divergence.
//!
//! Run a sampled sweep under `nix develop`:
//!
//! ```text
//! cargo test -p borzoi-msbuild --test fsproj_msbuild_corpus_diff \
//!   -- --ignored --nocapture
//! ```
//!
//! By default the runner uses `BORZOI_CORPUS` (or
//! `BORZOI_MSBUILD_CORPUS` when set), visits a deterministic sample, and
//! requires at least one project/facet comparison. Set
//! `BORZOI_MSBUILD_EXHAUSTIVE=1` to visit every discovered project, or
//! `BORZOI_MSBUILD_PROJECT_LIST` to pass an explicit platform-separated
//! project list. The project list takes precedence over the ambient
//! `BORZOI_CORPUS` fallback, and only conflicts with an explicit
//! `BORZOI_MSBUILD_CORPUS`. Explicit project lists are never sampled;
//! `BORZOI_MSBUILD_STRIDE` / `BORZOI_MSBUILD_LIMIT` apply only to
//! discovered corpora. `BORZOI_MSBUILD_REPORT_JSONL=/path/out.jsonl` writes
//! one summary record per visited project. The default ratchets are strict:
//! `BORZOI_MSBUILD_MAX_DIVERGENCES=0`,
//! `BORZOI_MSBUILD_MAX_ERRORS=0`, and
//! `BORZOI_MSBUILD_MIN_COMPARED_PROJECTS=1`.
//!
//! This test deliberately keeps skipped facets visible. A skipped facet is not
//! evidence of correctness: it means the parser reported uncertainty specific
//! to that output (`items_uncertain`, `package_references_uncertain`,
//! `define_constants_uncertain`) and the oracle comparison would be unfair. For
//! `DefineConstants`, the only matched mismatch is MSBuild having extra
//! well-known SDK-injected symbols (`DEBUG`, `TRACE`, and TFM/platform symbols)
//! after every parser-reported symbol has been accounted for.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::{BufWriter, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use borzoi_oracle_harness::BoundedCommand;

mod common;

use borzoi_msbuild::{
    Diagnostic, GlobalJson, ItemKind, PackageRefOp, PackageReference,
    PackageReferenceUncertaintyCause, PackageReferenceUncertaintyCauseKind, ParsedProject,
    SdkPathEntry, SdkResolution, SdkResolveError, SdkVersion,
    StructuralPackageReferenceUncertainty, VersionSpec, find_global_json, parse_fsproj,
    parse_fsproj_with_imports, parse_global_json, resolve_sdk, target_frameworks, workloads,
};
use serde::{Deserialize, Serialize};

#[test]
#[ignore = "real-corpus MSBuild oracle; run explicitly under nix develop"]
fn fsproj_msbuild_corpus_diff() {
    let config = Config::from_env().unwrap_or_else(|e| panic!("{e}"));
    let mut run = Run::new(config);
    run.execute().unwrap_or_else(|e| panic!("{e}"));
    run.assert_success();
}

#[derive(Debug)]
struct Config {
    projects: Vec<PathBuf>,
    max_divergences: usize,
    max_errors: usize,
    min_compared_projects: usize,
    report_jsonl: Option<PathBuf>,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let project_source = project_source_from_env(
            std::env::var_os("BORZOI_MSBUILD_PROJECT_LIST"),
            std::env::var_os("BORZOI_MSBUILD_CORPUS"),
            std::env::var_os("BORZOI_CORPUS"),
        )?;

        let sampling =
            sampling_from_env_for_source(&project_source, env_bool("BORZOI_MSBUILD_EXHAUSTIVE"))?;
        let projects = projects_from_source(project_source, sampling)?;

        Ok(Self {
            projects,
            max_divergences: env_usize("BORZOI_MSBUILD_MAX_DIVERGENCES")?.unwrap_or(0),
            max_errors: env_usize("BORZOI_MSBUILD_MAX_ERRORS")?.unwrap_or(0),
            min_compared_projects: env_usize("BORZOI_MSBUILD_MIN_COMPARED_PROJECTS")?.unwrap_or(1),
            report_jsonl: std::env::var_os("BORZOI_MSBUILD_REPORT_JSONL").map(PathBuf::from),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProjectSource {
    ProjectList(OsString),
    CorpusRoot(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sampling {
    stride: usize,
    limit: usize,
}

impl Sampling {
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            stride: env_usize("BORZOI_MSBUILD_STRIDE")?.unwrap_or(13).max(1),
            limit: env_usize("BORZOI_MSBUILD_LIMIT")?.unwrap_or(20),
        })
    }
}

fn projects_from_source(
    project_source: ProjectSource,
    sampling: Option<Sampling>,
) -> Result<Vec<PathBuf>, String> {
    let mut projects = match project_source {
        ProjectSource::ProjectList(list) => split_project_list(&list),
        ProjectSource::CorpusRoot(root) => {
            if !root.is_dir() {
                return Err(format!("corpus root {} is not a directory", root.display()));
            }
            collect_fsprojs(&root)
        }
    };
    projects.sort();
    projects.dedup();

    if projects.is_empty() {
        return Err("no .fsproj files found".to_string());
    }

    if let Some(sampling) = sampling {
        projects = sample_projects(projects, sampling);
    }

    Ok(projects)
}

fn sampling_from_env_for_source(
    project_source: &ProjectSource,
    exhaustive: bool,
) -> Result<Option<Sampling>, String> {
    if should_sample_projects(project_source, exhaustive) {
        Sampling::from_env().map(Some)
    } else {
        Ok(None)
    }
}

fn should_sample_projects(project_source: &ProjectSource, exhaustive: bool) -> bool {
    !exhaustive && matches!(project_source, ProjectSource::CorpusRoot(_))
}

fn sample_projects(projects: Vec<PathBuf>, sampling: Sampling) -> Vec<PathBuf> {
    projects
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i % sampling.stride == 0)
        .map(|(_, p)| p)
        .take(sampling.limit)
        .collect()
}

fn project_source_from_env(
    project_list: Option<OsString>,
    explicit_msbuild_corpus: Option<OsString>,
    fallback_corpus: Option<OsString>,
) -> Result<ProjectSource, String> {
    if project_list.is_some() && explicit_msbuild_corpus.is_some() {
        return Err(
            "set only one of BORZOI_MSBUILD_PROJECT_LIST and BORZOI_MSBUILD_CORPUS".to_string(),
        );
    }

    if let Some(list) = project_list {
        return Ok(ProjectSource::ProjectList(list));
    }

    explicit_msbuild_corpus
        .or(fallback_corpus)
        .map(PathBuf::from)
        .map(ProjectSource::CorpusRoot)
        .ok_or_else(|| {
            "set BORZOI_MSBUILD_CORPUS, BORZOI_CORPUS, or \
             BORZOI_MSBUILD_PROJECT_LIST"
                .to_string()
        })
}

struct Run {
    config: Config,
    reports: Vec<ProjectReport>,
}

impl Run {
    fn new(config: Config) -> Self {
        Self {
            config,
            reports: Vec::new(),
        }
    }

    fn execute(&mut self) -> Result<(), String> {
        let mut writer = match &self.config.report_jsonl {
            Some(path) => {
                Some(BufWriter::new(fs::File::create(path).map_err(|e| {
                    format!("create report {}: {e}", path.display())
                })?))
            }
            None => None,
        };

        for project in &self.config.projects {
            let report = compare_project(project);
            print_report(&report);
            if let Some(w) = writer.as_mut() {
                serde_json::to_writer(&mut *w, &report)
                    .map_err(|e| format!("serialize report for {}: {e}", project.display()))?;
                w.write_all(b"\n")
                    .map_err(|e| format!("write report for {}: {e}", project.display()))?;
            }
            self.reports.push(report);
        }

        if let Some(mut w) = writer {
            w.flush().map_err(|e| format!("flush report: {e}"))?;
        }

        Ok(())
    }

    fn assert_success(&self) {
        let mut divergences = 0usize;
        let mut compared_projects = 0usize;
        let mut matched_facets = 0usize;
        let mut skipped_facets = 0usize;
        let mut error_projects = 0usize;

        for report in &self.reports {
            if report.status == ProjectStatus::Error {
                error_projects += 1;
            }
            if report.facets.iter().any(|f| f.status.counts_as_compared()) {
                compared_projects += 1;
            }
            for facet in &report.facets {
                match facet.status {
                    FacetStatus::Matched => matched_facets += 1,
                    FacetStatus::Skipped => skipped_facets += 1,
                    FacetStatus::Diverged => divergences += 1,
                }
            }
        }

        eprintln!(
            "msbuild corpus diff: visited={} compared_projects={} matched_facets={} \
             skipped_facets={} divergences={} error_projects={}",
            self.reports.len(),
            compared_projects,
            matched_facets,
            skipped_facets,
            divergences,
            error_projects,
        );

        assert!(
            compared_projects >= self.config.min_compared_projects,
            "msbuild corpus diff compared only {} project(s), below minimum {}",
            compared_projects,
            self.config.min_compared_projects
        );
        assert!(
            divergences <= self.config.max_divergences,
            "msbuild corpus diff found {} divergence(s), above maximum {}",
            divergences,
            self.config.max_divergences
        );
        assert!(
            error_projects <= self.config.max_errors,
            "msbuild corpus diff hit {} project error(s), above maximum {}",
            error_projects,
            self.config.max_errors
        );
    }
}

fn compare_project(project: &Path) -> ProjectReport {
    let display = project.display().to_string();
    let fsproj = match fs::canonicalize(project) {
        Ok(p) => p,
        Err(e) => {
            return ProjectReport::error(display, Stage::Setup, format!("canonicalize: {e}"));
        }
    };
    let source = match fs::read_to_string(&fsproj) {
        Ok(s) => s,
        Err(e) => return ProjectReport::error(display, Stage::Setup, format!("read: {e}")),
    };

    let sdk = match SdkContext::for_project(&fsproj) {
        Ok(sdk) => sdk,
        Err(e) => return ProjectReport::error(display, Stage::Setup, e),
    };
    let resolver = |name: &str| sdk.resolve(name);
    let mut extra_properties = HashMap::new();
    extra_properties.insert("DISABLE_ARCADE".to_string(), "true".to_string());
    let parsed = match parse_fsproj_with_imports(
        &source,
        &fsproj,
        &extra_properties,
        &common::oracle_environment(),
        Some(&resolver),
        None,
    ) {
        Ok(parsed) => parsed,
        Err(e) => return ProjectReport::error(display, Stage::Parse, e.to_string()),
    };

    let msbuild = match run_msbuild(&fsproj) {
        Ok(msbuild) => msbuild,
        Err(e) => return ProjectReport::error(display, Stage::Oracle, e),
    };

    let facets = vec![
        compare_compile(&parsed, &msbuild),
        compare_project_references(&parsed, &msbuild),
        compare_package_references(&parsed, &msbuild),
        compare_framework_references(&parsed, &msbuild),
        compare_target_frameworks(&parsed, &msbuild),
        compare_define_constants(&parsed, &msbuild),
        compare_lang_version(&parsed, &msbuild),
    ];

    let status = if facets.iter().any(|f| f.status == FacetStatus::Diverged) {
        ProjectStatus::Diverged
    } else if facets.iter().any(|f| f.status == FacetStatus::Matched) {
        ProjectStatus::Compared
    } else {
        ProjectStatus::Skipped
    };

    ProjectReport {
        project: display,
        status,
        stage: None,
        error: None,
        facets,
        diagnostics: parsed
            .diagnostics
            .iter()
            .take(20)
            .map(render_diagnostic)
            .collect(),
        omitted_diagnostics: parsed.diagnostics.len().saturating_sub(20),
    }
}

/// Budget for one `dotnet msbuild` evaluation. A cold one restores packages and
/// walks the whole SDK import chain, which is legitimately minutes, so the bound
/// is far above the harness's per-request default: it is there to stop an
/// evaluation that has *stalled* — blocked on a NuGet lock held by a concurrent
/// run in a sibling worktree, say — from hanging the sweep forever, not to police
/// a slow one. A blown budget is reported like any other MSBuild failure: the
/// corpus sweep records it against that project and moves on.
const MSBUILD_TIMEOUT: Duration = Duration::from_secs(1800);

fn run_msbuild(fsproj: &Path) -> Result<MsbuildOutput, String> {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(
        fsproj
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", fsproj.display()))?,
    );
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
    cmd.args([
        "msbuild",
        "-nologo",
        "-getItem:Compile,CompileBefore,CompileAfter,ProjectReference,PackageReference,FrameworkReference",
        "-getProperty:DefineConstants,LangVersion,TargetFrameworks,TargetFramework",
        "-p:DISABLE_ARCADE=true",
    ]);
    cmd.arg(fsproj);

    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run()
        .map_err(|e| format!("dotnet msbuild: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "dotnet msbuild exited with {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    let stdout =
        String::from_utf8(out.stdout).map_err(|e| format!("msbuild stdout not UTF-8: {e}"))?;
    serde_json::from_str(&stdout).map_err(|e| format!("parse msbuild JSON: {e}\n{stdout}"))
}

fn compare_compile(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    if parsed.items_uncertain {
        return FacetReport::skipped(
            "compile",
            format!("items_uncertain=true; {}", uncertainty_summary(parsed)),
        );
    }
    let ours: Vec<CompileView> = parsed
        .items
        .iter()
        .filter(|item| is_compile_kind(item.kind))
        .map(|item| CompileView {
            kind: item_kind_name(item.kind).to_string(),
            path: path_key(&item.include),
            link: normalize_link(item.link.as_deref().unwrap_or("")),
        })
        .collect();
    let theirs = msbuild.compile_views();
    compare_vec("compile", ours, theirs)
}

fn compare_project_references(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    let ours: Vec<String> = parsed
        .project_references
        .iter()
        .map(|item| path_key(&item.include))
        .collect();
    let theirs: Vec<String> = msbuild
        .items
        .project_reference
        .iter()
        .map(|item| path_key(Path::new(&item.full_path)))
        .collect();
    compare_vec("project_references", ours, theirs)
}

fn compare_package_references(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    if parsed.package_references_uncertain {
        return FacetReport::skipped(
            "package_references",
            format!(
                "package_references_uncertain=true; {}",
                package_uncertainty_summary(parsed)
            ),
        );
    }
    if parsed
        .package_references
        .iter()
        .any(|p| p.op != PackageRefOp::Include)
    {
        return FacetReport::skipped(
            "package_references",
            "PackageReference Update items are captured separately by this crate, \
             while MSBuild's item view folds them into existing items"
                .to_string(),
        );
    }
    let ours: Vec<PackageView> = parsed
        .package_references
        .iter()
        .map(package_view_from_ours)
        .collect();
    let theirs: Vec<PackageView> = msbuild
        .items
        .package_reference
        .iter()
        .map(package_view_from_msbuild)
        .collect();
    compare_vec("package_references", ours, theirs)
}

fn compare_framework_references(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    if parsed.package_references_uncertain {
        return FacetReport::skipped(
            "framework_references",
            format!(
                "package_references_uncertain=true; {}",
                package_uncertainty_summary(parsed)
            ),
        );
    }
    let ours: Vec<String> = parsed
        .framework_references
        .iter()
        .map(|f| f.name.clone())
        .collect();
    let theirs: Vec<String> = msbuild
        .items
        .framework_reference
        .iter()
        .map(|item| item.identity.clone())
        .collect();
    compare_vec("framework_references", ours, theirs)
}

fn compare_target_frameworks(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    compare_vec(
        "target_frameworks",
        target_frameworks(parsed),
        msbuild.properties.declared_tfms(),
    )
}

fn compare_define_constants(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    if parsed.define_constants_uncertain {
        return FacetReport::skipped(
            "define_constants",
            format!(
                "define_constants_uncertain=true; {}",
                diagnostics_summary(parsed)
            ),
        );
    }
    let ours = parsed.define_constants.clone();
    let theirs = split_msbuild_list(&msbuild.properties.define_constants);
    if ours == theirs {
        return FacetReport::matched("define_constants", format!("{} symbol(s)", ours.len()));
    }
    if let Some(extra) = sdk_injected_define_constants_extra(&ours, &theirs) {
        return FacetReport::matched(
            "define_constants",
            format!(
                "MSBuild has {} SDK-injected extra symbol(s): {extra:?}",
                extra.len(),
            ),
        );
    }
    FacetReport::diverged(
        "define_constants",
        format!("ours={ours:?}; msbuild={theirs:?}"),
    )
}

fn compare_lang_version(parsed: &ParsedProject, msbuild: &MsbuildOutput) -> FacetReport {
    let ours = parsed.lang_version.clone().unwrap_or_default();
    let theirs = msbuild.properties.lang_version.trim().to_string();
    if ours.is_empty() && theirs.is_empty() {
        return FacetReport::skipped("lang_version", "unset on both sides".to_string());
    }
    compare_scalar("lang_version", ours, theirs)
}

fn compare_vec<T>(facet: &'static str, ours: Vec<T>, theirs: Vec<T>) -> FacetReport
where
    T: std::fmt::Debug + PartialEq,
{
    if ours == theirs {
        FacetReport::matched(facet, format!("{} entrie(s)", ours.len()))
    } else {
        FacetReport::diverged(
            facet,
            format!(
                "ours_len={} msbuild_len={}; {}",
                ours.len(),
                theirs.len(),
                first_difference(&ours, &theirs),
            ),
        )
    }
}

fn compare_scalar(facet: &'static str, ours: String, theirs: String) -> FacetReport {
    if ours == theirs {
        FacetReport::matched(facet, format!("{ours:?}"))
    } else {
        FacetReport::diverged(facet, format!("ours={ours:?}; msbuild={theirs:?}"))
    }
}

fn first_difference<T>(ours: &[T], theirs: &[T]) -> String
where
    T: std::fmt::Debug + PartialEq,
{
    let limit = ours.len().min(theirs.len());
    for i in 0..limit {
        if ours[i] != theirs[i] {
            return format!(
                "first difference at [{i}]: ours={:?}; msbuild={:?}",
                ours[i], theirs[i]
            );
        }
    }
    if ours.len() > theirs.len() {
        format!(
            "first extra ours[{}]={:?}",
            theirs.len(),
            ours[theirs.len()]
        )
    } else if theirs.len() > ours.len() {
        format!(
            "first extra msbuild[{}]={:?}",
            ours.len(),
            theirs[ours.len()]
        )
    } else {
        "different values".to_string()
    }
}

fn package_view_from_ours(pr: &PackageReference) -> PackageView {
    PackageView {
        id: pr.id.clone(),
        version: pr.version.clone(),
        version_override: pr.version_override.clone(),
        include_assets: pr.include_assets.clone(),
        exclude_assets: pr.exclude_assets.clone(),
        private_assets: pr.private_assets.clone(),
    }
}

fn package_view_from_msbuild(item: &MsbuildItem) -> PackageView {
    let field = |name: &str| item.metadata.get(name).filter(|v| !v.is_empty()).cloned();
    PackageView {
        id: item.identity.clone(),
        version: field("Version"),
        version_override: field("VersionOverride"),
        include_assets: field("IncludeAssets"),
        exclude_assets: field("ExcludeAssets"),
        private_assets: field("PrivateAssets"),
    }
}

#[derive(Debug)]
struct SdkContext {
    roots: Vec<PathBuf>,
    nuget_packages_dir: Option<PathBuf>,
    spec: VersionSpec,
    msbuild_sdks: BTreeMap<String, SdkVersion>,
    /// Whether the project's global.json engages workload-set
    /// selection — the corpus's real `dotnet msbuild` comparisons see
    /// the same file, so the locators must degrade in step.
    global_json_pins_workload_set: bool,
}

impl SdkContext {
    fn for_project(project: &Path) -> Result<Self, String> {
        let project_dir = project
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", project.display()))?;
        let dotnet_root = std::env::var_os("DOTNET_ROOT").map(PathBuf::from);
        let nuget_packages_dir = resolve_nuget_packages_dir();

        let Some(global_json_path) = find_global_json(project_dir) else {
            let dotnet_root = dotnet_root
                .ok_or_else(|| "DOTNET_ROOT is not set; run under nix develop".to_string())?;
            return Ok(Self {
                roots: vec![dotnet_root],
                nuget_packages_dir,
                spec: VersionSpec::any_version(true),
                msbuild_sdks: BTreeMap::new(),
                global_json_pins_workload_set: false,
            });
        };

        let text = fs::read_to_string(&global_json_path)
            .map_err(|e| format!("read {}: {e}", global_json_path.display()))?;
        let parsed = parse_global_json(&text)
            .map_err(|e| format!("parse {}: {e}", global_json_path.display()))?;
        let GlobalJson {
            sdk,
            msbuild_sdks,
            pins_workload_set,
        } = parsed;
        let (paths, spec) = match sdk {
            Some(mut sdk) => {
                let paths = sdk.paths.take();
                (paths, sdk.into_spec(true))
            }
            None => (None, VersionSpec::any_version(true)),
        };
        let roots = expand_sdk_paths(dotnet_root, &global_json_path, paths)?;

        Ok(Self {
            roots,
            nuget_packages_dir,
            spec,
            msbuild_sdks,
            global_json_pins_workload_set: pins_workload_set,
        })
    }

    fn resolve(&self, sdk_name: &str) -> Result<SdkResolution, SdkResolveError> {
        // Same workload context the corpus's real `dotnet msbuild`
        // comparisons run under (the test process environment).
        // Empty home-ish values count as unset (`string.IsNullOrEmpty`
        // in .NET's CliFolderPathCalculatorCore).
        let non_empty = |var: &str| std::env::var_os(var).filter(|value| !value.is_empty());
        let user_dotnet_root = non_empty("DOTNET_CLI_HOME")
            .or_else(|| non_empty("HOME"))
            .map(|home| PathBuf::from(home).join(".dotnet"));
        // Per-variable effective-value semantics, mirroring the LSP's
        // `SdkDiscoveryEnv::from_process_env` (PACK_ROOTS goes through
        // IsNullOrEmpty upstream; the other two are presence checks).
        let overrides_present = std::env::var_os("DOTNETSDK_WORKLOAD_MANIFEST_ROOTS").is_some()
            || std::env::var_os("DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS").is_some()
            || non_empty("DOTNETSDK_WORKLOAD_PACK_ROOTS").is_some();
        let workload_env = workloads::WorkloadEnvironment {
            user_dotnet_root: user_dotnet_root.as_deref(),
            overrides_present,
            global_json_pins_workload_set: self.global_json_pins_workload_set,
        };
        let mut version_not_satisfied: Option<SdkResolveError> = None;
        for root in &self.roots {
            match resolve_sdk(
                root,
                self.nuget_packages_dir.as_deref(),
                sdk_name,
                Some(&self.spec),
                Some(&self.msbuild_sdks),
                &workload_env,
            ) {
                Ok(resolution) => return Ok(resolution),
                Err(e @ SdkResolveError::VersionNotSatisfied { .. }) => {
                    version_not_satisfied = Some(e);
                }
                Err(SdkResolveError::NotFound) => {}
                // The root MSBuild would use has workload state outside
                // the exactness envelope; degrade rather than fall
                // through to a lower-priority root.
                Err(e @ SdkResolveError::UnsupportedLayout { .. }) => return Err(e),
            }
        }
        Err(version_not_satisfied.unwrap_or(SdkResolveError::NotFound))
    }
}

fn expand_sdk_paths(
    dotnet_root: Option<PathBuf>,
    global_json_path: &Path,
    paths: Option<Vec<SdkPathEntry>>,
) -> Result<Vec<PathBuf>, String> {
    let Some(entries) = paths else {
        return dotnet_root
            .map(|p| vec![p])
            .ok_or_else(|| "DOTNET_ROOT is not set; run under nix develop".to_string());
    };
    let global_json_dir = global_json_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", global_json_path.display()))?;
    let roots: Vec<PathBuf> = entries
        .into_iter()
        .filter_map(|entry| match entry {
            SdkPathEntry::Host => dotnet_root.clone(),
            SdkPathEntry::Relative(path) => Some(global_json_dir.join(path)),
        })
        .collect();
    Ok(roots)
}

fn resolve_nuget_packages_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("NUGET_PACKAGES") {
        return Some(PathBuf::from(dir));
    }
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    };
    home.map(|h| PathBuf::from(h).join(".nuget").join("packages"))
}

fn split_project_list(list: &OsString) -> Vec<PathBuf> {
    std::env::split_paths(list).collect()
}

fn collect_fsprojs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_fsprojs_into(root, &mut out);
    out
}

fn collect_fsprojs_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if should_skip_dir(&name) {
                continue;
            }
            collect_fsprojs_into(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("fsproj") {
            out.push(path);
        }
    }
}

fn should_skip_dir(name: &str) -> bool {
    matches!(name, ".git" | "target" | "artifacts" | "bin" | "obj")
}

fn env_bool(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| {
        let v = v.to_string_lossy();
        v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
    })
}

fn env_usize(name: &str) -> Result<Option<usize>, String> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    value
        .parse::<usize>()
        .map(Some)
        .map_err(|e| format!("{name}={value:?} is not a usize: {e}"))
}

fn split_msbuild_list(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn sdk_injected_define_constants_extra(ours: &[String], theirs: &[String]) -> Option<Vec<String>> {
    let mut remaining = theirs.to_vec();
    for item in ours {
        let pos = remaining.iter().position(|candidate| candidate == item)?;
        remaining.remove(pos);
    }
    remaining
        .iter()
        .all(|symbol| is_known_sdk_injected_define_constant(symbol))
        .then_some(remaining)
}

fn is_known_sdk_injected_define_constant(symbol: &str) -> bool {
    matches!(symbol, "DEBUG" | "TRACE") || is_known_tfm_or_platform_define_constant(symbol)
}

fn is_known_tfm_or_platform_define_constant(symbol: &str) -> bool {
    let base = symbol.strip_suffix("_OR_GREATER").unwrap_or(symbol);
    is_framework_define_base(base)
        || is_platform_define_base(base)
        || is_framework_platform_define_base(base)
}

fn is_framework_platform_define_base(symbol: &str) -> bool {
    TARGET_PLATFORM_DEFINE_PREFIXES.iter().any(|platform| {
        let needle = format!("_{platform}");
        let Some(index) = symbol.find(&needle) else {
            return false;
        };
        let framework = &symbol[..index];
        let platform = &symbol[index + 1..];
        is_framework_define_base(framework) && is_platform_define_base(platform)
    })
}

fn is_framework_define_base(symbol: &str) -> bool {
    matches!(
        symbol,
        "NET" | "NETCOREAPP" | "NETSTANDARD" | "NETFRAMEWORK"
    ) || has_version_after_prefix(symbol, "NET")
        || has_version_after_prefix(symbol, "NETCOREAPP")
        || has_version_after_prefix(symbol, "NETSTANDARD")
        || has_version_after_prefix(symbol, "NETFRAMEWORK")
}

fn is_platform_define_base(symbol: &str) -> bool {
    TARGET_PLATFORM_DEFINE_PREFIXES.iter().any(|prefix| {
        symbol == *prefix
            || symbol
                .strip_prefix(prefix)
                .is_some_and(is_define_version_suffix)
    })
}

fn has_version_after_prefix(symbol: &str, prefix: &str) -> bool {
    symbol
        .strip_prefix(prefix)
        .is_some_and(is_define_version_suffix)
}

fn is_define_version_suffix(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix
            .split('_')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

const TARGET_PLATFORM_DEFINE_PREFIXES: &[&str] = &[
    "ANDROID",
    "BROWSER",
    "IOS",
    "MACCATALYST",
    "MACOS",
    "TVOS",
    "WINDOWS",
];

fn is_compile_kind(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Compile | ItemKind::CompileBefore | ItemKind::CompileAfter
    )
}

fn item_kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Compile => "Compile",
        ItemKind::CompileBefore => "CompileBefore",
        ItemKind::CompileAfter => "CompileAfter",
        ItemKind::ProjectReference => "ProjectReference",
    }
}

fn path_key(path: &Path) -> String {
    let path = fs::canonicalize(path).unwrap_or_else(|_| normalize_lexical(path));
    to_forward_slashes(&path)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

fn normalize_link(link: &str) -> String {
    link.replace('\\', "/")
}

fn to_forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn uncertainty_summary(parsed: &ParsedProject) -> String {
    if let Some(first) = parsed.compile_condition_uncertainties.first() {
        return format!(
            "first compile condition uncertainty: condition={:?}, reason={:?}",
            first.condition, first.reason
        );
    }
    diagnostics_summary(parsed)
}

fn diagnostics_summary(parsed: &ParsedProject) -> String {
    let rendered: Vec<String> = parsed
        .diagnostics
        .iter()
        .take(3)
        .map(render_diagnostic)
        .collect();
    if rendered.is_empty() {
        "no diagnostics captured".to_string()
    } else {
        format!(
            "first diagnostic(s): {}; omitted={}",
            rendered.join(" | "),
            parsed.diagnostics.len().saturating_sub(rendered.len())
        )
    }
}

fn package_uncertainty_summary(parsed: &ParsedProject) -> String {
    let rendered: Vec<String> = parsed
        .package_reference_uncertainties
        .iter()
        .take(3)
        .map(render_package_reference_uncertainty_cause)
        .collect();
    if rendered.is_empty() {
        return diagnostics_summary(parsed);
    }
    format!(
        "first package uncertainty cause(s): {}; omitted={}",
        rendered.join(" | "),
        parsed
            .package_reference_uncertainties
            .len()
            .saturating_sub(rendered.len())
    )
}

fn render_package_reference_uncertainty_cause(cause: &PackageReferenceUncertaintyCause) -> String {
    let message = match &cause.kind {
        PackageReferenceUncertaintyCauseKind::Diagnostic(kind) => {
            format!("{kind:?}")
        }
        PackageReferenceUncertaintyCauseKind::Structural(kind) => {
            structural_package_reference_uncertainty_message(kind)
        }
        PackageReferenceUncertaintyCauseKind::DirectoryPackagesProps { path } => {
            format!(
                "{} is not folded into package reference capture",
                path.display()
            )
        }
        PackageReferenceUncertaintyCauseKind::ManagePackageVersionsCentrally => {
            "ManagePackageVersionsCentrally=true; central package versions are not folded in"
                .to_string()
        }
        PackageReferenceUncertaintyCauseKind::PackageVersion => {
            "<PackageVersion> central package metadata is not folded in".to_string()
        }
        PackageReferenceUncertaintyCauseKind::GlobalPackageReference => {
            "<GlobalPackageReference> implicit package is not folded in".to_string()
        }
        PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault => {
            "<ItemDefinitionGroup> package defaults are not applied".to_string()
        }
        PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation => {
            "a package decision read a property whose write could not be pinned down".to_string()
        }
        PackageReferenceUncertaintyCauseKind::UnsupportedItemOperation { item, operation } => {
            format!("<{item} {operation}=...> can change the dependency item set")
        }
        PackageReferenceUncertaintyCauseKind::UnevaluableIdentity { value } => {
            format!("dependency identity {value:?} could not be reduced to literal items")
        }
        PackageReferenceUncertaintyCauseKind::UnsupportedGlob { pattern } => {
            format!("dependency identity glob {pattern:?} is not expanded")
        }
        PackageReferenceUncertaintyCauseKind::UnsupportedExclude { value } => {
            format!("Exclude={value:?} could not be reduced to literal identities")
        }
        PackageReferenceUncertaintyCauseKind::UnevaluableMetadata { name, value } => {
            format!("metadata {name}={value:?} could not be evaluated")
        }
        PackageReferenceUncertaintyCauseKind::VersionlessPackageReference { id } => {
            format!("<PackageReference Include={id:?}> has no local version")
        }
        PackageReferenceUncertaintyCauseKind::DuplicateUpdateIdentity { id } => {
            format!(
                "<PackageReference Update> names {id:?} more than once; MSBuild applies such \
                 an update position-independently"
            )
        }
    };
    format!("{:?} {message}", cause.origin)
}

fn structural_package_reference_uncertainty_message(
    kind: &StructuralPackageReferenceUncertainty,
) -> String {
    match kind {
        StructuralPackageReferenceUncertainty::ProjectSdkUnsupported { sdk } => {
            format!("project SDK '{sdk}' was not evaluated and may hide dependency items")
        }
        StructuralPackageReferenceUncertainty::ExplicitSdkUnsupported { sdk } => {
            format!("explicit SDK import '{sdk}' was not evaluated and may hide dependency items")
        }
        StructuralPackageReferenceUncertainty::SdkImportProjectUnresolved { sdk, project } => {
            format!(
                "dropped SDK import '{sdk}' Project=\"{project}\" because the Project path could not be resolved"
            )
        }
        StructuralPackageReferenceUncertainty::SdkImportProjectRejected { sdk, project } => {
            format!(
                "rejected SDK import '{sdk}' Project=\"{project}\" because it is not a safe SDK-relative path"
            )
        }
        StructuralPackageReferenceUncertainty::ImportProjectUnresolved { project } => {
            format!(
                "dropped <Import Project=\"{project}\"> because the Project path could not be resolved"
            )
        }
        StructuralPackageReferenceUncertainty::UnsupportedChoose => {
            "unsupported <Choose> may hide dependency items".to_string()
        }
    }
}

fn render_diagnostic(diagnostic: &Diagnostic) -> String {
    format!("{:?} @ {:?}", diagnostic.kind, diagnostic.origin)
}

fn print_report(report: &ProjectReport) {
    match report.status {
        ProjectStatus::Compared | ProjectStatus::Skipped | ProjectStatus::Diverged => {
            let divergent = report
                .facets
                .iter()
                .filter(|f| f.status == FacetStatus::Diverged)
                .count();
            let skipped = report
                .facets
                .iter()
                .filter(|f| f.status == FacetStatus::Skipped)
                .count();
            eprintln!(
                "{}: {:?} (divergent_facets={}, skipped_facets={})",
                report.project, report.status, divergent, skipped
            );
            for facet in &report.facets {
                if facet.status != FacetStatus::Matched {
                    eprintln!("  {:?} {}: {}", facet.status, facet.name, facet.detail);
                }
            }
        }
        ProjectStatus::Error => {
            eprintln!(
                "{}: error at {:?}: {}",
                report.project,
                report.stage,
                report.error.as_deref().unwrap_or("<missing error>")
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct MsbuildOutput {
    #[serde(default, rename = "Items")]
    items: MsbuildItems,
    #[serde(default, rename = "Properties")]
    properties: MsbuildProperties,
}

impl MsbuildOutput {
    fn compile_views(&self) -> Vec<CompileView> {
        let mut out = Vec::new();
        out.extend(
            self.items
                .compile_before
                .iter()
                .map(|item| item.compile_view("CompileBefore")),
        );
        out.extend(
            self.items
                .compile
                .iter()
                .map(|item| item.compile_view("Compile")),
        );
        out.extend(
            self.items
                .compile_after
                .iter()
                .map(|item| item.compile_view("CompileAfter")),
        );
        out
    }
}

#[derive(Debug, Default, Deserialize)]
struct MsbuildItems {
    #[serde(default, rename = "Compile")]
    compile: Vec<MsbuildItem>,
    #[serde(default, rename = "CompileBefore")]
    compile_before: Vec<MsbuildItem>,
    #[serde(default, rename = "CompileAfter")]
    compile_after: Vec<MsbuildItem>,
    #[serde(default, rename = "ProjectReference")]
    project_reference: Vec<MsbuildItem>,
    #[serde(default, rename = "PackageReference")]
    package_reference: Vec<MsbuildItem>,
    #[serde(default, rename = "FrameworkReference")]
    framework_reference: Vec<MsbuildItem>,
}

#[derive(Debug, Default, Deserialize)]
struct MsbuildProperties {
    #[serde(default, rename = "DefineConstants")]
    define_constants: String,
    #[serde(default, rename = "LangVersion")]
    lang_version: String,
    #[serde(default, rename = "TargetFrameworks")]
    target_frameworks: String,
    #[serde(default, rename = "TargetFramework")]
    target_framework: String,
}

impl MsbuildProperties {
    fn declared_tfms(&self) -> Vec<String> {
        let plural = split_msbuild_list(&self.target_frameworks);
        if !plural.is_empty() {
            return plural;
        }
        let singular = self.target_framework.trim();
        if singular.is_empty() {
            Vec::new()
        } else {
            vec![singular.to_string()]
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct MsbuildItem {
    #[serde(default, rename = "FullPath")]
    full_path: String,
    #[serde(default, rename = "Identity")]
    identity: String,
    #[serde(default, rename = "Link")]
    link: String,
    #[serde(flatten)]
    metadata: BTreeMap<String, String>,
}

impl MsbuildItem {
    fn compile_view(&self, kind: &str) -> CompileView {
        CompileView {
            kind: kind.to_string(),
            path: path_key(Path::new(&self.full_path)),
            link: normalize_link(&self.link),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct CompileView {
    kind: String,
    path: String,
    link: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct PackageView {
    id: String,
    version: Option<String>,
    version_override: Option<String>,
    include_assets: Option<String>,
    exclude_assets: Option<String>,
    private_assets: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProjectReport {
    project: String,
    status: ProjectStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<Stage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    facets: Vec<FacetReport>,
    diagnostics: Vec<String>,
    omitted_diagnostics: usize,
}

impl ProjectReport {
    fn error(project: String, stage: Stage, error: String) -> Self {
        Self {
            project,
            status: ProjectStatus::Error,
            stage: Some(stage),
            error: Some(error),
            facets: Vec::new(),
            diagnostics: Vec::new(),
            omitted_diagnostics: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProjectStatus {
    Compared,
    Skipped,
    Diverged,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Setup,
    Parse,
    Oracle,
}

#[derive(Debug, Serialize)]
struct FacetReport {
    name: &'static str,
    status: FacetStatus,
    detail: String,
}

impl FacetReport {
    fn matched(name: &'static str, detail: String) -> Self {
        Self {
            name,
            status: FacetStatus::Matched,
            detail,
        }
    }

    fn skipped(name: &'static str, detail: String) -> Self {
        Self {
            name,
            status: FacetStatus::Skipped,
            detail,
        }
    }

    fn diverged(name: &'static str, detail: String) -> Self {
        Self {
            name,
            status: FacetStatus::Diverged,
            detail,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FacetStatus {
    Matched,
    Skipped,
    Diverged,
}

impl FacetStatus {
    fn counts_as_compared(self) -> bool {
        matches!(self, Self::Matched | Self::Diverged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalisation_collapses_parent_components() {
        assert_eq!(
            normalize_lexical(Path::new("/repo/a/../b/./C.fs")),
            PathBuf::from("/repo/b/C.fs")
        );
    }

    #[test]
    fn sdk_define_extra_consumes_ours_before_filtering_known_symbols() {
        assert_eq!(
            sdk_injected_define_constants_extra(
                &["NET6_0".to_string()],
                &["NET6_0".to_string(), "NET6_0_OR_GREATER".to_string()],
            ),
            Some(vec!["NET6_0_OR_GREATER".to_string()])
        );
        assert_eq!(
            sdk_injected_define_constants_extra(
                &["NET6_0".to_string(), "NET6_0".to_string()],
                &["NET6_0".to_string(), "NET6_0_OR_GREATER".to_string()],
            ),
            None
        );
    }

    #[test]
    fn split_msbuild_list_trims_and_drops_empty_entries() {
        assert_eq!(
            split_msbuild_list(" A ; ;B; "),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn define_constants_subset_does_not_match_unknown_msbuild_extra() {
        let parsed = parsed_with_define_constants(&["PROJECT"]);
        let msbuild = msbuild_with_define_constants(&["PROJECT", "DROPPED"]);

        let report = compare_define_constants(&parsed, &msbuild);

        assert_eq!(report.status, FacetStatus::Diverged);
    }

    #[test]
    fn define_constants_match_with_sdk_injected_msbuild_extras() {
        let parsed = parsed_with_define_constants(&["PROJECT"]);
        let msbuild = msbuild_with_define_constants(&[
            "PROJECT",
            "DEBUG",
            "TRACE",
            "NET",
            "NET10_0",
            "NET10_0_OR_GREATER",
            "NETCOREAPP",
            "NETCOREAPP3_1_OR_GREATER",
            "WINDOWS",
            "WINDOWS7_0_OR_GREATER",
        ]);

        let report = compare_define_constants(&parsed, &msbuild);

        assert_eq!(report.status, FacetStatus::Matched);
    }

    fn parsed_with_define_constants(symbols: &[&str]) -> ParsedProject {
        let source = format!(
            "<Project><PropertyGroup><DefineConstants>{}</DefineConstants></PropertyGroup></Project>",
            symbols.join(";")
        );
        parse_fsproj(
            &source,
            Path::new("/repo/proj/Demo.fsproj"),
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("well-formed XML parses")
    }

    fn msbuild_with_define_constants(symbols: &[&str]) -> MsbuildOutput {
        MsbuildOutput {
            items: MsbuildItems::default(),
            properties: MsbuildProperties {
                define_constants: symbols.join(";"),
                ..MsbuildProperties::default()
            },
        }
    }

    #[test]
    fn project_list_overrides_the_fallback_corpus() {
        assert_eq!(
            project_source_from_env(
                Some(OsString::from("/tmp/one.fsproj")),
                None,
                Some(OsString::from("/tmp/corpus")),
            )
            .unwrap(),
            ProjectSource::ProjectList(OsString::from("/tmp/one.fsproj")),
        );
    }

    #[test]
    fn project_list_conflicts_with_an_explicit_msbuild_corpus() {
        let error = project_source_from_env(
            Some(OsString::from("/tmp/one.fsproj")),
            Some(OsString::from("/tmp/msbuild-corpus")),
            Some(OsString::from("/tmp/fallback-corpus")),
        )
        .unwrap_err();

        assert!(error.contains("BORZOI_MSBUILD_PROJECT_LIST"));
        assert!(error.contains("BORZOI_MSBUILD_CORPUS"));
    }

    #[test]
    fn project_list_is_not_sampled_by_default() {
        let paths = vec![
            PathBuf::from("a.fsproj"),
            PathBuf::from("b.fsproj"),
            PathBuf::from("c.fsproj"),
        ];
        let source = ProjectSource::ProjectList(std::env::join_paths(&paths).unwrap());
        let sampling = sampling_from_env_for_source(&source, false).unwrap();

        assert_eq!(projects_from_source(source, sampling).unwrap(), paths);
    }

    #[test]
    fn corpus_source_is_sampled_only_when_not_exhaustive() {
        let source = ProjectSource::CorpusRoot(PathBuf::from("/tmp/corpus"));

        assert!(should_sample_projects(&source, false));
        assert!(!should_sample_projects(&source, true));
    }

    #[test]
    fn sample_projects_uses_stride_and_limit() {
        let projects = (0..30)
            .map(|i| PathBuf::from(format!("{i:02}.fsproj")))
            .collect();

        assert_eq!(
            sample_projects(
                projects,
                Sampling {
                    stride: 13,
                    limit: 2,
                },
            ),
            vec![PathBuf::from("00.fsproj"), PathBuf::from("13.fsproj")]
        );
    }
}
