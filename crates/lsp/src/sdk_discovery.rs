//! Imperative shell that gathers the inputs `parse_fsproj_with_imports`
//! needs to resolve `Sdk="…"` references.
//!
//! The msbuild crate's [`locate_dotnet_sdk`] is deliberately policy-free:
//! it takes a `$DOTNET_ROOT`, an optional `$NUGET_PACKAGES`, an optional
//! [`VersionSpec`] (typically built from `global.json`), and an optional
//! `msbuild-sdks` pin map. Choosing those values is the host's job.
//! This module is that host — it sniffs the process environment, walks
//! upward from a project file looking for `global.json`, and packages
//! the discovered context into a [`SdkDiscovery`] whose
//! [`SdkDiscovery::resolve`] method can be wrapped as the
//! [`SdkResolver`] closure.
//!
//! ## `$DOTNET_ROOT` discovery
//!
//! Most users don't set `$DOTNET_ROOT` explicitly — the installer drops
//! `dotnet` somewhere on `$PATH` and calls it a day. We follow the same
//! breadcrumb the .NET host uses: if `$DOTNET_ROOT` is unset, walk
//! `$PATH` for a `dotnet` binary, canonicalise it (the installer often
//! lays down `/usr/local/bin/dotnet` as a symlink into the real install
//! root), and take the parent of the resolved path.
//!
//! Wrapper layouts (Nix's `makeWrapper`, asdf/mise shims, …) defeat
//! that breadcrumb because the canonicalised wrapper sits in a `bin/`
//! directory with no `sdk/` sibling. When the PATH walk fails to find
//! any candidate with an adjacent `sdk/`, we fall back to running
//! `dotnet --info` on the first viable executable and parsing the
//! `.NET SDKs installed:` section. The wrapper's job is to forward to
//! the real binary, so `--info` reports the install paths even when
//! we can't reach them directly.
//!
//! ## `$NUGET_PACKAGES` discovery
//!
//! NuGet itself defaults to `$HOME/.nuget/packages` (Unix) /
//! `%USERPROFILE%\.nuget\packages` (Windows) when the env var is unset.
//! We replicate that default here so `locate_dotnet_sdk` has somewhere
//! to look for `Sdk="Name/Version"` pins backed by a NuGet package.
//!
//! [`locate_dotnet_sdk`]: borzoi_msbuild::locate_dotnet_sdk
//! [`SdkResolver`]: borzoi_msbuild::SdkResolver

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use borzoi_msbuild::{
    GlobalJsonError, SdkPathEntry, SdkResolution, SdkResolveError, SdkVersion, VersionSpec,
    find_global_json, parse_global_json, resolve_sdk, workloads,
};

/// Errors that can occur while gathering SDK-resolution context for a
/// project. A missing `global.json` is *not* an error (it means "no
/// pin, use latest"); IO and parse failures of an *existing* one are.
#[derive(Debug)]
pub enum DiscoveryError {
    /// `project_path` was relative. The upward `global.json` walk
    /// only follows lexical parents, so a relative path like
    /// `App.fsproj` would stop at the project's own directory and
    /// miss any `global.json` in a real ancestor. Mirrors
    /// `parse_fsproj_with_imports`'s rejection of relative project
    /// paths — callers must absolutise before either call.
    RelativeProjectPath(PathBuf),
    /// Neither `$DOTNET_ROOT` was set nor was a `dotnet` binary
    /// findable on `$PATH`. Without one we don't know where to look
    /// for installed SDKs.
    MissingDotnetRoot,
    /// `find_global_json` found a `global.json` but reading it failed.
    GlobalJsonRead {
        path: PathBuf,
        source: std::io::Error,
    },
    /// `parse_global_json` rejected the document.
    GlobalJsonParse {
        path: PathBuf,
        source: GlobalJsonError,
    },
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelativeProjectPath(path) => {
                write!(f, "project path must be rooted (got {})", path.display())
            }
            Self::MissingDotnetRoot => f.write_str(
                "could not locate a .NET SDK install: \
                 neither $DOTNET_ROOT was set nor was `dotnet` found on $PATH",
            ),
            Self::GlobalJsonRead { path, source } => {
                write!(
                    f,
                    "failed to read global.json at {}: {source}",
                    path.display()
                )
            }
            Self::GlobalJsonParse { path, source } => {
                write!(
                    f,
                    "failed to parse global.json at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RelativeProjectPath(_) | Self::MissingDotnetRoot => None,
            Self::GlobalJsonRead { source, .. } => Some(source),
            Self::GlobalJsonParse { source, .. } => Some(source),
        }
    }
}

