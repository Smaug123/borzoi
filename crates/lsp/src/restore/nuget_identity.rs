//! The identity of the NuGet that restores a project: the
//! `AssemblyInformationalVersion` of `NuGet.Commands.dll` in the SDK the
//! muxer runs for it (`docs/nuget-dgspec-boundary-plan.md`, *Per-SDK drift*).
//! B₁ commits only for an identity on its verified list, so this is the key
//! its trust turns on.
//!
//! Found without running anything: the host SDK is the one SDK discovery
//! selects ([`SdkDiscovery::host_sdk_dir`]), and the version is read from the
//! assembly's metadata. `tests/all/restore_graph.rs` holds the answer to the
//! identity the oracle reports for the `NuGet.Commands` that loaded in the
//! SDK `dotnet` itself selects, under a `global.json` pinning each installed
//! SDK in turn.
//!
//! That is the NuGet restore runs only by default. MSBuild imports
//! `$(NuGetRestoreTargets)` (the host SDK's `NuGet.targets` unless set), whose
//! tasks load from `$(RestoreTaskAssemblyFile)` (`NuGet.Build.Tasks.dll`
//! beside it unless set), and NuGet loads beside its tasks. A project, its
//! imports or its environment can set either, and restore then runs another
//! NuGet. So the project is evaluated as the restore evaluates it
//! ([`crate::workspace::restore_task_routing`]), and anything but the default
//! route, with both values trusted, declines: following a redirect would mean
//! modelling where it lands.
//!
//! Residual: the restore runs the muxer of `dotnet_root`, while discovery
//! takes its host root from the environment. Where the SDK discovery selects
//! is not in `dotnet_root`, this declines; where it is, a `global.json` whose
//! `sdk.paths` lists `$host$` among other roots could still have the muxer
//! choose another root first.

use std::fmt;
use std::path::{Path, PathBuf};

use borzoi_assembly::Ecma335Assembly;
use borzoi_nuget::dgspec::NuGetIdentity;

use crate::paths::paths_equal;
use crate::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};

/// Why the restoring NuGet's identity cannot be said.
#[derive(Debug)]
pub enum IdentityDecline {
    /// SDK discovery failed for the project.
    Discovery(String),
    /// No installed SDK is the one the muxer would select.
    NoHostSdk(String),
    /// The selected SDK is not in the install whose muxer runs the restore.
    OtherInstall { sdk: PathBuf, dotnet_root: PathBuf },
    /// `NuGet.Commands.dll` could not be read or names no version.
    Unreadable { path: PathBuf, why: String },
    /// The project did not evaluate, so its route to NuGet is unknown.
    NotEvaluated,
    /// The evaluation routes restore to task code other than the host SDK's
    /// own, or cannot say where.
    Redirected {
        task_assembly_file: EvaluatedProperty,
        restore_targets: EvaluatedProperty,
    },
}

/// One evaluated property, as far as the evaluation can vouch for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluatedProperty {
    /// No evaluated file assigns it.
    Unset,
    /// Its value's provenance is untrusted.
    Untrusted,
    Trusted(String),
}

/// How a project's evaluation routes restore to NuGet's task code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTaskRouting {
    /// `$(RestoreTaskAssemblyFile)`: the task assembly `NuGet.targets` loads.
    pub task_assembly_file: EvaluatedProperty,
    /// `$(NuGetRestoreTargets)`: the restore targets file MSBuild imports.
    pub restore_targets: EvaluatedProperty,
}

/// The value `NuGet.targets` gives `RestoreTaskAssemblyFile` when nothing has
/// set it: the task assembly beside the targets file.
const DEFAULT_TASK_ASSEMBLY_FILE: &str = "NuGet.Build.Tasks.dll";

/// Whether `routing` is the default route into the SDK at `sdk`: its own
/// `NuGet.targets`, loading the task assembly beside it.
fn routes_to_host(routing: &RestoreTaskRouting, sdk: &Path) -> bool {
    let EvaluatedProperty::Trusted(targets) = &routing.restore_targets else {
        return false;
    };
    // MSBuild spells the default `$(MSBuildToolsPath)\NuGet.targets`.
    let targets = PathBuf::from(targets.replace('\\', "/"));
    let same_file = match (
        std::fs::canonicalize(&targets),
        std::fs::canonicalize(sdk.join("NuGet.targets")),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    same_file
        && routing.task_assembly_file
            == EvaluatedProperty::Trusted(DEFAULT_TASK_ASSEMBLY_FILE.to_owned())
}

impl fmt::Display for IdentityDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(why) => write!(f, "SDK discovery failed: {why}"),
            Self::NoHostSdk(why) => write!(f, "no host SDK: {why}"),
            Self::OtherInstall { sdk, dotnet_root } => write!(
                f,
                "the selected SDK {} is not in the install {} whose muxer restores",
                sdk.display(),
                dotnet_root.display()
            ),
            Self::Unreadable { path, why } => write!(f, "{}: {why}", path.display()),
            Self::NotEvaluated => f.write_str("the project does not evaluate"),
            Self::Redirected {
                task_assembly_file,
                restore_targets,
            } => write!(
                f,
                "restore is routed past the host SDK's NuGet: RestoreTaskAssemblyFile \
                 {task_assembly_file:?}, NuGetRestoreTargets {restore_targets:?}"
            ),
        }
    }
}

/// The identity of the NuGet that `dotnet_root`'s muxer restores `project`
/// with, under the environment `env` describes.
pub fn restoring_nuget(
    project: &Path,
    dotnet_root: &Path,
    env: &SdkDiscoveryEnv,
) -> Result<NuGetIdentity, IdentityDecline> {
    let discovery = SdkDiscovery::for_project(project, env)
        .map_err(|e| IdentityDecline::Discovery(e.to_string()))?;
    let sdk = discovery
        .host_sdk_dir()
        .map_err(|e| IdentityDecline::NoHostSdk(e.to_string()))?;
    let install = sdk.parent().and_then(Path::parent);
    if !install.is_some_and(|install| paths_equal(install, dotnet_root)) {
        return Err(IdentityDecline::OtherInstall {
            sdk,
            dotnet_root: dotnet_root.to_path_buf(),
        });
    }
    let routing = crate::workspace::restore_task_routing(project, env)
        .ok_or(IdentityDecline::NotEvaluated)?;
    if !routes_to_host(&routing, &sdk) {
        return Err(IdentityDecline::Redirected {
            task_assembly_file: routing.task_assembly_file,
            restore_targets: routing.restore_targets,
        });
    }
    nuget_commands_identity(&sdk.join("NuGet.Commands.dll"))
}

/// The informational version of the `NuGet.Commands.dll` at `path`.
pub fn nuget_commands_identity(path: &Path) -> Result<NuGetIdentity, IdentityDecline> {
    let unreadable = |why: String| IdentityDecline::Unreadable {
        path: path.to_path_buf(),
        why,
    };
    let bytes = std::fs::read(path).map_err(|e| unreadable(e.to_string()))?;
    let assembly = Ecma335Assembly::parse(&bytes).map_err(|e| unreadable(format!("{e:?}")))?;
    match assembly.informational_version() {
        Ok(Some(version)) => Ok(NuGetIdentity::new(version)),
        Ok(None) => Err(unreadable("no AssemblyInformationalVersion".to_owned())),
        Err(e) => Err(unreadable(format!("{e:?}"))),
    }
}
