//! The output-directory verdict as the **LSP actually computes it**: through
//! [`Workspace::new`], against the host's real SDK install.
//!
//! Every other test of this verdict is SDK-blind. `Workspace::default()`
//! deliberately carries an empty [`SdkDiscoveryEnv`], and the `borzoi-msbuild`
//! differential drives the walker directly. Neither sees what the runtime
//! path sees, and the difference is not cosmetic: the SDK chain **writes
//! `OutDir` itself** —
//!
//! ```xml
//! <OutDir Condition="'$(OutDir)' == ''">$(OutputPath)</OutDir>
//! ```
//!
//! — so under a resolved SDK the walker finds a write on `OutDir` for *every*
//! project, redirected or not. Read as a user declaration it is doubly wrong:
//! it is the default rather than a redirect, and its value is one this walk
//! cannot finish computing, because the framework segment is appended in a
//! targets file outside the chain the walker follows. A verdict built on it
//! commits to a `bin/Debug/`-shaped partial answer, or declines every project
//! in the workspace.
//!
//! So this group's job is to pin the verdicts in the one configuration that
//! ships. `Default` for an ordinary project is the load-bearing case: it is a
//! positive claim that the standard layout holds, and losing it takes every
//! project reference in the workspace down with it.
//!
//! Requires the .NET SDK on PATH — run under `nix develop`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borzoi::workspace::Workspace;
use borzoi_msbuild::OutputDirVerdict;
use tempfile::TempDir;

/// The verdict the LSP's own graph walk records for a one-project workspace
/// whose `<PropertyGroup>` carries `body`.
fn verdict_under_real_sdk(body: &str) -> OutputDirVerdict {
    let tmp = TempDir::new().expect("temp dir");
    let proj = tmp.path().join("Lib.fsproj");
    std::fs::write(
        &proj,
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
             <TargetFramework>net10.0</TargetFramework>\n    {body}\n  \
             </PropertyGroup>\n</Project>\n"
        ),
    )
    .expect("write project");

    let mut ws = Workspace::new();
    assert!(
        ws.dotnet_root_for_project(&proj).is_some(),
        "this group needs a resolvable dotnet root (`dotnet` on PATH / \
         $DOTNET_ROOT) — run under `nix develop`"
    );
    let graph = ws.project_graph_with_producer_tfms(&proj, &BTreeMap::new());
    node_verdict(&graph.nodes, &proj)
}

fn node_verdict(nodes: &[borzoi::project_graph::ProjectNode], proj: &Path) -> OutputDirVerdict {
    let canon: PathBuf = std::fs::canonicalize(proj).unwrap_or_else(|_| proj.to_path_buf());
    nodes
        .iter()
        .find(|n| std::fs::canonicalize(&n.path).unwrap_or_else(|_| n.path.clone()) == canon)
        .unwrap_or_else(|| panic!("{} is in the graph", proj.display()))
        .output_dir
        .clone()
}

/// **The regression this group exists for.** An ordinary SDK project declares
/// no redirect, so the standard `bin/<config>/<tfm>/` layout holds and the
/// fold may scan it. The SDK's own `OutDir` write must not be mistaken for a
/// declaration — read as one it takes out project-reference resolution across
/// the whole workspace, which no SDK-blind test can observe.
#[test]
fn an_ordinary_sdk_project_reports_the_default_layout() {
    assert_eq!(verdict_under_real_sdk(""), OutputDirVerdict::Default);
}

/// A user redirect still commits, under the real SDK chain as without it —
/// the SDK's `'$(OutDir)' == ''` condition means its own write never fires
/// over a user value, so ignoring that write costs no coverage.
#[test]
fn a_user_out_dir_still_commits_under_the_real_sdk() {
    assert_eq!(
        verdict_under_real_sdk("<OutDir>artifacts/</OutDir>"),
        OutputDirVerdict::Declared {
            path: "artifacts/".to_owned(),
        }
    );
}

/// A configuration-dependent redirect declines, and it must decline *here* in
/// particular: the LSP injects `Configuration` as a global
/// (`workspace::default_build_properties`), so unlike an SDK-blind walk this
/// one evaluates the reference cleanly and would commit to `Debug`'s
/// directory. The user may have built `Release`, and a stale `Debug` assembly
/// sitting there would be folded against current source.
#[test]
fn a_configuration_dependent_out_dir_declines() {
    for body in [
        "<OutDir>artifacts/$(Configuration)/</OutDir>",
        // Gated rather than referenced: the value never mentions the
        // configuration, so only the gate gives it away.
        "<OutDir Condition=\"'$(Configuration)' == 'Debug'\">fast/</OutDir>",
        // Laundered through a helper: neither the gate nor the body says
        // `$(Configuration)`, and only the evaluated value gives it away.
        "<Which>$(Configuration)</Which><OutDir>out/$(Which)/</OutDir>",
    ] {
        assert_eq!(
            verdict_under_real_sdk(body),
            OutputDirVerdict::Unknown,
            "{body} must not commit to one configuration's directory"
        );
    }
}

/// A user `<OutputPath>` redirect is not something this walk can turn into a
/// directory — MSBuild derives `OutDir` from it in a targets file, appending
/// the framework — so it declines. It must not read as the default layout:
/// the project is building to `elsewhere/<tfm>/`, and a `bin` scan would
/// report a built producer as unbuilt.
#[test]
fn a_user_output_path_declines_rather_than_claiming_the_default() {
    assert_eq!(
        verdict_under_real_sdk("<OutputPath>elsewhere/</OutputPath>"),
        OutputDirVerdict::Unknown
    );
}