/// Inputs sniffed from the process environment. Held as a value so
/// tests can construct one without mutating the global environment;
/// production code calls [`Self::from_process_env`].
///
/// The [`Default`] impl matches [`Self::from_process_env`]'s prerelease
/// policy (CLI host, `true`). The auto-derived default would set it
/// to `false`, silently filtering prereleases for callers using
/// `SdkDiscoveryEnv { dotnet_root: Some(_), ..Default::default() }`.
#[derive(Debug, Clone)]
pub struct SdkDiscoveryEnv {
    /// `$DOTNET_ROOT`, if set. When `None`, [`SdkDiscovery::for_project`]
    /// falls back to a `$PATH` search.
    pub dotnet_root: Option<PathBuf>,
    /// `$NUGET_PACKAGES`, if set. When `None`, we synthesise
    /// `<home>/.nuget/packages` from [`Self::home_dir`].
    pub nuget_packages_dir: Option<PathBuf>,
    /// `$HOME` on Unix, `%USERPROFILE%` on Windows, if set to a
    /// non-empty value (empty counts as unset, as everywhere .NET
    /// consults these). Consulted as a fallback for `$NUGET_PACKAGES`
    /// and, after [`Self::dotnet_cli_home`], for the user-local dotnet
    /// root; `None` here just means those fallbacks can't fire.
    pub home_dir: Option<PathBuf>,
    /// `$PATH`. Only consulted as a fallback for `$DOTNET_ROOT`.
    pub search_path: Option<OsString>,
    /// Host policy for prerelease pickup: the .NET CLI host passes
    /// `true`, the Visual Studio host passes `false`. We're a CLI host.
    /// This default can still be overridden inside a `global.json` via
    /// `sdk.allowPrerelease`.
    pub host_default_allow_prerelease: bool,
    /// `$DOTNET_CLI_HOME`, if set to a non-empty value (.NET's
    /// `CliFolderPathCalculatorCore` checks `string.IsNullOrEmpty`, so
    /// an empty value falls back to the platform home rather than
    /// producing a relative `.dotnet` root). With [`Self::home_dir`] it
    /// derives the user-local dotnet root
    /// (`{DOTNET_CLI_HOME ?? home}/.dotnet`) that workload locator
    /// resolution consults when the install carries a `userlocal`
    /// marker.
    pub dotnet_cli_home: Option<PathBuf>,
    /// True when any workload-resolution override variable is set
    /// (`DOTNETSDK_WORKLOAD_MANIFEST_ROOTS`,
    /// `DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS`,
    /// `DOTNETSDK_WORKLOAD_PACK_ROOTS`). The in-house locator resolution
    /// degrades rather than model those redirections.
    pub workload_overrides_present: bool,
    /// The environment MSBuild would fold in as initial properties when
    /// evaluating a project under this configuration.
    ///
    /// This lives here, alongside the SDK-discovery inputs, so that an
    /// explicitly-constructed environment is hermetic *by construction*:
    /// [`Self::from_process_env`] fills it with the real process environment,
    /// while a hand-built value (what [`crate::workspace::Workspace::with_env`]
    /// takes, and what the test harnesses pass) defaults to empty. Reading
    /// `std::env` at the evaluation site instead would let a host variable
    /// named `FOO` change how `$(FOO)` evaluates in a workspace that had
    /// explicitly asked not to see the host.
    ///
    /// An empty map is a *claim* — see `parse_fsproj`'s docs — not an absence
    /// of information: it says "no environment variables are set", which is
    /// what a hermetic caller means.
    pub build_environment: HashMap<String, String>,
}

impl Default for SdkDiscoveryEnv {
    fn default() -> Self {
        Self {
            dotnet_root: None,
            nuget_packages_dir: None,
            home_dir: None,
            search_path: None,
            host_default_allow_prerelease: true,
            dotnet_cli_home: None,
            workload_overrides_present: false,
            build_environment: HashMap::new(),
        }
    }
}

impl SdkDiscoveryEnv {
    /// Read every relevant env var from the current process. The
    /// `host_default_allow_prerelease` is hard-coded to `true` (CLI
    /// host); callers wanting the VS-host default can build the struct
    /// literally.
    pub fn from_process_env() -> Self {
        let mut env = Self::from_env_lookup(|name| std::env::var_os(name));
        // .NET's CliFolderPathCalculatorCore falls back to
        // `Environment.GetFolderPath(UserProfile)` — the OS account
        // profile — when the home env vars are absent or empty (some
        // service/GUI launch environments strip them). Rust's
        // `std::env::home_dir` mirrors that chain (env vars first,
        // then the OS account database: `GetUserProfileDirectoryW` on
        // Windows, getpwuid on Unix), so a user-local workload install
        // still resolves rather than degrading. This effectful lookup
        // lives here, not in the pure `from_env_lookup` core.
        if env.home_dir.is_none() {
            env.home_dir = std::env::home_dir().filter(|home| !home.as_os_str().is_empty());
        }
        // The MSBuild property environment: the raw process env plus the
        // reserved-ish properties MSBuild computes itself
        // (`MSBuildUserExtensionsPath`), which a real editor evaluation needs to
        // reach certainty on the SDK import chain — the user-extension import
        // gates in `Microsoft.Common.props` read it, and an undefined read turns
        // the whole walk opaque. Effectful (reads the folder-derivation env), so
        // it lives here rather than in the pure `from_env_lookup` core. `home_dir`
        // is already resolved above.
        env.build_environment = crate::fsproj_diagnostics::msbuild_property_environment(
            crate::fsproj_diagnostics::process_environment(),
            env.home_dir.as_deref(),
            |name| std::env::var_os(name),
        );
        env
    }

