use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};
use tempfile::TempDir;

use super::version_spec::{RollForward, VersionSpec, select_sdk_version};
use super::{SdkVersion, locate_dotnet_sdk};
use crate::sdk_resolver::SdkResolveError;
use crate::{DiagnosticKind, parse_fsproj_with_imports};

/// Create `{dotnet_root}/sdk/{version}/Sdks/{sdk_name}/Sdk/Sdk.props` and
/// `Sdk.targets` (both empty). Returns the inner `Sdk/` directory.
fn install_sdk(dotnet_root: &Path, version: &str, sdk_name: &str) -> std::path::PathBuf {
    let sdk_root = dotnet_root
        .join("sdk")
        .join(version)
        .join("Sdks")
        .join(sdk_name)
        .join("Sdk");
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(sdk_root.join("Sdk.props"), "<Project/>").unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();
    sdk_root
}

/// Create
/// `{nuget_packages_dir}/{name-lowercased}/{version}/sdk/Sdk.{props,targets}`
/// (both empty). Returns the inner `sdk/` directory. Mirrors NuGet's
/// on-disk layout for restored MSBuild SDK packages whose inner
/// directory uses lowercase `sdk/` (e.g. `Microsoft.DotNet.Arcade.Sdk`).
fn install_sdk_nuget(
    nuget_packages_dir: &Path,
    version: &str,
    sdk_name: &str,
) -> std::path::PathBuf {
    install_sdk_nuget_cased(nuget_packages_dir, version, sdk_name, "sdk")
}

/// Like [`install_sdk_nuget`] but lets the caller pick the casing of
/// the inner SDK directory. Real packages ship both `Sdk/` (capital,
/// Microsoft canonical convention used by `Microsoft.Build.NoTargets`,
/// `Microsoft.NET.ILLink.Tasks`, …) and `sdk/` (lowercase, used by
/// `Microsoft.DotNet.Arcade.Sdk`, …); the resolver must probe both.
fn install_sdk_nuget_cased(
    nuget_packages_dir: &Path,
    version: &str,
    sdk_name: &str,
    inner: &str,
) -> std::path::PathBuf {
    let sdk_root = nuget_packages_dir
        .join(sdk_name.to_ascii_lowercase())
        .join(version)
        .join(inner);
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(sdk_root.join("Sdk.props"), "<Project/>").unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();
    sdk_root
}

/// True iff `path` lives under a `…/sdk/<version>/Sdks/…` layout (the
/// shared `$DOTNET_ROOT` store), false iff it lives under a NuGet
/// `…/<pkg-lowercased>/<version>/sdk/…` layout. Used by the union tests
/// to assert which root the resolver picked.
fn picked_from_dotnet_root(props_path: &Path) -> bool {
    props_path
        .components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("Sdks"))
}

/// Given an `Sdk.props` path produced by `locate_dotnet_sdk`, recover
/// the version-directory name. Works for both on-disk layouts:
///
/// - `$DOTNET_ROOT`: `…/sdk/<version>/Sdks/<sdk_name>/Sdk/Sdk.props`
///   — four `parent()` hops past `Sdk.props → Sdk → {sdk_name} → Sdks`
///   land on `<version>`.
/// - NuGet: `…/<sdk_name>/<version>/sdk/Sdk.props` — two `parent()`
///   hops past `Sdk.props → sdk` land on `<version>`.
fn version_dir_name(props_path: &Path) -> &std::ffi::OsStr {
    if picked_from_dotnet_root(props_path) {
        props_path
            .parent() // Sdk
            .and_then(Path::parent) // {sdk_name}
            .and_then(Path::parent) // Sdks
            .and_then(Path::parent) // {version}
            .and_then(Path::file_name)
            .expect("dotnet-root props path has the canonical layout")
    } else {
        props_path
            .parent() // sdk
            .and_then(Path::parent) // {version}
            .and_then(Path::file_name)
            .expect("nuget props path has the canonical layout")
    }
}

#[test]
fn missing_dotnet_root_returns_none() {
    let tmp = TempDir::new().unwrap();
    let nowhere = tmp.path().join("does-not-exist");
    assert!(locate_dotnet_sdk(&nowhere, None, "Microsoft.NET.Sdk", None, None).is_err());
}

#[test]
fn empty_sdk_dir_returns_none() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("sdk")).unwrap();
    assert!(locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).is_err());
}

#[test]
fn single_sdk_is_returned() {
    let tmp = TempDir::new().unwrap();
    let installed = install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).unwrap();
    assert_eq!(paths.props, installed.join("Sdk.props"));
    assert_eq!(paths.targets, installed.join("Sdk.targets"));
}

#[test]
fn highest_numeric_version_wins() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.100", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "9.0.100", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "10.0.100", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).unwrap();
    // Lexicographic ordering would pick "9.0.100" over "10.0.100".
    // Numeric tuple ordering must put 10 first.
    assert_eq!(version_dir_name(&paths.props), "10.0.100");
}

#[test]
fn stable_beats_prerelease_at_same_numeric_prefix() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "9.0.100-preview.1.24101.2", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "9.0.100", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "9.0.100-rc.2.24474.11", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).unwrap();
    assert_eq!(version_dir_name(&paths.props), "9.0.100");
}

#[test]
fn missing_targets_falls_back_to_lower_version() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.100", "Microsoft.NET.Sdk");

    // 9.0.100 has props but no targets — must be skipped.
    let broken = tmp
        .path()
        .join("sdk")
        .join("9.0.100")
        .join("Sdks")
        .join("Microsoft.NET.Sdk")
        .join("Sdk");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("Sdk.props"), "<Project/>").unwrap();

    let paths = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).unwrap();
    assert_eq!(version_dir_name(&paths.props), "8.0.100");
}

#[test]
fn sdks_other_than_requested_are_ignored() {
    let tmp = TempDir::new().unwrap();
    // Only Web variant installed; we ask for the base. Must return None.
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk.Web");
    assert!(locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).is_err());
}

#[test]
fn unparseable_version_dirs_are_skipped() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "NuGetFallbackFolder", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).unwrap();
    assert_eq!(version_dir_name(&paths.props), "8.0.401");
}

#[test]
fn files_under_sdk_dir_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let sdk_dir = tmp.path().join("sdk");
    fs::create_dir_all(&sdk_dir).unwrap();
    // A stray file (not a directory) at the version slot must not crash
    // or be considered.
    fs::write(sdk_dir.join("README"), "ignore me").unwrap();
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None).unwrap();
    assert_eq!(version_dir_name(&paths.props), "8.0.401");
}

#[cfg(unix)]
#[test]
fn symlinked_version_dir_is_followed() {
    // On Nix systems the real SDK lives under
    // `/nix/store/...-dotnet-sdk/share/dotnet/sdk/{version}/Sdks/`,
    // and `$DOTNET_ROOT/sdk/{version}` is typically a symlink into
    // the store. If we filtered on `DirEntry::file_type().is_dir()`,
    // the symlink would be skipped (file_type doesn't traverse
    // symlinks on Unix) and the resolver would wrongly return None.
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    install_sdk(&real, "8.0.401", "Microsoft.NET.Sdk");

    // `$DOTNET_ROOT` for the test is a fresh dir whose `sdk/8.0.401`
    // is a symlink to the real version dir.
    let fake_dotnet = tmp.path().join("fake_dotnet");
    let fake_sdk_dir = fake_dotnet.join("sdk");
    fs::create_dir_all(&fake_sdk_dir).unwrap();
    let real_version_dir = real.join("sdk").join("8.0.401");
    std::os::unix::fs::symlink(&real_version_dir, fake_sdk_dir.join("8.0.401")).unwrap();

    let paths = locate_dotnet_sdk(&fake_dotnet, None, "Microsoft.NET.Sdk", None, None)
        .expect("resolver should follow the symlink");
    // Path is unresolved (we don't canonicalise), so it goes through
    // the symlink directly. The file_name check is enough.
    assert_eq!(version_dir_name(&paths.props), "8.0.401");
}

#[test]
fn per_import_version_pin_picks_pinned_nuget_copy() {
    // Real-world scenario from `../HeterogeneousCollections`:
    // `<Project Sdk="Microsoft.Build.NoTargets/1.0.80">`. With both
    // `1.0.80` and a newer restored copy in NuGet, the resolver
    // must pick the pinned one — MSBuild honours per-import version
    // pins, and silently picking the higher restored version would
    // import different `Sdk.props`/`Sdk.targets` than MSBuild would
    // use (a parse-model divergence we'd then surface as misleading
    // diagnostics).
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "1.0.80", "Microsoft.Build.NoTargets");
    install_sdk_nuget(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets/1.0.80",
        None,
        None,
    )
    .expect("pinned NuGet copy should resolve");
    assert_eq!(version_dir_name(&paths.props), "1.0.80");
    assert!(!picked_from_dotnet_root(&paths.props));
}

