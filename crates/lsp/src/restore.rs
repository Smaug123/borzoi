//! On-demand `dotnet restore` for the assets-absent case.
//!
//! When a project has no `obj/project.assets.json`, we obtain its resolved
//! reference set by running the *real* `dotnet restore` — the ground truth —
//! rather than reimplementing NuGet/SDK resolution statically. That decision is
//! deliberate (see `docs/nuget-restore-plan.md` and the branch history):
//! reproducing restore's closure from static MSBuild evaluation is unsound in
//! the general case (restore runs arbitrary MSBuild *targets*) and a moving
//! target across SDK versions (pruning, target-framework fallback, and the NuGet
//! resolver itself are computed in SDK tasks/data, not evaluation-visible XML).
//! Letting the SDK do its own work is exact by construction and needs no
//! per-SDK-version re-validation.
//!
//! The restore is shaped to be fast, isolated, and side-effect-free:
//!
//! - **Scratch-redirected.** `-p:MSBuildProjectExtensionsPath=<scratch>/` sends
//!   *all* generated restore output (the assets file, `.nuget.g.props`/`.targets`,
//!   the cache) into a throwaway directory, so the user's `obj/` is never
//!   touched — we never risk corrupting their tree if we get something wrong.
//! - **Entry-only.** `-p:RestoreRecursive=false` restores just the entry, not the
//!   whole `<ProjectReference>` graph. Restoring the graph under one shared
//!   extensions path would make every node write the *same*
//!   `<scratch>/project.assets.json`, so a dependency's assets could clobber the
//!   entry's and be read as the entry's result. The entry's own assets are still
//!   complete — package/framework DLLs and per-reference producer TFMs / flowed
//!   packages — because restore evaluates the references to record them even when
//!   it does not restore *them*.
//! - **Offline.** `-p:RestoreSources=<empty-dir>` (plus a cleared
//!   `RestoreAdditionalProjectSources`) overrides every configured feed — the
//!   project's own `<RestoreSources>`, `Directory.Build.props`, and the ambient
//!   config's `nuget.org` — with a single empty local folder, so restore reads
//!   no feed. A cached project restores in ~0.5s; a
//!   cold cache fails fast (~0.7s) with `NU1101` instead of stalling on the
//!   network, which nothing could make fast anyway — so we decline and degrade.
//!   `-p:NuGetAudit=false` keeps the audit from reaching its own sources.
//!   The SDK's `library-packs` folders survive the override: its targets list
//!   `RestoreAdditionalProjectSources` in `TreatAsLocalProperty` and then
//!   append them. They are local folders, so the restore stays offline, and
//!   restore extracts from them into the package folder, as the user's own
//!   restore does.
//! - **Certified exact.** The offline restore reads a subset of the user's
//!   sources, so it can settle for a version the user's feeds would beat: one
//!   above the range's minimum (`NU1603`), held by the package folder or a
//!   `library-packs` folder, or the highest it can see for a floating range.
//!   Its result is used only if every dependency edge resolved to the
//!   inclusive minimum of its range, the one version every superset of its
//!   sources selects too; anything else declines (see `restore/exact.rs`). So an
//!   `FSharp.Core` request the SDK bundles resolves from a cold package folder,
//!   and one below it declines rather than resolving to the bundled version.
//! - **Package folder pinned to NuGet's own.** `-p:RestorePackagesPath=<folder>`
//!   pins the *package* folder to the one NuGet itself would use for the
//!   project — `$NUGET_PACKAGES`, else the `globalPackagesFolder` of the config
//!   files NuGet loads, else `<home>/.nuget/packages` — computed by
//!   [`borzoi_nuget::settings::global_packages_folder`], which declines what it
//!   cannot say exactly (and then so does the restore). Reading another folder
//!   would resolve against packages the user's own restore never sees.
//!
//!   The pin masks a project's own `RestorePackagesPath` (residual: such a
//!   project restores from the settings' folder, not its own). A folder inside
//!   the project's directory, or inside the directory of the project-tree
//!   config file that chose it, declines: restore can extract into the folder
//!   from the SDK's `library-packs` sources (see **Offline**), and the source
//!   tree is not ours to write.
//!   (Residual: a folder a config chooses elsewhere in a repository, through
//!   `..` past its own directory, or one the user config or `NUGET_PACKAGES`
//!   puts inside a repository, is written to as a cache.) So does a folder
//!   whose path MSBuild would not read verbatim from a `-p:` argument.
//!   After the restore, the config files its spec records must be the ones the
//!   folder was computed from, or the result is discarded: MSBuild's settings
//!   task loads them itself, so this catches a `RestoreConfigFile`, a
//!   `RestoreRootConfigDirectory`, and any walk modelled differently from
//!   NuGet's, with the real NuGet as the judge.
//! - **No source-tree writes.** `-p:RestorePackagesWithLockFile=false` plus a
//!   redirected `NuGetLockFilePath` keep a lock-file-enabled project from writing
//!   `packages.lock.json` beside the project. A project that *already* has a
//!   `packages.lock.json` is declined outright: restore would resolve its pinned
//!   graph, which a fresh resolve here could diverge from.
//! - **Project-rooted.** The command's working directory is the project
//!   directory, so `dotnet` resolves the SDK from the project's `global.json`
//!   (rather than the LSP process's cwd) — the same SDK the evaluator saw.
//! - **Only from a real directory.** A project whose directory is reached
//!   through a symlink is declined. MSBuild runs a project's targets with the
//!   process's working directory set to the project's directory, which the OS
//!   reports by its real path, so restore resolves the project's relative
//!   `<ProjectReference>`s under a spelling its evaluation never used, and
//!   NuGet drops them: the restore succeeds without them, whatever directory it
//!   is run from. A project file that is itself a symlink, in a real directory,
//!   restores whole.
//! - **Bounded.** It runs under [`BoundedCommand`] with a deadline as a backstop.
//!
//! These isolation guarantees are enforced through MSBuild global properties, so
//! they hold for well-behaved projects but are **not** a security boundary. The
//! opt-in ([`crate::semantic::SemanticState::set_on_demand_restore_enabled`]) is:
//! restore executes the project's MSBuild targets, so it runs only on a workspace
//! the host has trusted — and a trusted project's targets can write the tree or
//! reach the network via `<Exec>` regardless of these flags. In particular a
//! project that lists a redirected property under `TreatAsLocalProperty` can
//! locally reassign it and escape the corresponding redirect. We do not chase
//! that (it would need OS-level sandboxing, and the trust opt-in already bounds
//! it); the flags exist to keep *ordinary* projects side-effect-free, not to
//! contain a hostile one.
//!
//! - **B₁ in shadow.** Every restore that writes a spec also hands it to the
//!   spawn-free resolver (`borzoi_nuget::dgspec::resolve_dgspec`) and logs
//!   whether that agreed with the restore ([`shadow`]). The restore's answer
//!   is served whatever B₁ says. The restore writes a detailed log into
//!   scratch, from which the NuGet that ran is read ([`nuget_identity`]).
//!
//! On success the scratch assets file is read exactly as a normal
//! `obj/project.assets.json` would be, then the scratch directory is removed.
//! [`RestoreOutcome`] distinguishes a *stable* decline (cold cache, a locked
//! project, no restore environment — cache the empty env) from a *transient*
//! failure (spawn error, timeout, wedge — don't cache, so a later request
//! retries).
//!
//! The entry point is a synchronous, value-returning function with no
//! `&mut`-workspace dependency, so a future change can dispatch it on a
//! background worker pool (keeping the single-threaded LSP dispatch loop
//! responsive) with only the shell around it changing.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use borzoi_spawn::BoundedCommand;

