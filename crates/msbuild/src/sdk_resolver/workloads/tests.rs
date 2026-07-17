//! Unit tests for workload locator resolution over synthetic on-disk
//! layouts. Behaviours mirror the ground-truth probes recorded in
//! docs/completed/sdk-chain-exactness-plan.md (2026-07-09): known-list ordering,
//! descending band fallback, silent skipping of absent ids, and a
//! degrade for every layout shape outside the envelope.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::SdkResolution;

const TARGETS: &str = super::WORKLOAD_MANIFEST_TARGETS_LOCATOR;
const AUTO: &str = super::WORKLOAD_AUTO_IMPORT_PROPS_LOCATOR;

fn no_user_env() -> WorkloadEnvironment<'static> {
    WorkloadEnvironment {
        user_dotnet_root: None,
        overrides_present: false,
        global_json_pins_workload_set: false,
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Build a dotnet root with one host SDK version dir and a known-list.
fn host_sdk(root: &Path, version: &str, known: &[&str]) -> PathBuf {
    let version_dir = root.join("sdk").join(version);
    write(
        &version_dir.join("KnownWorkloadManifests.txt"),
        &known.join("\n"),
    );
    version_dir
}

/// Install a versioned manifest `{root}/sdk-manifests/{band}/{id}/{version}`
/// with both the json and targets files.
fn manifest(root: &Path, band: &str, id: &str, version: &str) -> PathBuf {
    let dir = root.join("sdk-manifests").join(band).join(id).join(version);
    write(&dir.join("WorkloadManifest.json"), "{}");
    write(&dir.join("WorkloadManifest.targets"), "<Project/>");
    dir
}

fn resolve_targets(
    root: &Path,
    version_dir: &Path,
    env: &WorkloadEnvironment<'_>,
) -> Result<SdkResolution, SdkResolveError> {
    resolve_workload_locator(TARGETS, root, version_dir, env)
}

#[test]
fn manifest_targets_locator_returns_known_list_order() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    // Known list deliberately NOT alphabetical: the import order is the
    // file's order (pinned against `dotnet msbuild -preprocess`).
    let version_dir = host_sdk(root, "10.0.300", &["zeta.workload", "alpha.workload"]);
    let zeta = manifest(root, "10.0.100", "zeta.workload", "1.0.0");
    let alpha = manifest(root, "10.0.100", "alpha.workload", "2.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![zeta, alpha]));
}

#[test]
fn manifest_id_missing_on_disk_is_skipped() {
    // `samsung.net.sdk.tizen` behaviour: in the known list, nothing on
    // disk, silently skipped.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["present.workload", "absent.workload"]);
    let present = manifest(root, "10.0.100", "present.workload", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![present]));
}

#[test]
fn flat_manifest_layout_resolves_to_id_dir() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["flat.workload"]);
    let id_dir = root
        .join("sdk-manifests")
        .join("10.0.100")
        .join("flat.workload");
    write(&id_dir.join("WorkloadManifest.json"), "{}");
    write(&id_dir.join("WorkloadManifest.targets"), "<Project/>");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![id_dir]));
}

#[test]
fn band_fallback_prefers_newest_band_not_above_host() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    // Present under three bands; 11.0.100 is above the host band and
    // must be ignored, 10.0.100 beats 9.0.100.
    manifest(root, "9.0.100", "w.workload", "1.0.0");
    let expected = manifest(root, "10.0.100", "w.workload", "2.0.0");
    manifest(root, "11.0.100", "w.workload", "3.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![expected]));
}

#[test]
fn exact_host_band_dir_wins_over_older() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    let expected = manifest(root, "10.0.300", "w.workload", "2.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![expected]));
}

#[test]
fn multiple_manifest_versions_pick_the_latest() {
    // ResolveManifestDirectory (dotnet/sdk): highest version wins.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    let latest = manifest(root, "10.0.100", "w.workload", "1.0.1");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![latest]));
}