#[test]
fn per_import_version_pin_fails_when_only_other_versions_present() {
    // The flipside: if the pinned version isn't installed but other
    // versions of the same SDK are, the resolver must NOT silently
    // fall through to one of them. Surfacing `VersionNotSatisfied`
    // tells the user which versions they have so they can either
    // install the pin or change it; a silent fallthrough hides the
    // disagreement and parses against the wrong SDK.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets");

    let err = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets/1.0.80",
        None,
        None,
    )
    .expect_err("per-import pin to a missing version must miss");
    match err {
        SdkResolveError::VersionNotSatisfied { spec, available } => {
            let pin = SdkVersion::parse("1.0.80").unwrap();
            // Effective spec is the per-import pin with `Disable`.
            assert_eq!(spec.version(), Some(&pin));
            assert_eq!(spec.roll_forward(), RollForward::Disable);
            let printed: Vec<String> = available.iter().map(SdkVersion::to_string).collect();
            assert_eq!(printed, vec!["3.7.134".to_string()]);
        }
        SdkResolveError::NotFound => {
            panic!("expected VersionNotSatisfied (NuGet had alternatives), got NotFound")
        }
        SdkResolveError::UnsupportedLayout { reason } => {
            panic!("expected VersionNotSatisfied, got UnsupportedLayout: {reason}")
        }
    }
}

#[test]
fn per_import_version_pin_overrides_caller_spec() {
    // When both a caller `spec` (e.g. from `global.json`) and a
    // per-import pin are in play, the per-import wins — the project
    // file is explicitly requesting a specific package version, which
    // is a different question from `global.json`'s host-SDK pin.
    //
    // Without the override, a caller spec pinning some other
    // version with `Disable` would block the per-import from
    // resolving in `$DOTNET_ROOT`; this test catches that
    // regression.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "9.0.100", "Microsoft.NET.Sdk");

    // Caller spec disallows 9.0.100 directly (host-SDK pin to a
    // different version). Per-import asks for 9.0.100 exactly, and
    // it wins.
    let caller_pin = SdkVersion::parse("8.0.401").unwrap();
    let caller_spec = VersionSpec::with_version(caller_pin, RollForward::Disable, false);
    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk/9.0.100",
        Some(&caller_spec),
        None,
    )
    .expect("per-import pin should override caller spec");
    assert_eq!(version_dir_name(&paths.props), "9.0.100");
    assert!(picked_from_dotnet_root(&paths.props));
}

#[test]
fn rejects_traversal_in_versioned_sdk_reference() {
    // The name half of `Name/Version` still goes through the
    // safe-name filter — a malicious form must not slip through
    // just because of the version separator.
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");
    // Empty name half rejected.
    assert!(locate_dotnet_sdk(tmp.path(), None, "/Version", None, None).is_err());
    // `..` as the name half rejected.
    assert!(locate_dotnet_sdk(tmp.path(), None, "../1.0.0", None, None).is_err());
    // Backslash inside the name half rejected.
    assert!(locate_dotnet_sdk(tmp.path(), None, "foo\\bar/1.0.0", None, None).is_err());
    // NUL inside the name half rejected.
    assert!(locate_dotnet_sdk(tmp.path(), None, "foo\0bar/1.0.0", None, None).is_err());
}

#[test]
fn rejects_malformed_version_half_even_when_name_is_safe() {
    // If a project installs the bare name `Microsoft.NET.Sdk` but
    // declares `Sdk="Microsoft.NET.Sdk/<gibberish>"`, the resolver
    // must NOT silently degenerate to the bare-name lookup. Better
    // a clean None → `SdkNotFound` than a wrong partial resolution
    // that hides whatever the user actually wrote.
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");

    // Empty version.
    assert!(locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk/", None, None).is_err());
    // `..` in version — would escape if forwarded blindly.
    assert!(locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk/../1.0.0", None, None).is_err());
    // Multi-SDK joined string. MSBuild does support `A;B` form for
    // multiple SDKs, but the seam delivers it as one opaque value
    // and v1a doesn't decompose. Better to surface SdkNotFound
    // than to drop the second SDK on the floor.
    assert!(
        locate_dotnet_sdk(
            tmp.path(),
            None,
            "Microsoft.NET.Sdk/1.0.0;Other/2.0",
            None,
            None
        )
        .is_err()
    );
    // Non-semver gibberish.
    assert!(locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk/abc", None, None).is_err());
    // Multi-SDK string where the *first* version carries a prerelease
    // tail. Without prerelease-character validation, the whole
    // `preview.1;Other/2.0` tail would be accepted as an opaque
    // suffix and the resolver would splice `Microsoft.NET.Sdk` while
    // dropping `Other` on the floor.
    assert!(
        locate_dotnet_sdk(
            tmp.path(),
            None,
            "Microsoft.NET.Sdk/1.0.0-preview.1;Other/2.0",
            None,
            None,
        )
        .is_err()
    );
    // Same shape with a `/` smuggled into the suffix.
    assert!(
        locate_dotnet_sdk(
            tmp.path(),
            None,
            "Microsoft.NET.Sdk/1.0.0-rc/../etc",
            None,
            None
        )
        .is_err()
    );
}

#[test]
fn rejects_path_escape_via_absolute_sdk_name() {
    // A malicious or buggy `<Project Sdk="/etc/passwd">` would
    // otherwise make `PathBuf::join("/etc/passwd")` discard the
    // `{dotnet_root}/sdk/{version}/Sdks/` prefix and let the
    // resolver read files outside `dotnet_root`. Stage a "real"
    // SDK on disk so we know the failure is the name-validator
    // and not a missing-files fallthrough.
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");
    // Pretend `/Sdks/Microsoft.NET.Sdk` *also* exists outside the
    // sandbox — we don't have to actually create it; the
    // validator must reject the input before any filesystem
    // probe.
    assert!(locate_dotnet_sdk(tmp.path(), None, "/Microsoft.NET.Sdk", None, None).is_err());
    assert!(locate_dotnet_sdk(tmp.path(), None, "/etc/passwd", None, None).is_err());
}

#[test]
fn rejects_parent_dir_traversal() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");
    assert!(locate_dotnet_sdk(tmp.path(), None, "..", None, None).is_err());
    assert!(locate_dotnet_sdk(tmp.path(), None, "../etc", None, None).is_err());
    assert!(locate_dotnet_sdk(tmp.path(), None, "foo/../bar", None, None).is_err());
}

#[test]
fn rejects_separators_inside_name() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");
    // Backslash on macOS/Linux is just a regular byte to the
    // filesystem, but it would matter on Windows. Reject in
    // either case to keep behaviour portable.
    assert!(locate_dotnet_sdk(tmp.path(), None, "foo/bar", None, None).is_err());
    assert!(locate_dotnet_sdk(tmp.path(), None, "foo\\bar", None, None).is_err());
    // Empty rejected.
    assert!(locate_dotnet_sdk(tmp.path(), None, "", None, None).is_err());
    // NUL rejected.
    assert!(locate_dotnet_sdk(tmp.path(), None, "foo\0bar", None, None).is_err());
}

#[test]
fn accepts_real_dotnet_sdk_names() {
    // Sanity: the validator must not exclude any legitimate name
    // we'd actually see in the wild. (`install_sdk` would itself
    // fail under a Windows-only invalid name, so this is also a
    // soft cross-platform check.)
    for name in [
        "Microsoft.NET.Sdk",
        "Microsoft.NET.Sdk.Web",
        "Microsoft.NET.Sdk.Worker",
        "Microsoft.NET.Sdk.Razor",
        "Microsoft.Build.NoTargets",
        "MSBuild.Sdk.Extras",
    ] {
        let tmp = TempDir::new().unwrap();
        install_sdk(tmp.path(), "8.0.401", name);
        assert!(
            locate_dotnet_sdk(tmp.path(), None, name, None, None).is_ok(),
            "validator wrongly rejected {name:?}"
        );
    }
}

#[test]
fn variant_sdks_resolve_to_their_own_directories() {
    // Install the in-box base SDK and its multi-TFM variants
    // (`Microsoft.NET.Sdk.Web`, `…Worker`, `…Razor`) side-by-side at
    // the same version. Each lookup must return that variant's own
    // `Sdks/<name>/Sdk/` directory and not bleed into a sibling's.
    //
    // `accepts_real_dotnet_sdk_names` only proves each name parses
    // when installed *alone*. The variants share the same `sdk/<v>/`
    // version slot, so an evaluator-side mistake that ignored the
    // `Sdk` attribute (e.g. always picked the first `Sdks/` entry)
    // would silently pick whichever variant `read_dir` happened to
    // yield first. This test catches that.
    let tmp = TempDir::new().unwrap();
    let names = [
        "Microsoft.NET.Sdk",
        "Microsoft.NET.Sdk.Web",
        "Microsoft.NET.Sdk.Worker",
        "Microsoft.NET.Sdk.Razor",
    ];
    for name in names {
        install_sdk(tmp.path(), "8.0.401", name);
    }
    for name in names {
        let paths = locate_dotnet_sdk(tmp.path(), None, name, None, None).unwrap();
        // `…/sdk/<version>/Sdks/<sdk_name>/Sdk/Sdk.props` — two parents
        // past `Sdk.props` lands on `<sdk_name>`.
        let sdk_segment = paths
            .props
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .expect("dotnet-root props path has the canonical layout");
        assert_eq!(
            sdk_segment,
            std::ffi::OsStr::new(name),
            "lookup for {name:?} landed in the wrong Sdks/ subdirectory: {}",
            paths.props.display(),
        );
    }
}

// ---------------- locate_dotnet_sdk wired to parse_fsproj_with_imports ----------------

#[test]
fn end_to_end_resolver_splices_sdk_imports() {
    // Install a synthetic `Microsoft.NET.Sdk` whose `Sdk.props` seeds
    // a property that the project body references, then parse the
    // project with `locate_dotnet_sdk` as the resolver. If the wiring
    // works, the body's `$(SdkSeed)` substitutes; if it doesn't,
    // we'd see an `UndefinedProperty` diagnostic and the Compile
    // item path would be literally `$(SdkSeed).fs`.
    let dotnet = TempDir::new().unwrap();
    let sdk_root = dotnet
        .path()
        .join("sdk")
        .join("8.0.401")
        .join("Sdks")
        .join("Microsoft.NET.Sdk")
        .join("Sdk");
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(
        sdk_root.join("Sdk.props"),
        r#"<Project><PropertyGroup><SdkSeed>fromprops</SdkSeed></PropertyGroup></Project>"#,
    )
    .unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();

    let proj = TempDir::new().unwrap();
    let project_path = proj.path().join("Demo.fsproj");
    fs::write(
        &project_path,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();

    let dotnet_root = dotnet.path().to_path_buf();
    let resolver = |name: &str| {
        locate_dotnet_sdk(&dotnet_root, None, name, None, None).map(crate::SdkResolution::from)
    };
    let canon_project = fs::canonicalize(&project_path).unwrap();
    let canon_proj_dir = canon_project.parent().unwrap();
    let result = parse_fsproj_with_imports(
        &fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .unwrap();

    let item_paths: Vec<_> = result.items.iter().map(|i| i.include.clone()).collect();
    assert_eq!(item_paths, vec![canon_proj_dir.join("fromprops.fs")]);
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            d.kind,
            DiagnosticKind::SdkNotFound { .. } | DiagnosticKind::UndefinedProperty { .. }
        )),
        "SDK resolved end-to-end; no diagnostics expected: {:?}",
        result.diagnostics,
    );
}

