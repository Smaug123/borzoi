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
//!   only the warm global-packages cache. A cached project restores in ~0.5s; a
//!   cold cache fails fast (~0.7s) with `NU1101` instead of stalling on the
//!   network, which nothing could make fast anyway — so we decline and degrade.
//!   `-p:NuGetAudit=false` keeps the audit from reaching its own sources.
//! - **Package folder pinned.** `-p:RestorePackagesPath=<cache>` forces NuGet's
//!   *package* folder to the resolved warm cache, overriding a project
//!   `RestorePackagesPath` or a config `globalPackagesFolder` that would extract
//!   packages into a repo-local directory — a source-tree write the output
//!   redirects above do not cover.
//! - **No source-tree writes.** `-p:RestorePackagesWithLockFile=false` plus a
//!   redirected `NuGetLockFilePath` keep a lock-file-enabled project from writing
//!   `packages.lock.json` beside the project. A project that *already* has a
//!   `packages.lock.json` is declined outright: restore would resolve its pinned
//!   graph, which a fresh resolve here could diverge from.
//! - **Project-rooted.** The command's working directory is the project
//!   directory, so `dotnet` resolves the SDK from the project's `global.json`
//!   (rather than the LSP process's cwd) — the same SDK the evaluator saw.
//! - **Real paths only.** A project whose path is not its real path (a symlink
//!   in it, of a directory or of the project file) is declined. Restore runs in
//!   the project's directory, which the OS reports by its real path, so such a
//!   project would be spelt two ways in one evaluation, and NuGet then drops its
//!   `ProjectReference`s from the graph. Naming it by its real path instead moves
//!   the directories MSBuild searches for `Directory.Build.props` and the like
//!   away from the ones the LSP evaluated. Neither restores the project the LSP
//!   sees.
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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use borzoi_spawn::BoundedCommand;

use crate::project_assets::{
    ResolvedAssemblies, resolve_assemblies_for_tfm, resolve_assemblies_root_only,
};
use crate::sdk_discovery::SdkDiscoveryEnv;
use crate::workspace::ServedTfm;

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
struct ScratchDir {
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
    let Some(env) = restore_env else {
        return RestoreOutcome::Declined;
    };

    let Some(inputs) = restore_inputs(project, env) else {
        return RestoreOutcome::Declined;
    };

    let Some(scratch) = ScratchDir::new() else {
        return RestoreOutcome::TransientFailure;
    };

    match run_offline_restore(
        &inputs.project,
        &scratch.path,
        dotnet_root,
        &inputs.cache_root,
        env,
    ) {
        RunResult::Succeeded => {}
        RunResult::Declined => {
            tracing::info!(
                project = %project.display(),
                "on-demand restore declined (cold cache or failure); assembly env defaults to empty"
            );
            return RestoreOutcome::Declined;
        }
        RunResult::Transient => return RestoreOutcome::TransientFailure,
    }

