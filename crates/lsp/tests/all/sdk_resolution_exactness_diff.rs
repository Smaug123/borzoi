//! SDK-resolution differential oracle — Surface A (Stage 2): committing
//! resolution exactness.
//!
//! See `docs/completed/sdk-resolution-oracle-plan.md`. A generative sweep over synthetic
//! NuGet-distributed Project SDKs and version pins, asserting **certain-implies-
//! exact**: wherever `SdkDiscovery::resolve` *commits*, real MSBuild (through the
//! resident condition oracle) must agree.
//!
//! - We return `Single(props)`  ⟹ MSBuild resolved the SDK to the same
//!   `Sdk.props` path.
//! - We return `NotFound` / `VersionNotSatisfied` (a committed "does not
//!   resolve") ⟹ MSBuild also fails to resolve it.
//! - We return `UnsupportedLayout` (a pure decline) ⟹ no claim.
//!
//! Pin forms exercised: per-import `Sdk="Name/Version"`, `msbuild-sdks`, and
//! both at once (per-import must win). Each may also carry a `global.json`
//! `sdk` block (a host-SDK version spec), which must **not** govern the NuGet
//! package pin — a documented `locate_dotnet_sdk` boundary.
//!
//! One resident oracle serves the whole sweep (a shared global-packages folder,
//! a unique SDK name per case to sidestep per-process SDK-resolver caching).
//! Runs under `nix develop`, like the other self-contained real-MSBuild diffs.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common;
use crate::common::SdkDirCasing;
use borzoi::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
use borzoi_msbuild::{SdkResolution, SdkResolveError};
use borzoi_spawn::BoundedCommand;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, RngAlgorithm, TestRng, TestRunner};
use tempfile::TempDir;

fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("canonicalize {}: {e}", path.display()))
}

/// Canonical, NuGet-normalised version strings used for the *on-disk*
/// global-packages folder directory names (NuGet always stores the normalised
/// 3-component form). A prerelease is included so exact prerelease pins are
/// exercised; ranges/floating are out of our model, so all are concrete.
const DIR_VERSIONS: &[&str] = &["1.0.0", "1.2.3", "2.0.0", "2.5.100", "1.5.0-rc.1"];

/// Version spellings used in the *pin* (the `Sdk="Name/Version"` attribute or
/// the `msbuild-sdks` value), drawn independently of the directory names. It
/// includes 2-component aliases (`1.0`, `2.0`) equivalent to canonical
/// directories (`1.0.0`, `2.0.0`), so the sweep exercises the `1.0` vs `1.0.0`
/// normalisation boundary that `SdkVersion` deliberately collapses — a
/// regression that resolved (or failed) an equivalent spelling differently from
/// MSBuild is then caught.
const PIN_VERSIONS: &[&str] = &[
    "1.0.0",
    "1.0",
    "1.2.3",
    "2.0.0",
    "2.0",
    "2.5.100",
    "1.5.0-rc.1",
];

/// How the project pins the SDK version.
#[derive(Debug, Clone, Copy)]
enum PinForm {
    /// `Sdk="Name/Version"` at the import site.
    PerImport,
    /// `Sdk="Name"` plus a `global.json` `msbuild-sdks` entry.
    MsBuildSdks,
    /// Both: `Sdk="Name/Version"` *and* an `msbuild-sdks` entry naming a
    /// different version. The per-import version must win (a documented
    /// `locate_dotnet_sdk` precedence).
    Both,
}

#[derive(Debug, Clone)]
struct Scenario {
    /// Canonical versions materialised in the global-packages folder for this
    /// SDK (directory names).
    present: Vec<&'static str>,
    /// The spelling the project pins to (the per-import version, or the
    /// `msbuild-sdks` version for the `MsBuildSdks` form). May be a 2-component
    /// alias, and may be absent from `present` (then both sides fail).
    requested: &'static str,
    /// The losing `msbuild-sdks` spelling for the `Both` form.
    other: &'static str,
    form: PinForm,
    /// Add a `global.json` `sdk` block (host-SDK version spec). It must not
    /// govern the NuGet package pin.
    host_spec: bool,
    /// The casing of the inner SDK directory materialised on disk (`Sdk/` vs
    /// `sdk/`). On a case-sensitive filesystem these are distinct, so the
    /// `sdk/` spelling exercises `collect_from_nuget`'s lowercase probe; a
    /// regression removing it fails here against MSBuild.
    casing: SdkDirCasing,
}

