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
    crate::common::host_fs::canonicalize(path)
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", path.display()))
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
    let env = common::sdk_oracle_discovery_env(&gpf, tmp.path().join("dotnet"));
    let disc = SdkDiscovery::for_project(
        &project_path,
        &env,
        &borzoi_msbuild::observed::Observer::new(),
    )
    .expect("discovery");
    let our_props = match disc
        .resolve("Foo/1.2.3", &borzoi_msbuild::observed::Observer::new())
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

/// Which copy of the probe SDK a case resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Folder {
    /// The copy in the folder the project's config file names.
    Configured,
    /// The copy in the home's default folder, `<home>/.nuget/packages`.
    Home,
    /// The copy in the folder `NUGET_PACKAGES` names.
    Environment,
}

/// How a case sets up the global packages folder NuGet's settings choose.
#[derive(Debug, Clone, Copy)]
struct FolderCase {
    label: &'static str,
    /// The `globalPackagesFolder` value of the project's config file, if any;
    /// `{configured}` stands for the configured folder's absolute path.
    configured_value: Option<&'static str>,
    /// Whether the configured folder holds the probe SDK.
    configured_holds_it: bool,
    /// Whether `NUGET_PACKAGES` names a third folder holding it.
    environment_folder: bool,
    /// Whether the config names the home's default folder as a fallback
    /// package folder.
    home_is_a_fallback: bool,
    /// Where real MSBuild finds the probe SDK, `None` for nowhere.
    expect: Option<Folder>,
    /// Whether `SdkDiscovery` declines rather than commit to an answer.
    we_decline: bool,
}

const FOLDER_CASES: &[FolderCase] = &[
    FolderCase {
        label: "an absolute configured folder outranks the home's",
        configured_value: Some("{configured}"),
        configured_holds_it: true,
        environment_folder: false,
        home_is_a_fallback: false,
        expect: Some(Folder::Configured),
        we_decline: false,
    },
    FolderCase {
        label: "a relative configured folder resolves against its config file",
        configured_value: Some("../configured"),
        configured_holds_it: true,
        environment_folder: false,
        home_is_a_fallback: false,
        expect: Some(Folder::Configured),
        we_decline: false,
    },
    FolderCase {
        label: "a configured folder without the package does not fall back to the home's",
        configured_value: Some("{configured}"),
        configured_holds_it: false,
        environment_folder: false,
        home_is_a_fallback: false,
        expect: None,
        // Fallback folders and the download are not modelled.
        we_decline: true,
    },
    FolderCase {
        label: "a configured folder without the package falls back to a fallback folder",
        configured_value: Some("{configured}"),
        configured_holds_it: false,
        environment_folder: false,
        home_is_a_fallback: true,
        expect: Some(Folder::Home),
        we_decline: true,
    },
    FolderCase {
        label: "no configured folder is the home's",
        configured_value: None,
        configured_holds_it: false,
        environment_folder: false,
        home_is_a_fallback: false,
        expect: Some(Folder::Home),
        we_decline: false,
    },
    FolderCase {
        label: "NUGET_PACKAGES outranks a configured folder",
        configured_value: Some("{configured}"),
        configured_holds_it: true,
        environment_folder: true,
        home_is_a_fallback: false,
        expect: Some(Folder::Environment),
        we_decline: false,
    },
];