    let assets_path = scratch.path.join("project.assets.json");
    if !assets_path.is_file() {
        tracing::info!(
            project = %project.display(),
            "on-demand restore produced no assets file; assembly env defaults to empty"
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
    /// The project, whose path is its real path (see the module docs).
    project: PathBuf,
    /// The warm cache the restore reads.
    cache_root: PathBuf,
}

/// The inputs of an offline restore of `project`, or `None` if the project
/// must not be restored here.
fn restore_inputs(project: &Path, env: &SdkDiscoveryEnv) -> Option<RestoreInputs> {
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

    // The (absolute) warm cache to pin package extraction/read to. A project's
    // `RestorePackagesPath` or a config `globalPackagesFolder` would otherwise
    // point NuGet's *package* folder at a repo-local directory, extracting
    // package files into the source tree (the output redirects only cover the
    // *generated* restore output). Forcing it to the resolved cache keeps
    // extraction out of the tree and reads from the warm cache the LSP knows.
    let Some(cache_root) = cache_root(env) else {
        tracing::info!(
            project = %project.display(),
            "on-demand restore declined: no global-packages cache root known"
        );
        return None;
    };
    let Some(project) = as_real_path(project) else {
        tracing::info!(
            project = %project.display(),
            "on-demand restore declined: the project is not named by its real path"
        );
        return None;
    };
    Some(RestoreInputs {
        project,
        cache_root,
    })
}

/// `path`, made absolute, if that is already its real path: no symlink in it,
/// no `..`, and every component spelt as the file system spells it. `None`
/// otherwise, or if it does not resolve. The result is in ordinary form (see
/// [`without_verbatim_prefix`]).
fn as_real_path(path: &Path) -> Option<PathBuf> {
    let absolute = ordinary(std::path::absolute(path).ok()?);
    let real = ordinary(std::fs::canonicalize(path).ok()?);
    (real == absolute).then_some(absolute)
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
    /// The dependency-graph spec's text. Its paths into the scratch directory
    /// it was written under (since removed) are the one part that differs from
    /// run to run, so `scratch` is returned for a comparison to normalise.
    Written { text: String, scratch: PathBuf },
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
            let Some(file) = project.file_name() else {
                return RestoreGraph::Declined;
            };
            let mut name = file.to_os_string();
            name.push(".nuget.dgspec.json");
            (Verb::Restore, scratch.path.join(name))
        }
    };
    let Some(cmd) = offline_restore_command(
        verb,
        project,
        &scratch.path,
        dotnet_root,
        &inputs.cache_root,
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
        Ok(text) => RestoreGraph::Written {
            text,
            scratch: scratch.path.clone(),
        },
        Err(_) => RestoreGraph::Declined,
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
    cache_root: &Path,
    env: &SdkDiscoveryEnv,
) -> RunResult {
    let Some(cmd) = offline_restore_command(
        Verb::Restore,
        project,
        scratch,
        dotnet_root,
        cache_root,
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
    cache_root: &Path,
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
    // Pin the *package* folder (extraction + read) to the resolved warm
    // cache, overriding a project `RestorePackagesPath` or config
    // `globalPackagesFolder` that would otherwise extract packages into a
    // repo-local directory (a source-tree write the output redirects miss).
    .arg(property_arg("RestorePackagesPath", cache_root.as_os_str()))
    .arg("-p:RestoreRecursive=false")
    .arg(property_arg("RestoreSources", empty_source.as_os_str()))
    .arg("-p:RestoreAdditionalProjectSources=")
    .arg("-p:RestorePackagesWithLockFile=false")
    .arg(property_arg(
        "NuGetLockFilePath",
        scratch.join("packages.lock.json").as_os_str(),
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

/// `path` with a trailing `/`, as MSBuild directory properties require.
fn with_trailing_slash(path: &Path) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    s.push("/");
    s
}

/// The absolute global-packages cache to pin restore's package folder to:
/// `$NUGET_PACKAGES` when the environment set it, else NuGet's documented
/// default `<home>/.nuget/packages`. Absolutized (a relative value would, under
/// the project-directory cwd, resolve inside the source tree). `None` when
/// neither is known — the caller then declines rather than let a repo-local
/// package folder take effect. An empty value counts as unavailable.
fn cache_root(env: &SdkDiscoveryEnv) -> Option<PathBuf> {
    let non_empty = |path: &Path| (!path.as_os_str().is_empty()).then(|| path.to_path_buf());
    let root = env
        .nuget_packages_dir
        .as_deref()
        .and_then(non_empty)
        .or_else(|| {
            env.home_dir
                .as_deref()
                .and_then(non_empty)
                .map(|home| home.join(".nuget").join("packages"))
        })?;
    Some(std::path::absolute(&root).unwrap_or(root))
}

/// `-p:<name>=<value>` as a single argument (value may be a non-UTF-8 path).
fn property_arg(name: &str, value: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut arg = std::ffi::OsString::from("-p:");
    arg.push(name);
    arg.push("=");
    arg.push(value);
    arg
}

/// Set the subprocess environment to *exactly* the project's build environment,
/// so restore evaluates the same graph the LSP did and no host variable leaks
/// in:
///
/// - **Clear** the inherited environment first. Without this a hermetic
///   (`Workspace::with_env`) caller's curated env would be polluted by every host
///   variable, and a project conditioned on e.g. `$(HOME)` could evaluate one way
///   and restore another. For the process-derived environment (the real server)
///   [`SdkDiscoveryEnv::build_environment`] *is* the full process env, so clearing
///   then re-installing it is a no-op in content.
/// - Install [`SdkDiscoveryEnv::build_environment`] — the MSBuild-visible
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
fn apply_env(cmd: &mut Command, env: &SdkDiscoveryEnv) {
    // The synthetic property the evaluator seeds; the real dotnet must compute it.
    const SYNTHETIC: &str = "MSBuildUserExtensionsPath";
    cmd.env_clear();
    for (name, value) in &env.build_environment {
        if !name.eq_ignore_ascii_case(SYNTHETIC) {
            cmd.env(name, value);
        }
    }
    if let Some(dotnet_root) = &env.dotnet_root {
        cmd.env("DOTNET_ROOT", dotnet_root);
    }
    if let Some(path) = &env.search_path {
        cmd.env("PATH", path);
    }
    if let Some(nuget) = &env.nuget_packages_dir {
        cmd.env("NUGET_PACKAGES", nuget);
    }
    if let Some(home) = &env.home_dir {
        cmd.env("HOME", home);
    }
    if let Some(cli_home) = &env.dotnet_cli_home {
        cmd.env("DOTNET_CLI_HOME", cli_home);
    }
}

#[cfg(test)]
mod tests {
    use super::without_verbatim_prefix;

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