#[test]
fn end_to_end_resolver_splices_variant_sdk() {
    // Like `end_to_end_resolver_splices_sdk_imports` but with
    // `Sdk="Microsoft.NET.Sdk.Web"`. The evaluator should pass the
    // attribute verbatim to the resolver; any latent special-casing
    // of the base SDK name (e.g. hard-coded probes, conditional
    // splice logic) would only fire on the base and would silently
    // skip the splice here. The seed-property trick lets us detect
    // that without depending on Web-specific MSBuild semantics.
    let dotnet = TempDir::new().unwrap();
    let sdk_root = dotnet
        .path()
        .join("sdk")
        .join("8.0.401")
        .join("Sdks")
        .join("Microsoft.NET.Sdk.Web")
        .join("Sdk");
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(
        sdk_root.join("Sdk.props"),
        r#"<Project><PropertyGroup><SdkSeed>fromwebprops</SdkSeed></PropertyGroup></Project>"#,
    )
    .unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();

    let proj = TempDir::new().unwrap();
    let project_path = proj.path().join("Demo.fsproj");
    fs::write(
        &project_path,
        r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();

    let dotnet_root = dotnet.path().to_path_buf();
    let resolver = |name: &str| {
        locate_dotnet_sdk(&dotnet_root, None, name, None, None).map(crate::SdkResolution::from)
    };
    let canon_project = fs::canonicalize(&project_path).unwrap();
    let canon_proj_dir = canon_project.parent().unwrap();
    let result = parse_fsproj_with_imports(
        &fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .unwrap();

    let item_paths: Vec<_> = result.items.iter().map(|i| i.include.clone()).collect();
    assert_eq!(item_paths, vec![canon_proj_dir.join("fromwebprops.fs")]);
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            d.kind,
            DiagnosticKind::SdkNotFound { .. } | DiagnosticKind::UndefinedProperty { .. }
        )),
        "SDK resolved end-to-end; no diagnostics expected: {:?}",
        result.diagnostics,
    );
}

