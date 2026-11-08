//! The environment restore runs under, as B₁ reads it.
//!
//! NuGet reads environment variables while restoring, and some of them move
//! what a restore computes: `NUGET_USE_LEGACY_ASSET_TARGET_FALLBACK_DEPENDENCY_RESOLUTION`
//! drops the dependencies of a group chosen through the asset-target fallback
//! list, and the home variables locate a user config whose failure to load
//! fails the restore, whatever the spec says. So [`resolve_dgspec`] takes the
//! environment the restore would run under, and every variable in it has a
//! verdict ([`environment_variable`]):
//!
//! - **Modelled.** `DOTNET_CLI_HOME` and `HOME` locate the user config folder
//!   ([`user_config_folder`]), whose config files B₁ holds to the vocabulary
//!   it holds the spec's to.
//! - **Inert** ([`INERT_ENVIRONMENT_VARIABLES`]). Each is graded by
//!   `dgspec_resolve_diff.rs` set to a value that would move a restore that
//!   read it.
//! - **Declined.** Every other variable named with a `NUGET_` prefix, in any
//!   case, and the `DOTNET_` variables NuGet reads that are not admitted.
//!   NuGet builds some names at run time (`NUGET_RETRY_HTTP_` is a prefix),
//!   so a `NUGET_` variable declines whether or not NuGet is known to read it.
//! - **Unread.** Everything else. `dgspec_resolve_diff.rs` scans the verified
//!   NuGet's assemblies for every `NUGET_` and `DOTNET_` name they hold and
//!   fails if one is unread here.
//!
//! What this does not see: variables NuGet reaches only through the .NET
//! runtime. The runtime reads `HOME` (modelled) and the temporary folder
//! (`TMPDIR`), which restore writes to; B₁ assumes it is writable, as it
//! assumes the machine can run a restore at all. On Windows NuGet finds the
//! user config through `APPDATA` instead, which is not modelled, so every
//! Windows host declines.
//!
//! [`resolve_dgspec`]: super::resolve_dgspec

use super::resolve::config_is_admitted;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

/// The variables NuGet reads that cannot move a restore B₁ commits to, each
/// with its reason:
///
/// - `NUGET_PACKAGES`, `NUGET_FALLBACK_PACKAGES`: the SDK resolves them into
///   the spec's `packagesPath` and `fallbackFolders`, and restore reads the
///   spec's.
/// - `NUGET_HTTP_CACHE_PATH`, `NUGET_PLUGINS_CACHE_PATH`, `NUGET_PLUGIN_PATHS`,
///   `NUGET_NETCORE_PLUGIN_PATHS`, `NUGET_NETFX_PLUGIN_PATHS`,
///   `DOTNET_HOST_PATH`: the HTTP cache and the credential plugins (which NuGet
///   launches through the .NET host) serve HTTP sources, and B₁ admits only
///   local ones. The CLI sets `DOTNET_HOST_PATH` for every MSBuild node.
/// - `NUGET_XMLDOC_MODE`, `NUGET_CERT_REVOCATION_MODE`,
///   `DOTNET_NUGET_SIGNATURE_VERIFICATION`: package extraction and signature
///   verification, which a restore from a warm packages folder does not do.
/// - `NUGET_CONCURRENCY_LIMIT`: how many requests run at once.
/// - `NUGET_SHOW_STACK`: whether an error's log message carries its stack.
/// - `DOTNET_CLI_UI_LANGUAGE`: the language of the CLI's messages, which
///   changes no log code.
pub const INERT_ENVIRONMENT_VARIABLES: &[&str] = &[
    "DOTNET_CLI_UI_LANGUAGE",
    "DOTNET_HOST_PATH",
    "DOTNET_NUGET_SIGNATURE_VERIFICATION",
    "NUGET_CERT_REVOCATION_MODE",
    "NUGET_CONCURRENCY_LIMIT",
    "NUGET_FALLBACK_PACKAGES",
    "NUGET_HTTP_CACHE_PATH",
    "NUGET_NETCORE_PLUGIN_PATHS",
    "NUGET_NETFX_PLUGIN_PATHS",
    "NUGET_PACKAGES",
    "NUGET_PLUGINS_CACHE_PATH",
    "NUGET_PLUGIN_PATHS",
    "NUGET_SHOW_STACK",
    "NUGET_XMLDOC_MODE",
];

/// The variables that locate the user config folder, most significant first.
const HOME_VARIABLES: &[&str] = &["DOTNET_CLI_HOME", "HOME"];

/// The `DOTNET_` variables the verified NuGet reads that B₁ neither admits
/// nor models. Empty today: the scan finds only admitted and modelled ones.
const DECLINED_DOTNET_VARIABLES: &[&str] = &[];

/// B₁'s verdict on one environment variable, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentVariable {
    /// It locates the user config folder.
    Modelled,
    /// NuGet reads it, and it cannot move a restore B₁ commits to.
    Inert,
    /// B₁ declines when it is set.
    Declined,
    /// NuGet does not read it.
    Unread,
}