    /// Core of [`Self::from_process_env`] with `std::env::var_os`
    /// abstracted out, so tests can drive the env-interpretation rules
    /// (e.g. empty-means-unset) without mutating the process
    /// environment.
    fn from_env_lookup(get: impl Fn(&str) -> Option<OsString>) -> Self {
        // Empty home-ish env vars count as unset: .NET's
        // CliFolderPathCalculatorCore checks `string.IsNullOrEmpty`,
        // and keeping `Some("")` would later derive *relative* roots
        // (`.dotnet`, `.nuget/packages`) against whatever the process
        // cwd happens to be.
        let non_empty = |name: &str| get(name).filter(|value| !value.is_empty());
        // NuGet's documented default is `%USERPROFILE%\.nuget\packages`
        // on Windows and `$HOME/.nuget/packages` everywhere else. On
        // Windows both vars are commonly populated (Git Bash / MSYS /
        // Cygwin set `HOME` to a POSIX-style path that's *not* where
        // NuGet writes), so we have to prefer `USERPROFILE` there or
        // we'd miss packages restored by `dotnet`.
        let home_dir = if cfg!(windows) {
            non_empty("USERPROFILE")
                .or_else(|| non_empty("HOME"))
                .map(PathBuf::from)
        } else {
            non_empty("HOME")
                .or_else(|| non_empty("USERPROFILE"))
                .map(PathBuf::from)
        };
        Self {
            // The pure core cannot enumerate the environment (it only has a
            // per-name lookup), so the property snapshot is filled by the
            // effectful `from_process_env`; a hand-built env stays hermetic.
            build_environment: HashMap::new(),
            dotnet_root: get("DOTNET_ROOT").map(PathBuf::from),
            nuget_packages_dir: get("NUGET_PACKAGES").map(PathBuf::from),
            home_dir,
            search_path: get("PATH"),
            host_default_allow_prerelease: true,
            dotnet_cli_home: non_empty("DOTNET_CLI_HOME").map(PathBuf::from),
            // Each variable mirrors how the workload machinery itself
            // reads it (dotnet/sdk, fetched 2026-07-10):
            // MANIFEST_ROOTS is a `!= null` check (an empty value still
            // prepends an empty — cwd-relative — manifest root);
            // IGNORE_DEFAULT_ROOTS is presence-only (`== null`), so
            // even `=false` drops the default roots; PACK_ROOTS goes
            // through `string.IsNullOrEmpty`, so an empty value is a
            // true no-op and must not degrade resolution.
            workload_overrides_present: get("DOTNETSDK_WORKLOAD_MANIFEST_ROOTS").is_some()
                || get("DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS").is_some()
                || non_empty("DOTNETSDK_WORKLOAD_PACK_ROOTS").is_some(),
        }
    }
}

/// SDK-resolution context discovered for a single project. Built by
/// [`Self::for_project`]; consumed via [`Self::resolve`], which the
/// caller wraps as a closure to hand to
/// `parse_fsproj_with_imports`.
///
/// The accessors ([`Self::roots`], [`Self::global_json_path`], …) exist
/// so the LSP can surface the resolved context to the user (in
/// diagnostics, in logs, in "show resolved project" requests).
///
/// `roots` is an *ordered* list of SDK install directories. .NET 10's
/// `global.json` `sdk.paths` field lets a workspace pin multiple roots
/// (e.g. a repo-local `./dotnet` plus the host install via `$host$`);
/// [`Self::resolve`] iterates them in order, first match wins. When no
/// `paths` field is present (the common case) the list carries a
/// single element — the host root.
#[derive(Debug, Clone)]
pub struct SdkDiscovery {
    roots: Vec<PathBuf>,
    nuget_packages_dir: Option<PathBuf>,
    /// Always populated. Without an `sdk` block (or without a
    /// `global.json` at all) this carries an unpinned spec built from
    /// the host's prerelease policy — necessary so the policy still
    /// applies. `locate_dotnet_sdk` treats `spec=None` as "no
    /// constraint, accept prereleases", which would silently bypass a
    /// host that opted out.
    spec: VersionSpec,
    msbuild_sdks: BTreeMap<String, SdkVersion>,
    global_json_path: Option<PathBuf>,
    /// The user-local dotnet root (`{DOTNET_CLI_HOME ?? home}/.dotnet`)
    /// workload locator resolution consults when the install carries a
    /// `userlocal` marker. `None` when neither env var was available.
    user_dotnet_root: Option<PathBuf>,
    /// See [`SdkDiscoveryEnv::workload_overrides_present`].
    workload_overrides_present: bool,
    /// Whether the discovered `global.json` engages workload-set
    /// selection (`sdk.workloadVersion` — see
    /// `GlobalJson::pins_workload_set`). Workload locator resolution
    /// degrades when set: MSBuild hands the same `global.json` to its
    /// workload manifest provider, which then selects a workload set
    /// (or fails) instead of the loose manifests we enumerate. `false`
    /// when no `global.json` was found.
    global_json_pins_workload_set: bool,
    /// The non-empty `MSBuildSDKsPath` the build environment carried, if
    /// any. MSBuild routes `<Project Sdk="…">` resolution through this
    /// directory when it is set — probed against dotnet 8.0.420 and
    /// 10.0.301: `MSBuildSDKsPath=/nonexistent dotnet msbuild` on a
    /// `Microsoft.NET.Sdk` project fails with MSB4236, so a real build's
    /// resolution is contingent on a path this crate does not model
    /// (where the redirect lands is version- and resolver-dependent).
    /// When present, [`Self::resolve`] declines *every* name rather than
    /// resolve through [`Self::roots`] as if the override were absent,
    /// which would import a chain the real build never uses. There is no
    /// workload-locator exemption: MSBuild serves even a workload locator
    /// from the override when it holds a matching entry (probed), so that
    /// resolution is contingent on the override too. `None` when the name
    /// is unset or empty — MSBuild treats an empty value as unset
    /// (probed: it computes its own directory and resolution succeeds).
    unmodelled_sdks_path_override: Option<String>,
}