#[test]
fn end_to_end_resolver_missing_sdk_yields_sdk_not_found() {
    // The seam contract: returning `None` from the resolver must
    // surface as `SdkNotFound`, not `UnsupportedConstruct`. The
    // walker still produces a result.
    let dotnet = TempDir::new().unwrap();
    fs::create_dir_all(dotnet.path().join("sdk")).unwrap(); // empty
    let proj = TempDir::new().unwrap();
    let project_path = proj.path().join("Demo.fsproj");
    fs::write(
        &project_path,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="lib.fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();

    let dotnet_root = dotnet.path().to_path_buf();
    let resolver = |name: &str| {
        locate_dotnet_sdk(&dotnet_root, None, name, None, None).map(crate::SdkResolution::from)
    };
    let canon_project = fs::canonicalize(&project_path).unwrap();
    let result = parse_fsproj_with_imports(
        &fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .unwrap();

    let kinds: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|d| match &d.kind {
            DiagnosticKind::SdkNotFound { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec!["Microsoft.NET.Sdk"]);
    // Body still produces its compile item — best-effort fallback.
    assert_eq!(result.items.len(), 1);
}

// ---------------- locate_dotnet_sdk with VersionSpec ----------------

#[test]
fn spec_exact_pin_with_disable_picks_pinned_version() {
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "8.0.100", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(tmp.path(), "9.0.100", "Microsoft.NET.Sdk");

    let pin = SdkVersion::parse("8.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let paths =
        locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", Some(&spec), None).unwrap();
    // Despite 9.0.100 being installed (and otherwise preferred under
    // the spec-less branch), Disable forces the lower exact match.
    assert_eq!(version_dir_name(&paths.props), "8.0.100");
}

#[test]
fn spec_disable_with_missing_pin_yields_version_not_satisfied() {
    // Only 9.0.100 is installed, but the spec pins 8.0.100 with
    // `disable`. The resolver must surface the spec-side failure
    // (with the installed list attached) so the caller can produce
    // a `SdkVersionNotSatisfied` diagnostic.
    let tmp = TempDir::new().unwrap();
    install_sdk(tmp.path(), "9.0.100", "Microsoft.NET.Sdk");

    let pin = SdkVersion::parse("8.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let err = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", Some(&spec), None)
        .expect_err("spec misses");
    match err {
        SdkResolveError::VersionNotSatisfied { spec: s, available } => {
            assert_eq!(s, spec);
            // `available` is sorted; only one installed version.
            assert_eq!(available.len(), 1);
            assert_eq!(available[0].to_string(), "9.0.100");
        }
        SdkResolveError::NotFound => panic!("expected VersionNotSatisfied, got NotFound"),
        SdkResolveError::UnsupportedLayout { reason } => {
            panic!("expected VersionNotSatisfied, got UnsupportedLayout: {reason}")
        }
    }
}

#[test]
fn spec_not_found_dominates_version_not_satisfied_when_no_candidates_exist() {
    // `dotnet_root/sdk/{version}/Sdks/Microsoft.NET.Sdk/` is missing
    // entirely. The spec is moot because we couldn't even reach the
    // selection step — `NotFound` is the right diagnostic, not
    // `VersionNotSatisfied` (which would imply some versions are
    // installed, just not the right ones).
    let tmp = TempDir::new().unwrap();
    let pin = SdkVersion::parse("8.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let err = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", Some(&spec), None)
        .expect_err("nothing here");
    assert!(matches!(err, SdkResolveError::NotFound));
}

#[test]
fn end_to_end_resolver_version_not_satisfied_surfaces_in_diagnostic() {
    // The contract for the new diagnostic: when the resolver returns
    // `VersionNotSatisfied`, the walker emits
    // `DiagnosticKind::SdkVersionNotSatisfied` carrying the spec and
    // available list (rather than the generic `SdkNotFound`). The
    // body still walks — same best-effort fallback.
    let dotnet = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "9.0.100", "Microsoft.NET.Sdk");

    let proj = TempDir::new().unwrap();
    let project_path = proj.path().join("Demo.fsproj");
    fs::write(
        &project_path,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="lib.fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();

    let dotnet_root = dotnet.path().to_path_buf();
    let pin = SdkVersion::parse("8.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let resolver = |name: &str| {
        locate_dotnet_sdk(&dotnet_root, None, name, Some(&spec), None)
            .map(crate::SdkResolution::from)
    };
    let canon_project = fs::canonicalize(&project_path).unwrap();
    let result = parse_fsproj_with_imports(
        &fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .unwrap();

    let mut found = false;
    for d in &result.diagnostics {
        if let DiagnosticKind::SdkVersionNotSatisfied {
            name,
            spec: diag_spec,
            available,
        } = &d.kind
        {
            assert_eq!(name, "Microsoft.NET.Sdk");
            assert_eq!(*diag_spec, spec);
            assert_eq!(available.len(), 1);
            assert_eq!(available[0].to_string(), "9.0.100");
            found = true;
        }
    }
    assert!(
        found,
        "expected a SdkVersionNotSatisfied diagnostic; got: {:?}",
        result.diagnostics
    );
    // Best-effort body still emits the Compile item.
    assert_eq!(result.items.len(), 1);
}

// ---------------- NuGet-cache fallback ----------------

#[test]
fn per_import_pin_resolves_against_nuget_cache() {
    // Third-party SDKs (Arcade, NoTargets, …) live in the per-user
    // NuGet cache, not `$DOTNET_ROOT`. With a per-import version
    // pin supplying the version source upstream's NuGet SDK resolver
    // requires, a request for one of these names against an empty
    // `$DOTNET_ROOT` but populated NuGet cache must resolve.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "1.0.80", "Microsoft.Build.NoTargets");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets/1.0.80",
        None,
        None,
    )
    .unwrap();
    assert!(
        !picked_from_dotnet_root(&paths.props),
        "path should be under the NuGet cache: {}",
        paths.props.display()
    );
    assert_eq!(version_dir_name(&paths.props), "1.0.80");
}

#[test]
fn nuget_lowercases_package_directory() {
    // The user names the SDK with mixed casing (matching NuGet.org's
    // canonical id), but the on-disk package directory is always
    // lowercased. Without this, `<Project Sdk="Microsoft.DotNet.Arcade.Sdk/9.0.0">`
    // would `read_dir` a directory that doesn't exist on case-sensitive
    // filesystems.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    // Install under the lowercased name (as `dotnet restore` would).
    install_sdk_nuget(nuget.path(), "9.0.0", "Microsoft.DotNet.Arcade.Sdk");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        // Resolve via the mixed-case canonical id (as in fsproj). The
        // version pin is the version source the NuGet half requires;
        // the lowercasing assertion is independent of it.
        "Microsoft.DotNet.Arcade.Sdk/9.0.0",
        None,
        None,
    )
    .unwrap();
    assert!(!picked_from_dotnet_root(&paths.props));
    assert_eq!(version_dir_name(&paths.props), "9.0.0");
}

#[test]
fn nuget_inner_sdk_directory_can_be_capitalized() {
    // Microsoft's documented convention for MSBuild Project SDK packages
    // is a capital-S `Sdk/` directory at the package root, and packages
    // like `Microsoft.Build.NoTargets`, `Microsoft.NET.ILLink.Tasks`,
    // and `Microsoft.Build.Traversal` ship that casing. On
    // case-sensitive filesystems, probing only lowercase `sdk/` skips
    // these entirely. The resolver must accept either casing.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget_cased(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets", "Sdk");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets/3.7.134",
        None,
        None,
    )
    .unwrap();
    assert!(!picked_from_dotnet_root(&paths.props));
    assert_eq!(version_dir_name(&paths.props), "3.7.134");
    // Verify the exact inner directory casing is preserved — proves the
    // probe actually matched against `Sdk/`, not a case-insensitive
    // filesystem coincidence.
    assert_eq!(
        paths.props.parent().and_then(Path::file_name).unwrap(),
        std::ffi::OsStr::new("Sdk"),
    );
}

#[test]
fn unpinned_reference_ignores_nuget_even_when_dotnet_has_lower() {
    // Without a version source for NuGet, the cache is not consulted
    // at all — even if it carries a higher version than `$DOTNET_ROOT`.
    // This matches upstream's `Microsoft.Build.NuGetSdkResolver`,
    // which refuses to scan the cache without a `Sdk="Name/Version"`
    // per-import pin (or, in the upstream model, an `msbuild-sdks`
    // entry in `global.json` — deferred). The DOTNET pick wins
    // regardless of NuGet's ordering.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "8.0.100", "Microsoft.NET.Sdk");
    install_sdk_nuget(nuget.path(), "9.0.100", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        None,
        None,
    )
    .unwrap();
    assert!(
        picked_from_dotnet_root(&paths.props),
        "no per-import pin → NuGet must be ignored; got {}",
        paths.props.display()
    );
    assert_eq!(version_dir_name(&paths.props), "8.0.100");
}

#[test]
fn unpinned_reference_to_nuget_only_sdk_yields_not_found() {
    // The codex-P2 fix: unpinned reference + DOTNET has none + NuGet
    // cache has versions → NotFound. Upstream's NuGet SDK resolver
    // emits "did not resolve this SDK because there was no version
    // specified in the project or global.json"; we surface the same
    // outcome (as `SdkNotFound`) instead of silently picking the
    // highest restored copy, which could import a different SDK
    // than MSBuild would.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "1.0.80", "Microsoft.Build.NoTargets");
    install_sdk_nuget(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets");

    let err = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets",
        None,
        None,
    )
    .expect_err("unpinned reference must not silently pick from cache");
    assert!(matches!(err, SdkResolveError::NotFound));
}

#[test]
fn per_import_pin_only_in_nuget_picks_nuget_copy() {
    // Cross-root selection under per-import: when DOTNET has version A
    // but the pin is to version B (only restored in NuGet), the
    // resolver picks the NuGet copy. Demonstrates that the per-import
    // pin reaches across both roots and isn't tied to either.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "8.0.100", "Microsoft.NET.Sdk");
    install_sdk_nuget(nuget.path(), "9.0.100", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk/9.0.100",
        None,
        None,
    )
    .unwrap();
    assert!(!picked_from_dotnet_root(&paths.props));
    assert_eq!(version_dir_name(&paths.props), "9.0.100");
}

#[test]
fn per_import_pin_present_in_both_dotnet_wins_tie() {
    // When both roots host the pinned version, MSBuild's "shared over
    // per-user" preference kicks in — `$DOTNET_ROOT` wins. The
    // `Disable` filter from the per-import pin admits exactly the
    // pinned version in both roots, so this is the standing case for
    // the tie-breaker.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "8.0.401", "Microsoft.NET.Sdk");
    install_sdk_nuget(nuget.path(), "8.0.401", "Microsoft.NET.Sdk");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk/8.0.401",
        None,
        None,
    )
    .unwrap();
    assert!(
        picked_from_dotnet_root(&paths.props),
        "tie should go to $DOTNET_ROOT, got {}",
        paths.props.display()
    );
}

#[test]
fn nuget_dir_missing_falls_through_to_dotnet_root() {
    // Passing `Some(missing_dir)` must not abort the lookup — same
    // tolerance as a `$DOTNET_ROOT/sdk` that doesn't exist. The
    // `$DOTNET_ROOT` candidate still resolves.
    let dotnet = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "8.0.401", "Microsoft.NET.Sdk");
    let tmp = TempDir::new().unwrap();
    let missing_nuget = tmp.path().join("does-not-exist");

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(&missing_nuget),
        "Microsoft.NET.Sdk",
        None,
        None,
    )
    .unwrap();
    assert_eq!(version_dir_name(&paths.props), "8.0.401");
}

#[test]
fn both_roots_empty_yields_not_found() {
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();

    let err = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        None,
        None,
    )
    .expect_err("neither root has the SDK");
    assert!(matches!(err, SdkResolveError::NotFound));
}

#[test]
fn caller_spec_does_not_make_nuget_eligible() {
    // Under the codex-P2 fix, the caller `spec` (a `global.json`
    // `sdk.version` pin) is *not* a version source for NuGet — its
    // scope is the host .NET SDK install only. So setting a spec
    // does not cause the resolver to start considering NuGet
    // candidates. Here DOTNET has `1.0.300` (which the spec admits)
    // and NuGet has `1.0.399`; pre-fix the resolver would have
    // picked the higher NuGet copy. Post-fix it sticks with DOTNET's
    // admitted candidate.
    //
    // Forcing NuGet selection requires a per-import pin — that
    // direction is exercised by `per_import_pin_only_in_nuget_*`.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "1.0.300", "Microsoft.NET.Sdk");
    install_sdk_nuget(nuget.path(), "1.0.399", "Microsoft.NET.Sdk");

    let pin = SdkVersion::parse("1.0.300").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::LatestPatch, false);
    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        Some(&spec),
        None,
    )
    .unwrap();
    assert_eq!(version_dir_name(&paths.props), "1.0.300");
    assert!(picked_from_dotnet_root(&paths.props));
}

#[test]
fn version_not_satisfied_reports_dotnet_root_available_list() {
    // The diagnostic must surface the `$DOTNET_ROOT` versions the spec
    // was consulted against — those are the versions the spec
    // *could* have admitted but didn't. NuGet versions aren't filtered
    // by the spec (they're third-party Project SDK packages, not
    // governed by `global.json`'s `sdk.version`), so they wouldn't
    // help the user pick a satisfying pin — listing them would
    // mislead.
    //
    // We seed multiple DOTNET versions to verify the list is the
    // full DOTNET set, sorted ascending; NuGet is empty for this
    // SDK name so the resolver actually reaches the
    // `VersionNotSatisfied` branch (a non-empty NuGet entry would
    // pass through unfiltered and the call would succeed).
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "10.0.100", "Microsoft.NET.Sdk");
    install_sdk(dotnet.path(), "9.0.100", "Microsoft.NET.Sdk");

    let pin = SdkVersion::parse("8.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let err = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        Some(&spec),
        None,
    )
    .expect_err("disable + missing pin must miss");
    match err {
        SdkResolveError::VersionNotSatisfied { spec: s, available } => {
            assert_eq!(s, spec);
            let printed: Vec<String> = available.iter().map(SdkVersion::to_string).collect();
            // `available` is sorted ascending.
            assert_eq!(printed, vec!["9.0.100".to_string(), "10.0.100".to_string()]);
        }
        SdkResolveError::NotFound => panic!("expected VersionNotSatisfied, got NotFound"),
        SdkResolveError::UnsupportedLayout { reason } => {
            panic!("expected VersionNotSatisfied, got UnsupportedLayout: {reason}")
        }
    }
}