/// B₁'s verdict on the variable `name`.
///
/// Names compare exactly for the modelled variables, which the runtime looks
/// up by their exact spelling, and ignoring ASCII case otherwise, which can
/// only decline more: a home variable spelled in another case declines.
pub fn environment_variable(name: &OsStr) -> EnvironmentVariable {
    let Some(name) = name.to_str() else {
        // NuGet's names are ASCII, so it cannot read this one, but a prefix
        // test on its bytes still declines one that looks like its own.
        return if starts_with_ignoring_case(name.as_encoded_bytes(), b"NUGET_") {
            EnvironmentVariable::Declined
        } else {
            EnvironmentVariable::Unread
        };
    };
    let is = |names: &[&str]| names.iter().any(|known| known.eq_ignore_ascii_case(name));
    if HOME_VARIABLES.contains(&name) {
        EnvironmentVariable::Modelled
    } else if is(HOME_VARIABLES) {
        EnvironmentVariable::Declined
    } else if is(INERT_ENVIRONMENT_VARIABLES) {
        EnvironmentVariable::Inert
    } else if starts_with_ignoring_case(name.as_bytes(), b"NUGET_") || is(DECLINED_DOTNET_VARIABLES)
    {
        EnvironmentVariable::Declined
    } else {
        EnvironmentVariable::Unread
    }
}

fn starts_with_ignoring_case(name: &[u8], prefix: &[u8]) -> bool {
    name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Why B₁ declined the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentDecline {
    /// A variable B₁ declines is set (its name, lossily decoded).
    Variable { name: String },
    /// The user config folder cannot be located: the home variable that
    /// decides it is empty or not an absolute path, or neither is set. NuGet
    /// reads an empty `DOTNET_CLI_HOME` as the working folder, not as unset.
    HomeUnknown,
    /// A user config file is outside the vocabulary B₁ admits, or could not be
    /// listed or read.
    UserConfig { path: PathBuf },
    /// NuGet locates the user config on Windows through `APPDATA`, which B₁
    /// does not model.
    WindowsHost,
}

impl fmt::Display for EnvironmentDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvironmentDecline::Variable { name } => {
                write!(f, "the environment sets {name}, which may move restore")
            }
            EnvironmentDecline::HomeUnknown => {
                write!(f, "the environment does not locate the user config folder")
            }
            EnvironmentDecline::UserConfig { path } => write!(
                f,
                "the user config file {} is outside the admitted vocabulary",
                path.display()
            ),
            EnvironmentDecline::WindowsHost => {
                write!(f, "the user config of a Windows host is not modelled")
            }
        }
    }
}

/// Every variable of `environment` has a verdict B₁ can commit under, and the
/// user config files restore would load are admitted.
pub(super) fn check_environment(
    environment: &BTreeMap<OsString, OsString>,
) -> Result<(), EnvironmentDecline> {
    if cfg!(windows) {
        return Err(EnvironmentDecline::WindowsHost);
    }
    if let Some(name) = environment
        .keys()
        .find(|name| environment_variable(name) == EnvironmentVariable::Declined)
    {
        return Err(EnvironmentDecline::Variable {
            name: name.to_string_lossy().into_owned(),
        });
    }
    user_configs_are_admitted(&user_config_folder(environment)?)
}

/// The folder restore loads user configs from: `.nuget/NuGet` under the first
/// home variable that is set.
///
/// The precedence is restore's, found by probing: a set `DOTNET_CLI_HOME` wins
/// over `HOME`, and an empty one is not skipped (restore wrote its default
/// config under the working folder).
fn user_config_folder(
    environment: &BTreeMap<OsString, OsString>,
) -> Result<PathBuf, EnvironmentDecline> {
    let home = HOME_VARIABLES
        .iter()
        .find_map(|name| environment.get(OsStr::new(name)))
        .map(Path::new)
        .filter(|home| home.is_absolute())
        .ok_or(EnvironmentDecline::HomeUnknown)?;
    Ok(home.join(".nuget").join("NuGet"))
}

/// Every config file restore would load from the user config folder is
/// admitted: `NuGet.Config`, and each `.config` file in its `config` folder.
///
/// A folder without them is admitted: restore writes the default config there,
/// which holds only `nuget.org` as a source, and the spec's sources override
/// it. The file names compare ignoring ASCII case, which on a case-sensitive
/// file system checks files restore does not load, and so can only decline
/// more.
fn user_configs_are_admitted(folder: &Path) -> Result<(), EnvironmentDecline> {
    let refuse = |path: &Path| EnvironmentDecline::UserConfig {
        path: path.to_owned(),
    };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(refuse(folder)),
    };
    for entry in entries {
        let path = entry.map_err(|_| refuse(folder))?.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name.eq_ignore_ascii_case("NuGet.Config") {
            if !config_is_admitted(&path) {
                return Err(refuse(&path));
            }
        } else if name.eq_ignore_ascii_case("config") {
            let extra = std::fs::read_dir(&path).map_err(|_| refuse(&path))?;
            for file in extra {
                let file = file.map_err(|_| refuse(&path))?.path();
                let loaded = file
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("config"));
                if loaded && !config_is_admitted(&file) {
                    return Err(refuse(&file));
                }
            }
        }
    }
    Ok(())
}