impl SdkDiscovery {
    /// Gather SDK-resolution context for the project at `project_path`.
    ///
    /// `project_path` must be rooted (matching
    /// `parse_fsproj_with_imports`'s contract). The upward
    /// `global.json` walk is rooted at the project's parent directory;
    /// a relative path's lexical parents do not include the real CWD's
    /// ancestors, so a `global.json` higher up would be silently
    /// missed. `project_path` itself is not required to exist on
    /// disk — only its ancestor directories are probed.
    pub fn for_project(project_path: &Path, env: &SdkDiscoveryEnv) -> Result<Self, DiscoveryError> {
        if !project_path.has_root() {
            return Err(DiscoveryError::RelativeProjectPath(
                project_path.to_path_buf(),
            ));
        }
        // Collapse `.` and `..` lexically before walking. Without
        // this, `/repo/a/../b/App.fsproj` would walk through `/repo/a`
        // (not a real ancestor!) and could pick up an unrelated
        // `global.json` there. We can't canonicalise via the
        // filesystem — the project file isn't required to exist on
        // disk and ancestor symlinks may not be resolved — but a
        // lexical pass suffices for the cases the LSP actually hits.
        let normalised = normalise_lexically(project_path);
        let walk_start: PathBuf = normalised
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // `walk_start` is also the cwd we hand to the `dotnet --info`
        // fallback. asdf/mise shims pick the active toolchain by
        // walking upward from cwd looking for `.tool-versions` /
        // `mise.toml`, so invoking the shim from our process cwd (the
        // editor's launch dir — often `$HOME`) would silently route
        // through the wrong SDK. Anchoring to the project dir matches
        // what `dotnet build` would do from a terminal in that dir.
        let nuget_packages_dir = resolve_nuget_packages_dir(env);
        // Host root resolution is conditionally fatal: required when
        // there's no `sdk.paths` field (today's behaviour preserved),
        // optional when `sdk.paths` is set (it may name explicit
        // alternatives that don't need the host). The decision lives
        // in [`expand_sdk_paths`] so this site doesn't have to know.
        let (roots, spec, msbuild_sdks, global_json_path, global_json_pins_workload_set) =
            match find_global_json(&walk_start) {
                Some(path) => {
                    let text = std::fs::read_to_string(&path).map_err(|source| {
                        DiscoveryError::GlobalJsonRead {
                            path: path.clone(),
                            source,
                        }
                    })?;
                    let parsed = parse_global_json(&text).map_err(|source| {
                        DiscoveryError::GlobalJsonParse {
                            path: path.clone(),
                            source,
                        }
                    })?;
                    let (paths, spec) = match parsed.sdk {
                        Some(mut s) => {
                            let paths = s.paths.take();
                            (paths, s.into_spec(env.host_default_allow_prerelease))
                        }
                        None => (
                            None,
                            VersionSpec::any_version(env.host_default_allow_prerelease),
                        ),
                    };
                    let roots = expand_sdk_paths(env, &walk_start, &path, paths)?;
                    (
                        roots,
                        spec,
                        parsed.msbuild_sdks,
                        Some(path),
                        parsed.pins_workload_set,
                    )
                }
                None => {
                    let host_root = resolve_dotnet_root(env, &walk_start)
                        .ok_or(DiscoveryError::MissingDotnetRoot)?;
                    (
                        vec![host_root],
                        VersionSpec::any_version(env.host_default_allow_prerelease),
                        BTreeMap::new(),
                        None,
                        false,
                    )
                }
            };

        Ok(Self {
            roots,
            nuget_packages_dir,
            spec,
            msbuild_sdks,
            global_json_path,
            global_json_pins_workload_set,
            // First *non-empty* candidate, mirroring .NET's
            // `string.IsNullOrEmpty` fallback chain. `from_env_lookup`
            // already filters empties from the process env; this guard
            // covers hand-built `SdkDiscoveryEnv` values, where an
            // empty entry would otherwise derive the cwd-relative
            // user root `.dotnet`.
            user_dotnet_root: [env.dotnet_cli_home.as_deref(), env.home_dir.as_deref()]
                .into_iter()
                .flatten()
                .find(|home| !home.as_os_str().is_empty())
                .map(|home| home.join(".dotnet")),
            workload_overrides_present: env.workload_overrides_present,
            unmodelled_sdks_path_override: build_env_sdks_path_override(&env.build_environment),
        })
    }