#[test]
fn prerelease_manifest_versions_order_by_semver_not_lexically() {
    // SdkVersion implements SemVer 2.0.0 paragraph 11.4 (numeric
    // prerelease identifiers compare numerically — pinned by
    // `prerelease_numeric_identifiers_compared_numerically` in the
    // sdk_resolver tests), so preview.10 beats preview.2 exactly as
    // MSBuild's comparer would.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "10.0.0-preview.2");
    let latest = manifest(root, "10.0.100", "w.workload", "10.0.0-preview.10");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![latest]));
}

#[test]
fn unparseable_manifest_version_directory_degrades() {
    // We cannot reproduce MSBuild's version comparison for a name our
    // SdkVersion refuses; degrade rather than guess an ordering.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "not a version!");

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn manifest_without_targets_file_is_silently_excluded() {
    // CachingWorkloadResolver keeps only directories containing the
    // targets file; a manifest without one contributes no import.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    let dir = root
        .join("sdk-manifests")
        .join("10.0.100")
        .join("w.workload")
        .join("1.0.0");
    write(&dir.join("WorkloadManifest.json"), "{}");
    // No WorkloadManifest.targets.

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(Vec::new()));
}

#[test]
fn workloadsets_dir_degrades() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    fs::create_dir_all(root.join("sdk-manifests/10.0.100/workloadsets")).unwrap();

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn install_state_pin_degrades() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(
        &root.join("metadata/workloads/10.0.300/InstallState/default.json"),
        "{}",
    );

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn arch_scoped_install_state_pin_degrades() {
    // The real file-based path carries a process-architecture segment
    // (WorkloadInstallType.GetInstallStateFolder); any arch dir counts.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(
        &root.join("metadata/workloads/Arm64/10.0.300/InstallState/default.json"),
        "{}",
    );

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn msi_installer_type_marker_degrades() {
    // MSI-based installs keep their install state under ProgramData,
    // outside anything we are handed.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(
        &root.join("metadata/workloads/10.0.300/installertype/msi"),
        "",
    );

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn fallback_passes_over_unresolvable_bands() {
    // A newer band has the id directory but no resolvable manifest; an
    // older band has a valid one. MSBuild filters unresolvable
    // candidates before picking the highest band, so the older band's
    // manifest is imported.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    fs::create_dir_all(root.join("sdk-manifests/10.0.200/w.workload")).unwrap();
    let older = manifest(root, "9.0.100", "w.workload", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![older]));
}

#[test]
fn userlocal_marker_without_user_root_degrades() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn userlocal_marker_that_is_a_directory_is_not_a_marker() {
    // CLR `File.Exists` is false for a directory, so `IsUserLocal` reads a
    // `userlocal` *directory* as "no marker" — a global install that resolves
    // normally, not a present marker that would degrade or consult a user root.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    let w = manifest(root, "10.0.100", "w.workload", "1.0.0");
    fs::create_dir_all(root.join("metadata/workloads/10.0.300/userlocal")).unwrap();

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![w]));
}

#[test]
fn stray_non_version_directory_under_sdk_manifests_degrades() {
    // `FallbackForMissingManifest` runs `new SdkFeatureBand(dirName)` over every
    // directory under `sdk-manifests`, with no try/catch — a non-version name
    // throws a `FormatException` and fails the real resolution. We must degrade,
    // not silently skip it and return a supposedly-exact result.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    // A known id absent from the host band forces the fallback enumeration.
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    fs::create_dir_all(root.join("sdk-manifests/10.0.300")).unwrap();
    fs::create_dir_all(root.join("sdk-manifests/not-a-band")).unwrap();

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(
        matches!(err, SdkResolveError::UnsupportedLayout { .. }),
        "got {err:?}"
    );
}

#[test]
fn stray_non_version_directory_does_not_degrade_when_nothing_is_missing() {
    // The band enumeration (and its throw) only runs when a known id is missing,
    // mirroring MSBuild's per-missing-id `FallbackForMissingManifest`. When the
    // whole known set resolves in the primary pass, a stray directory alongside
    // is never enumerated, so it must not degrade an otherwise-exact result.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    let w = manifest(root, "10.0.300", "w.workload", "1.0.0");
    fs::create_dir_all(root.join("sdk-manifests/not-a-band")).unwrap();

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![w]));
}