use borzoi_nuget::settings::{FolderOrigin, GlobalPackagesFolder, global_packages_folder};

use crate::paths::{lexically_normalize, paths_equal};
use crate::project_assets::{
    ResolvedAssemblies, resolve_assemblies_for_tfm, resolve_assemblies_root_only,
};
use crate::sdk_discovery::SdkDiscoveryEnv;
use crate::workspace::ServedTfm;
use borzoi_nuget::dgspec::NuGetIdentity;
use shadow::{RestoreRecord, ShadowVerdict};

mod exact;
pub mod nuget_identity;
pub mod shadow;

/// Backstop deadline for the restore subprocess. Offline resolution makes a warm
/// restore ~0.5s and a cold one fail fast, so this is only ever reached by a
/// pathological project — at which point declining (empty env) is correct.
const RESTORE_DEADLINE: Duration = Duration::from_secs(30);

/// What an on-demand restore produced. The caller maps a stable decline to a
/// cached empty env and a transient failure to a *retryable* empty env (so a
/// later request tries again rather than being served the empty result forever).
pub enum RestoreOutcome {
    /// Restore succeeded; these are the entry's compile assemblies.
    Resolved(ResolvedAssemblies),
    /// Restore ran and declined stably (cold cache, a locked project, no restore
    /// environment, an unreadable/absent assets file). Re-running will not help
    /// until an input changes, so the caller caches the empty env.
    Declined,
    /// Restore did not complete (spawn failure, timeout, wedge). Transient — the
    /// caller must not cache the empty env.
    TransientFailure,
}

/// A scratch directory removed when dropped, so a restore leaves nothing behind
/// even if the caller returns early.
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create a fresh scratch directory with an unpredictable, non-sequential
    /// name (hashed from pid, a monotonic counter, and a high-resolution
    /// timestamp) so another process on a shared temp directory cannot guess it.
    /// `create` (non-recursive) is the real guarantee: it fails if the path
    /// already exists, so a pre-created or symlinked directory is never reused;
    /// `0700` keeps it private.
    fn new() -> Option<Self> {
        use std::hash::{Hash, Hasher};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (std::process::id(), counter, nanos).hash(&mut hasher);
        // Absolutize the base: `std::env::temp_dir()` can be relative (a relative
        // `TMPDIR`), and we hand this path to a child whose cwd is the *project*
        // directory — a relative path would then resolve inside the project tree
        // (writing generated files there) while `Drop` removes a different path.
        let base = std::env::temp_dir();
        let base = std::path::absolute(&base).unwrap_or(base);
        let path = base.join(format!("borzoi-restore-{:016x}", hasher.finish()));

        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).ok()?;
        Some(Self { path })
    }
}