#[test]
fn caller_spec_rejecting_dotnet_yields_version_not_satisfied_even_with_nuget_cache() {
    // Companion to the test above: when the spec rejects every
    // `$DOTNET_ROOT` candidate, the result is `VersionNotSatisfied`
    // — *not* a silent NuGet fallback. Without a version source for
    // NuGet (per-import or `msbuild-sdks`), the cache is not even
    // looked at. The diagnostic carries the DOTNET versions the
    // spec was consulted against so the user can either install a
    // matching version or update the pin.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "9.0.100", "Microsoft.NET.Sdk");
    install_sdk_nuget(nuget.path(), "10.0.100", "Microsoft.NET.Sdk");

    let pin = SdkVersion::parse("8.0.100").unwrap();
    let spec = VersionSpec::with_version(pin, RollForward::Disable, false);
    let err = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        Some(&spec),
        None,
    )
    .expect_err("caller spec rejects DOTNET; no NuGet version source ⇒ unsatisfied");
    match err {
        SdkResolveError::VersionNotSatisfied { spec: s, available } => {
            assert_eq!(s, spec);
            let printed: Vec<String> = available.iter().map(SdkVersion::to_string).collect();
            assert_eq!(printed, vec!["9.0.100".to_string()]);
        }
        SdkResolveError::NotFound => panic!("expected VersionNotSatisfied, got NotFound"),
        SdkResolveError::UnsupportedLayout { reason } => {
            panic!("expected VersionNotSatisfied, got UnsupportedLayout: {reason}")
        }
    }
}

// ---------------- locate_dotnet_sdk with msbuild-sdks pin map ----------------

#[test]
fn msbuild_sdks_entry_alone_makes_nuget_eligible() {
    // A `global.json` `msbuild-sdks` entry is the project-wide
    // equivalent of `Sdk="Name/Version"`: it's a NuGet version source
    // and unlocks the cache for that SDK. With only the cache populated
    // (no `$DOTNET_ROOT` copy) and no per-import pin, the resolver must
    // still resolve via NuGet.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets");

    let mut pins = std::collections::BTreeMap::new();
    pins.insert(
        "Microsoft.Build.NoTargets".to_owned(),
        SdkVersion::parse("3.7.134").unwrap(),
    );
    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets",
        None,
        Some(&pins),
    )
    .expect("msbuild-sdks entry should unlock NuGet");
    assert!(!picked_from_dotnet_root(&paths.props));
    assert_eq!(version_dir_name(&paths.props), "3.7.134");
}

#[test]
fn msbuild_sdks_pin_overrides_caller_spec_in_dotnet_root() {
    // Like the per-import pin, an `msbuild-sdks` entry acts on both
    // roots with `Disable` semantics — including overriding the caller
    // `spec` (which is the `global.json` `sdk` block). Caller spec
    // pins to a *different* version with `Disable`; the project-wide
    // map asks for the version that's actually installed, and it wins.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "9.0.100", "Microsoft.NET.Sdk");

    let caller_pin = SdkVersion::parse("8.0.401").unwrap();
    let caller_spec = VersionSpec::with_version(caller_pin, RollForward::Disable, false);

    let mut pins = std::collections::BTreeMap::new();
    pins.insert(
        "Microsoft.NET.Sdk".to_owned(),
        SdkVersion::parse("9.0.100").unwrap(),
    );

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        Some(&caller_spec),
        Some(&pins),
    )
    .expect("msbuild-sdks pin should override caller spec");
    assert_eq!(version_dir_name(&paths.props), "9.0.100");
    assert!(picked_from_dotnet_root(&paths.props));
}

#[test]
fn per_import_pin_wins_over_msbuild_sdks_entry() {
    // Precedence: a per-import `Sdk="Name/Version"` is the more
    // specific request (written at the import site), so it wins over
    // a project-wide `msbuild-sdks[Name]` for the same SDK. Stage
    // both versions in NuGet so the resolver can actually pick either.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "1.0.80", "Microsoft.Build.NoTargets");
    install_sdk_nuget(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets");

    let mut pins = std::collections::BTreeMap::new();
    pins.insert(
        "Microsoft.Build.NoTargets".to_owned(),
        SdkVersion::parse("3.7.134").unwrap(),
    );

    // Per-import pins to 1.0.80; msbuild-sdks pins to 3.7.134. The
    // per-import version wins.
    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets/1.0.80",
        None,
        Some(&pins),
    )
    .expect("per-import pin should beat msbuild-sdks");
    assert_eq!(version_dir_name(&paths.props), "1.0.80");
}

#[test]
fn msbuild_sdks_pin_to_missing_version_surfaces_unsatisfied() {
    // Parallel to `per_import_version_pin_fails_when_only_other_versions_present`:
    // when the project-wide pin names a version that isn't installed
    // but other versions of the same SDK are, the resolver must NOT
    // silently fall through. The diagnostic carries the alternatives
    // from *both* roots (the pin filtered both).
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk_nuget(nuget.path(), "3.7.134", "Microsoft.Build.NoTargets");

    let mut pins = std::collections::BTreeMap::new();
    pins.insert(
        "Microsoft.Build.NoTargets".to_owned(),
        SdkVersion::parse("1.0.80").unwrap(),
    );

    let err = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.Build.NoTargets",
        None,
        Some(&pins),
    )
    .expect_err("msbuild-sdks pin to missing version must miss");
    match err {
        SdkResolveError::VersionNotSatisfied { spec, available } => {
            let pin = SdkVersion::parse("1.0.80").unwrap();
            assert_eq!(spec.version(), Some(&pin));
            assert_eq!(spec.roll_forward(), RollForward::Disable);
            let printed: Vec<String> = available.iter().map(SdkVersion::to_string).collect();
            assert_eq!(printed, vec!["3.7.134".to_string()]);
        }
        SdkResolveError::NotFound => {
            panic!("expected VersionNotSatisfied (NuGet had alternatives), got NotFound")
        }
        SdkResolveError::UnsupportedLayout { reason } => {
            panic!("expected VersionNotSatisfied, got UnsupportedLayout: {reason}")
        }
    }
}

#[test]
fn msbuild_sdks_entry_for_other_sdk_does_not_affect_lookup() {
    // Lookup is by name. An `msbuild-sdks` entry for a *different* SDK
    // does not unlock the NuGet half for the SDK being requested —
    // without a pin for the requested name, the cache stays gated and
    // the caller `spec` rules `$DOTNET_ROOT` as usual.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    install_sdk(dotnet.path(), "8.0.100", "Microsoft.NET.Sdk");
    install_sdk_nuget(nuget.path(), "9.0.100", "Microsoft.NET.Sdk");

    // Pin a different SDK; the request is for Microsoft.NET.Sdk.
    let mut pins = std::collections::BTreeMap::new();
    pins.insert(
        "Microsoft.Build.NoTargets".to_owned(),
        SdkVersion::parse("3.7.134").unwrap(),
    );

    let paths = locate_dotnet_sdk(
        dotnet.path(),
        Some(nuget.path()),
        "Microsoft.NET.Sdk",
        None,
        Some(&pins),
    )
    .expect("DOTNET pick should resolve normally");
    assert!(
        picked_from_dotnet_root(&paths.props),
        "NuGet cache must stay gated when the pin is for a different SDK"
    );
    assert_eq!(version_dir_name(&paths.props), "8.0.100");
}

#[test]
fn end_to_end_per_import_pin_resolves_from_nuget() {
    // Mirror of `end_to_end_resolver_splices_sdk_imports`, but the
    // synthetic SDK lives only in the NuGet cache and the project's
    // `Sdk=` attribute supplies the per-import version pin that
    // unlocks the cache. Exercises the full pipeline end-to-end:
    // substitution, splice ordering, the resolver's NuGet branch,
    // and the per-import version-source rule.
    let dotnet = TempDir::new().unwrap();
    let nuget = TempDir::new().unwrap();
    let sdk_root = nuget.path().join("mysdk").join("1.2.3").join("sdk");
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(
        sdk_root.join("Sdk.props"),
        r#"<Project><PropertyGroup><SdkSeed>fromnuget</SdkSeed></PropertyGroup></Project>"#,
    )
    .unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();

    let proj = TempDir::new().unwrap();
    let project_path = proj.path().join("Demo.fsproj");
    fs::write(
        &project_path,
        r#"<Project Sdk="MySdk/1.2.3">
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();

    let dotnet_root = dotnet.path().to_path_buf();
    let nuget_dir = nuget.path().to_path_buf();
    let resolver = |name: &str| {
        locate_dotnet_sdk(&dotnet_root, Some(&nuget_dir), name, None, None)
            .map(crate::SdkResolution::from)
    };
    let canon_project = fs::canonicalize(&project_path).unwrap();
    let canon_proj_dir = canon_project.parent().unwrap();
    let result = parse_fsproj_with_imports(
        &fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .unwrap();

    let item_paths: Vec<_> = result.items.iter().map(|i| i.include.clone()).collect();
    assert_eq!(item_paths, vec![canon_proj_dir.join("fromnuget.fs")]);
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            d.kind,
            DiagnosticKind::SdkNotFound { .. } | DiagnosticKind::UndefinedProperty { .. }
        )),
        "SDK resolved end-to-end from NuGet; no diagnostics expected: {:?}",
        result.diagnostics,
    );
}