/// NuGet's MSBuild SDK resolver reads `Sdk="Name/Version"` packages from the
/// global packages folder of the settings it loads for the project, which a
/// config file can move away from the home's default. Each case puts a copy
/// of one probe SDK in the folders it sets up, runs real MSBuild (the
/// resident oracle, under exactly the environment `SdkDiscovery` models) and
/// `SdkDiscovery::resolve` on one project, and requires both to land on the
/// same copy, and that copy to be the one the case names: agreement alone
/// would pass a case whose fixture never exercised the folder choice.
#[test]
fn versioned_sdk_resolves_from_the_folder_nuget_settings_choose() {
    const SDK: &str = "Borzoi.Probe.Sdk";
    const VERSION: &str = "1.0.0";
    for case in FOLDER_CASES {
        let tmp = TempDir::new().unwrap();
        // By its real path: MSBuild reports the resolved file by the path
        // NuGet joined, and the settings walk is lexical.
        let root = canon(tmp.path());
        let home = root.join("home");
        let configured = root.join("configured");
        let environment_folder = root.join("environment");
        let proj_dir = root.join("proj");
        let project_path = proj_dir.join("App.fsproj");

        let mut copies = vec![(
            Folder::Home,
            common::write_nuget_sdk(
                &home.join(".nuget").join("packages"),
                SDK,
                VERSION,
                common::SdkDirCasing::Upper,
            ),
        )];
        std::fs::create_dir_all(&configured).unwrap();
        if case.configured_holds_it {
            copies.push((
                Folder::Configured,
                common::write_nuget_sdk(&configured, SDK, VERSION, common::SdkDirCasing::Upper),
            ));
        }
        if case.environment_folder {
            copies.push((
                Folder::Environment,
                common::write_nuget_sdk(
                    &environment_folder,
                    SDK,
                    VERSION,
                    common::SdkDirCasing::Upper,
                ),
            ));
        }
        // Offline: a missing package is a failure to resolve, not a download.
        let config_section = case
            .configured_value
            .map(|value| {
                format!(
                    "<config><add key=\"globalPackagesFolder\" value=\"{}\" /></config>",
                    value.replace("{configured}", configured.to_str().unwrap())
                )
            })
            .unwrap_or_default();
        let fallback_section = if case.home_is_a_fallback {
            format!(
                "<fallbackPackageFolders><add key=\"home\" value=\"{}\" /></fallbackPackageFolders>",
                home.join(".nuget").join("packages").display()
            )
        } else {
            String::new()
        };
        std::fs::create_dir_all(&proj_dir).unwrap();
        std::fs::write(
            proj_dir.join("NuGet.Config"),
            format!(
                "<?xml version=\"1.0\"?><configuration>\
                 <packageSources><clear/></packageSources>\
                 {config_section}{fallback_section}</configuration>"
            ),
        )
        .unwrap();
        common::write_boundary_global_json(&proj_dir);

        // The build environment of this process, with the home moved to the
        // case's and the NuGet variables the case sets and no others.
        let mut env = SdkDiscoveryEnv::from_process_env();
        for name in ["NUGET_PACKAGES", "DOTNET_CLI_HOME", "HOME"] {
            env.build_environment.remove(name);
        }
        env.home_dir = Some(home.clone());
        env.dotnet_cli_home = None;
        env.nuget_packages_dir = case.environment_folder.then(|| environment_folder.clone());

        let sdk_ref = format!("{SDK}/{VERSION}");
        let mut oracle = common::SdkOracle::spawn_in(&borzoi::restore::restore_environment(&env));
        let theirs = oracle
            .resolve(&sdk_ref, &project_path)
            .map(|props| canon(&props));
        let ours = SdkDiscovery::for_project(
            &project_path,
            &env,
            &borzoi_msbuild::observed::Observer::new(),
        )
        .expect("discovery")
        .resolve(&sdk_ref, &borzoi_msbuild::observed::Observer::new());
        let expected = case.expect.map(|want| {
            let (_, props) = copies
                .iter()
                .find(|(copy, _)| *copy == want)
                .unwrap_or_else(|| panic!("{}: the case holds no {want:?} copy", case.label));
            canon(props)
        });
        assert_eq!(
            theirs, expected,
            "{}: real MSBuild did not resolve the copy the case names",
            case.label
        );
        match ours {
            Err(borzoi_msbuild::SdkResolveError::UnsupportedLayout { .. }) if case.we_decline => {}
            Ok(SdkResolution::Single(paths)) if !case.we_decline => assert_eq!(
                Some(canon(&paths.props)),
                theirs,
                "{}: SdkDiscovery and real MSBuild resolve different copies",
                case.label
            ),
            other => panic!(
                "{}: expected {}, got {other:?}",
                case.label,
                if case.we_decline {
                    "a decline"
                } else {
                    "a single-root resolution"
                }
            ),
        }
    }
}