impl ScratchDir {
    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Resolve an assets-absent project's reference set by running a bounded,
/// offline, entry-only, scratch-redirected `dotnet restore` and reading the
/// result. On success this returns the same [`ResolvedAssemblies`] the
/// assets-file path does, so it slots into the same downstream composition.
///
/// `restore_env` is `None` for callers that must not restore (the observability
/// / differential surface, which assumes an already-restored project); then this
/// declines. `Some(env)` supplies the SDK-discovery environment the subprocess
/// runs under (cache root, dotnet root, PATH, MSBuild-visible variables).
pub fn restore_to_scratch_assemblies(
    project: &Path,
    dotnet_root: &Path,
    target_framework: &ServedTfm,
    restore_env: Option<&SdkDiscoveryEnv>,
) -> RestoreOutcome {
    let restored = restore_to_scratch(project, dotnet_root, target_framework, restore_env);
    if let Some(shadow) = &restored.shadow {
        log_shadow(project, shadow);
    }
    restored.outcome
}

/// What [`restore_to_scratch`] did: the outcome the LSP serves, and B₁'s
/// shadow verdict on the spec the restore read.
pub struct ScratchRestore {
    pub outcome: RestoreOutcome,
    /// `None` when the restore did not run to completion, or wrote no spec.
    pub shadow: Option<ShadowRun>,
}

/// B₁'s shadow verdict, and how long it took to reach.
pub struct ShadowRun {
    /// The NuGet the restore ran, when its log names one.
    pub identity: Option<NuGetIdentity>,
    pub verdict: ShadowVerdict,
    pub elapsed: Duration,
}

/// Log B₁'s shadow verdict. A disagreement is a B₁ bug, so it is an error.
fn log_shadow(project: &Path, shadow: &ShadowRun) {
    let elapsed_ms = shadow.elapsed.as_millis();
    let verdict = shadow.verdict.kind();
    let nuget = shadow
        .identity
        .as_ref()
        .map_or("unknown", |identity| identity.as_str());
    match &shadow.verdict {
        ShadowVerdict::Agreed(frameworks) => tracing::info!(
            project = %project.display(),
            verdict,
            nuget,
            elapsed_ms,
            frameworks = frameworks.len(),
            "B₁ shadow: agrees with the restore"
        ),
        ShadowVerdict::Declined(decline) => tracing::info!(
            project = %project.display(),
            verdict,
            nuget,
            elapsed_ms,
            %decline,
            "B₁ shadow: declined"
        ),
        ShadowVerdict::NotRun(why) => tracing::info!(
            project = %project.display(),
            verdict,
            nuget,
            elapsed_ms,
            why = ?why,
            "B₁ shadow: not run"
        ),
        ShadowVerdict::Disagreed(disagreement) => tracing::error!(
            project = %project.display(),
            verdict,
            nuget,
            elapsed_ms,
            %disagreement,
            "B₁ shadow: disagrees with the restore (a B₁ bug; the restore's answer is served)"
        ),
    }
}

/// [`restore_to_scratch_assemblies`], returning B₁'s shadow verdict beside
/// the outcome rather than logging it (see [`shadow`]). The verdict never
/// changes the outcome.
pub fn restore_to_scratch(
    project: &Path,
    dotnet_root: &Path,
    target_framework: &ServedTfm,
    restore_env: Option<&SdkDiscoveryEnv>,
) -> ScratchRestore {
    let outcome = |outcome| ScratchRestore {
        outcome,
        shadow: None,
    };
    let Some(env) = restore_env else {
        return outcome(RestoreOutcome::Declined);
    };

    let Some(inputs) = restore_inputs(project, env) else {
        return outcome(RestoreOutcome::Declined);
    };
    let project = inputs.project.as_path();

    let Some(scratch) = ScratchDir::new() else {
        return outcome(RestoreOutcome::TransientFailure);
    };

    let succeeded = match run_offline_restore(
        &inputs.project,
        &scratch.path,
        dotnet_root,
        &inputs.packages.folder,
        env,
    ) {
        RunResult::Succeeded => true,
        RunResult::Declined => false,
        RunResult::Transient => return outcome(RestoreOutcome::TransientFailure),
    };

    // NuGet writes the spec it read, and the assets file, even when
    // resolution fails.
    let Some(spec) = restored_spec_path(&scratch.path, project)
        .and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return outcome(RestoreOutcome::Declined);
    };
    let assets_path = scratch.path.join("project.assets.json");
    let assets_text = std::fs::read_to_string(&assets_path).ok();
    let shadow = Some(run_shadow(
        env,
        &spec,
        &scratch.path,
        assets_text.as_deref(),
    ));
    ScratchRestore {
        outcome: served_outcome(
            &inputs,
            &scratch,
            succeeded,
            &spec,
            assets_text,
            dotnet_root,
            target_framework,
        ),
        shadow,
    }
}

/// B₁ over the spec a restore read, checked against what NuGet did in that
/// restore, which wrote into `scratch`.
///
/// Whether NuGet's restore succeeded is read from the `success` NuGet records
/// in `project.nuget.cache`, not from the command's exit status: a target the
/// project hooks after `Restore` can fail the command once NuGet has
/// restored, and B₁ models NuGet's restore, not the project's targets. Which
/// NuGet restored is read from the restore's log ([`nuget_identity`]).
fn run_shadow(
    env: &SdkDiscoveryEnv,
    spec: &str,
    scratch: &Path,
    assets: Option<&str>,
) -> ShadowRun {
    let started = std::time::Instant::now();
    let identity = match std::fs::read_to_string(restore_log(scratch)) {
        Ok(log) => nuget_identity::restoring_nuget(&log),
        Err(e) => Err(nuget_identity::IdentityDecline::NoLog(e.to_string())),
    };
    let named = identity.as_ref().ok().cloned();
    let verdict = match nuget_restore_succeeded(scratch) {
        Err(why) => ShadowVerdict::NotRun(shadow::NotRun::NoNuGetResult(why)),
        Ok(succeeded) => {
            let assets: Option<serde_json::Value> =
                assets.and_then(|text| serde_json::from_str(text).ok());
            shadow::shadow(
                identity,
                spec,
                RestoreRecord {
                    succeeded,
                    assets: assets.as_ref(),
                },
                &restore_environment(env),
            )
        }
    };
    ShadowRun {
        identity: named,
        verdict,
        elapsed: started.elapsed(),
    }
}