fn scenario_strategy() -> impl Strategy<Value = Scenario> {
    let present = proptest::collection::vec(
        proptest::sample::select(DIR_VERSIONS),
        1..=DIR_VERSIONS.len(),
    );
    let requested = proptest::sample::select(PIN_VERSIONS);
    let other = proptest::sample::select(PIN_VERSIONS);
    let form = prop_oneof![
        Just(PinForm::PerImport),
        Just(PinForm::MsBuildSdks),
        Just(PinForm::Both),
    ];
    let casing = prop_oneof![Just(SdkDirCasing::Upper), Just(SdkDirCasing::Lower)];
    (present, requested, other, form, proptest::bool::ANY, casing).prop_map(
        |(mut present, requested, other, form, host_spec, casing)| {
            present.sort_unstable();
            present.dedup();
            Scenario {
                present,
                requested,
                other,
                form,
                host_spec,
                casing,
            }
        },
    )
}

/// What our resolver committed for one case — tracked so the anti-vacuity floor
/// can require genuine *positive* resolutions, not merely committed "no"s.
#[derive(PartialEq)]
enum Outcome {
    /// `Single` that matched MSBuild's resolved path.
    PositiveMatch,
    /// A committed `NotFound`/`VersionNotSatisfied` that MSBuild also failed.
    NegativeMatch,
    /// A pure decline (or a locator result) — no claim.
    NoClaim,
}

/// Shared per-sweep state: one GPF, one resident oracle, one dummy dotnet root,
/// and the real host SDK version (for the `host_spec` dimension).
struct Harness {
    _tmp: TempDir,
    gpf: PathBuf,
    dotnet_root: PathBuf,
    cases_root: PathBuf,
    host_sdk_version: String,
    oracle: RefCell<common::SdkOracle>,
    next_id: Cell<u32>,
}

impl Harness {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let gpf = tmp.path().join("gpf");
        std::fs::create_dir_all(&gpf).unwrap();
        let cases_root = tmp.path().join("cases");
        // A `{}` boundary above every case: a case whose `global.json` is empty
        // (`PerImport` with `host_spec=false`) writes none of its own, and both
        // MSBuild and `SdkDiscovery` would otherwise walk past a nested `TMPDIR`
        // into an ancestor `global.json`. This pins nothing but stops that walk.
        common::write_boundary_global_json(&cases_root);
        Harness {
            gpf: gpf.clone(),
            dotnet_root: tmp.path().join("dotnet"),
            cases_root,
            host_sdk_version: query_host_sdk_version(),
            oracle: RefCell::new(common::SdkOracle::spawn(&gpf)),
            next_id: Cell::new(0),
            _tmp: tmp,
        }
    }

    fn check(&self, scenario: &Scenario) -> Result<Outcome, TestCaseError> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        // Unique name per case: the resident oracle's SDK resolver may cache by
        // name across requests, so a fresh name guarantees a fresh resolution.
        let name = format!("TestSdk{id}");

        for version in &scenario.present {
            common::write_nuget_sdk(&self.gpf, &name, version, scenario.casing);
        }

        let case_dir = self.cases_root.join(format!("case{id}"));
        common::write_offline_nuget_config(&case_dir);
        let project_path = case_dir.join("App.fsproj");

        // The `Sdk=""` attribute string, and any `msbuild-sdks` global.json entry.
        let (sdk_ref, msbuild_sdks) = match scenario.form {
            PinForm::PerImport => (format!("{name}/{}", scenario.requested), None),
            PinForm::MsBuildSdks => (name.clone(), Some(scenario.requested)),
            PinForm::Both => (
                format!("{name}/{}", scenario.requested),
                Some(scenario.other),
            ),
        };
        self.write_global_json(&case_dir, &name, msbuild_sdks, scenario.host_spec);

        let theirs = self.oracle.borrow_mut().resolve(&sdk_ref, &project_path);

        let env = SdkDiscoveryEnv {
            dotnet_root: Some(self.dotnet_root.clone()),
            nuget_packages_dir: Some(self.gpf.clone()),
            ..SdkDiscoveryEnv::default()
        };
        let disc = SdkDiscovery::for_project(&project_path, &env).expect("discovery");
        let ours = disc.resolve(&sdk_ref);

        let ctx = || {
            format!(
                "{sdk_ref} (present: {:?}, host_spec: {}, casing: {:?})",
                scenario.present, scenario.host_spec, scenario.casing
            )
        };
        match ours {
            Ok(SdkResolution::Single(paths)) => {
                let theirs = theirs.ok_or_else(|| {
                    TestCaseError::fail(format!(
                        "we committed Single({}) but MSBuild did not resolve {}",
                        paths.props.display(),
                        ctx()
                    ))
                })?;
                prop_assert_eq!(
                    canon(&paths.props),
                    canon(&theirs),
                    "resolved Sdk.props diverges for {}",
                    ctx()
                );
                Ok(Outcome::PositiveMatch)
            }
            Err(SdkResolveError::NotFound | SdkResolveError::VersionNotSatisfied { .. }) => {
                prop_assert!(
                    theirs.is_none(),
                    "we claimed {} does not resolve, but MSBuild resolved it to {:?}",
                    ctx(),
                    theirs
                );
                Ok(Outcome::NegativeMatch)
            }
            // A pure decline makes no claim; and a locator resolution can't arise
            // from these non-locator names.
            Err(SdkResolveError::UnsupportedLayout { .. }) | Ok(SdkResolution::Roots(_)) => {
                Ok(Outcome::NoClaim)
            }
        }
    }

    /// Write the `global.json` a case needs, or none. Combines an optional
    /// `msbuild-sdks` pin with an optional host-SDK `sdk` block (the real
    /// installed version, `rollForward: disable` — the strictest caller spec,
    /// which must not reach the NuGet pin).
    fn write_global_json(
        &self,
        case_dir: &Path,
        name: &str,
        msbuild_sdks: Option<&str>,
        host_spec: bool,
    ) {
        let mut members = Vec::new();
        if host_spec {
            members.push(format!(
                "\"sdk\": {{ \"version\": \"{}\", \"rollForward\": \"disable\" }}",
                self.host_sdk_version
            ));
        }
        if let Some(version) = msbuild_sdks {
            members.push(format!("\"msbuild-sdks\": {{ \"{name}\": \"{version}\" }}"));
        }
        if members.is_empty() {
            return;
        }
        std::fs::write(
            case_dir.join("global.json"),
            format!("{{ {} }}", members.join(", ")),
        )
        .expect("write global.json");
    }
}