    /// Resolve a single SDK reference. Wrap as a closure to pass to
    /// `parse_fsproj_with_imports`:
    ///
    /// ```ignore
    /// let disc = SdkDiscovery::for_project(&path, &env)?;
    /// let r: &SdkResolver = &|name| disc.resolve(name);
    /// let parsed = parse_fsproj_with_imports(src, &path, &extra, Some(r), None)?;
    /// ```
    ///
    /// Iterates [`Self::roots`] in order, calling [`resolve_sdk`] for
    /// each (which routes the workload locator SDK names through their
    /// layout-envelope resolution and everything else through
    /// `locate_dotnet_sdk`). First `Ok` wins (matching the .NET host's
    /// first-match semantics). An `UnsupportedLayout` from any root
    /// short-circuits: the root MSBuild *would* use has a workload
    /// state we cannot resolve exactly, and trying a lower-priority
    /// root instead could produce a different file set than a real
    /// build. On all-error outcomes the result is `NotFound` unless at
    /// least one root reported `VersionNotSatisfied`, in which case the
    /// returned `available` list is the dedup-sorted union across the
    /// consulted roots — the most informative error for a workspace
    /// with a pinned `sdk.version` and multiple roots.
    pub fn resolve(&self, sdk_name: &str) -> Result<SdkResolution, SdkResolveError> {
        // A build-environment `MSBuildSDKsPath` reroutes MSBuild's SDK
        // resolution through the default resolver's `$(MSBuildSDKsPath)/<name>/
        // Sdk` directory (see the field's doc). We do not model where it lands,
        // so resolving through `roots` here would be a guess; decline instead,
        // the same "degrade, don't guess" contract `resolve_sdk` uses for
        // layouts it cannot resolve exactly.
        //
        // The decline covers *every* name — no exemption for version pins or
        // workload locators. A version pin does not force NuGet resolution:
        // MSBuild's default resolver still serves a pinned name straight from
        // `MSBuildSDKsPath` when NuGet has nothing restored for it (probed
        // against dotnet 8.0.420 — a `Sdk="MySdk"` with `msbuild-sdks={MySdk:
        // 1.2.3}` and `MySdk/Sdk` under `MSBuildSDKsPath` resolves from that
        // directory). A workload locator is *also* served from the override
        // when it contains a matching entry (probed against dotnet 10.0.301: an
        // `MSBuildSDKsPath/Microsoft.NET.SDK.WorkloadAutoImportPropsLocator/Sdk`
        // is imported in preference to the workload resolver's empty result), so
        // its resolution too depends on a path we do not model. Whether NuGet or
        // the override has any given name is runtime state we decline to
        // reimplement the resolver cascade to predict — so we decline the lot,
        // over-declining a name MSBuild could resolve independently rather than
        // risk committing a resolution the override changes (correctness over
        // availability; declining is always sound — the walker degrades).
        if let Some(path) = &self.unmodelled_sdks_path_override {
            return Err(SdkResolveError::UnsupportedLayout {
                reason: format!(
                    "the build environment sets MSBuildSDKsPath={path}, which reroutes \
                     SDK resolution in a way this resolver does not model"
                ),
            });
        }
        let workload_env = workloads::WorkloadEnvironment {
            user_dotnet_root: self.user_dotnet_root.as_deref(),
            overrides_present: self.workload_overrides_present,
            global_json_pins_workload_set: self.global_json_pins_workload_set,
        };
        let locate = |root: &Path, name: &str| {
            resolve_sdk(
                root,
                self.nuget_packages_dir.as_deref(),
                name,
                Some(&self.spec),
                Some(&self.msbuild_sdks),
                &workload_env,
            )
        };
        resolve_across_roots(&self.roots, sdk_name, &locate)
    }

    /// The ordered list of SDK install roots [`Self::resolve`] consults.
    /// Empty when an explicit `paths: []` (or all-`$host$` with host
    /// resolution failed) leaves no usable root; queries on an
    /// empty-roots discovery return [`SdkResolveError::NotFound`].
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn nuget_packages_dir(&self) -> Option<&Path> {
        self.nuget_packages_dir.as_deref()
    }

    /// The constraint applied during resolution. Always present (see
    /// the struct field's doc for why `None` would be unsafe). To
    /// distinguish "from a `global.json`" from "synthesised from host
    /// policy", check [`Self::global_json_path`].
    pub fn spec(&self) -> &VersionSpec {
        &self.spec
    }

    pub fn msbuild_sdks(&self) -> &BTreeMap<String, SdkVersion> {
        &self.msbuild_sdks
    }

    /// Path of the `global.json` that informed this discovery, or
    /// `None` if no `global.json` was found above `project_path`.
    pub fn global_json_path(&self) -> Option<&Path> {
        self.global_json_path.as_deref()
    }
}

/// On Windows the launcher binary is `dotnet.exe`; everywhere else
/// it's `dotnet`. We don't honour `%PATHEXT%` — the .NET installer
/// always writes `dotnet.exe` (not `dotnet.cmd` or similar), so a
/// single-name probe is sufficient.
#[cfg(windows)]
const DOTNET_BIN: &str = "dotnet.exe";
#[cfg(not(windows))]
const DOTNET_BIN: &str = "dotnet";