/// Whether NuGet's restore into `scratch` succeeded, as NuGet recorded it in
/// the `project.nuget.cache` beside the assets file (`CacheFile.Success`),
/// which it writes whether or not the restore succeeds.
fn nuget_restore_succeeded(scratch: &Path) -> Result<bool, String> {
    let path = scratch.join("project.nuget.cache");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cache: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    cache["success"]
        .as_bool()
        .ok_or_else(|| format!("{}: no boolean `success`", path.display()))
}

/// The outcome the LSP serves from a restore that wrote `spec`.
fn served_outcome(
    inputs: &RestoreInputs,
    scratch: &ScratchDir,
    succeeded: bool,
    spec: &str,
    assets: Option<String>,
    dotnet_root: &Path,
    target_framework: &ServedTfm,
) -> RestoreOutcome {
    let project = inputs.project.as_path();
    if !succeeded {
        tracing::info!(
            project = %project.display(),
            "on-demand restore declined (cold cache or failure); assembly env defaults to empty"
        );
        return RestoreOutcome::Declined;
    }
    if !loads_the_modelled_config_files(spec, inputs) {
        return RestoreOutcome::Declined;
    }

    let assets_path = scratch.path.join("project.assets.json");
    let Some(assets) = assets else {
        tracing::info!(
            project = %project.display(),
            "on-demand restore produced no assets file; assembly env defaults to empty"
        );
        return RestoreOutcome::Declined;
    };
    if let Err(uncertified) =
        exact::certify_exact_minimums(spec, &assets, project, &redirected_lock_file(&scratch.path))
    {
        tracing::info!(
            project = %project.display(),
            %uncertified,
            "on-demand restore declined: the user's restore could resolve differently"
        );
        return RestoreOutcome::Declined;
    }

    // Read the scratch assets exactly as an `obj/project.assets.json` would be:
    // a chosen TFM selects its target (plan E3); `NoneDeclared` (⇒ `as_deref` is
    // `None`) falls back to requiring a single-target restore. `Untrusted` never
    // reaches here (the caller returns empty first).
    let resolved = match target_framework.as_deref() {
        Some(tfm) => resolve_assemblies_for_tfm(&assets_path, dotnet_root, tfm),
        None => resolve_assemblies_root_only(&assets_path, dotnet_root),
    };
    match resolved {
        Ok(resolved) => {
            tracing::info!(
                project = %project.display(),
                packages = resolved.package_dlls.len(),
                framework = resolved.framework_dlls.len(),
                "on-demand restore succeeded (no project.assets.json)"
            );
            RestoreOutcome::Resolved(resolved)
        }
        Err(error) => {
            tracing::warn!(
                project = %project.display(),
                %error,
                "on-demand restore assets unreadable; assembly env defaults to empty"
            );
            RestoreOutcome::Declined
        }
    }
}

/// What an offline restore of a project is run with.
struct RestoreInputs {
    /// The project.
    project: PathBuf,
    /// The global packages folder NuGet would use for the project, which the
    /// restore is pinned to, and the config files it was computed from.
    packages: GlobalPackagesFolder,
}

/// The inputs of an offline restore of `project`, or `None` if the project
/// must not be restored here.
fn restore_inputs(project: &Path, env: &SdkDiscoveryEnv) -> Option<RestoreInputs> {
    let Some(project) = in_its_real_directory(project) else {
        tracing::info!(
            project = %project.display(),
            "on-demand restore declined: the project's directory is not spelt by its real path, \
             and restoring it there drops its project references"
        );
        return None;
    };
    let project = project.as_path();
    // A project with a `packages.lock.json` expects restore to resolve its
    // *pinned* graph; a fresh resolve here could pick different versions, so
    // decline rather than risk over-resolving. (`RestorePackagesWithLockFile`
    // without an existing lock file is fine — the fresh resolve is what the first
    // real restore would lock, and the redirect below keeps us from writing it.)
    //
    // Residual: a project that relocates its lock file with `NuGetLockFilePath`
    // is not detected here (that needs the evaluated property, which this layer
    // does not have); such a project gets a fresh resolve. Exotic — the default
    // sibling path is the overwhelmingly common lock-file location.
    if project
        .parent()
        .is_some_and(|dir| dir.join("packages.lock.json").is_file())
    {
        tracing::info!(
            project = %project.display(),
            "on-demand restore declined: a packages.lock.json pins the graph"
        );
        return None;
    }

    // The package folder NuGet itself would use, under exactly the
    // environment the restore runs in (see the module docs).
    let directory = project.parent()?;
    let packages = match global_packages_folder(directory, &restore_environment(env)) {
        Ok(packages) => packages,
        Err(decline) => {
            tracing::info!(
                project = %project.display(),
                %decline,
                "on-demand restore declined: the global packages folder NuGet would use is not known"
            );
            return None;
        }
    };
    // The tree the folder must stay out of: the project's directory, or the
    // directory of the project-tree config file that chose the folder, which
    // is an ancestor of it (`repo/NuGet.Config` choosing `repo/.packages` for
    // `repo/src/App.fsproj`).
    let tree = match &packages.origin {
        FolderOrigin::Config { file } => file
            .parent()
            .filter(|folder| is_within(directory, folder))
            .unwrap_or(directory),
        FolderOrigin::Environment | FolderOrigin::Default => directory,
    };
    if is_within(&packages.folder, tree) {
        tracing::info!(
            project = %project.display(),
            folder = %packages.folder.display(),
            tree = %tree.display(),
            "on-demand restore declined: the global packages folder is inside the project's source tree"
        );
        return None;
    }
    if !passes_through_a_property_argument(&packages.folder) {
        tracing::info!(
            project = %project.display(),
            folder = %packages.folder.display(),
            "on-demand restore declined: the global packages folder cannot be passed to MSBuild verbatim"
        );
        return None;
    }
    Some(RestoreInputs {
        project: project.to_path_buf(),
        packages,
    })
}

