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