/// The real SDK version the devshell's `dotnet` selects, read once in an
/// isolated directory. An empty tempdir is *not* enough: `dotnet` walks
/// ancestors for the nearest `global.json`, so a nested `TMPDIR` beneath one
/// (an invalid pin, or one that redirects `sdk.paths`) would make
/// `dotnet --version` fail or report the wrong version. An empty `{}`
/// `global.json` pins nothing but stops the walk. Pinning the resulting version
/// into a case's `global.json` `sdk.version` keeps the muxer satisfiable while
/// still exercising a concrete caller spec.
fn query_host_sdk_version() -> String {
    let dir = TempDir::new().unwrap();
    common::write_boundary_global_json(dir.path());
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(dir.path()).arg("--version");
    let out = BoundedCommand::new(cmd)
        .run()
        .expect("dotnet --version ran");
    assert!(out.status.success(), "dotnet --version failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The number of cases each sweep runs; the anti-vacuity floor is a fraction of
/// it.
const SWEEP_CASES: u32 = 96;

/// Drive the certain-implies-exact sweep with `runner`, asserting the property
/// on every generated case and the anti-vacuity positive-match floor at the end.
///
/// Anti-vacuity: certain-implies-exact passes trivially by declining
/// everything, and a floor that counted committed "no"s could be met entirely
/// by absent-version cases. Require a healthy floor of *positive* resolved-path
/// matches, so a regression to blanket-declining (or to never resolving) fails
/// here even while the property holds.
fn run_sweep(mut runner: TestRunner) {
    let harness = Harness::new();
    let positive = Cell::new(0u32);
    runner
        .run(&scenario_strategy(), |scenario| {
            if harness.check(&scenario)? == Outcome::PositiveMatch {
                positive.set(positive.get() + 1);
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(
        positive.get() >= SWEEP_CASES / 4,
        "only {} of {} cases were positive resolved-path matches — the differential is \
         barely exercising real resolution",
        positive.get(),
        SWEEP_CASES
    );
}

fn sweep_config() -> ProptestConfig {
    ProptestConfig {
        cases: SWEEP_CASES,
        // Fixtures are ephemeral tempdirs regenerated each run, so persisting a
        // failing seed buys nothing (and integration tests have no lib.rs/main.rs
        // for proptest to anchor a regressions file to — hence the warning we
        // silence here).
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

/// Entropy-seeded sweep: explores fresh scenarios across runs, widening coverage
/// over time. Its companion below pins deterministic coverage.
#[test]
fn certain_resolution_matches_msbuild() {
    run_sweep(TestRunner::new(sweep_config()));
}

/// Fixed-seed companion to [`certain_resolution_matches_msbuild`]. The
/// entropy-seeded sweep catches a regression confined to a narrow (pin-form ×
/// available-versions × host-spec × casing) corner only on the runs whose seed
/// happens to hit it — and with persistence disabled, a failing seed is never
/// replayed. This runs the identical property under a hardcoded RNG seed, so the
/// committed gate exercises the *same* scenarios every run: deterministic
/// coverage that fails reproducibly. Mirrors `fsproj_packageref_generative_diff`.
#[test]
fn certain_resolution_matches_msbuild_fixed_seed() {
    run_sweep(TestRunner::new_with_rng(
        sweep_config(),
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    ));
}