/// Whether `path` is `directory` or beneath it, lexically or once the part of
/// `path` that exists is resolved through its symbolic links. A path that
/// cannot be resolved counts as inside.
fn is_within(path: &Path, directory: &Path) -> bool {
    let under = |path: &Path, directory: &Path| {
        path.ancestors()
            .any(|ancestor| paths_equal(ancestor, directory))
    };
    if under(path, directory) {
        return true;
    }
    let Some(existing) = path.ancestors().find(|ancestor| ancestor.exists()) else {
        return true;
    };
    let (Ok(real), Ok(real_directory)) = (
        std::fs::canonicalize(existing),
        std::fs::canonicalize(directory),
    ) else {
        return true;
    };
    let rest = path.strip_prefix(existing).unwrap_or(Path::new(""));
    under(&real.join(rest), &real_directory)
}

/// Whether `path`, as the value of a `-p:Name=value` argument, reaches MSBuild
/// verbatim: MSBuild splits the value at `;` and `,`, unescapes `%XX`, and
/// treats `$`, `@`, quotes and the wildcard characters specially, so a path
/// holding any of them (or a backslash, which MSBuild may read as a separator,
/// a control character, or bytes that are not UTF-8)
/// would pin some other folder.
fn passes_through_a_property_argument(path: &Path) -> bool {
    path.to_str().is_some_and(|text| {
        !text.chars().any(|c| {
            c.is_control()
                || matches!(
                    c,
                    ';' | ',' | '%' | '$' | '@' | '\'' | '"' | '*' | '?' | '\\'
                )
        })
    })
}

/// Where a restore whose output is redirected to `scratch` writes the
/// dependency-graph spec it read for `project`.
fn restored_spec_path(scratch: &Path, project: &Path) -> Option<PathBuf> {
    let mut name = project.file_name()?.to_os_string();
    name.push(".nuget.dgspec.json");
    Some(scratch.join(name))
}

/// Whether the dependency-graph spec `text`, written by MSBuild for the
/// restore of `inputs.project`, records exactly the config files
/// `inputs.packages` was computed from. MSBuild's settings task loads them
/// itself, so a disagreement means the folder pinned may not be the one NuGet
/// would have chosen (under a `RestoreConfigFile`, say), and the result must
/// not be trusted.
fn loads_the_modelled_config_files(text: &str, inputs: &RestoreInputs) -> bool {
    let recorded = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|spec| {
            let projects = spec.get("projects")?.as_object()?;
            let (_, project) = projects
                .iter()
                .find(|(name, _)| paths_equal(Path::new(name), &inputs.project))?;
            project
                .pointer("/restore/configFilePaths")?
                .as_array()?
                .iter()
                .map(|path| path.as_str().map(PathBuf::from))
                .collect::<Option<Vec<PathBuf>>>()
        });
    let agree = recorded.as_deref() == Some(inputs.packages.config_files.as_slice());
    if !agree {
        tracing::info!(
            project = %inputs.project.display(),
            recorded = ?recorded,
            modelled = ?inputs.packages.config_files,
            "on-demand restore declined: restore loaded other config files than the package folder was computed from"
        );
    }
    agree
}

/// `project`, made absolute and lexically normalised, if its directory is
/// spelt by its real path: no symlink in it. Case may differ where the platform
/// folds it (a restore keeps the references then; probed on macOS), and the
/// file itself may be a symlink, which MSBuild evaluates beside the link.
/// `None` otherwise, or if the directory does not resolve. The result is in
/// ordinary form (see [`without_verbatim_prefix`]), the form MSBuild is
/// handed.
fn in_its_real_directory(project: &Path) -> Option<PathBuf> {
    let spelt = ordinary(lexically_normalize(&std::path::absolute(project).ok()?));
    let directory = spelt.parent()?;
    let real = ordinary(std::fs::canonicalize(directory).ok()?);
    paths_equal(&real, directory).then_some(spelt)
}

/// `path` in ordinary form. On Windows a real path comes back verbatim
/// (`\\?\C:\…`), and a caller's path may already be verbatim; both sides of a
/// comparison go through here, and MSBuild is handed the ordinary form.
fn ordinary(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    match path.to_str() {
        Some(text) => PathBuf::from(without_verbatim_prefix(text).into_owned()),
        None => path,
    }
}

