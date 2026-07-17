//! SDK-resolution differential oracle — Stage 1 smoke test.
//!
//! See `docs/completed/sdk-resolution-oracle-plan.md`. This proves the plumbing before
//! any generativity: one synthetic NuGet-distributed Project SDK, resolved both
//! by real MSBuild (through the resident condition oracle's `project` op, via a
//! `_ResolvedSdkProps` marker) and by our `SdkDiscovery::resolve`, must agree on
//! the resolved `Sdk.props` path.
//!
//! Runs under `nix develop` (it builds and drives `tools/msbuild-condition-oracle`,
//! and the devshell supplies `dotnet`), like the other self-contained real-MSBuild
//! diffs in this crate.

use std::path::Path;

use crate::common;

use borzoi::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
use borzoi_msbuild::SdkResolution;
use tempfile::TempDir;

/// Canonicalise so the macOS `/var` ↔ `/private/var` symlink (and any
/// normalisation MSBuild applies) can't desync the two sides.
fn canon(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("canonicalize {}: {e}", path.display()))
}

#[test]
fn synthetic_nuget_pinned_sdk_resolves_to_the_same_props_as_msbuild() {
    let tmp = TempDir::new().unwrap();
    let gpf = tmp.path().join("gpf");
    let proj_dir = tmp.path().join("proj");
    let project_path = proj_dir.join("App.fsproj");

    // One synthetic NuGet SDK, and an offline nuget.config beside the project so
    // resolution never reaches the network.
    let expected_props = common::write_nuget_sdk(&gpf, "Foo", "1.2.3", common::SdkDirCasing::Upper);
    common::write_offline_nuget_config(&proj_dir);
    // Isolate from any ancestor `global.json` above a nested `TMPDIR`: both
    // MSBuild and `SdkDiscovery` walk upward for the nearest one, so an
    // unrelated ancestor could break or silently alter resolution.
    common::write_boundary_global_json(&proj_dir);

    // MSBuild's ground truth, via the resident oracle.
    let mut oracle = common::SdkOracle::spawn(&gpf);
    let their_props = oracle
        .resolve("Foo/1.2.3", &project_path)
        .expect("real MSBuild resolves the synthetic NuGet-pinned SDK");

    // Our resolver, over the same global-packages folder. A dummy dotnet root
    // keeps discovery's roots non-empty; the per-import pin drives NuGet
    // resolution, which does not consult it.
    let env = SdkDiscoveryEnv {
        dotnet_root: Some(tmp.path().join("dotnet")),
        nuget_packages_dir: Some(gpf.clone()),
        ..SdkDiscoveryEnv::default()
    };
    let disc = SdkDiscovery::for_project(&project_path, &env).expect("discovery");
    let our_props = match disc
        .resolve("Foo/1.2.3")
        .expect("we resolve the pinned SDK")
    {
        SdkResolution::Single(paths) => paths.props,
        other => panic!("expected a single-root resolution, got {other:?}"),
    };

    assert_eq!(
        canon(&their_props),
        canon(&our_props),
        "MSBuild and SdkDiscovery must resolve the pinned SDK to the same Sdk.props"
    );
    // And both must be the file we materialised.
    assert_eq!(canon(&our_props), canon(&expected_props));
}