// ---------------- SdkVersion parsing and ordering ----------------

#[test]
fn version_parse_stable() {
    let v = SdkVersion::parse("8.0.401").unwrap();
    assert_eq!(v.numeric, vec![8, 0, 401]);
    assert_eq!(v.prerelease, None);
}

#[test]
fn version_parse_prerelease() {
    let v = SdkVersion::parse("9.0.100-preview.1.24101.2").unwrap();
    assert_eq!(v.numeric, vec![9, 0, 100]);
    assert_eq!(v.prerelease.as_deref(), Some("preview.1.24101.2"));
}

#[test]
fn version_parse_rejects_non_numeric() {
    assert!(SdkVersion::parse("NuGetFallbackFolder").is_none());
    assert!(SdkVersion::parse("8.x.401").is_none());
    assert!(SdkVersion::parse("").is_none());
    assert!(SdkVersion::parse("-preview").is_none());
}

#[test]
fn version_parse_rejects_non_semver_prerelease() {
    // `SdkVersion::parse` doubles as the version-half validator for
    // `Sdk="Name/Version"` references. Anything outside the SemVer 2
    // prerelease alphabet (`[0-9A-Za-z.-]`) must be refused so that
    // multi-SDK strings, traversal attempts, or NUL bytes can't ride
    // in on the prerelease tail.
    assert!(SdkVersion::parse("1.0.0-").is_none(), "empty suffix");
    assert!(
        SdkVersion::parse("1.0.0-preview.1;Other/2.0").is_none(),
        "semicolon (multi-SDK joiner)"
    );
    assert!(
        SdkVersion::parse("1.0.0-rc/../etc").is_none(),
        "forward slash"
    );
    assert!(SdkVersion::parse("1.0.0-with space").is_none(), "space");
    assert!(SdkVersion::parse("1.0.0-with\0nul").is_none(), "NUL");
    // Sanity: legitimate .NET SDK prereleases still parse.
    assert!(SdkVersion::parse("9.0.100-preview.1.24101.2").is_some());
    assert!(SdkVersion::parse("9.0.100-rc.2.24474.11").is_some());
}

#[test]
fn version_ordering_numeric_dominates_lex() {
    let v9 = SdkVersion::parse("9.0.100").unwrap();
    let v10 = SdkVersion::parse("10.0.100").unwrap();
    assert!(v10 > v9, "10.0.100 must compare greater than 9.0.100");
}

#[test]
fn version_ordering_stable_beats_prerelease() {
    let stable = SdkVersion::parse("9.0.100").unwrap();
    let preview = SdkVersion::parse("9.0.100-preview.1").unwrap();
    let rc = SdkVersion::parse("9.0.100-rc.2").unwrap();
    assert!(stable > preview);
    assert!(stable > rc);
    // Lex compare within prereleases: "preview..." < "rc..."
    assert!(rc > preview);
}

#[test]
fn version_ordering_shorter_pads_zero() {
    let short = SdkVersion::parse("8.0").unwrap();
    let long = SdkVersion::parse("8.0.1").unwrap();
    assert!(long > short);
    let eq = SdkVersion::parse("8.0.0").unwrap();
    assert!(short == eq, "missing trailing components count as zero");
}

#[test]
fn prerelease_numeric_identifiers_compared_numerically() {
    // SemVer 2.0.0 §11.4.1: dot-separated prerelease identifiers
    // consisting only of digits compare numerically, not as ASCII
    // strings. Lexicographic comparison would say `preview.2` is
    // greater than `preview.10` (because the digit `2` > `1`), so a
    // naive string compare lets `locate_dotnet_sdk` pick a stale SDK.
    let p2 = SdkVersion::parse("9.0.100-preview.2").unwrap();
    let p10 = SdkVersion::parse("9.0.100-preview.10").unwrap();
    assert!(p10 > p2, "preview.10 must outrank preview.2");
}

#[test]
fn prerelease_numeric_lower_than_alphanumeric() {
    // SemVer 2.0.0 §11.4.3: numeric identifiers always have lower
    // precedence than alphanumeric ones, regardless of the integer
    // value. `999` < `alpha` even though `999 > 9` numerically.
    let num = SdkVersion::parse("9.0.100-999").unwrap();
    let alpha = SdkVersion::parse("9.0.100-alpha").unwrap();
    assert!(alpha > num);
}

#[test]
fn prerelease_more_identifiers_is_greater_with_equal_prefix() {
    // SemVer 2.0.0 §11.4.4: when one prerelease is a prefix of the
    // other, the longer one has higher precedence.
    let short = SdkVersion::parse("9.0.100-alpha").unwrap();
    let long = SdkVersion::parse("9.0.100-alpha.1").unwrap();
    assert!(long > short);
}

#[test]
fn parse_rejects_leading_zero_numeric_prerelease_identifier() {
    // SemVer 2.0.0 §9: Numeric identifiers MUST NOT include leading
    // zeroes. Without this rule, `preview.01` would parse to a
    // distinct `SdkVersion` from `preview.1` (different string), but
    // the numeric prerelease comparator parses both as `1` and
    // reports `Ordering::Equal` — breaking the `a == b iff a.cmp(b)
    // == Equal` contract and making selection nondeterministic for
    // those directory names.
    assert!(SdkVersion::parse("9.0.100-preview.01").is_none());
    assert!(SdkVersion::parse("9.0.100-01").is_none());
    assert!(SdkVersion::parse("9.0.100-preview.00").is_none());
    // Plain `0` is still a valid numeric identifier.
    assert!(SdkVersion::parse("9.0.100-preview.0").is_some());
    // Alphanumeric identifiers may legitimately start with a digit,
    // including with a leading zero — §9 only constrains *numeric*
    // identifiers, defined as all-digit.
    assert!(SdkVersion::parse("9.0.100-0a").is_some());
    assert!(SdkVersion::parse("9.0.100-01a").is_some());
}

#[test]
fn parse_rejects_empty_prerelease_identifier() {
    // SemVer 2.0.0 §9: Identifiers MUST NOT be empty.
    assert!(SdkVersion::parse("9.0.100-.").is_none());
    assert!(SdkVersion::parse("9.0.100-a.").is_none());
    assert!(SdkVersion::parse("9.0.100-a..b").is_none());
    assert!(SdkVersion::parse("9.0.100-.a").is_none());
}

// ---------------- Property tests ----------------

/// Generator for the numeric component vector of an `SdkVersion`. We
/// keep the values small enough that the printed dotted form is short
/// and easy to read in failure output, but large enough that the
/// "numeric dominates lex" property has bite (any single component can
/// exceed nine).
fn numeric_strategy() -> impl Strategy<Value = Vec<u64>> {
    proptest::collection::vec(0u64..=99, 1usize..=4)
}

fn prerelease_strategy() -> impl Strategy<Value = Option<String>> {
    // SemVer numeric identifiers forbid leading zeroes, so the regex
    // matches `0` *or* a non-zero leading digit followed by up to two
    // more digits. Without this constraint the generator could emit
    // `preview.01`, which `SdkVersion::parse` legitimately rejects —
    // breaking the roundtrip property for a reason unrelated to it.
    prop_oneof![
        2 => Just(None),
        1 => "preview\\.(0|[1-9][0-9]{0,2})".prop_map(Some),
        1 => "rc\\.(0|[1-9][0-9]{0,2})".prop_map(Some),
    ]
}

fn version_strategy() -> impl Strategy<Value = SdkVersion> {
    // Mirror `SdkVersion::parse`'s trailing-zero normalisation: the
    // parsed form `8.0.0` and `8.0` are indistinguishable, so the
    // generator must not produce un-normalised inputs or the
    // roundtrip property breaks for legitimate reasons.
    (numeric_strategy(), prerelease_strategy()).prop_map(|(mut numeric, prerelease)| {
        while numeric.len() > 1 && *numeric.last().unwrap() == 0 {
            numeric.pop();
        }
        SdkVersion {
            numeric,
            prerelease,
        }
    })
}

fn format_version(v: &SdkVersion) -> String {
    let head = v
        .numeric
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");
    match &v.prerelease {
        Some(suffix) => format!("{head}-{suffix}"),
        None => head,
    }
}

#[test]
fn proptest_parse_roundtrips_printed_form() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    runner
        .run(&version_strategy(), |v| {
            let printed = format_version(&v);
            let reparsed = SdkVersion::parse(&printed)
                .ok_or_else(|| TestCaseError::fail(format!("failed to parse {printed:?}")))?;
            prop_assert_eq!(reparsed, v);
            Ok(())
        })
        .unwrap();
}