/// `text` without a Windows verbatim prefix: `\\?\C:\x` is `C:\x`, and
/// `\\?\UNC\server\share` is `\\server\share`. Anything else is unchanged.
fn without_verbatim_prefix(text: &str) -> std::borrow::Cow<'_, str> {
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        std::borrow::Cow::Owned(format!(r"\\{unc}"))
    } else {
        std::borrow::Cow::Borrowed(text.strip_prefix(r"\\?\").unwrap_or(text))
    }
}

/// What [`restore_graph`] produced.
pub enum RestoreGraph {
    /// The dependency-graph spec's text, and the scratch directory it was
    /// written under, which lives as long as this value does. The spec names
    /// that directory (its restore output path, and the empty folder that is
    /// its only package source), so the directory is what a consumer of the
    /// spec needs to find in place, and the one part of the spec that differs
    /// from run to run. For [`RestoreGraphSource::Restored`] it also holds the
    /// restore's other outputs, the assets file among them.
    Written { text: String, scratch: ScratchDir },
    /// The project must not be restored here (as for
    /// [`restore_to_scratch_assemblies`]), or MSBuild wrote no spec.
    Declined,
    /// The invocation did not complete (spawn failure, timeout, wedge).
    TransientFailure,
}

/// Which dependency-graph spec [`restore_graph`] obtains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreGraphSource {
    /// MSBuild's `GenerateRestoreGraphFile` target: the spec computed without
    /// restoring (docs/nuget-dgspec-boundary-plan.md, A₀).
    Generated,
    /// The spec the restore of [`restore_to_scratch_assemblies`] itself
    /// writes beside its assets file: by definition what that restore read. It
    /// is written even when resolution then fails.
    Restored,
}

/// The dependency-graph spec of an offline restore of `project`, obtained the
/// way `source` says, under exactly the SDK, environment and global properties
/// [`restore_to_scratch_assemblies`] restores with. This is the half-A oracle
/// of `docs/nuget-dgspec-boundary-plan.md`; the LSP itself never calls it.
///
/// It declines what [`restore_to_scratch_assemblies`] declines, including a
/// project whose directory is reached through a symlink: NuGet drops such a
/// project's `ProjectReference`s from the spec restore reads, so the two routes
/// would disagree.
pub fn restore_graph(
    project: &Path,
    dotnet_root: &Path,
    env: &SdkDiscoveryEnv,
    source: RestoreGraphSource,
) -> RestoreGraph {
    let Some(inputs) = restore_inputs(project, env) else {
        return RestoreGraph::Declined;
    };
    let project = inputs.project.as_path();
    let Some(scratch) = ScratchDir::new() else {
        return RestoreGraph::TransientFailure;
    };
    let (verb, written) = match source {
        RestoreGraphSource::Generated => {
            let output = scratch.path.join("restore-graph.dg.json");
            (
                Verb::GenerateRestoreGraph {
                    output: output.clone(),
                },
                output,
            )
        }
        RestoreGraphSource::Restored => {
            let Some(written) = restored_spec_path(&scratch.path, project) else {
                return RestoreGraph::Declined;
            };
            (Verb::Restore, written)
        }
    };
    let Some(cmd) = offline_restore_command(
        verb,
        project,
        &scratch.path,
        dotnet_root,
        &inputs.packages.folder,
        env,
    ) else {
        return RestoreGraph::TransientFailure;
    };
    match (run_bounded(project, cmd), source) {
        (RunResult::Transient, _) => return RestoreGraph::TransientFailure,
        (RunResult::Declined, RestoreGraphSource::Generated) => return RestoreGraph::Declined,
        // A restore that fails to resolve has still written the spec it read.
        (RunResult::Declined | RunResult::Succeeded, _) => {}
    }
    match std::fs::read_to_string(&written) {
        Ok(text) if loads_the_modelled_config_files(&text, &inputs) => {
            RestoreGraph::Written { text, scratch }
        }
        _ => RestoreGraph::Declined,
    }
}

/// Outcome of the restore subprocess itself.
enum RunResult {
    /// Exit 0 — the assets file should be present.
    Succeeded,
    /// Ran but exited non-zero (a stable decline, e.g. `NU1101` on a cold cache).
    Declined,
    /// Did not run to completion (spawn error, timeout, wedge).
    Transient,
}

/// Run the bounded, offline, entry-only, scratch-redirected restore.
fn run_offline_restore(
    project: &Path,
    scratch: &Path,
    dotnet_root: &Path,
    packages_folder: &Path,
    env: &SdkDiscoveryEnv,
) -> RunResult {
    let Some(cmd) = offline_restore_command(
        Verb::Restore,
        project,
        scratch,
        dotnet_root,
        packages_folder,
        env,
    ) else {
        return RunResult::Transient;
    };
    run_bounded(project, cmd)
}

fn run_bounded(project: &Path, cmd: Command) -> RunResult {
    match BoundedCommand::new(cmd).timeout(RESTORE_DEADLINE).run() {
        Ok(output) if output.status.success() => RunResult::Succeeded,
        Ok(_) => RunResult::Declined,
        Err(failure) => {
            tracing::info!(project = %project.display(), %failure, "on-demand restore did not complete");
            RunResult::Transient
        }
    }
}

/// What a restore-shaped invocation asks MSBuild to do.
enum Verb {
    /// `dotnet restore`.
    Restore,
    /// `dotnet msbuild -t:GenerateRestoreGraphFile`: compute the
    /// dependency-graph spec restore would read, write it to `output`, and stop.
    GenerateRestoreGraph { output: PathBuf },
}