#[test]
fn stray_file_under_sdk_manifests_is_ignored_like_get_directories() {
    // `Directory.GetDirectories` returns only directories, so a regular file
    // such as `.DS_Store` under `sdk-manifests` is invisible to the real band
    // enumeration. It must neither be parsed as a feature band (spurious
    // degrade) nor treated as an id directory; the fallback here still resolves
    // the missing id from a lower band.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    // `w.workload` is absent from the host band, present in a lower band.
    let w = manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("sdk-manifests/.DS_Store"), "");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![w]));
}

#[test]
#[cfg(unix)]
fn symlinked_manifest_id_directory_is_followed_like_get_directories() {
    // `Directory.GetDirectories` includes a symlink to a directory (package
    // managers and user-local layouts create them). The band enumeration must
    // follow the link, not skip it as a non-directory — otherwise a valid
    // manifest is dropped and an incomplete set is certified as exact.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    // The real manifest content lives outside sdk-manifests; the id directory in
    // the host band is a symlink to it.
    let real = root.join("real-w");
    write(&real.join("1.0.0/WorkloadManifest.json"), "{}");
    write(&real.join("1.0.0/WorkloadManifest.targets"), "<Project/>");
    fs::create_dir_all(root.join("sdk-manifests/10.0.300")).unwrap();
    std::os::unix::fs::symlink(&real, root.join("sdk-manifests/10.0.300/w.workload")).unwrap();

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(
        result,
        SdkResolution::Roots(vec![root.join("sdk-manifests/10.0.300/w.workload/1.0.0")])
    );
}

#[test]
fn known_manifest_list_with_a_leading_bom_still_resolves() {
    // `File.ReadAllLines` strips a leading UTF-8 BOM; we must too, or the first
    // known id carries U+FEFF, fails the safe-id check in band fallback, and
    // degrades a valid layout. The id here is absent from the host band, so it
    // is resolved through the known-list-driven fallback.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = root.join("sdk").join("10.0.300");
    write(
        &version_dir.join("KnownWorkloadManifests.txt"),
        "\u{feff}w.workload\n",
    );
    let w = manifest(root, "10.0.100", "w.workload", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![w]));
}