/// Pure-ish lookup: explicit `$DOTNET_ROOT` wins; otherwise walk
/// `$PATH` for the `dotnet` launcher, accepting only entries whose
/// canonicalised parent actually contains an `sdk/` directory. The
/// second check guards against asdf/mise/etc. shim scripts that exec
/// the real `dotnet` from elsewhere — `canonicalize` returns the
/// shim's own path in that case, and its parent is the shims
/// directory (no `sdk/`). When we hit one we keep iterating, in case
/// the user has both a shim and a real install on `$PATH`.
///
/// If every `$PATH` candidate fails the adjacent-`sdk/` check, we
/// invoke `dotnet --info` on the first candidate that at least exists
/// and parse the `.NET SDKs installed:` section. That covers Nix-style
/// wrappers whose canonicalised parent is a `bin/` directory without
/// an `sdk/` sibling.
///
/// Project a `global.json` `sdk.paths` field into the ordered list of
/// SDK install roots [`SdkDiscovery::resolve`] consults.
///
/// `paths == None` is the "no `paths` field present" case (the common
/// one): the result is a single-element vec carrying the discovered
/// host root, and a failure to resolve that root surfaces as
/// [`DiscoveryError::MissingDotnetRoot`] — bit-for-bit today's
/// behaviour for workspaces with no `paths` field.
///
/// `paths == Some(entries)` is the .NET-10 opt-in: each entry projects
/// independently and the host root becomes optional. `Host` entries
/// expand to the discovered host root if available, else are *skipped*
/// (with a stderr log line) so a `paths: ["$host$", "./alt"]` setup
/// on a machine without `dotnet` still resolves via `./alt`. `Relative`
/// entries are joined against `global_json_path`'s parent directory —
/// not the project's or the process's cwd — matching the .NET host's
/// reading of the field. No canonicalisation and no existence check:
/// `locate_dotnet_sdk` treats a missing directory as "no SDKs here",
/// which is the natural fall-through behaviour and avoids racing the
/// filesystem from this site.
///
/// Empty result (`Some([])` opt-out, or a list whose only `Host` entry
/// got skipped) is well-defined and not an error here — the resolver
/// will simply return `NotFound` for every lookup. That matches the
/// strict reading of `paths: []` (the workspace is opting out of the
/// host install and the LSP must not paper over it).
fn expand_sdk_paths(
    env: &SdkDiscoveryEnv,
    walk_start: &Path,
    global_json_path: &Path,
    paths: Option<Vec<SdkPathEntry>>,
) -> Result<Vec<PathBuf>, DiscoveryError> {
    let Some(entries) = paths else {
        // No `paths` field in `global.json` ⇒ host-only, strict.
        let host_root =
            resolve_dotnet_root(env, walk_start).ok_or(DiscoveryError::MissingDotnetRoot)?;
        return Ok(vec![host_root]);
    };
    let global_json_dir = global_json_path
        .parent()
        .expect("find_global_json returns a file path with a parent");
    // Resolve the host once, up front — entries can reference `$host$`
    // multiple times (unusual but legal) and we want a single decision
    // about whether the host is available.
    //
    // `$host$` deliberately uses [`resolve_host_dotnet_root`] (no
    // `--info` fallback) rather than the general [`resolve_dotnet_root`].
    // Running `dotnet --info` inside a workspace whose own `sdk.paths`
    // we're resolving lets the muxer's SDK selection feed back into the
    // path we recover, so `$host$` could silently expand to one of the
    // relative entries (or to whatever SDK the workspace pins).
    // Wrapper-layout users who need `$host$` to resolve can set
    // `$DOTNET_ROOT` explicitly — there's no ambient signal the
    // resolver can trust here.
    let host_root = resolve_host_dotnet_root(env);
    let roots = entries
        .into_iter()
        .filter_map(|entry| match entry {
            SdkPathEntry::Host => host_root.clone().or_else(|| {
                crate::log_warn!(
                    "sdk.paths skipping `$host$` (no DOTNET_ROOT and no `dotnet` on PATH with an adjacent `sdk/` directory)",
                    global_json = global_json_path.display()
                );
                None
            }),
            SdkPathEntry::Relative(s) => Some(global_json_dir.join(s)),
        })
        .collect();
    Ok(roots)
}

/// Outcome of the lexical PATH probe. `Direct` is a verified install
/// root (its parent has an `sdk/` directory); `Wrapper` is the
/// canonicalised path of a `dotnet` executable whose layout doesn't
/// match an install (asdf/mise/Nix shim, …), useful only as input to
/// the `--info` subprocess.
enum HostProbe {
    Direct(PathBuf),
    Wrapper(PathBuf),
}

/// PATH/env probe shared by [`resolve_dotnet_root`] and
/// [`resolve_host_dotnet_root`]. Pure-ish: touches the filesystem
/// (`is_file`, `canonicalize`, `is_dir`) but never spawns a
/// subprocess. Callers decide whether to spend a `--info` round-trip
/// on a `Wrapper` result.
fn probe_dotnet_root(env: &SdkDiscoveryEnv) -> Option<HostProbe> {
    if let Some(root) = &env.dotnet_root {
        return Some(HostProbe::Direct(root.clone()));
    }
    let path = env.search_path.as_ref()?;
    let mut wrapper_fallback: Option<PathBuf> = None;
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(DOTNET_BIN);
        if !candidate.is_file() {
            continue;
        }
        // The launcher is commonly installed as a symlink (e.g.
        // `/usr/local/bin/dotnet` → `/usr/local/share/dotnet/dotnet`);
        // we want the directory containing the *real* binary, since
        // that's where `sdk/` and `packs/` live.
        let Ok(resolved) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        let Some(parent) = resolved.parent() else {
            continue;
        };
        if parent.join("sdk").is_dir() {
            return Some(HostProbe::Direct(parent.to_path_buf()));
        }
        // Looks like a wrapper / shim — remember it and keep walking
        // in case a real install sits further down `$PATH`. We stash
        // the *canonicalised* path so the fallback subprocess works
        // even when `$PATH` had relative components (`.`, `tools`,
        // …): `run_dotnet_info` sets `current_dir` to the project
        // dir, and a relative `candidate` would then resolve against
        // that, spawning the wrong (or no) executable.
        wrapper_fallback.get_or_insert(resolved);
    }
    wrapper_fallback.map(HostProbe::Wrapper)
}

/// `cwd_hint` is used as the working directory for the `--info`
/// fallback only — see the caller's note on asdf/mise-style shims.
fn resolve_dotnet_root(env: &SdkDiscoveryEnv, cwd_hint: &Path) -> Option<PathBuf> {
    match probe_dotnet_root(env)? {
        HostProbe::Direct(root) => Some(root),
        HostProbe::Wrapper(wrapper) => {
            let info = run_dotnet_info(&wrapper, cwd_hint)?;
            parse_dotnet_info_sdk_root(&info)
        }
    }
}

/// Restricted variant of [`resolve_dotnet_root`] that *never* invokes
/// `dotnet --info`. Used when expanding `$host$` from `sdk.paths`,
/// where the only available `--info`-cwd would sit inside the
/// workspace whose `sdk.paths` we're resolving — running `--info`
/// there can let the workspace's own paths feed back into the result,
/// silently aliasing `$host$` to a relative entry. Returning `None`
/// for wrapper layouts here is the deliberate tradeoff: users on Nix
/// / asdf / mise who need `$host$` to resolve must set `$DOTNET_ROOT`
/// explicitly.
fn resolve_host_dotnet_root(env: &SdkDiscoveryEnv) -> Option<PathBuf> {
    match probe_dotnet_root(env)? {
        HostProbe::Direct(root) => Some(root),
        HostProbe::Wrapper(_) => None,
    }
}

