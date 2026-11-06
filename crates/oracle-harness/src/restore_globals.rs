//! The global properties of the LSP's offline restore, as the oracles that
//! stand in for it pass them.
//!
//! `docs/nuget-dgspec-boundary-plan.md` ("The reference configuration is
//! `restore.rs`'s own") requires every half-A oracle run to evaluate the project
//! under exactly the globals `crates/lsp/src/restore.rs` hands `dotnet`, since a
//! census or differential under any other set grades a different function from
//! the one the LSP serves. The restore-cone census (`borzoi-msbuild`'s
//! `restore_cone_census.rs`) runs MSBuild in-process and so cannot call that
//! command builder; it takes the set from here, and `borzoi`'s
//! `restore_cone_globals.rs` holds this list to the command line `restore.rs`
//! actually runs, argument for argument.

use std::ffi::OsString;
use std::path::Path;

/// The target `GenerateRestoreGraph` invocations run: the dependency-graph spec
/// restore would read, written to `RestoreGraphOutputPath`.
pub const GENERATE_RESTORE_GRAPH_TARGET: &str = "GenerateRestoreGraphFile";

/// The name of the empty directory, under the scratch directory, that is the
/// restore's only package source.
pub const EMPTY_SOURCE: &str = "empty-source";

/// The global properties `restore.rs` passes `dotnet msbuild
/// -t:GenerateRestoreGraphFile` for a restore whose output lands in `scratch`,
/// whose package folder is `cache_root`, and whose spec is written to `output`,
/// in the order it passes them. The caller creates `scratch/`[`EMPTY_SOURCE`].
pub fn generate_restore_graph_globals(
    scratch: &Path,
    cache_root: &Path,
    output: &Path,
) -> Vec<(&'static str, OsString)> {
    let mut extensions = scratch.as_os_str().to_os_string();
    extensions.push("/");
    vec![
        ("RestoreGraphOutputPath", output.into()),
        ("MSBuildProjectExtensionsPath", extensions.clone()),
        ("RestoreOutputPath", extensions),
        ("RestorePackagesPath", cache_root.into()),
        ("RestoreRecursive", "false".into()),
        ("RestoreSources", scratch.join(EMPTY_SOURCE).into()),
        ("RestoreAdditionalProjectSources", "".into()),
        ("RestorePackagesWithLockFile", "false".into()),
        (
            "NuGetLockFilePath",
            scratch.join("packages.lock.json").into(),
        ),
        ("NuGetAudit", "false".into()),
        ("NuGetInteractive", "false".into()),
    ]
}