#[test]
fn user_root_participates_in_the_primary_band_only() {
    // Per SdkDirectoryWorkloadManifestProvider: the user root joins the
    // *current-band* enumeration only; band fallback consults the
    // dotnet root alone. The real-machine shape: dotnet ships the id
    // under 10.0.100 (fallback), the user root's older 9.0.100 copies —
    // including a user-only id — are invisible to a 10.0.300 host.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["shared.workload", "useronly.workload"]);
    let shared = manifest(&root, "10.0.100", "shared.workload", "2.0.0");
    manifest(&user, "9.0.100", "shared.workload", "1.0.0");
    manifest(&user, "9.0.100", "useronly.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_targets(&root, &version_dir, &env).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![shared]));
}

#[test]
fn user_root_shadows_dotnet_root_in_the_current_band() {
    // Same-named id dir in both roots' *current* band: the user root
    // wins (dotnet/sdk: "take the first one" over [user, dotnet]).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["w.workload"]);
    manifest(&root, "10.0.300", "w.workload", "1.0.0");
    let user_copy = manifest(&user, "10.0.300", "w.workload", "2.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_targets(&root, &version_dir, &env).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![user_copy]));
}

#[test]
fn user_root_shadows_by_name_even_when_it_holds_no_valid_manifest() {
    // The user root claims an id *by directory name* before its contents are
    // validated (MSBuild overwrites `directoriesWithManifests` by name, then
    // `ProbeDirectory` skips a directory that resolves to nothing). So an empty
    // user id-dir shadows the dotnet root's *same-band* copy out of existence —
    // it is NOT backfilled from dotnet's current band. With no lower band to
    // fall back to, the id resolves to nothing at all.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["w.workload"]);
    // dotnet has a valid current-band manifest that would resolve if not shadowed.
    manifest(&root, "10.0.300", "w.workload", "1.0.0");
    // The user root's same-band id-dir exists but is empty (no version subdir).
    fs::create_dir_all(user.join("sdk-manifests/10.0.300/w.workload")).unwrap();
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_targets(&root, &version_dir, &env).unwrap();
    assert_eq!(
        result,
        SdkResolution::Roots(vec![]),
        "the empty user id-dir shadows dotnet's same-band copy by name; it must \
         not fall through to that copy"
    );
}

#[test]
fn user_root_equal_to_dotnet_root_participates_once() {
    // A user-local dotnet install: DOTNET_ROOT is ~/.dotnet, so the
    // user root *is* the dotnet root. It must not be consulted twice
    // (which would look like a same-band collision).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    let expected = manifest(root, "10.0.300", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(root),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_targets(root, &version_dir, &env).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![expected]));
}

#[test]
fn known_manifests_file_missing_means_current_band_only() {
    // No KnownWorkloadManifests.txt / IncludedWorkloadManifests.txt:
    // there is no fallback pass and no preferential order — the current
    // band's directory enumeration alone drives the result.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = root.join("sdk").join("10.0.300");
    fs::create_dir_all(&version_dir).unwrap();
    let current = manifest(root, "10.0.300", "b.workload", "1.0.0");
    manifest(root, "10.0.100", "old-band.workload", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![current]));
}

#[test]
fn prerelease_host_sdk_degrades() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.100-preview.3.25201.16", &["w.workload"]);

    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn env_overrides_present_degrade() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");

    let env = WorkloadEnvironment {
        user_dotnet_root: None,
        overrides_present: true,
        global_json_pins_workload_set: false,
    };
    let err = resolve_targets(root, &version_dir, &env).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn host_band_derives_from_patch_hundreds() {
    // 10.0.301 belongs to band 10.0.300: a marker under the *band* dir
    // (not the literal version) is what gates user-local consultation.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.301", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    // Marker under 10.0.300 must be honoured for host 10.0.301.
    let err = resolve_targets(root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn auto_import_returns_empty_when_no_pack_ships_auto_import_props() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    // A packs tree exists but nothing ships AutoImport.props.
    write(
        &root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/data/FrameworkList.xml"),
        "",
    );

    let result = resolve_workload_locator(AUTO, root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(Vec::new()));
}

#[test]
fn auto_import_degrades_when_any_auto_import_props_exists() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(
        &root.join("packs/Fake.Workload.Sdk/1.0.0/AutoImport.props"),
        "<Project/>",
    );

    let err = resolve_workload_locator(AUTO, root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn unknown_manifest_in_current_band_is_included_after_known_ids() {
    // The primary pass is directory-driven: a third-party manifest in
    // the current band that is absent from the known list is still
    // imported, ordered after every known-list id.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["zeta.workload"]);
    let zeta = manifest(root, "10.0.300", "zeta.workload", "1.0.0");
    let third_party = manifest(root, "10.0.300", "acme.thirdparty.workload", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![zeta, third_party]));
}

#[test]
fn unknown_manifests_order_alphabetically_after_known() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["known.workload"]);
    let known = manifest(root, "10.0.300", "known.workload", "1.0.0");
    let b = manifest(root, "10.0.300", "b.extra", "1.0.0");
    let a = manifest(root, "10.0.300", "a.extra", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![known, a, b]));
}

#[test]
fn outdated_manifest_ids_are_skipped() {
    // SdkDirectoryWorkloadManifestProvider skips a hardcoded outdated
    // set wherever it appears.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    let expected = manifest(root, "10.0.300", "w.workload", "1.0.0");
    manifest(root, "10.0.300", "microsoft.net.workload.android", "1.0.0");

    let result = resolve_targets(root, &version_dir, &no_user_env()).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![expected]));
}