/// Deadline for the `dotnet --info` fallback. A healthy invocation is
/// sub-second warm and a few seconds cold (first-run JIT + SDK
/// enumeration); a `dotnet` that takes a minute is wedged — first-run
/// migration deadlocks and shim loops have both been seen — and an
/// unbounded wait here would hang SDK discovery, and with it every
/// per-file request behind it, for the rest of the session.
const DOTNET_INFO_TIMEOUT: Duration = Duration::from_secs(60);

/// Execute `dotnet --info` in `cwd` and return its stdout as a UTF-8
/// string. Returns `None` on any kind of execution failure (exit code,
/// non-UTF-8 stdout, IO error, or exceeding [`DOTNET_INFO_TIMEOUT`]) —
/// callers treat that as "fall back to returning no `dotnet_root`".
///
/// We set the subprocess `current_dir` explicitly because asdf/mise
/// and similar shims pick the active toolchain by walking up from cwd
/// looking for `.tool-versions` / `mise.toml`. Inheriting the LSP's
/// own cwd (typically the editor's launch dir, often `$HOME`) would
/// silently resolve a different SDK than `dotnet build` would from a
/// terminal in the project directory.
fn run_dotnet_info(dotnet: &Path, cwd: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new(dotnet);
    // `--info` writes its payload to stdout; stderr is irrelevant and
    // may contain unrelated warnings (telemetry-first-run on some
    // distros, etc.). `output_bounded` captures it into a buffer we
    // drop, so it cannot leak noise into the LSP's stderr stream.
    cmd.arg("--info")
        .current_dir(cwd)
        // Avoid the telemetry first-run banner munging stdout on
        // freshly-extracted SDKs. These vars are idempotent — they
        // only matter when the corresponding files don't yet exist.
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1");
    let output = crate::spawn::output_bounded(cmd, DOTNET_INFO_TIMEOUT).ok()?;
    // Deliberately ignore `output.status`: in wrapper-only PATH setups
    // `dotnet --info` can print the `.NET SDKs installed:` section and
    // still exit non-zero (workload first-run temp setup failures,
    // unsatisfied project-cwd SDK selection, …). The downstream parser
    // is the real oracle — it returns `None` when the section is
    // missing or malformed, which is the right "fail if section can't
    // be recovered" behaviour.
    String::from_utf8(output.stdout).ok()
}

/// Recover a candidate `dotnet_root` from `dotnet --info` stdout.
///
/// Prefers the `Base Path:` line under `Runtime Environment:`, which
/// reports the directory of the SDK that dotnet actually selected
/// (accounting for `global.json`, `sdk.paths`, version rolls, …):
///
/// ```text
/// Runtime Environment:
///  OS Name:     ubuntu
///  ...
///  Base Path:   /usr/share/dotnet/sdk/8.0.401/
/// ```
///
/// The Base Path is `<dotnet_root>/sdk/<version>/`, so two `parent()`
/// steps yield `dotnet_root`. Using the selected SDK matters in
/// multi-root setups (e.g. the F# compiler repo's `sdk.paths`:
/// `[".dotnet", "$host$"]`): naively picking the first entry from
/// `.NET SDKs installed:` would otherwise return a non-selected root.
///
/// Falls back to the `.NET SDKs installed:` section for the rare
/// payloads where Base Path is absent — typically very old
/// `dotnet --info` outputs, or partial outputs from a non-zero exit:
///
/// ```text
/// .NET SDKs installed:
///   8.0.401 [/usr/share/dotnet/sdk]
///   9.0.100-preview.4 [/usr/share/dotnet/sdk]
/// ```
///
/// The bracketed path is `<dotnet_root>/sdk`, so one `parent()` step
/// yields `dotnet_root`. The first entry is fine in the fallback
/// regime: setups without Base Path are also unlikely to have
/// multiple roots, and a user with several roots wired together
/// should set `$DOTNET_ROOT` explicitly.
fn parse_dotnet_info_sdk_root(info: &str) -> Option<PathBuf> {
    parse_base_path(info).or_else(|| parse_sdks_installed(info))
}

/// Parse the `Base Path:` line — the directory of the SDK dotnet has
/// selected for this invocation. Returns `dotnet_root` (two parents
/// up from the SDK version directory).
fn parse_base_path(info: &str) -> Option<PathBuf> {
    for raw_line in info.lines() {
        let trimmed = raw_line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Base Path:") {
            let path = Path::new(rest.trim());
            // `<dotnet_root>/sdk/<version>/` → drop the version → drop
            // the `sdk` segment → `dotnet_root`.
            return path.parent().and_then(Path::parent).map(Path::to_path_buf);
        }
    }
    None
}