#[test]
fn proptest_ordering_is_total_and_consistent() {
    // For any two versions, exactly one of `<`, `==`, `>` holds, and
    // ordering is antisymmetric / transitive. Generating triples gives
    // us transitivity coverage too.
    let mut runner = TestRunner::new(PtConfig {
        cases: 512,
        ..PtConfig::default()
    });
    runner
        .run(
            &(version_strategy(), version_strategy(), version_strategy()),
            |(a, b, c)| {
                let ab = a.cmp(&b);
                let ba = b.cmp(&a);
                prop_assert_eq!(ab, ba.reverse(), "antisymmetric");
                let ac = a.cmp(&c);
                let bc = b.cmp(&c);
                if ab == Ordering::Equal && bc == Ordering::Equal {
                    prop_assert_eq!(ac, Ordering::Equal, "transitive (==)");
                }
                if ab == Ordering::Less && bc == Ordering::Less {
                    prop_assert_eq!(ac, Ordering::Less, "transitive (<)");
                }
                if ab == Ordering::Greater && bc == Ordering::Greater {
                    prop_assert_eq!(ac, Ordering::Greater, "transitive (>)");
                }
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn proptest_highest_wins_against_reference_max() {
    // Generate a non-empty set of versions; install each as a fake
    // SDK; assert that locate_dotnet_sdk picks the same one
    // `iter().max()` would. The two implementations must agree.
    let mut runner = TestRunner::new(PtConfig {
        cases: 64,
        ..PtConfig::default()
    });
    let strategy = proptest::collection::vec(version_strategy(), 1usize..=6);
    let stable_count = Cell::new(0usize);
    let prerelease_count = Cell::new(0usize);
    runner
        .run(&strategy, |versions| {
            let tmp = TempDir::new().unwrap();
            for v in &versions {
                let name = format_version(v);
                // Two different generator runs can occasionally
                // produce the same printed form; the second
                // install_sdk would overwrite. That's fine — we just
                // want at least one installed copy.
                install_sdk(tmp.path(), &name, "Microsoft.NET.Sdk");
            }
            let expected = versions.iter().max().cloned().unwrap();
            if expected.prerelease.is_none() {
                stable_count.set(stable_count.get() + 1);
            } else {
                prerelease_count.set(prerelease_count.get() + 1);
            }
            let picked = locate_dotnet_sdk(tmp.path(), None, "Microsoft.NET.Sdk", None, None)
                .map_err(|e| TestCaseError::fail(format!("expected resolution, got {e:?}")))?;
            let picked_version = version_dir_name(&picked.props).to_str().unwrap().to_owned();
            prop_assert_eq!(picked_version, format_version(&expected));
            Ok(())
        })
        .unwrap();
    // Sanity: we exercised both regimes. The bias is ~2:1 stable, and
    // we run 64 cases, so seeing zero of either is overwhelmingly
    // unlikely under a correct generator — bound the false-positive
    // rate well below 1e-11 per the property-based-testing skill.
    let s = stable_count.get();
    let p = prerelease_count.get();
    assert!(
        s >= 5 && p >= 5,
        "distribution skew: {s} stable / {p} prerelease in 64 cases"
    );
}

#[test]
fn proptest_resolver_matches_layered_oracle() {
    // Property under test: `locate_dotnet_sdk` agrees with a layered
    // oracle that mirrors the resolver's semantics one-to-one —
    //
    //   per_import → Disable spec over the per-import version
    //   dotnet_spec = per_import ?? caller_spec
    //   dotnet_pick = select(dotnet_versions, dotnet_spec)
    //   nuget_pick  = if per_import is set: select(nuget_versions, per_import)
    //                 else:                  None  (NuGet not consulted)
    //   combine via (DOTNET wins ties, NuGet on strict-higher),
    //   with (None,None) splitting NotFound/VersionNotSatisfied by
    //   whether the considered candidate set was empty.
    //
    // Exercises every interaction: admission filter, NuGet
    // gating on per-import, cross-root comparison, source-root
    // back-mapping, NotFound vs VersionNotSatisfied split, and the
    // `available` list.
    //
    // Universe: four versions with distinct majors, ordering
    // unambiguous, no feature-band collisions.
    let universe: [SdkVersion; 4] = [
        SdkVersion::parse("1.0.0").unwrap(),
        SdkVersion::parse("2.0.0").unwrap(),
        SdkVersion::parse("3.0.0").unwrap(),
        SdkVersion::parse("4.0.0").unwrap(),
    ];

    #[derive(Clone, Copy, Debug)]
    enum Where {
        DotnetOnly,
        NugetOnly,
        Both,
        Neither,
    }

    #[derive(Clone, Copy, Debug)]
    enum SpecChoice {
        NoneSpec,
        ExactV0,
        ExactV1,
        UnknownPin,
    }

    #[derive(Clone, Copy, Debug)]
    enum PerImportChoice {
        NoPin,
        PinV0,
        PinV1,
        PinUnknown,
    }

    // Five scenarios biased to drive each observable bucket:
    //   - UnpinnedDotnetHit: per_import=None, caller=NoneSpec, slots
    //     biased to DOTNET / Both. Drives DotnetPick.
    //   - UnpinnedDotnetMiss: per_import=None, caller=UnknownPin,
    //     slots biased to DOTNET / Both / NuGet. Drives
    //     VersionNotSatisfied (because under the new semantic NuGet
    //     is not consulted without a pin).
    //   - UnpinnedNugetOnly: per_import=None, caller=NoneSpec, slots
    //     in {NugetOnly, Neither}. Drives NotFound (NuGet ignored,
    //     DOTNET empty).
    //   - PinnedNugetWin: per_import=PinV0, slot[0]=NugetOnly
    //     (deterministic), slots[1..] uniform. The pin lives in
    //     NuGet only, so the cascade picks NuGet — drives NugetPick
    //     with a wide margin.
    //   - PinnedMixed: per_import set, slots uniform, caller any.
    //     Exercises the tie path and per-import overrides; also
    //     contributes incidentally to every bucket.
    //
    // Without the per-import variable, the NugetPick bucket can only
    // be reached via a per-import pin under the new semantic, so it
    // must appear in the strategy. Likewise NotFound for a populated
    // NuGet cache requires the per-import to be None.
    #[derive(Clone, Copy, Debug)]
    enum Scenario {
        UnpinnedDotnetHit,
        UnpinnedDotnetMiss,
        UnpinnedNugetOnly,
        PinnedNugetWin,
        PinnedMixed,
    }

    let dotnet_heavy = prop_oneof![
        2 => Just(Where::DotnetOnly),
        2 => Just(Where::Both),
        1 => Just(Where::NugetOnly),
        1 => Just(Where::Neither),
    ];
    let nuget_or_neither = prop_oneof![
        2 => Just(Where::NugetOnly),
        1 => Just(Where::Neither),
    ];
    let mixed_where = prop_oneof![
        1 => Just(Where::DotnetOnly),
        1 => Just(Where::NugetOnly),
        1 => Just(Where::Both),
        1 => Just(Where::Neither),
    ];
    let mixed_spec = prop_oneof![
        1 => Just(SpecChoice::NoneSpec),
        1 => Just(SpecChoice::ExactV0),
        1 => Just(SpecChoice::ExactV1),
        1 => Just(SpecChoice::UnknownPin),
    ];
    let mixed_pin = prop_oneof![
        2 => Just(PerImportChoice::PinV0),
        2 => Just(PerImportChoice::PinV1),
        1 => Just(PerImportChoice::PinUnknown),
    ];

    let unpinned_hit = proptest::array::uniform4(dotnet_heavy.clone())
        .prop_map(|w| {
            (
                Scenario::UnpinnedDotnetHit,
                w,
                SpecChoice::NoneSpec,
                PerImportChoice::NoPin,
            )
        })
        .boxed();
    let unpinned_miss = proptest::array::uniform4(dotnet_heavy)
        .prop_map(|w| {
            (
                Scenario::UnpinnedDotnetMiss,
                w,
                SpecChoice::UnknownPin,
                PerImportChoice::NoPin,
            )
        })
        .boxed();
    let unpinned_nuget_only = proptest::array::uniform4(nuget_or_neither)
        .prop_map(|w| {
            (
                Scenario::UnpinnedNugetOnly,
                w,
                SpecChoice::NoneSpec,
                PerImportChoice::NoPin,
            )
        })
        .boxed();
    let pinned_mixed = (
        proptest::array::uniform4(mixed_where.clone()),
        mixed_spec.clone(),
        mixed_pin,
    )
        .prop_map(|(w, s, p)| (Scenario::PinnedMixed, w, s, p))
        .boxed();
    // PinnedNugetWin pins V0 and forces slot[0] = NugetOnly so the
    // pin's only home is the NuGet cache. The remaining slots vary
    // freely; caller_spec varies too but is overridden by per_import.
    // Per the resolver's gating, dotnet_chosen = None and
    // nuget_chosen = Some(V0), so every case in this scenario lands
    // in the NugetPick bucket.
    let pinned_nuget_win = (proptest::array::uniform3(mixed_where), mixed_spec)
        .prop_map(|(tail, s)| {
            let installs = [Where::NugetOnly, tail[0], tail[1], tail[2]];
            (
                Scenario::PinnedNugetWin,
                installs,
                s,
                PerImportChoice::PinV0,
            )
        })
        .boxed();

    let strategy = prop_oneof![
        1 => unpinned_hit,
        1 => unpinned_miss,
        1 => unpinned_nuget_only,
        1 => pinned_nuget_win,
        1 => pinned_mixed,
    ];

    let mut runner = TestRunner::new(PtConfig {
        cases: 1024,
        ..PtConfig::default()
    });
    let dotnet_pick_count = Cell::new(0usize);
    let nuget_pick_count = Cell::new(0usize);
    let version_not_satisfied_count = Cell::new(0usize);
    let not_found_count = Cell::new(0usize);

    #[derive(Clone, Debug, PartialEq)]
    enum OracleOutcome {
        DotnetPick(SdkVersion),
        NugetPick(SdkVersion),
        VersionNotSatisfied,
        NotFound,
    }

    runner
        .run(
            &strategy,
            |(_scenario, installs, spec_choice, per_import_choice)| {
                // 99.0.0 sits above the entire universe, so a `Disable`
                // pinned to it admits nothing.
                let caller_spec: Option<VersionSpec> = match spec_choice {
                    SpecChoice::NoneSpec => None,
                    SpecChoice::ExactV0 => Some(VersionSpec::with_version(
                        universe[0].clone(),
                        RollForward::Disable,
                        true,
                    )),
                    SpecChoice::ExactV1 => Some(VersionSpec::with_version(
                        universe[1].clone(),
                        RollForward::Disable,
                        true,
                    )),
                    SpecChoice::UnknownPin => Some(VersionSpec::with_version(
                        SdkVersion::parse("99.0.0").unwrap(),
                        RollForward::Disable,
                        true,
                    )),
                };
                let per_import_version: Option<SdkVersion> = match per_import_choice {
                    PerImportChoice::NoPin => None,
                    PerImportChoice::PinV0 => Some(universe[0].clone()),
                    PerImportChoice::PinV1 => Some(universe[1].clone()),
                    PerImportChoice::PinUnknown => Some(SdkVersion::parse("99.0.0").unwrap()),
                };

                let tmp = TempDir::new().unwrap();
                let dotnet_root = tmp.path().join("dotnet");
                let nuget_dir = tmp.path().join("nuget");
                fs::create_dir_all(&dotnet_root).unwrap();
                fs::create_dir_all(&nuget_dir).unwrap();

                let mut dotnet_versions: Vec<SdkVersion> = Vec::new();
                let mut nuget_versions: Vec<SdkVersion> = Vec::new();
                for (i, w) in installs.iter().enumerate() {
                    let v = &universe[i];
                    let name = format_version(v);
                    match w {
                        Where::DotnetOnly => {
                            install_sdk(&dotnet_root, &name, "Microsoft.NET.Sdk");
                            dotnet_versions.push(v.clone());
                        }
                        Where::NugetOnly => {
                            install_sdk_nuget(&nuget_dir, &name, "Microsoft.NET.Sdk");
                            nuget_versions.push(v.clone());
                        }
                        Where::Both => {
                            install_sdk(&dotnet_root, &name, "Microsoft.NET.Sdk");
                            install_sdk_nuget(&nuget_dir, &name, "Microsoft.NET.Sdk");
                            dotnet_versions.push(v.clone());
                            nuget_versions.push(v.clone());
                        }
                        Where::Neither => {}
                    }
                }

                // Oracle: mirror the resolver's two-stage selection
                // one-to-one. Per-import overrides caller spec for
                // DOTNET and gates NuGet entirely.
                let per_import_spec = per_import_version
                    .as_ref()
                    .map(|v| VersionSpec::with_version(v.clone(), RollForward::Disable, true));
                let dotnet_spec_ref = per_import_spec.as_ref().or(caller_spec.as_ref());
                let dotnet_chosen = select_sdk_version(&dotnet_versions, dotnet_spec_ref).cloned();
                let nuget_chosen = if per_import_spec.is_some() {
                    select_sdk_version(&nuget_versions, per_import_spec.as_ref()).cloned()
                } else {
                    None
                };
                // Effective NuGet visibility for the (None, None)
                // branch: same gating — without per-import, NuGet
                // versions don't count toward "anything was on disk".
                let nuget_considered: &[SdkVersion] = if per_import_spec.is_some() {
                    &nuget_versions
                } else {
                    &[]
                };
                let expected: OracleOutcome = match (dotnet_chosen, nuget_chosen) {
                    (Some(d), Some(n)) => {
                        if d >= n {
                            OracleOutcome::DotnetPick(d)
                        } else {
                            OracleOutcome::NugetPick(n)
                        }
                    }
                    (Some(d), None) => OracleOutcome::DotnetPick(d),
                    (None, Some(n)) => OracleOutcome::NugetPick(n),
                    (None, None) => {
                        if dotnet_versions.is_empty() && nuget_considered.is_empty() {
                            OracleOutcome::NotFound
                        } else {
                            OracleOutcome::VersionNotSatisfied
                        }
                    }
                };

                let sdk_name = match per_import_version.as_ref() {
                    Some(v) => format!("Microsoft.NET.Sdk/{}", format_version(v)),
                    None => "Microsoft.NET.Sdk".to_owned(),
                };
                let actual = locate_dotnet_sdk(
                    &dotnet_root,
                    Some(&nuget_dir),
                    &sdk_name,
                    caller_spec.as_ref(),
                    None,
                );

                match (&expected, &actual) {
                    (OracleOutcome::DotnetPick(v), Ok(paths)) => {
                        let printed = version_dir_name(&paths.props).to_str().unwrap().to_owned();
                        let actual_version = SdkVersion::parse(&printed).ok_or_else(|| {
                            TestCaseError::fail(format!("bad version {printed:?}"))
                        })?;
                        prop_assert_eq!(&actual_version, v);
                        prop_assert!(
                            picked_from_dotnet_root(&paths.props),
                            "oracle expected DOTNET pick, resolver returned {paths:?}"
                        );
                        dotnet_pick_count.set(dotnet_pick_count.get() + 1);
                    }
                    (OracleOutcome::NugetPick(v), Ok(paths)) => {
                        let printed = version_dir_name(&paths.props).to_str().unwrap().to_owned();
                        let actual_version = SdkVersion::parse(&printed).ok_or_else(|| {
                            TestCaseError::fail(format!("bad version {printed:?}"))
                        })?;
                        prop_assert_eq!(&actual_version, v);
                        prop_assert!(
                            !picked_from_dotnet_root(&paths.props),
                            "oracle expected NuGet pick, resolver returned {paths:?}"
                        );
                        nuget_pick_count.set(nuget_pick_count.get() + 1);
                    }
                    (
                        OracleOutcome::VersionNotSatisfied,
                        Err(SdkResolveError::VersionNotSatisfied { spec: s, available }),
                    ) => {
                        // Effective spec = per-import if set, else
                        // caller; the resolver's diagnostic carries
                        // whichever is governing.
                        let expected_spec = per_import_spec.clone().or(caller_spec.clone());
                        prop_assert_eq!(Some(s.clone()), expected_spec);
                        // `available` is the union the spec was
                        // consulted against, sorted ascending. With
                        // per-import set, both roots; without, only
                        // DOTNET (NuGet not consulted).
                        let mut expected_avail = dotnet_versions.clone();
                        if per_import_spec.is_some() {
                            for v in &nuget_versions {
                                if !expected_avail.contains(v) {
                                    expected_avail.push(v.clone());
                                }
                            }
                        }
                        expected_avail.sort();
                        prop_assert_eq!(available.clone(), expected_avail);
                        version_not_satisfied_count.set(version_not_satisfied_count.get() + 1);
                    }
                    (OracleOutcome::NotFound, Err(SdkResolveError::NotFound)) => {
                        prop_assert!(
                            dotnet_versions.is_empty() && nuget_considered.is_empty(),
                            "NotFound requires the considered candidate set to be empty"
                        );
                        not_found_count.set(not_found_count.get() + 1);
                    }
                    (_, _) => {
                        return Err(TestCaseError::fail(format!(
                            "oracle/resolver disagree: expected={expected:?}, actual={actual:?}"
                        )));
                    }
                }
                Ok(())
            },
        )
        .unwrap();

    // Each scenario contributes ~205 cases (1024 × 1/5). Within each
    // dedicated scenario the target bucket is hit on the overwhelming
    // majority of cases:
    //   - UnpinnedDotnetHit:   ≥ ~200 dotnet_pick (≥1 DOTNET slot;
    //                         miss probability (2/6)^4 ≈ 0.012)
    //   - UnpinnedDotnetMiss:  ≥ ~200 version_not_satisfied (likewise)
    //   - UnpinnedNugetOnly:   ≡ 205 not_found (NuGet ignored;
    //                         DOTNET empty by construction)
    //   - PinnedNugetWin:      ≡ 205 nuget_pick (pin pinned to a
    //                         NuGet-only slot)
    //   - PinnedMixed:         contributes incidentally to all
    //                         buckets; not relied on for any
    //                         coverage threshold.
    // Threshold 50 leaves enormous headroom: a binomial with mean
    // 200 and stddev ≈ 1.6 puts P[<50] far below 1e-100, and the
    // union-bound across four buckets stays well under 1e-11.
    let d = dotnet_pick_count.get();
    let n = nuget_pick_count.get();
    let r = version_not_satisfied_count.get();
    let nf = not_found_count.get();
    assert!(
        d >= 50 && n >= 50 && r >= 50 && nf >= 50,
        "distribution skew: dotnet_pick={d}, nuget_pick={n}, version_not_satisfied={r}, not_found={nf}"
    );
}