/// The one way this module invokes `dotnet` against a project, whatever the
/// verb, so every invocation runs under the same SDK, working directory,
/// environment and global properties: an offline, entry-only restore whose
/// output lands in `scratch`. The dependency-graph oracle ([`restore_graph`])
/// relies on that sameness, since what it generates is only restore's input if
/// MSBuild evaluated the project identically. `None` if the scratch layout
/// could not be created.
fn offline_restore_command(
    verb: Verb,
    project: &Path,
    scratch: &Path,
    dotnet_root: &Path,
    packages_folder: &Path,
    env: &SdkDiscoveryEnv,
) -> Option<Command> {
    // An empty local folder used as the *only* restore source. As an MSBuild
    // property this overrides the project's `<RestoreSources>` and the ambient
    // config's feeds alike (the property takes precedence over config sources),
    // so restore reads only the global-packages cache — offline.
    let empty_source = scratch.join("empty-source");
    std::fs::create_dir_all(&empty_source).ok()?;

    // `MSBuildProjectExtensionsPath` (trailing slash required — it's an MSBuild
    // directory property) redirects the whole restore output tree into scratch.
    let ext_path = with_trailing_slash(scratch);

    // Invoke the muxer from the discovered SDK root when we can: setting
    // `DOTNET_ROOT` on the child does not make a `PATH`-resolved muxer use that
    // root for SDK lookup, so a `dotnet` from a different install could pick the
    // wrong SDK (or none). Fall back to `PATH` only when the root has no muxer.
    let dotnet_exe_name = if cfg!(windows) {
        "dotnet.exe"
    } else {
        "dotnet"
    };
    let dotnet_exe = dotnet_root.join(dotnet_exe_name);
    let mut cmd = if dotnet_exe.is_file() {
        Command::new(dotnet_exe)
    } else {
        Command::new(dotnet_exe_name)
    };
    match verb {
        Verb::Restore => {
            cmd.arg("restore").arg(project).arg("-nologo");
            // A detailed log, which names the assembly each task ran from:
            // the evidence of which NuGet restored ([`nuget_identity`]).
            let mut logger = std::ffi::OsString::from("-flp:LogFile=");
            logger.push(restore_log(scratch));
            logger.push(";Verbosity=detailed");
            cmd.arg(logger);
        }
        Verb::GenerateRestoreGraph { output } => {
            cmd.arg("msbuild")
                .arg(project)
                .arg("-nologo")
                .arg("-t:GenerateRestoreGraphFile")
                .arg(property_arg("RestoreGraphOutputPath", output.as_os_str()));
        }
    }
    // Residual: a workspace's caller-supplied global properties
    // (`Workspace::with_env_and_extra_build_properties`) are not forwarded as
    // `-p:` here, so a package/TFM conditioned on such a global could restore
    // differently. The real server supplies none (empty extras) and its default
    // `Configuration`/`Platform` match restore's own defaults, so this bites only
    // a caller that sets graph-affecting globals — the test harnesses.
    cmd.arg(property_arg(
        "MSBuildProjectExtensionsPath",
        ext_path.as_os_str(),
    ))
    // `RestoreOutputPath` defaults from `MSBuildProjectExtensionsPath`, but a
    // project/import can override it (e.g. `custom/`), which would write the
    // assets into the source tree and leave scratch empty. Pin it to scratch
    // too so the whole output stays there regardless.
    .arg(property_arg("RestoreOutputPath", ext_path.as_os_str()))
    // Pin the *package* folder (extraction and read) to the one NuGet would
    // use (see the module docs).
    .arg(property_arg(
        "RestorePackagesPath",
        packages_folder.as_os_str(),
    ))
    .arg("-p:RestoreRecursive=false")
    .arg(property_arg("RestoreSources", empty_source.as_os_str()))
    .arg("-p:RestoreAdditionalProjectSources=")
    .arg("-p:RestorePackagesWithLockFile=false")
    .arg(property_arg(
        "NuGetLockFilePath",
        redirected_lock_file(scratch).as_os_str(),
    ))
    // NuGet's package audit contacts its `auditSources` even when
    // `RestoreSources` points at the empty folder, so an unreachable audit
    // endpoint would block until the deadline. Disable it to stay offline.
    .arg("-p:NuGetAudit=false")
    // The one global `dotnet restore` adds beyond its target (see
    // `tests/all/restore_graph.rs`, which diffs what each verb forwards to
    // MSBuild). Passing it for every verb makes `msbuild` evaluate the project
    // under the same globals, so a project conditioned on it restores and
    // generates alike.
    .arg("-p:NuGetInteractive=false");
    // Resolve the SDK from the project's own `global.json` tree, not the LSP
    // process's working directory.
    if let Some(dir) = project.parent() {
        cmd.current_dir(dir);
    }
    apply_env(&mut cmd, env);
    Some(cmd)
}

/// The detailed log a restore into `scratch` writes.
fn restore_log(scratch: &Path) -> PathBuf {
    scratch.join("restore.log")
}

/// Where a restore whose output is redirected to `scratch` is told its lock
/// file lives.
fn redirected_lock_file(scratch: &Path) -> PathBuf {
    scratch.join("packages.lock.json")
}

/// `path` with a trailing `/`, as MSBuild directory properties require.
fn with_trailing_slash(path: &Path) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    s.push("/");
    s
}

/// `-p:<name>=<value>` as a single argument (value may be a non-UTF-8 path).
fn property_arg(name: &str, value: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut arg = std::ffi::OsString::from("-p:");
    arg.push(name);
    arg.push("=");
    arg.push(value);
    arg
}