/// Parse the `.NET SDKs installed:` section as a last-resort fallback
/// when `Base Path:` is absent. The bracketed path is
/// `<dotnet_root>/sdk`, so one `parent()` step yields `dotnet_root`.
fn parse_sdks_installed(info: &str) -> Option<PathBuf> {
    let mut in_section = false;
    for raw_line in info.lines() {
        let trimmed = raw_line.trim();
        if trimmed == ".NET SDKs installed:" {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        // The list is indented; a non-indented line ends it. A blank
        // line also ends the section (some host versions emit one
        // before the next heading).
        if trimmed.is_empty() || !raw_line.starts_with([' ', '\t']) {
            return None;
        }
        let open = raw_line.find('[')?;
        let close = raw_line.rfind(']')?;
        if close <= open + 1 {
            return None;
        }
        let sdk_base = Path::new(raw_line[open + 1..close].trim());
        return sdk_base.parent().map(Path::to_path_buf);
    }
    None
}

/// Lexical normalisation: drop `.`, pop on `..`, preserve everything
/// else. Mirrors the helper in `borzoi-msbuild`'s
/// `imports::detect_implicit_imports`. Pure — does not touch the
/// filesystem.
fn normalise_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Iterate `roots` in order calling `locate` for each, with first-match
/// semantics: the first `Ok` wins and short-circuits, matching the
/// .NET host's behaviour.
///
/// Error aggregation: collect [`SdkResolveError::VersionNotSatisfied`]
/// `available` lists across consulted roots and, if no root yielded
/// `Ok`, return their dedup-sorted union under a single
/// `VersionNotSatisfied { spec, available }`. The returned `spec` is
/// the one from the *first* `VersionNotSatisfied` reported by
/// `locate` — this matters because `locate_dotnet_sdk` reports the
/// *effective* spec, which can be a per-import `Sdk="Name/Version"`
/// pin or a `msbuild-sdks` override rather than the discovery-wide
/// `global.json` spec; preserving it keeps the resulting diagnostic
/// accurate. In practice the spec is invariant across roots for a
/// given `sdk_name` lookup (it's derived from name + global.json +
/// per-import pin, not the root), so "first seen" is well-defined.
///
/// If the resulting union is empty (no root reported
/// `VersionNotSatisfied`, or every one did so with an empty
/// `available`), return [`SdkResolveError::NotFound`]. Empty `roots`
/// short-circuits to `NotFound` without calling `locate`.
///
/// Pure over `(roots, sdk_name, locate)` and total: the function is
/// extracted from [`SdkDiscovery::resolve`] specifically so the
/// iteration logic can be property-tested without touching disk —
/// tests substitute a deterministic `locate` that returns prepared
/// outcomes per root.
fn resolve_across_roots(
    roots: &[PathBuf],
    sdk_name: &str,
    locate: &dyn Fn(&Path, &str) -> Result<SdkResolution, SdkResolveError>,
) -> Result<SdkResolution, SdkResolveError> {
    let mut effective_spec: Option<VersionSpec> = None;
    let mut union: Vec<SdkVersion> = Vec::new();
    for root in roots {
        match locate(root, sdk_name) {
            Ok(resolution) => return Ok(resolution),
            Err(SdkResolveError::NotFound) => continue,
            Err(SdkResolveError::VersionNotSatisfied { spec, available }) => {
                if effective_spec.is_none() {
                    effective_spec = Some(spec);
                }
                union.extend(available);
            }
            // The root MSBuild would consult has workload state outside
            // the exactness envelope; falling through to another root
            // could resolve a *different* file set than a real build.
            // Degrade immediately.
            Err(err @ SdkResolveError::UnsupportedLayout { .. }) => return Err(err),
        }
    }
    if union.is_empty() {
        // Either no root reported `VersionNotSatisfied`, or every
        // one did but with an empty `available`. `locate_dotnet_sdk`
        // never emits the latter — it falls through to `NotFound`
        // itself when both probe roots are empty — but the fold
        // stays well-defined if some other locator ever does.
        Err(SdkResolveError::NotFound)
    } else {
        union.sort();
        union.dedup();
        Err(SdkResolveError::VersionNotSatisfied {
            spec: effective_spec.expect("non-empty union implies at least one VersionNotSatisfied"),
            available: union,
        })
    }
}

/// `$NUGET_PACKAGES` wins; otherwise fall back to NuGet's documented
/// default of `<home>/.nuget/packages`. Returns `None` if neither is
/// available — `locate_dotnet_sdk` takes the NuGet dir as `Option`, so
/// missing it just means "no NuGet fallback for pinned SDKs".
fn resolve_nuget_packages_dir(env: &SdkDiscoveryEnv) -> Option<PathBuf> {
    if let Some(dir) = &env.nuget_packages_dir {
        return Some(dir.clone());
    }
    env.home_dir
        .as_deref()
        // Guard hand-built envs: an empty home would derive the
        // cwd-relative `.nuget/packages`, which is nowhere NuGet
        // writes.
        .filter(|home| !home.as_os_str().is_empty())
        .map(|home| home.join(".nuget").join("packages"))
}

/// The non-empty `MSBuildSDKsPath` a build environment carries, if any (see
/// [`SdkDiscovery::unmodelled_sdks_path_override`]).
///
/// Matched case-insensitively on the key: MSBuild reads the exact-case env var
/// (case-sensitive on Unix), so a variant spelling matching here can only make
/// us decline where the real build would resolve — an over-decline (an
/// unresolved SDK, never a wrong commit), which is the safe direction. An
/// empty value is treated as unset, mirroring MSBuild (probed: an empty
/// `MSBuildSDKsPath` leaves resolution working against the toolset's own dir).
///
/// The scan returns the *first non-empty* case-insensitive match rather than
/// an arbitrary one: on Unix an exact non-empty `MSBuildSDKsPath` can coexist
/// with an empty case variant, and picking the empty one would miss the real
/// override that MSBuild does read.
fn build_env_sdks_path_override(build_environment: &HashMap<String, String>) -> Option<String> {
    build_environment
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("MSBuildSDKsPath"))
        .map(|(_, value)| value)
        .find(|value| !value.is_empty())
        .cloned()
}

#[cfg(test)]
mod tests;
