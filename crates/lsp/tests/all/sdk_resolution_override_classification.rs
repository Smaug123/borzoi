//! SDK-resolution differential oracle — Surface B (Stage 3): the
//! `MSBuildSDKsPath`-override decline classification.
//!
//! See `docs/completed/sdk-resolution-oracle-plan.md`. This is the review-loop (#940) gap
//! made mechanical. `MSBuildSDKsPath` is not honoured by the in-process oracle,
//! so ground truth here comes from a `dotnet msbuild` subprocess: for each named
//! scenario a **two-probe** (override→a valid synthetic Sdks dir vs override
//! absent) establishes whether MSBuild's resolution *depends on* the override.
//!
//! Contract (one-sided — our resolver deliberately over-declines):
//! **Soundness**: `msbuild_depends ⟹ we decline via the override guard`. If the
//! override changes what MSBuild resolves, we must not commit. The converse is
//! not required: `SdkDiscovery::resolve` declines *every* name under the
//! override (the sound choice), over-declining names MSBuild would resolve
//! independently.
//!
//! This includes the workload locators. An earlier design exempted them, on the
//! evidence that `Microsoft.NET.SDK.WorkloadAutoImportPropsLocator` resolves
//! independently under a `/nonexistent` override. This oracle surfaced the hole
//! (round-2 review): when the override *contains*
//! `MSBuildSDKsPath/…WorkloadAutoImportPropsLocator/Sdk`, MSBuild serves the
//! locator *from the override* (probed against dotnet 10.0.301 — it imports that
//! `Sdk.props`), so the locator depends on the override after all. The
//! `workload-populated` witness below pins exactly that case: it depends, and we
//! must decline.
//!
//! Runs under `nix develop` (needs `dotnet` and `$DOTNET_ROOT`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::common;
use borzoi::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
use borzoi_msbuild::SdkResolveError;
use borzoi_spawn::BoundedCommand;
use tempfile::TempDir;

const PROBE_TIMEOUT: Duration = Duration::from_secs(300);

/// The outcome of one `dotnet msbuild` resolution probe. `Resolved` carries the
/// `_ResolvedSdkProps` marker (empty for an SDK, like a workload locator, that
/// resolves without importing our synthetic `Sdk.props`).
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Failed,
    Resolved(String),
}

/// Clear the child environment and restore only the runtime whitelist
/// (`PATH`/`HOME`/`TMPDIR` + `DOTNET_*`/`NUGET_*`), so no ambient MSBuild setting
/// leaks in. MSBuild folds every inherited variable in as an initial property,
/// and settings like `MSBuildEnableWorkloadResolver` or an ambient
/// `MSBuildSDKsPath` would silently change what the probe resolves, so the
/// witness would no longer represent its declared scenario. Mirrors `scrub` in
/// `fsproj_environment_diff.rs`, and for the same reason.
fn scrub(cmd: &mut Command) {
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
}