/// The environment restore's `dotnet` runs under, *exactly*: the project's
/// build environment, so restore evaluates the same graph the LSP did and no
/// host variable leaks in. The spawn-free restore
/// (`borzoi_nuget::dgspec::resolve_dgspec`) is handed this same map, since
/// NuGet reads some of these variables while restoring. So is SDK discovery's
/// model of NuGet's settings ([`crate::sdk_discovery`]): MSBuild's NuGet SDK
/// resolver reads them in the build this environment stands in for.
///
/// - It is built from nothing, not from the inherited environment. Otherwise a
///   hermetic (`Workspace::with_env`) caller's curated env would be polluted by
///   every host variable, and a project conditioned on e.g. `$(HOME)` could
///   evaluate one way and restore another. For the process-derived environment
///   (the real server) [`SdkDiscoveryEnv::build_environment`] *is* the full
///   process env, so this changes nothing in content.
/// - It holds [`SdkDiscoveryEnv::build_environment`] — the MSBuild-visible
///   variables the evaluator used — **except** `MSBuildUserExtensionsPath`: that
///   is a *synthetic* property our static evaluator seeds because it doesn't run
///   real MSBuild, and overlaying our value was observed to suppress the SDK's
///   implicit `FSharp.Core` reference (a real `dotnet` computes the path itself).
///   Residual: a project whose environment *genuinely* sets
///   `MSBuildUserExtensionsPath` to redirect user-extension imports is restored
///   under the computed default instead — exotic (real env-set user-extension
///   redirection affecting the package graph is essentially unheard of).
/// - Then the SDK-locating vars from discovery, which win over anything above and
///   cover an env whose `build_environment` omits them.
pub fn restore_environment(env: &SdkDiscoveryEnv) -> BTreeMap<OsString, OsString> {
    // The synthetic property the evaluator seeds; the real dotnet must compute it.
    const SYNTHETIC: &str = "MSBuildUserExtensionsPath";
    let mut environment: BTreeMap<OsString, OsString> = env
        .build_environment
        .iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(SYNTHETIC))
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    let discovered = [
        (
            "DOTNET_ROOT",
            env.dotnet_root.as_ref().map(|path| path.as_os_str()),
        ),
        ("PATH", env.search_path.as_deref()),
        (
            "NUGET_PACKAGES",
            env.nuget_packages_dir.as_ref().map(|path| path.as_os_str()),
        ),
        ("HOME", env.home_dir.as_ref().map(|path| path.as_os_str())),
        (
            "DOTNET_CLI_HOME",
            env.dotnet_cli_home.as_ref().map(|path| path.as_os_str()),
        ),
    ];
    for (name, value) in discovered {
        if let Some(value) = value {
            override_variable(&mut environment, name, value, cfg!(windows));
        }
    }
    environment
}

/// Set `name` to `value` in `environment`, first removing every key that names
/// the same variable: on Windows (`case_insensitive`) that is every spelling of
/// `name` in any case, since a child's environment keeps one of `Path` and
/// `PATH`, and not necessarily the last one inserted.
fn override_variable(
    environment: &mut BTreeMap<OsString, OsString>,
    name: &str,
    value: &std::ffi::OsStr,
    case_insensitive: bool,
) {
    if case_insensitive {
        environment.retain(|key, _| !key.eq_ignore_ascii_case(name));
    }
    environment.insert(OsString::from(name), value.to_owned());
}

/// Set the subprocess environment to exactly [`restore_environment`].
fn apply_env(cmd: &mut Command, env: &SdkDiscoveryEnv) {
    cmd.env_clear();
    cmd.envs(restore_environment(env));
}

#[cfg(test)]
mod tests {
    use super::{override_variable, without_verbatim_prefix};
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn an_override_replaces_every_spelling_only_where_case_is_ignored() {
        let inherited: BTreeMap<OsString, OsString> = [("Path", "inherited"), ("PATHEXT", "x")]
            .into_iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect();
        let overridden = |case_insensitive: bool| {
            let mut environment = inherited.clone();
            override_variable(
                &mut environment,
                "PATH",
                OsStr::new("discovered"),
                case_insensitive,
            );
            environment
                .into_iter()
                .map(|(name, value)| (name.into_string().unwrap(), value.into_string().unwrap()))
                .collect::<Vec<_>>()
        };
        let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
        assert_eq!(
            overridden(true),
            vec![pair("PATH", "discovered"), pair("PATHEXT", "x")]
        );
        assert_eq!(
            overridden(false),
            vec![
                pair("PATH", "discovered"),
                pair("PATHEXT", "x"),
                pair("Path", "inherited"),
            ]
        );
    }

    #[test]
    fn a_folder_with_an_msbuild_separator_is_not_passed_as_a_property() {
        use super::passes_through_a_property_argument;
        use std::path::Path;
        assert!(passes_through_a_property_argument(Path::new(
            "/home/u/.nuget/packages"
        )));
        assert!(passes_through_a_property_argument(Path::new(
            "/a b/c-d_e.f+g~h=i"
        )));
        for bad in [
            "/tmp/cache,Marker=value",
            "/tmp/a;b",
            "/tmp/a%3bb",
            "/tmp/$(X)",
            "/tmp/@(X)",
            "/tmp/'a'",
            "/tmp/\"a\"",
            "/tmp/*",
            "/tmp/a?",
            "/tmp/a\\\\b",
            "/tmp/a\nb",
        ] {
            assert!(!passes_through_a_property_argument(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn a_verbatim_prefix_is_removed_and_nothing_else_is() {
        let cases = [
            (r"\\?\C:\proj\App.fsproj", r"C:\proj\App.fsproj"),
            (
                r"\\?\UNC\server\share\App.fsproj",
                r"\\server\share\App.fsproj",
            ),
            (r"C:\proj\App.fsproj", r"C:\proj\App.fsproj"),
            (r"\\server\share\App.fsproj", r"\\server\share\App.fsproj"),
            ("/home/u/App.fsproj", "/home/u/App.fsproj"),
        ];
        for (input, expected) in cases {
            assert_eq!(without_verbatim_prefix(input), expected, "{input}");
        }
        // Idempotent, so normalising an already-ordinary side changes nothing.
        for (input, _) in cases {
            let once = without_verbatim_prefix(input).into_owned();
            assert_eq!(without_verbatim_prefix(&once), once);
        }
    }
}