#[test]
fn auto_import_degrades_on_the_sdk_subfolder_layout() {
    // The layout CachingWorkloadResolver actually imports from:
    // packs/{id}/{version}/Sdk/AutoImport.props.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");
    write(
        &root.join("packs/Fake.Workload.Sdk/1.0.0/Sdk/AutoImport.props"),
        "<Project/>",
    );

    let err = resolve_workload_locator(AUTO, root, &version_dir, &no_user_env()).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn auto_import_scans_user_packs_even_without_user_manifests() {
    // WorkloadResolver.Create gates the *pack* roots only on the user
    // profile directory existing — not on it containing sdk-manifests
    // (that gate belongs to SdkDirectoryWorkloadManifestProvider). A
    // user-local pack shipping AutoImport.props must degrade the
    // auto-import locator even when the user root holds no manifests.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["w.workload"]);
    manifest(&root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");
    write(
        &user.join("packs/Fake.Workload.Sdk/1.0.0/Sdk/AutoImport.props"),
        "<Project/>",
    );

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let err = resolve_workload_locator(AUTO, &root, &version_dir, &env).unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn manifestless_user_root_still_resolves_targets_from_dotnet() {
    // A user root with packs but no sdk-manifests joins pack scanning
    // only; the manifest enumeration stays dotnet-root-exact.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["w.workload"]);
    let expected = manifest(&root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");
    write(
        &user.join("packs/Microsoft.NETCore.App.Ref/10.0.0/data/FrameworkList.xml"),
        "",
    );

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_targets(&root, &version_dir, &env).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![expected]));
}

#[test]
fn auto_import_empty_when_manifestless_user_root_ships_no_props() {
    // The user root exists and is consulted for packs, but neither root
    // holds an AutoImport.props candidate: the empty result stays exact.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["w.workload"]);
    manifest(&root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");
    fs::create_dir_all(user.join("packs")).unwrap();

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_workload_locator(AUTO, &root, &version_dir, &env).unwrap();
    assert_eq!(result, SdkResolution::Roots(Vec::new()));
}

#[test]
fn nonexistent_user_root_participates_nowhere() {
    // Directory.Exists(userProfileDir) is false upstream: the user root
    // joins neither manifest enumeration nor pack scanning, and
    // resolution proceeds against the dotnet root alone.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("dotnet");
    let user = tmp.path().join("missing-user-dotnet");
    let version_dir = host_sdk(&root, "10.0.300", &["w.workload"]);
    let expected = manifest(&root, "10.0.100", "w.workload", "1.0.0");
    write(&root.join("metadata/workloads/10.0.300/userlocal"), "");

    let env = WorkloadEnvironment {
        user_dotnet_root: Some(&user),
        overrides_present: false,
        global_json_pins_workload_set: false,
    };
    let result = resolve_targets(&root, &version_dir, &env).unwrap();
    assert_eq!(result, SdkResolution::Roots(vec![expected]));
}

#[test]
fn global_json_workload_set_pin_degrades_both_locators() {
    // An sdk.workloadVersion in the governing global.json selects a
    // workload set (or fails the real evaluation when the set is
    // missing) — either way our loose-manifest enumeration is not what
    // MSBuild would import.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");

    let env = WorkloadEnvironment {
        user_dotnet_root: None,
        overrides_present: false,
        global_json_pins_workload_set: true,
    };
    for locator in [TARGETS, AUTO] {
        let err = resolve_workload_locator(locator, root, &version_dir, &env).unwrap_err();
        assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
    }
}

#[test]
fn locator_under_rejecting_spec_reports_version_not_satisfied() {
    // A global.json spec that excludes every installed host SDK keeps
    // the same diagnostic split as ordinary SDK resolution: the
    // available-version list survives, rather than collapsing to
    // NotFound.
    use super::super::version_spec::{RollForward, VersionSpec};

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.100", "w.workload", "1.0.0");

    let pin = SdkVersion::parse("9.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let err =
        crate::resolve_sdk(root, None, TARGETS, Some(&spec), None, &no_user_env()).unwrap_err();
    let SdkResolveError::VersionNotSatisfied { available, .. } = err else {
        panic!("expected VersionNotSatisfied, got {err:?}");
    };
    assert_eq!(available.len(), 1);
    assert_eq!(available[0].to_string(), "10.0.300");
}

#[test]
fn resolve_sdk_routes_locators_and_ordinary_names() {
    // End-to-end through the public entry point: an ordinary SDK
    // resolves Single, the locator resolves Roots against the same
    // host SDK selection.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    let expected = manifest(root, "10.0.100", "w.workload", "1.0.0");
    let sdk_dir = version_dir.join("Sdks/Some.Sdk/Sdk");
    write(&sdk_dir.join("Sdk.props"), "<Project/>");
    write(&sdk_dir.join("Sdk.targets"), "<Project/>");

    let ordinary = crate::resolve_sdk(root, None, "Some.Sdk", None, None, &no_user_env()).unwrap();
    assert!(matches!(ordinary, SdkResolution::Single(_)));

    let locator = crate::resolve_sdk(root, None, TARGETS, None, None, &no_user_env()).unwrap();
    assert_eq!(locator, SdkResolution::Roots(vec![expected]));
}

/// Run `f` with `dir`'s permissions set to `mode`, restoring them
/// afterwards so `TempDir` cleanup can delete the tree.
#[cfg(unix)]
fn with_dir_mode<R>(dir: &Path, mode: u32, f: impl FnOnce() -> R) -> R {
    use std::os::unix::fs::PermissionsExt;
    let original = fs::metadata(dir).unwrap().permissions();
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    let result = f();
    fs::set_permissions(dir, original).unwrap();
    result
}

#[cfg(unix)]
#[test]
fn unreadable_band_directory_degrades() {
    // The dotnet provider enumerates the band directory after a
    // Directory.Exists guard; a permission failure there throws and
    // fails the evaluation. Returning the empty enumeration instead
    // would certify a wrong import set as exact.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.300", "w.workload", "1.0.0");
    let band_dir = root.join("sdk-manifests/10.0.300");

    let result = with_dir_mode(&band_dir, 0o000, || {
        resolve_targets(root, &version_dir, &no_user_env())
    });
    // Root bypasses permission bits; only assert degrade when the
    // chmod actually bit (it does everywhere we run tests).
    if fs::read_dir(&band_dir).is_err() {
        assert!(matches!(
            result.unwrap_err(),
            SdkResolveError::UnsupportedLayout { .. }
        ));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_packs_directory_degrades_auto_import() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.300", "w.workload", "1.0.0");
    let packs = root.join("packs");
    fs::create_dir_all(&packs).unwrap();

    let result = with_dir_mode(&packs, 0o000, || {
        resolve_workload_locator(AUTO, root, &version_dir, &no_user_env())
    });
    if fs::read_dir(&packs).is_err() {
        assert!(matches!(
            result.unwrap_err(),
            SdkResolveError::UnsupportedLayout { .. }
        ));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_known_manifests_file_degrades() {
    // Upstream File.ReadAllLines would throw; treating the list as
    // absent would silently change ordering and band fallback.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let version_dir = host_sdk(root, "10.0.300", &["w.workload"]);
    manifest(root, "10.0.300", "w.workload", "1.0.0");
    let known = version_dir.join("KnownWorkloadManifests.txt");

    use std::os::unix::fs::PermissionsExt;
    let original = fs::metadata(&known).unwrap().permissions();
    fs::set_permissions(&known, fs::Permissions::from_mode(0o000)).unwrap();
    let result = resolve_targets(root, &version_dir, &no_user_env());
    let unreadable = fs::read_to_string(&known).is_err();
    fs::set_permissions(&known, original).unwrap();
    if unreadable {
        assert!(matches!(
            result.unwrap_err(),
            SdkResolveError::UnsupportedLayout { .. }
        ));
    }
}