/// Resolve `sdk_ref` as the SDK of `project_dir/App.fsproj` through real
/// `dotnet msbuild`, optionally with `MSBuildSDKsPath` set to `sdks_override`.
/// The project and its offline `nuget.config` must already be written.
fn run_probe(project_dir: &Path, gpf: &Path, sdks_override: Option<&Path>) -> Probe {
    let mut cmd = Command::new("dotnet");
    scrub(&mut cmd);
    cmd.current_dir(project_dir)
        .args([
            "msbuild",
            "App.fsproj",
            "-nologo",
            "-getProperty:_ResolvedSdkProps",
        ])
        // After the scrub, override the (re-added) devshell `NUGET_PACKAGES` with
        // the synthetic GPF this scenario restored into.
        .env("NUGET_PACKAGES", gpf);
    match sdks_override {
        Some(dir) => {
            cmd.env("MSBuildSDKsPath", dir);
        }
        None => {
            cmd.env_remove("MSBuildSDKsPath");
        }
    }
    let out = BoundedCommand::new(cmd)
        .timeout(PROBE_TIMEOUT)
        .run()
        .expect("dotnet msbuild probe ran");
    if out.status.success() {
        Probe::Resolved(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Probe::Failed
    }
}

/// Write `name/Sdk/Sdk.{props,targets}` under an `MSBuildSDKsPath`-style flat
/// Sdks dir, with the `_ResolvedSdkProps` marker in `Sdk.props`.
fn write_override_sdk(sdks_dir: &Path, name: &str) {
    let sdk = sdks_dir.join(name).join("Sdk");
    std::fs::create_dir_all(&sdk).unwrap();
    std::fs::write(
        sdk.join("Sdk.props"),
        "<Project><PropertyGroup>\
         <_ResolvedSdkProps>$(MSBuildThisFileFullPath)</_ResolvedSdkProps>\
         </PropertyGroup></Project>",
    )
    .unwrap();
    std::fs::write(sdk.join("Sdk.targets"), "<Project/>").unwrap();
}

/// One classification scenario and its resolved facts.
struct Case {
    label: &'static str,
    /// Whether the two-probe found MSBuild's resolution to depend on the
    /// override.
    depends: bool,
    /// The dependence this witness is constructed to demonstrate. Pinned per
    /// scenario and checked against `depends`, so a fixture or MSBuild-behaviour
    /// change that silently reclassifies a witness (e.g. `pinned-unrestored`
    /// ceasing to depend, which would let a pin-exemption regression pass) fails
    /// the test by name rather than merely weakening anti-vacuity.
    expected_depends: bool,
    /// Whether MSBuild resolved the SDK successfully in *both* probes. An
    /// independent witness that instead *failed* both ways would also read as
    /// `depends == false`, satisfying the exemption ratchet vacuously without
    /// ever demonstrating a real resolution — so the workload witness asserts
    /// this is `true`.
    resolves_both: bool,
    declined_by_override: bool,
    is_workload: bool,
}

/// Does our resolver decline `sdk_ref` *because of* the override guard (as
/// opposed to some other decline, e.g. a workload layout it can't model)? Keyed
/// on the guard's reason text so a workload-internal `UnsupportedLayout` is not
/// mistaken for the override decline.
fn declined_by_override(disc: &SdkDiscovery, sdk_ref: &str) -> bool {
    matches!(
        disc.resolve(sdk_ref),
        Err(SdkResolveError::UnsupportedLayout { reason }) if reason.contains("MSBuildSDKsPath")
    )
}

/// Build discovery for `project_path` with the override present in the build
/// environment, over a real `$DOTNET_ROOT` (so workload resolution can run).
fn discovery_with_override(project_path: &Path, gpf: &Path, override_sdks: &Path) -> SdkDiscovery {
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset — run under `nix develop`, which supplies the real SDK root");
    let env = SdkDiscoveryEnv {
        dotnet_root: Some(dotnet_root),
        nuget_packages_dir: Some(gpf.to_path_buf()),
        build_environment: HashMap::from([(
            "MSBuildSDKsPath".to_string(),
            override_sdks.to_string_lossy().into_owned(),
        )]),
        ..SdkDiscoveryEnv::default()
    };
    SdkDiscovery::for_project(project_path, &env).expect("discovery")
}

/// Set up a scenario directory (offline nuget.config + the project) and run the
/// two-probe plus our resolver, returning the classification facts.
fn run_case(
    root: &Path,
    label: &'static str,
    sdk_ref: &str,
    gpf: &Path,
    override_sdks: &Path,
    expected_depends: bool,
    is_workload: bool,
) -> Case {
    let dir = root.join(label);
    common::write_offline_nuget_config(&dir);
    let project_path = dir.join("App.fsproj");
    std::fs::write(
        &project_path,
        format!(r#"<Project Sdk="{sdk_ref}"><Target Name="B" /></Project>"#),
    )
    .unwrap();

    let with = run_probe(&dir, gpf, Some(override_sdks));
    let without = run_probe(&dir, gpf, None);
    let depends = with != without;
    let resolves_both = matches!(with, Probe::Resolved(_)) && matches!(without, Probe::Resolved(_));

    let disc = discovery_with_override(&project_path, gpf, override_sdks);
    Case {
        label,
        depends,
        expected_depends,
        resolves_both,
        declined_by_override: declined_by_override(&disc, sdk_ref),
        is_workload,
    }
}

#[test]
fn override_decline_is_sound_including_workload_locators() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let gpf = root.join("gpf");
    std::fs::create_dir_all(&gpf).unwrap();
    // Isolate from any ancestor `global.json` above a nested `TMPDIR`: both the
    // `dotnet msbuild` probe and `SdkDiscovery::for_project` walk upward for the
    // nearest one, so a malformed ancestor would panic the probe and valid
    // SDK/workload settings could silently reclassify the witnesses. An empty
    // `{}` boundary at `root` (above every case dir) pins nothing but stops the
    // walk.
    common::write_boundary_global_json(root);

    let mut cases = Vec::new();

    // 1. Unpinned custom name, present only under the override → served from
    //    it, so resolution DEPENDS on the override.
    {
        let ov = root.join("ov-unpinned");
        write_override_sdk(&ov, "UnpinnedSdk");
        cases.push(run_case(
            root,
            "unpinned",
            "UnpinnedSdk",
            &gpf,
            &ov,
            true,
            false,
        ));
    }
    // 2. NuGet-pinned and restored in the GPF → NuGet serves it, INDEPENDENT of
    //    the override. The override dir is empty here.
    {
        common::write_nuget_sdk(&gpf, "RestoredSdk", "1.0.0", common::SdkDirCasing::Upper);
        let ov = root.join("ov-restored"); // empty
        std::fs::create_dir_all(&ov).unwrap();
        cases.push(run_case(
            root,
            "pinned-restored",
            "RestoredSdk/1.0.0",
            &gpf,
            &ov,
            false,
            false,
        ));
    }
    // 3. NuGet-pinned but NOT restored, present under the override → falls
    //    through to the default resolver and is served from it, so DEPENDS on
    //    the override (the #940 bug-witness: exempting pins would unsoundly
    //    not-decline this).
    {
        let ov = root.join("ov-override-only");
        write_override_sdk(&ov, "OverrideOnlySdk");
        cases.push(run_case(
            root,
            "pinned-unrestored",
            "OverrideOnlySdk/2.0.0",
            &gpf,
            &ov,
            true,
            false,
        ));
    }
    // 4. Workload locator, override WITHOUT a matching entry → its own resolver
    //    serves it, INDEPENDENT of the override. (We still decline it — an
    //    over-decline — but soundness makes no demand here.)
    {
        let ov = root.join("ov-workload-empty"); // no locator entry
        std::fs::create_dir_all(&ov).unwrap();
        cases.push(run_case(
            root,
            "workload-empty-override",
            "Microsoft.NET.SDK.WorkloadAutoImportPropsLocator",
            &gpf,
            &ov,
            false,
            true,
        ));
    }
    // 5. Workload locator, override WITH a matching entry → MSBuild serves it
    //    from the override, so it DEPENDS on the override (the round-2
    //    bug-witness: the earlier unconditional workload exemption would
    //    unsoundly not-decline this). We must decline.
    {
        let ov = root.join("ov-workload-populated");
        write_override_sdk(&ov, "Microsoft.NET.SDK.WorkloadAutoImportPropsLocator");
        cases.push(run_case(
            root,
            "workload-populated-override",
            "Microsoft.NET.SDK.WorkloadAutoImportPropsLocator",
            &gpf,
            &ov,
            true,
            true,
        ));
    }

    // Each witness must exhibit the dependence it was built to demonstrate. This
    // pins the classification per name (in particular that `pinned-unrestored`
    // *depends*), so a fixture or MSBuild-behaviour drift that reclassified a
    // witness — quietly disarming the soundness check below — fails here first.
    for c in &cases {
        assert_eq!(
            c.depends, c.expected_depends,
            "[{}] MSBuild override-dependence was {}, expected {} — the fixture or MSBuild \
             behaviour changed; the witness no longer tests what it names",
            c.label, c.depends, c.expected_depends
        );
    }

    // Soundness: whenever MSBuild depends on the override, we must decline via
    // the override guard.
    for c in &cases {
        assert!(
            !c.depends || c.declined_by_override,
            "[{}] MSBuild's resolution depends on MSBuildSDKsPath, but our resolver did not \
             decline via the override guard — unsound (it would commit a resolution the \
             override changes)",
            c.label
        );
    }

    // The workload witnesses must genuinely *resolve* in MSBuild (not fail both
    // probes), else their dependence classification — and so the coverage they
    // provide — would be vacuous. The empty-override locator resolves in both
    // arms (independence); the populated one resolves in both (the override arm
    // from the override, the absent arm from the workload resolver), differing,
    // hence its dependence.
    for c in cases.iter().filter(|c| c.is_workload) {
        assert!(
            c.resolves_both,
            "[{}] the workload locator did not resolve in both probes — its classification, and \
             the coverage it provides, would be vacuous",
            c.label
        );
    }

    // Anti-vacuity: at least one witness must genuinely depend on the override,
    // else the soundness implication is hollow. (Guaranteed by the per-witness
    // checks above, but kept explicit.)
    assert!(
        cases.iter().any(|c| c.depends),
        "no scenario depended on the override — the two-probe or fixtures are broken"
    );
}
