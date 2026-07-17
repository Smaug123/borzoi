use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Unwrap the ordinary single-root resolution shape every non-locator
/// test expects.
fn single_root(resolution: SdkResolution) -> SdkPaths {
    match resolution {
        SdkResolution::Single(paths) => paths,
        SdkResolution::Roots(roots) => {
            panic!("expected a single-root SDK resolution, got roots: {roots:?}")
        }
    }
}

use borzoi_msbuild::{
    DiagnosticKind, RollForward, SdkResolveError, SdkResolver, parse_fsproj_with_imports,
};
use tempfile::TempDir;

use super::*;

/// Create `{dotnet_root}/sdk/{version}/Sdks/{sdk_name}/Sdk/Sdk.{props,targets}`
/// (both empty `<Project/>` stubs).
fn install_sdk(dotnet_root: &Path, version: &str, sdk_name: &str) {
    let sdk_root = dotnet_root
        .join("sdk")
        .join(version)
        .join("Sdks")
        .join(sdk_name)
        .join("Sdk");
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(sdk_root.join("Sdk.props"), "<Project/>").unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();
}

/// Build a `SdkDiscoveryEnv` with everything `None` except the fields
/// the caller overrides via the builder closure. Keeps tests from
/// leaking the host's real env vars.
fn env_with(f: impl FnOnce(&mut SdkDiscoveryEnv)) -> SdkDiscoveryEnv {
    let mut env = SdkDiscoveryEnv {
        host_default_allow_prerelease: true,
        ..SdkDiscoveryEnv::default()
    };
    f(&mut env);
    env
}

#[test]
fn dotnet_root_explicit_env_wins_without_path_probe() {
    // The explicit DOTNET_ROOT path doesn't even need to exist —
    // `resolve_dotnet_root` returns it as-is. (It'd fail later in
    // `locate_dotnet_sdk` if it didn't, but discovery itself doesn't
    // probe.)
    let env = env_with(|e| {
        e.dotnet_root = Some(PathBuf::from("/explicit/path/does/not/exist"));
        // Even with PATH set, the explicit env wins.
        e.search_path = Some(OsString::from("/this/should/be/ignored"));
    });
    let resolved = resolve_dotnet_root(&env, Path::new("."));
    assert_eq!(
        resolved.as_deref(),
        Some(Path::new("/explicit/path/does/not/exist"))
    );
}

#[test]
fn dotnet_root_falls_back_to_path_lookup() {
    let tmp = TempDir::new().unwrap();
    let dotnet_install = tmp.path().join("share").join("dotnet");
    fs::create_dir_all(&dotnet_install).unwrap();
    // Real installs have an `sdk/` sibling — the resolver requires it
    // to distinguish from shim/wrapper directories like asdf/mise.
    fs::create_dir_all(dotnet_install.join("sdk")).unwrap();
    let dotnet_bin = dotnet_install.join(super::DOTNET_BIN);
    fs::write(&dotnet_bin, b"#!/bin/sh\n").unwrap();

    let env = env_with(|e| {
        e.search_path = Some(OsString::from(dotnet_install.as_os_str()));
    });
    let resolved = resolve_dotnet_root(&env, Path::new(".")).expect("found on PATH");

    let expected = fs::canonicalize(&dotnet_install).unwrap();
    assert_eq!(resolved, expected);
}

#[test]
#[cfg(unix)]
fn dotnet_root_canonicalises_symlinks_on_path() {
    // The .NET installer commonly drops a symlink at /usr/local/bin/dotnet
    // pointing into /usr/local/share/dotnet/dotnet. We need the *real*
    // install root (which contains `sdk/`), not the symlink's parent.
    let tmp = TempDir::new().unwrap();
    let real_install = tmp.path().join("share").join("dotnet");
    fs::create_dir_all(&real_install).unwrap();
    fs::create_dir_all(real_install.join("sdk")).unwrap();
    let real_bin = real_install.join(super::DOTNET_BIN);
    fs::write(&real_bin, b"#!/bin/sh\n").unwrap();

    let path_dir = tmp.path().join("usr").join("local").join("bin");
    fs::create_dir_all(&path_dir).unwrap();
    let link = path_dir.join(super::DOTNET_BIN);
    std::os::unix::fs::symlink(&real_bin, &link).unwrap();

    let env = env_with(|e| {
        e.search_path = Some(OsString::from(path_dir.as_os_str()));
    });
    let resolved = resolve_dotnet_root(&env, Path::new(".")).expect("found via symlink");

    let expected = fs::canonicalize(&real_install).unwrap();
    assert_eq!(
        resolved, expected,
        "should return the real install root, not the symlink's parent dir"
    );
}

#[test]
fn dotnet_root_skips_path_entries_without_dotnet() {
    // First two PATH entries are empty; the third contains `dotnet`.
    // Verifies that resolution doesn't stop at the first directory.
    let tmp = TempDir::new().unwrap();
    let empty_a = tmp.path().join("empty-a");
    let empty_b = tmp.path().join("empty-b");
    let real = tmp.path().join("real");
    for d in [&empty_a, &empty_b, &real] {
        fs::create_dir_all(d).unwrap();
    }
    fs::create_dir_all(real.join("sdk")).unwrap();
    fs::write(real.join(super::DOTNET_BIN), b"").unwrap();

    let path_joined = std::env::join_paths([&empty_a, &empty_b, &real]).unwrap();
    let env = env_with(|e| e.search_path = Some(path_joined));
    let resolved = resolve_dotnet_root(&env, Path::new(".")).expect("found in third PATH entry");

    assert_eq!(resolved, fs::canonicalize(&real).unwrap());
}

#[test]
fn dotnet_root_skips_shim_dir_without_sdk_sibling() {
    // First PATH entry has a `dotnet` binary but no `sdk/` sibling —
    // characteristic of asdf/mise/Nix shim directories. The resolver
    // should skip it and prefer the second entry, which is a real
    // install. (This guards against returning a wrapper's `bin`
    // directory as the SDK root, which would then fail when
    // `locate_dotnet_sdk` tries to walk `sdk/`.)
    let tmp = TempDir::new().unwrap();
    let shim = tmp.path().join("shims");
    let real = tmp.path().join("share").join("dotnet");
    fs::create_dir_all(&shim).unwrap();
    fs::create_dir_all(&real).unwrap();
    fs::create_dir_all(real.join("sdk")).unwrap();
    // Both directories carry a `dotnet` binary; only `real` has the
    // adjacent `sdk/` that signals a real install.
    fs::write(shim.join(super::DOTNET_BIN), b"").unwrap();
    fs::write(real.join(super::DOTNET_BIN), b"").unwrap();

    let path_joined = std::env::join_paths([&shim, &real]).unwrap();
    let env = env_with(|e| e.search_path = Some(path_joined));
    let resolved =
        resolve_dotnet_root(&env, Path::new(".")).expect("real install in second PATH entry");

    assert_eq!(
        resolved,
        fs::canonicalize(&real).unwrap(),
        "should skip the shim dir (no sdk/ sibling) and pick the real install"
    );
}

#[test]
fn dotnet_root_returns_none_when_only_shims_on_path() {
    // No real install anywhere — every PATH entry with a `dotnet`
    // binary lacks the `sdk/` sibling. The shim file is empty, so
    // executing it fails (ENOEXEC), the `--info` fallback yields
    // nothing, and resolution returns None.
    let tmp = TempDir::new().unwrap();
    let shim = tmp.path().join("shims");
    fs::create_dir_all(&shim).unwrap();
    fs::write(shim.join(super::DOTNET_BIN), b"").unwrap();

    let env = env_with(|e| e.search_path = Some(OsString::from(shim.as_os_str())));
    assert!(
        resolve_dotnet_root(&env, Path::new(".")).is_none(),
        "shim-only PATH must not be accepted as an SDK root"
    );
}

#[test]
fn dotnet_root_returns_none_with_neither_env_nor_path() {
    let env = env_with(|_| {});
    assert!(resolve_dotnet_root(&env, Path::new(".")).is_none());
}

#[test]
fn dotnet_root_returns_none_when_path_has_no_dotnet() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path()).unwrap();
    // Tempdir is empty — no `dotnet` binary lives in it.
    let env = env_with(|e| e.search_path = Some(OsString::from(tmp.path().as_os_str())));
    assert!(resolve_dotnet_root(&env, Path::new(".")).is_none());
}

#[test]
fn parse_info_recovers_dotnet_root_from_sdks_section() {
    // The exact shape `dotnet --info` emits on every modern host. The
    // parser walks to the `.NET SDKs installed:` header, takes the
    // bracketed path from the first listed entry, and returns the
    // parent (the install root that has `sdk/` as a child).
    let info = "\
.NET SDK:
 Version:           8.0.401

.NET SDKs installed:
  8.0.401 [/usr/share/dotnet/sdk]
  9.0.100-preview.4 [/usr/share/dotnet/sdk]

.NET runtimes installed:
  Microsoft.NETCore.App 8.0.8 [/usr/share/dotnet/shared/Microsoft.NETCore.App]
";
    assert_eq!(
        super::parse_dotnet_info_sdk_root(info).as_deref(),
        Some(Path::new("/usr/share/dotnet"))
    );
}

#[test]
fn parse_info_returns_none_when_section_missing() {
    let info = "\
.NET SDK:
 Version:           8.0.401

.NET runtimes installed:
  Microsoft.NETCore.App 8.0.8 [/usr/share/dotnet/shared/Microsoft.NETCore.App]
";
    assert!(super::parse_dotnet_info_sdk_root(info).is_none());
}

#[test]
fn parse_info_returns_none_when_section_is_empty() {
    // Header present but no entries — followed straight by another
    // section header. The parser must not treat that header as an
    // SDK entry.
    let info = "\
.NET SDKs installed:
.NET runtimes installed:
  Microsoft.NETCore.App 8.0.8 [/usr/share/dotnet/shared/Microsoft.NETCore.App]
";
    assert!(super::parse_dotnet_info_sdk_root(info).is_none());
}

#[test]
fn parse_info_prefers_base_path_when_sdks_section_lists_other_root() {
    // The multi-root case: `global.json` uses `sdk.paths` like
    // `[".dotnet", "$host$"]` so `dotnet --info` lists SDKs from
    // *both* roots, but `Base Path:` reports the one dotnet actually
    // selected for this invocation. Picking the first SDKs-installed
    // entry would point us at the wrong root and `locate_dotnet_sdk`
    // would then scan a directory that doesn't contain the SDK
    // global.json asked for. Verify Base Path wins.
    let info = "\
.NET SDK:
 Version:           10.0.203

Runtime Environment:
 OS Platform: Linux
 Base Path:   /opt/host-dotnet/sdk/10.0.203/

.NET SDKs installed:
  9.0.100 [/repo/.dotnet/sdk]
  10.0.203 [/opt/host-dotnet/sdk]

.NET runtimes installed:
";
    assert_eq!(
        super::parse_dotnet_info_sdk_root(info).as_deref(),
        Some(Path::new("/opt/host-dotnet"))
    );
}

#[test]
fn parse_info_recovers_from_real_nix_payload() {
    // Captured from a Nix dev-shell `dotnet --info`. The wrapper at
    // /nix/store/.../bin/dotnet forwards to the real binary, whose
    // SDK lives under share/dotnet/sdk. We want
    // `<store-path>/share/dotnet` back as the root.
    let info = "\
.NET SDK:
 Version:           10.0.203

Runtime Environment:
 OS Platform: Darwin
 Base Path:   /nix/store/aaaaa-dotnet-sdk-10.0.203/share/dotnet/sdk/10.0.203/

Host:
  Version:      10.0.7

.NET SDKs installed:
  10.0.203 [/nix/store/aaaaa-dotnet-sdk-10.0.203/share/dotnet/sdk]

.NET runtimes installed:
  Microsoft.NETCore.App 10.0.7 [/nix/store/aaaaa-dotnet-sdk-10.0.203/share/dotnet/shared/Microsoft.NETCore.App]
";
    assert_eq!(
        super::parse_dotnet_info_sdk_root(info).as_deref(),
        Some(Path::new(
            "/nix/store/aaaaa-dotnet-sdk-10.0.203/share/dotnet"
        ))
    );
}

#[test]
#[cfg(unix)]
fn dotnet_root_falls_back_to_dotnet_info_for_wrapper_layouts() {
    // The Nix wrapper case: PATH points at a directory containing a
    // `dotnet` shell script whose parent has no `sdk/` sibling. The
    // wrapper itself, when executed with `--info`, prints the real
    // install root. The resolver must invoke `--info` and recover
    // that root.
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let real_root = tmp.path().join("nix-store").join("share").join("dotnet");
    fs::create_dir_all(real_root.join("sdk")).unwrap();

    let wrapper_dir = tmp.path().join("nix-store").join("bin");
    fs::create_dir_all(&wrapper_dir).unwrap();
    let wrapper_path = wrapper_dir.join(super::DOTNET_BIN);
    // Print a minimal but realistic `--info` payload. The pure
    // parser only looks at the SDKs section, but we include enough
    // surrounding context to keep this honest.
    let script = format!(
        r#"#!/bin/sh
cat <<EOF
.NET SDK:
 Version:           8.0.401

.NET SDKs installed:
  8.0.401 [{root}/sdk]

.NET runtimes installed:
EOF
"#,
        root = real_root.display(),
    );
    fs::write(&wrapper_path, &script).unwrap();
    fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755)).unwrap();

    let env = env_with(|e| e.search_path = Some(OsString::from(wrapper_dir.as_os_str())));
    let resolved =
        resolve_dotnet_root(&env, Path::new(".")).expect("fallback recovers root via --info");
    assert_eq!(resolved, real_root);
}

#[test]
#[cfg(unix)]
fn dotnet_root_fallback_runs_shim_in_project_dir() {
    // Regression for the cwd-sensitivity of asdf/mise-style shims: the
    // shim picks an SDK by walking up from cwd looking for
    // `.tool-versions`. If we inherit the LSP process cwd, projects
    // pinned via that mechanism resolve against the wrong toolchain.
    // The fake shim here writes its cwd into stdout in place of the
    // SDK base path; the assertion fails if the cwd we see isn't the
    // project directory we threaded through.
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let shim_dir = tmp.path().join("shims");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim_path = shim_dir.join(super::DOTNET_BIN);
    // `pwd` runs inside the shim; the printed SDK path is
    // `<cwd>/sdk`, so the parser yields `<cwd>` as the dotnet_root.
    let script = "#!/bin/sh\n\
                  printf '.NET SDKs installed:\\n  9.9.9 [%s/sdk]\\n' \"$(pwd)\"\n";
    fs::write(&shim_path, script).unwrap();
    fs::set_permissions(&shim_path, fs::Permissions::from_mode(0o755)).unwrap();

    // Build a real, canonicalised project directory so the assertion
    // can compare without worrying about /private/tmp on macOS.
    let project_dir = fs::canonicalize(tmp.path()).unwrap().join("proj");
    fs::create_dir_all(&project_dir).unwrap();

    let env = env_with(|e| e.search_path = Some(OsString::from(shim_dir.as_os_str())));
    let resolved = resolve_dotnet_root(&env, &project_dir)
        .expect("fallback should succeed when the shim prints a path");
    assert_eq!(
        resolved, project_dir,
        "shim was invoked from {:?} but should have seen project dir {:?}",
        resolved, project_dir,
    );
}

#[test]
fn nuget_packages_dir_explicit_env_wins() {
    let env = env_with(|e| {
        e.nuget_packages_dir = Some(PathBuf::from("/explicit/nuget"));
        e.home_dir = Some(PathBuf::from("/home/user"));
    });
    let resolved = resolve_nuget_packages_dir(&env);
    assert_eq!(resolved.as_deref(), Some(Path::new("/explicit/nuget")));
}

#[test]
fn nuget_packages_dir_falls_back_to_home_default() {
    let env = env_with(|e| e.home_dir = Some(PathBuf::from("/home/user")));
    let resolved = resolve_nuget_packages_dir(&env);
    assert_eq!(
        resolved.as_deref(),
        Some(Path::new("/home/user/.nuget/packages"))
    );
}

#[test]
fn nuget_packages_dir_none_when_neither() {
    let env = env_with(|_| {});
    assert!(resolve_nuget_packages_dir(&env).is_none());
}

#[test]
fn for_project_without_global_json() {
    // No global.json above the project ⇒ no spec, empty msbuild-sdks,
    // global_json_path = None. We still need a discoverable DOTNET_ROOT.
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(tmp.path().join("dotnet"));
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    // No global.json ⇒ unpinned spec carrying the host's prerelease
    // policy (CLI default = true).
    assert!(disc.spec().version().is_none());
    assert!(disc.spec().allow_prerelease());
    assert!(disc.msbuild_sdks().is_empty());
    assert!(disc.global_json_path().is_none());
    assert_eq!(disc.roots(), [tmp.path().join("dotnet")]);
}

#[test]
fn no_global_json_with_vs_host_still_filters_prereleases() {
    // The correctness case the previous shape got wrong: VS-style
    // host (allowPrerelease = false), no global.json, must still
    // refuse a higher prerelease SDK over a stable one.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(&dotnet, "9.0.100-preview.1.24070.1", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet.clone());
        e.host_default_allow_prerelease = false;
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");
    assert!(!disc.spec().allow_prerelease());

    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve"));
    assert!(
        paths.props.starts_with(dotnet.join("sdk").join("8.0.401")),
        "VS host with allow_prerelease=false must pick the stable SDK, got {}",
        paths.props.display()
    );
}

#[test]
fn for_project_with_global_json_populates_spec_and_pins() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{
            "sdk": { "version": "8.0.401", "rollForward": "feature" },
            "msbuild-sdks": { "My.Custom.Sdk": "1.2.3" }
        }"#,
    )
    .unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(tmp.path().join("dotnet"));
        // Force allowPrerelease=false at the host so we can verify the
        // spec carries the right policy (no global.json override here).
        e.host_default_allow_prerelease = false;
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let spec = disc.spec();
    assert_eq!(spec.roll_forward(), RollForward::Feature);
    assert!(!spec.allow_prerelease());
    assert_eq!(
        disc.msbuild_sdks()
            .get("My.Custom.Sdk")
            .map(|v| v.to_string()),
        Some("1.2.3".to_string())
    );
    assert_eq!(
        disc.global_json_path().map(Path::to_path_buf),
        Some(tmp.path().join("global.json"))
    );
}

#[test]
fn for_project_global_json_above_project_dir_is_found() {
    // global.json sits two ancestors up from the project. find_global_json
    // walks parents, so it should still be picked up.
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("a").join("b").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "version": "8.0.100" } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    // Pin from the global.json carries through into the spec.
    assert_eq!(
        disc.spec().version().map(|v| v.to_string()),
        Some("8.0.100".to_string())
    );
    assert_eq!(
        disc.global_json_path().map(Path::to_path_buf),
        Some(tmp.path().join("global.json"))
    );
}

#[test]
fn for_project_missing_dotnet_root_errors() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("App.fsproj");
    // No DOTNET_ROOT, no search_path ⇒ MissingDotnetRoot.
    let env = env_with(|_| {});
    let err = SdkDiscovery::for_project(&project, &env).unwrap_err();
    assert!(
        matches!(err, DiscoveryError::MissingDotnetRoot),
        "expected MissingDotnetRoot, got {err:?}"
    );
}

#[test]
fn for_project_normalises_dot_dot_segments_before_walk() {
    // Lay out two sibling project directories, each with its own
    // global.json. Pass `<root>/a/../b/App.fsproj`: only `<root>` and
    // `<root>/b` are real ancestors of the project; `<root>/a` is not
    // and its global.json must NOT be selected.
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(
        a.join("global.json"),
        r#"{ "sdk": { "version": "7.0.100" } }"#,
    )
    .unwrap();
    fs::write(
        b.join("global.json"),
        r#"{ "sdk": { "version": "8.0.100" } }"#,
    )
    .unwrap();

    let project = tmp.path().join("a").join("..").join("b").join("App.fsproj");
    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert_eq!(
        disc.global_json_path().map(Path::to_path_buf),
        Some(b.join("global.json")),
        "the `..` segment must be collapsed lexically, picking b/global.json not a/global.json"
    );
    assert_eq!(
        disc.spec().version().map(|v| v.to_string()),
        Some("8.0.100".to_string())
    );
}

#[test]
fn for_project_rejects_relative_project_path() {
    // A relative path's lexical parents don't include the CWD's
    // ancestors, so the upward `global.json` walk would silently skip
    // any pin in a real ancestor. We reject the input up front rather
    // than honour it with the wrong semantics.
    let env = env_with(|e| e.dotnet_root = Some(PathBuf::from("/dotnet")));
    let relative = Path::new("App.fsproj");
    let err = SdkDiscovery::for_project(relative, &env).unwrap_err();
    match err {
        DiscoveryError::RelativeProjectPath(path) => assert_eq!(path, relative),
        other => panic!("expected RelativeProjectPath, got {other:?}"),
    }
}

#[test]
fn default_env_has_cli_host_prerelease_policy() {
    // The auto-derive would set this to false (the bool default),
    // which would silently disagree with `from_process_env`. The
    // hand-rolled impl restores CLI parity.
    assert!(SdkDiscoveryEnv::default().host_default_allow_prerelease);
}

#[test]
fn for_project_surfaces_global_json_parse_error() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("App.fsproj");
    fs::write(tmp.path().join("global.json"), "not valid json {").unwrap();

    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
    let err = SdkDiscovery::for_project(&project, &env).unwrap_err();
    match err {
        DiscoveryError::GlobalJsonParse { path, .. } => {
            assert_eq!(path, tmp.path().join("global.json"));
        }
        other => panic!("expected GlobalJsonParse, got {other:?}"),
    }
}

#[test]
fn resolve_picks_global_json_pinned_sdk() {
    // End-to-end through `resolve`: install two SDK versions, pin to
    // 8.0.401 with rollForward=disable, verify that's the one picked
    // even though 9.0.100 is also installed and higher.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(&dotnet, "9.0.100", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "version": "8.0.401", "rollForward": "disable" } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet.clone()));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve"));
    assert!(
        paths.props.starts_with(dotnet.join("sdk").join("8.0.401")),
        "should have picked the pinned version, got {}",
        paths.props.display()
    );
}

#[test]
fn resolve_returns_version_not_satisfied_when_pin_unmet() {
    // Pin to 10.0.100, install 8.0.401 only ⇒ spec admits nothing,
    // resolver returns VersionNotSatisfied. (Versions ending below
    // x.y.100 are rejected by the global.json parser — feature bands
    // start there.)
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "version": "10.0.100", "rollForward": "disable" } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    match disc.resolve("Microsoft.NET.Sdk").unwrap_err() {
        SdkResolveError::VersionNotSatisfied { .. } => {}
        other => panic!("expected VersionNotSatisfied, got {other:?}"),
    }
}

#[test]
fn e2e_parse_fsproj_with_imports_via_discovery() {
    // End-to-end: lay out a DOTNET_ROOT containing Microsoft.NET.Sdk,
    // write an fsproj that uses `Sdk="Microsoft.NET.Sdk"`, run
    // discovery, splice the resolver into parse_fsproj_with_imports,
    // and verify the splice succeeded.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project_dir = tmp.path().join("proj");
    fs::create_dir_all(&project_dir).unwrap();
    let project = project_dir.join("App.fsproj");
    let source = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="Library.fs" />
  </ItemGroup>
</Project>"#;
    fs::write(&project, source).unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let resolver: &SdkResolver<'_> = &|name| disc.resolve(name);
    let parsed = parse_fsproj_with_imports(
        source,
        &project,
        &HashMap::new(),
        &HashMap::new(),
        Some(resolver),
        None,
    )
    .expect("parse");

    // The fsproj has exactly one Compile item and no SDK-resolution
    // diagnostics; if the resolver had failed, we'd see an
    // SdkNotFound / SdkVersionNotSatisfied entry here.
    assert_eq!(parsed.items.len(), 1);
    let sdk_diags: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| {
            matches!(
                d.kind,
                DiagnosticKind::SdkNotFound { .. } | DiagnosticKind::SdkVersionNotSatisfied { .. }
            )
        })
        .collect();
    assert!(
        sdk_diags.is_empty(),
        "expected no SDK diagnostics, got {sdk_diags:#?}"
    );
}

#[test]
fn e2e_msbuild_sdks_pin_overrides_caller_spec() {
    // Subtle interaction: caller's `spec` (from global.json sdk block)
    // wants 8.0.401 with rollForward=disable, but msbuild-sdks pins
    // the import to a NuGet-resolved version. Since the import has its
    // own pin source (msbuild-sdks), it goes through NuGet, not
    // $DOTNET_ROOT. We assert the resolver finds the NuGet copy.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");
    let nuget = tmp.path().join("nuget");
    let sdk_pkg_root = nuget.join("my.custom.sdk").join("1.2.3").join("Sdk");
    fs::create_dir_all(&sdk_pkg_root).unwrap();
    fs::write(sdk_pkg_root.join("Sdk.props"), "<Project/>").unwrap();
    fs::write(sdk_pkg_root.join("Sdk.targets"), "<Project/>").unwrap();

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "msbuild-sdks": { "My.Custom.Sdk": "1.2.3" } }"#,
    )
    .unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet);
        e.nuget_packages_dir = Some(nuget.clone());
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let paths = single_root(disc.resolve("My.Custom.Sdk").expect("resolved via NuGet"));
    assert!(
        paths.props.starts_with(&nuget),
        "msbuild-sdks pin should resolve via NuGet, got {}",
        paths.props.display()
    );
}

// ============================================================
// global.json `sdk.paths` honoured by for_project
//
// These tests pin the construction-side of stage 8b.2b: how
// `parsed.sdk.paths` projects into `SdkDiscovery::roots`. Each
// builds a hermetic on-disk layout (stub SDK installs + a written
// `global.json`), invokes `for_project`, and asserts either on
// `roots()` directly (when the geometry is the point) or on the
// `resolve()` outcome (when first-match-wins / pin propagation /
// union-on-VersionNotSatisfied is the point).
// ============================================================

#[test]
fn paths_resolved_against_global_json_dir() {
    // `paths: ["./alt"]` must expand against the `global.json` file's
    // directory, NOT the project directory. Layout: global.json at
    // tmp/a/global.json, project at tmp/a/b/proj/App.fsproj, SDK
    // installed at tmp/a/alt/. If the relative entry were joined
    // against the project dir, the lookup would scan
    // tmp/a/b/proj/alt/ — which doesn't exist — and resolve would
    // fail.
    let tmp = TempDir::new().unwrap();
    let alt_root = tmp.path().join("a").join("alt");
    install_sdk(&alt_root, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp
        .path()
        .join("a")
        .join("b")
        .join("proj")
        .join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("a").join("global.json"),
        r#"{ "sdk": { "paths": ["./alt"] } }"#,
    )
    .unwrap();

    // DOTNET_ROOT set to a directory with no installed SDKs — proves
    // the relative entry, not the host root, is doing the work.
    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("empty-host")));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert_eq!(disc.roots(), std::slice::from_ref(&alt_root));
    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve via alt"));
    assert!(
        paths
            .props
            .starts_with(alt_root.join("sdk").join("8.0.401")),
        "expected SDK from alt, got {}",
        paths.props.display()
    );
}

#[test]
fn host_token_expands_to_discovered_root() {
    // `paths: ["$host$"]` is just an explicit way of writing the
    // default — the resulting `roots` and `resolve` outcome must
    // match what no-`paths` produces for the same workspace.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": ["$host$"] } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet.clone()));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert_eq!(disc.roots(), std::slice::from_ref(&dotnet));
    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve via host"));
    assert!(paths.props.starts_with(dotnet.join("sdk").join("8.0.401")));
}

#[test]
fn host_token_orders_after_explicit_paths() {
    // Order is observable through `roots()` — the relative entry
    // comes first, `$host$` second. The matching `resolve` test
    // (relative_entry_wins_over_host_when_both_satisfy) pins
    // first-match-wins; this one pins the order itself so a
    // regression where the host gets prepended would be caught even
    // when both roots have the same SDK.
    let tmp = TempDir::new().unwrap();
    let alt = tmp.path().join("alt");
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&alt, "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": ["./alt", "$host$"] } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet.clone()));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert_eq!(disc.roots(), [alt, dotnet]);
}

#[test]
fn relative_entry_wins_over_host_when_both_satisfy() {
    // First-match-wins, even when the host has a *newer* SDK than
    // the relative entry. `paths: ["./alt", "$host$"]` with 8.0.401
    // in alt and 9.0.100 in the host must pick 8.0.401 from alt.
    let tmp = TempDir::new().unwrap();
    let alt = tmp.path().join("alt");
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&alt, "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(&dotnet, "9.0.100", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": ["./alt", "$host$"] } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve"));
    assert!(
        paths.props.starts_with(alt.join("sdk").join("8.0.401")),
        "first-match-wins: expected 8.0.401 from alt, got {}",
        paths.props.display()
    );
}

#[test]
fn empty_paths_list_returns_not_found() {
    // `paths: []` is the strict opt-out: zero roots, so resolve
    // *always* returns NotFound even when a usable host SDK exists.
    // Mirrors the .NET host's reading — the workspace is opting out
    // of the host install and the LSP must not silently fall back.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": [] } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert!(disc.roots().is_empty());
    let err = disc.resolve("Microsoft.NET.Sdk").unwrap_err();
    assert!(
        matches!(err, SdkResolveError::NotFound),
        "expected NotFound for paths: [], got {err:?}"
    );
}

#[test]
fn nonexistent_relative_root_falls_through() {
    // A `paths` entry that points at a directory which doesn't exist
    // must not panic, error, or short-circuit — `locate_dotnet_sdk`
    // simply reports NotFound for that root and the iteration
    // continues to the next entry. Tests two scenarios in one: the
    // first entry has no SDKs at all (directory absent), the second
    // entry resolves cleanly.
    let tmp = TempDir::new().unwrap();
    let alt = tmp.path().join("alt");
    install_sdk(&alt, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": ["./does-not-exist", "./alt"] } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("empty-host")));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve via alt"));
    assert!(paths.props.starts_with(alt.join("sdk").join("8.0.401")));
}

#[test]
fn host_token_skipped_when_host_resolution_fails() {
    // `paths: ["$host$", "./alt"]` with no DOTNET_ROOT and no PATH:
    // host resolution fails, `$host$` is dropped (NOT fatal — the
    // strict-MissingDotnetRoot error only fires when `paths` is
    // absent), and the surviving relative entry still resolves.
    let tmp = TempDir::new().unwrap();
    let alt = tmp.path().join("alt");
    install_sdk(&alt, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": ["$host$", "./alt"] } }"#,
    )
    .unwrap();

    // Hermetic env: no DOTNET_ROOT, no PATH ⇒ resolve_dotnet_root
    // returns None. Without `paths`, this would be MissingDotnetRoot.
    let env = env_with(|_| {});
    let disc = SdkDiscovery::for_project(&project, &env)
        .expect("paths: Some(_) must not fail on MissingDotnetRoot");

    assert_eq!(
        disc.roots(),
        std::slice::from_ref(&alt),
        "$host$ must be dropped, leaving only the explicit alt entry"
    );
    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve via alt"));
    assert!(paths.props.starts_with(alt.join("sdk").join("8.0.401")));
}

#[test]
#[cfg(unix)]
fn host_token_does_not_consult_dotnet_info() {
    // [P2] from codex review on PR #145. When expanding `$host$`, we
    // must NOT fall back to `dotnet --info`: the only `--info`-cwd we
    // have is the project dir, which sits inside a workspace whose
    // own `sdk.paths` redirect the muxer's SDK selection. The path
    // `--info` would print is the *resolved* SDK (filtered through
    // the workspace's own paths), not the host install that `$host$`
    // is supposed to name — so consulting it would silently produce
    // a duplicate root or, worse, mis-identify the host. With a
    // wrapper-only PATH and no DOTNET_ROOT, `$host$` must therefore
    // be skipped (same behaviour as "no host available at all"),
    // and the surviving relative entry resolves cleanly.
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let alt = tmp.path().join("alt");
    install_sdk(&alt, "8.0.401", "Microsoft.NET.Sdk");

    // Wrapper script that prints a path different from `alt`. If the
    // resolver ever runs `--info` here, the test fails: we'd see the
    // fake root added to `disc.roots()`.
    let wrapper_dir = tmp.path().join("nix-bin");
    fs::create_dir_all(&wrapper_dir).unwrap();
    let wrapper_path = wrapper_dir.join(super::DOTNET_BIN);
    let fake_root = tmp.path().join("info-reported");
    let script = format!(
        "#!/bin/sh\nprintf '.NET SDKs installed:\\n  9.9.9 [{root}/sdk]\\n'\n",
        root = fake_root.display(),
    );
    fs::write(&wrapper_path, script).unwrap();
    fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755)).unwrap();

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "paths": ["$host$", "./alt"] } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.search_path = Some(OsString::from(wrapper_dir.as_os_str())));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert_eq!(
        disc.roots(),
        std::slice::from_ref(&alt),
        "$host$ must skip the `dotnet --info` fallback; got roots {:?}",
        disc.roots()
    );
    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve via alt"));
    assert!(paths.props.starts_with(alt.join("sdk").join("8.0.401")));
}

#[test]
#[cfg(unix)]
fn no_paths_field_still_uses_dotnet_info_for_wrapper() {
    // Counter-test for the above: when `global.json` has *no* `paths`
    // field (or no `global.json` at all), wrapper-layout support is
    // unchanged — the resolver still falls back to `dotnet --info`
    // to recover the install root. This pins the asymmetry so a
    // future "simplify by dropping `--info` everywhere" change is
    // forced to be deliberate rather than incidental.
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let real_root = tmp.path().join("share").join("dotnet");
    install_sdk(&real_root, "8.0.401", "Microsoft.NET.Sdk");

    let wrapper_dir = tmp.path().join("nix-bin");
    fs::create_dir_all(&wrapper_dir).unwrap();
    let wrapper_path = wrapper_dir.join(super::DOTNET_BIN);
    let script = format!(
        "#!/bin/sh\nprintf '.NET SDKs installed:\\n  8.0.401 [{root}/sdk]\\n'\n",
        root = real_root.display(),
    );
    fs::write(&wrapper_path, script).unwrap();
    fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755)).unwrap();

    // `global.json` with an `sdk.version` but NO `paths` field exercises
    // the no-paths arm of `expand_sdk_paths`.
    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "sdk": { "version": "8.0.401" } }"#,
    )
    .unwrap();

    let env = env_with(|e| e.search_path = Some(OsString::from(wrapper_dir.as_os_str())));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert_eq!(disc.roots(), std::slice::from_ref(&real_root));
}

#[test]
fn global_json_version_pin_applies_across_roots() {
    // A pinned `sdk.version` constrains *every* root. Layout: two
    // alt roots, only the second has the pinned 8.0.401; both have
    // a 9.0.100 we expect to be filtered out. With rollForward=disable
    // and pin=8.0.401, resolve must land in altB.
    let tmp = TempDir::new().unwrap();
    let alt_a = tmp.path().join("altA");
    let alt_b = tmp.path().join("altB");
    install_sdk(&alt_a, "9.0.100", "Microsoft.NET.Sdk");
    install_sdk(&alt_b, "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(&alt_b, "9.0.100", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{
            "sdk": {
                "version": "8.0.401",
                "rollForward": "disable",
                "paths": ["./altA", "./altB"]
            }
        }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("empty-host")));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve"));
    assert!(
        paths.props.starts_with(alt_b.join("sdk").join("8.0.401")),
        "pinned version must skip altA (which has 9.0.100) and \
         land in altB's 8.0.401, got {}",
        paths.props.display()
    );
}

#[test]
fn version_not_satisfied_unions_available_across_roots() {
    // No root has the pinned 10.0.100 (rollForward=disable so no
    // roll-up). altA has 8.0.401, altB has 9.0.100. The aggregated
    // VersionNotSatisfied must list both versions in the `available`
    // union — the most informative diagnostic for the user.
    let tmp = TempDir::new().unwrap();
    let alt_a = tmp.path().join("altA");
    let alt_b = tmp.path().join("altB");
    install_sdk(&alt_a, "8.0.401", "Microsoft.NET.Sdk");
    install_sdk(&alt_b, "9.0.100", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{
            "sdk": {
                "version": "10.0.100",
                "rollForward": "disable",
                "paths": ["./altA", "./altB"]
            }
        }"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("empty-host")));
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let err = disc.resolve("Microsoft.NET.Sdk").unwrap_err();
    let SdkResolveError::VersionNotSatisfied { available, .. } = err else {
        panic!("expected VersionNotSatisfied, got {err:?}");
    };
    assert_eq!(
        available,
        vec![
            SdkVersion::parse("8.0.401").unwrap(),
            SdkVersion::parse("9.0.100").unwrap(),
        ],
        "available list must be the union across consulted roots"
    );
}

// ============================================================
// resolve_across_roots — iteration semantics
//
// These tests exercise the free function lifted out of
// SdkDiscovery::resolve so that the multi-root fold can be checked
// without standing up real SDK layouts on disk. The mock locator
// returns prepared outcomes per root and records how many times it
// was called, which lets the tests pin short-circuit behaviour as
// well as the union/NotFound terminal logic.
// ============================================================

use std::cell::Cell;
use std::cell::RefCell;

use borzoi_msbuild::{SdkPaths, SdkVersion};
use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};

/// A locator built from a deterministic sequence of outcomes —
/// the i-th call returns `outcomes[i]`. Records the total call
/// count so tests can assert short-circuit behaviour.
struct ScriptedLocator {
    outcomes: RefCell<std::vec::IntoIter<Result<SdkResolution, SdkResolveError>>>,
    call_count: Cell<usize>,
}

impl ScriptedLocator {
    fn new(outcomes: Vec<Result<SdkResolution, SdkResolveError>>) -> Self {
        Self {
            outcomes: RefCell::new(outcomes.into_iter()),
            call_count: Cell::new(0),
        }
    }

    fn locate(&self) -> impl Fn(&Path, &str) -> Result<SdkResolution, SdkResolveError> + '_ {
        |_root: &Path, _name: &str| {
            self.call_count.set(self.call_count.get() + 1);
            self.outcomes
                .borrow_mut()
                .next()
                .expect("locator called more times than outcomes scripted")
        }
    }
}

fn sdk_paths_stub(label: &str) -> SdkPaths {
    let root = PathBuf::from(format!("/stub/{label}"));
    SdkPaths {
        props: root.join("Sdk.props"),
        targets: root.join("Sdk.targets"),
        root,
    }
}

fn unpinned_spec() -> VersionSpec {
    VersionSpec::any_version(true)
}

#[test]
fn resolve_across_roots_empty_yields_not_found_without_calling_locate() {
    let locator = ScriptedLocator::new(vec![]);
    let result = resolve_across_roots(&[], "Microsoft.NET.Sdk", &locator.locate());
    assert!(matches!(result, Err(SdkResolveError::NotFound)));
    assert_eq!(locator.call_count.get(), 0);
}

#[test]
fn resolve_across_roots_first_ok_wins_and_short_circuits() {
    let roots = vec![
        PathBuf::from("/a"),
        PathBuf::from("/b"),
        PathBuf::from("/c"),
    ];
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::NotFound),
        Ok(SdkResolution::Single(sdk_paths_stub("b"))),
        // Third outcome must never be consulted.
        Err(SdkResolveError::NotFound),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    assert_eq!(result.unwrap(), SdkResolution::Single(sdk_paths_stub("b")));
    assert_eq!(locator.call_count.get(), 2, "locator must short-circuit");
}

#[test]
fn resolve_across_roots_all_not_found_yields_not_found() {
    let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::NotFound),
        Err(SdkResolveError::NotFound),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    assert!(matches!(result, Err(SdkResolveError::NotFound)));
    assert_eq!(locator.call_count.get(), 2);
}

#[test]
fn resolve_across_roots_mixed_errors_union_available() {
    // Two roots report VersionNotSatisfied with disjoint available
    // lists, one reports NotFound. Result: VersionNotSatisfied with
    // the union (deduped, sorted).
    let roots = vec![
        PathBuf::from("/a"),
        PathBuf::from("/b"),
        PathBuf::from("/c"),
    ];
    let avail_a = vec![SdkVersion::parse("8.0.401").unwrap()];
    let avail_c = vec![
        SdkVersion::parse("9.0.100").unwrap(),
        SdkVersion::parse("10.0.100").unwrap(),
    ];
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::VersionNotSatisfied {
            spec: unpinned_spec(),
            available: avail_a,
        }),
        Err(SdkResolveError::NotFound),
        Err(SdkResolveError::VersionNotSatisfied {
            spec: unpinned_spec(),
            available: avail_c,
        }),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    let Err(SdkResolveError::VersionNotSatisfied { available, .. }) = result else {
        panic!("expected VersionNotSatisfied, got {result:?}");
    };
    assert_eq!(
        available,
        vec![
            SdkVersion::parse("8.0.401").unwrap(),
            SdkVersion::parse("9.0.100").unwrap(),
            SdkVersion::parse("10.0.100").unwrap(),
        ]
    );
    assert_eq!(locator.call_count.get(), 3, "no Ok ⇒ all roots consulted");
}

#[test]
fn resolve_across_roots_dedups_overlapping_available_versions() {
    // Same version reported by two roots — must appear once in the
    // returned union, not twice. Also pins sort order is ascending.
    let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let dup = SdkVersion::parse("9.0.100").unwrap();
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::VersionNotSatisfied {
            spec: unpinned_spec(),
            // Out-of-order to confirm the function sorts.
            available: vec![SdkVersion::parse("10.0.100").unwrap(), dup.clone()],
        }),
        Err(SdkResolveError::VersionNotSatisfied {
            spec: unpinned_spec(),
            available: vec![dup, SdkVersion::parse("8.0.401").unwrap()],
        }),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    let Err(SdkResolveError::VersionNotSatisfied { available, .. }) = result else {
        panic!("expected VersionNotSatisfied, got {result:?}");
    };
    assert_eq!(
        available,
        vec![
            SdkVersion::parse("8.0.401").unwrap(),
            SdkVersion::parse("9.0.100").unwrap(),
            SdkVersion::parse("10.0.100").unwrap(),
        ]
    );
}

#[test]
fn resolve_across_roots_ok_after_version_not_satisfied_wins() {
    // The host's first-match-wins applies even when an earlier root
    // is a near-miss — confirms the iteration doesn't accidentally
    // get sticky on the first reported error.
    let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::VersionNotSatisfied {
            spec: unpinned_spec(),
            available: vec![SdkVersion::parse("8.0.401").unwrap()],
        }),
        Ok(SdkResolution::Single(sdk_paths_stub("b"))),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    assert_eq!(result.unwrap(), SdkResolution::Single(sdk_paths_stub("b")));
    assert_eq!(locator.call_count.get(), 2);
}

#[test]
fn resolve_across_roots_preserves_locator_reported_spec() {
    // `locate_dotnet_sdk` reports the *effective* spec — which for
    // per-import `Sdk="Name/Version"` pins or `msbuild-sdks`
    // overrides differs from the discovery-wide global.json spec.
    // The fold must carry that locator-reported spec through to the
    // aggregated `VersionNotSatisfied`, not substitute one of its
    // own. Regression: an earlier version of this function dropped
    // the locator-supplied spec and used a separately-threaded
    // `&self.spec`, which corrupted diagnostics for those callers.
    let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let pinned_spec = VersionSpec::with_version(
        SdkVersion::parse("9.0.100").unwrap(),
        RollForward::Disable,
        false,
    );
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::VersionNotSatisfied {
            spec: pinned_spec.clone(),
            available: vec![SdkVersion::parse("9.0.200").unwrap()],
        }),
        Err(SdkResolveError::NotFound),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    let Err(SdkResolveError::VersionNotSatisfied { spec, .. }) = result else {
        panic!("expected VersionNotSatisfied, got {result:?}");
    };
    assert_eq!(spec, pinned_spec);
}

#[test]
fn resolve_across_roots_uses_first_version_not_satisfied_spec() {
    // If two roots both report `VersionNotSatisfied` with different
    // specs (a hypothetical that doesn't arise in practice because
    // `locate_dotnet_sdk` derives the spec from `(sdk_name,
    // global.json, per-import pin)` which is root-invariant for a
    // given lookup, but the fold has no way to know that), the
    // first one wins. The available lists still union.
    let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let first_spec = VersionSpec::with_version(
        SdkVersion::parse("9.0.100").unwrap(),
        RollForward::Disable,
        false,
    );
    let second_spec = VersionSpec::with_version(
        SdkVersion::parse("10.0.100").unwrap(),
        RollForward::Disable,
        false,
    );
    let locator = ScriptedLocator::new(vec![
        Err(SdkResolveError::VersionNotSatisfied {
            spec: first_spec.clone(),
            available: vec![SdkVersion::parse("9.0.200").unwrap()],
        }),
        Err(SdkResolveError::VersionNotSatisfied {
            spec: second_spec,
            available: vec![SdkVersion::parse("10.0.200").unwrap()],
        }),
    ]);
    let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
    let Err(SdkResolveError::VersionNotSatisfied { spec, available }) = result else {
        panic!("expected VersionNotSatisfied, got {result:?}");
    };
    assert_eq!(spec, first_spec);
    assert_eq!(
        available,
        vec![
            SdkVersion::parse("9.0.200").unwrap(),
            SdkVersion::parse("10.0.200").unwrap(),
        ]
    );
}

// ----- PBT --------------------------------------------------

/// One scripted outcome per root for the PBT — kept as a small
/// closed enum so the property's spec function can run the same
/// fold the implementation does.
#[derive(Debug, Clone)]
enum RootOutcome {
    /// `Ok` carries an integer payload so the property can confirm
    /// *which* root's payload survived (not just "some Ok").
    Ok(u32),
    NotFound,
    /// Available list pre-stringified — random valid `SdkVersion`s
    /// would be cumbersome to generate, and the iteration only
    /// cares about the union, not the parse.
    VersionNotSatisfied(Vec<String>),
}

fn outcome_strategy() -> impl Strategy<Value = RootOutcome> {
    prop_oneof![
        any::<u32>().prop_map(RootOutcome::Ok),
        Just(RootOutcome::NotFound),
        // Versions drawn from a small pool so duplicates across roots
        // are common — that's the case the dedup leg of the fold cares
        // about. Strings rather than `SdkVersion`s so the strategy can
        // be `Clone` + `Debug` cheaply.
        prop::collection::vec(
            prop::sample::select(vec![
                "8.0.401".to_string(),
                "9.0.100".to_string(),
                "9.0.100-preview.1.24070.1".to_string(),
                "10.0.100".to_string(),
                "10.0.204".to_string(),
            ]),
            0..4,
        )
        .prop_map(RootOutcome::VersionNotSatisfied),
    ]
}

fn outcomes_to_results(
    outcomes: &[RootOutcome],
    spec: &VersionSpec,
) -> Vec<Result<SdkResolution, SdkResolveError>> {
    outcomes
        .iter()
        .map(|o| match o {
            RootOutcome::Ok(tag) => Ok(SdkResolution::Single(sdk_paths_stub(&format!("ok-{tag}")))),
            RootOutcome::NotFound => Err(SdkResolveError::NotFound),
            RootOutcome::VersionNotSatisfied(strs) => Err(SdkResolveError::VersionNotSatisfied {
                spec: spec.clone(),
                available: strs
                    .iter()
                    .map(|s| SdkVersion::parse(s).expect("pool version parses"))
                    .collect(),
            }),
        })
        .collect()
}

/// Reference implementation of the fold — the property check.
/// Returns `(result, expected_call_count)`.
fn reference_fold(
    outcomes: &[RootOutcome],
    spec: &VersionSpec,
) -> (Result<SdkResolution, SdkResolveError>, usize) {
    for (i, o) in outcomes.iter().enumerate() {
        if let RootOutcome::Ok(tag) = o {
            return (
                Ok(SdkResolution::Single(sdk_paths_stub(&format!("ok-{tag}")))),
                i + 1,
            );
        }
    }
    // No Ok ⇒ every root consulted.
    let mut union: Vec<SdkVersion> = Vec::new();
    for o in outcomes {
        if let RootOutcome::VersionNotSatisfied(strs) = o {
            for s in strs {
                union.push(SdkVersion::parse(s).unwrap());
            }
        }
    }
    if union.is_empty() {
        (Err(SdkResolveError::NotFound), outcomes.len())
    } else {
        union.sort();
        union.dedup();
        (
            Err(SdkResolveError::VersionNotSatisfied {
                spec: spec.clone(),
                available: union,
            }),
            outcomes.len(),
        )
    }
}

/// Property: across an arbitrary list of per-root outcomes,
/// `resolve_across_roots` returns exactly what the reference fold
/// computes — same outcome (Ok payload identity / NotFound /
/// VersionNotSatisfied union) and same number of locator calls
/// (first-match short-circuit observable through the call count).
///
/// Distribution sanity: across 256 cases, every fold branch must
/// fire at least a few times (first-Ok, all-NotFound, mixed-union,
/// empty-roots).
#[test]
fn resolve_across_roots_matches_reference_fold() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    let saw_ok = Cell::new(0u32);
    let saw_not_found_only = Cell::new(0u32);
    let saw_mixed_union = Cell::new(0u32);
    let saw_empty = Cell::new(0u32);

    let strat = prop::collection::vec(outcome_strategy(), 0..6);
    let spec = unpinned_spec();
    runner
        .run(&strat, |outcomes| {
            let (expected_result, expected_calls) = reference_fold(&outcomes, &spec);
            let results = outcomes_to_results(&outcomes, &spec);
            // One PathBuf per outcome — paths are otherwise opaque to
            // the iteration.
            let roots: Vec<PathBuf> = (0..outcomes.len())
                .map(|i| PathBuf::from(format!("/r{i}")))
                .collect();
            let locator = ScriptedLocator::new(results);
            let result = resolve_across_roots(&roots, "Microsoft.NET.Sdk", &locator.locate());
            prop_assert_eq!(&result, &expected_result);
            prop_assert_eq!(locator.call_count.get(), expected_calls);

            // Distribution accounting.
            if outcomes.is_empty() {
                saw_empty.set(saw_empty.get() + 1);
            } else if outcomes.iter().any(|o| matches!(o, RootOutcome::Ok(_))) {
                saw_ok.set(saw_ok.get() + 1);
            } else if outcomes.iter().all(|o| matches!(o, RootOutcome::NotFound)) {
                saw_not_found_only.set(saw_not_found_only.get() + 1);
            } else {
                saw_mixed_union.set(saw_mixed_union.get() + 1);
            }
            Ok(())
        })
        .unwrap();

    // Each run draws 0..6 outcomes from a 3-branch pool. Across
    // 256 cases the per-bucket counts should comfortably exceed 5.
    assert!(saw_empty.get() >= 5, "too few empty: {}", saw_empty.get());
    assert!(saw_ok.get() >= 20, "too few Ok: {}", saw_ok.get());
    assert!(
        saw_not_found_only.get() >= 5,
        "too few all-NotFound: {}",
        saw_not_found_only.get()
    );
    assert!(
        saw_mixed_union.get() >= 20,
        "too few mixed-union: {}",
        saw_mixed_union.get()
    );
}

// ============================================================
// Workload locator context: DOTNET_CLI_HOME, global.json pins
// ============================================================

#[test]
fn empty_dotnet_cli_home_is_treated_as_unset() {
    // .NET's CliFolderPathCalculatorCore checks string.IsNullOrEmpty:
    // an empty DOTNET_CLI_HOME must fall back to the platform home
    // rather than deriving the relative user root `.dotnet`.
    let env = SdkDiscoveryEnv::from_env_lookup(|name| match name {
        "DOTNET_CLI_HOME" => Some(OsString::new()),
        "HOME" | "USERPROFILE" => Some(OsString::from("/home/user")),
        _ => None,
    });
    assert_eq!(env.dotnet_cli_home, None);
    assert_eq!(env.home_dir, Some(PathBuf::from("/home/user")));
}

#[test]
fn non_empty_dotnet_cli_home_wins_over_home() {
    let env = SdkDiscoveryEnv::from_env_lookup(|name| match name {
        "DOTNET_CLI_HOME" => Some(OsString::from("/srv/cli-home")),
        "HOME" | "USERPROFILE" => Some(OsString::from("/home/user")),
        _ => None,
    });
    assert_eq!(env.dotnet_cli_home, Some(PathBuf::from("/srv/cli-home")));
}

/// Host SDK version dir `10.0.300` with a known-manifest list and one
/// matching manifest carrying both `WorkloadManifest.{json,targets}`.
fn install_workload_layout(dotnet_root: &Path) -> PathBuf {
    let version_dir = dotnet_root.join("sdk").join("10.0.300");
    fs::create_dir_all(&version_dir).unwrap();
    fs::write(version_dir.join("KnownWorkloadManifests.txt"), "w.workload").unwrap();
    let manifest_dir = dotnet_root.join("sdk-manifests/10.0.300/w.workload/1.0.0");
    fs::create_dir_all(&manifest_dir).unwrap();
    fs::write(manifest_dir.join("WorkloadManifest.json"), "{}").unwrap();
    fs::write(manifest_dir.join("WorkloadManifest.targets"), "<Project/>").unwrap();
    manifest_dir
}

#[test]
fn resolve_workload_locator_exactly_without_global_json_pin() {
    let tmp = TempDir::new().unwrap();
    let dotnet_root = tmp.path().join("dotnet");
    let manifest_dir = install_workload_layout(&dotnet_root);
    let project_dir = tmp.path().join("proj");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("global.json"),
        r#"{"sdk": {"version": "10.0.300"}}"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet_root.clone()));
    let disc = SdkDiscovery::for_project(&project_dir.join("App.fsproj"), &env).unwrap();
    let resolved = disc
        .resolve(workloads::WORKLOAD_MANIFEST_TARGETS_LOCATOR)
        .unwrap();
    assert_eq!(resolved, SdkResolution::Roots(vec![manifest_dir]));
}

#[test]
fn resolve_degrades_locators_when_global_json_pins_workload_set() {
    // Identical layout to the exact-resolution test above, except the
    // global.json also pins sdk.workloadVersion: MSBuild would hand
    // that file to its workload manifest provider and select a
    // workload set (or fail), so the locators must degrade.
    let tmp = TempDir::new().unwrap();
    let dotnet_root = tmp.path().join("dotnet");
    install_workload_layout(&dotnet_root);
    let project_dir = tmp.path().join("proj");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("global.json"),
        r#"{"sdk": {"version": "10.0.300", "workloadVersion": "10.0.300"}}"#,
    )
    .unwrap();

    let env = env_with(|e| e.dotnet_root = Some(dotnet_root.clone()));
    let disc = SdkDiscovery::for_project(&project_dir.join("App.fsproj"), &env).unwrap();
    let err = disc
        .resolve(workloads::WORKLOAD_MANIFEST_TARGETS_LOCATOR)
        .unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

#[test]
fn empty_home_is_treated_as_unset() {
    let env = SdkDiscoveryEnv::from_env_lookup(|name| match name {
        "HOME" | "USERPROFILE" => Some(OsString::new()),
        _ => None,
    });
    assert_eq!(env.home_dir, None);
}

#[test]
fn workload_override_detection_follows_upstream_value_semantics() {
    // PACK_ROOTS goes through string.IsNullOrEmpty upstream: empty is
    // a genuine no-op and must not degrade workload locators. The
    // other two variables are presence checks upstream (an empty
    // MANIFEST_ROOTS still prepends an empty root; IGNORE_DEFAULT_ROOTS
    // set to any value — even "false" — drops the default roots).
    let with_var = |name: &'static str, value: &'static str| {
        SdkDiscoveryEnv::from_env_lookup(move |queried| {
            (queried == name).then(|| OsString::from(value))
        })
    };
    assert!(!with_var("DOTNETSDK_WORKLOAD_PACK_ROOTS", "").workload_overrides_present);
    assert!(with_var("DOTNETSDK_WORKLOAD_PACK_ROOTS", "/roots").workload_overrides_present);
    assert!(with_var("DOTNETSDK_WORKLOAD_MANIFEST_ROOTS", "").workload_overrides_present);
    assert!(
        with_var("DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS", "false")
            .workload_overrides_present
    );
    assert!(!with_var("UNRELATED", "x").workload_overrides_present);
}

#[test]
fn empty_home_dir_in_hand_built_env_derives_no_user_root() {
    // A hand-built env can carry `home_dir: Some("")`; deriving the
    // cwd-relative user root `.dotnet` from it would scan an unrelated
    // workspace directory. With a userlocal marker present and no
    // usable user root, resolution must degrade instead.
    let tmp = TempDir::new().unwrap();
    let dotnet_root = tmp.path().join("dotnet");
    install_workload_layout(&dotnet_root);
    fs::create_dir_all(dotnet_root.join("metadata/workloads/10.0.300")).unwrap();
    fs::write(
        dotnet_root.join("metadata/workloads/10.0.300/userlocal"),
        "",
    )
    .unwrap();
    let project_dir = tmp.path().join("proj");
    fs::create_dir_all(&project_dir).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet_root.clone());
        e.home_dir = Some(PathBuf::new());
    });
    let disc = SdkDiscovery::for_project(&project_dir.join("App.fsproj"), &env).unwrap();
    let err = disc
        .resolve(workloads::WORKLOAD_MANIFEST_TARGETS_LOCATOR)
        .unwrap_err();
    assert!(matches!(err, SdkResolveError::UnsupportedLayout { .. }));
}

/// An explicitly-constructed environment is hermetic: it carries no build
/// environment, so a host variable cannot reach `$(…)` evaluation.
///
/// `Workspace::with_env` documents exactly this promise ("used by tests to
/// avoid leaking host env vars into project evaluation"), and it is structural
/// rather than a discipline: the snapshot is a *field*, filled only by
/// `from_process_env`.
#[test]
fn hand_built_env_carries_no_build_environment() {
    let env = SdkDiscoveryEnv::default();
    assert!(
        env.build_environment.is_empty(),
        "a hand-built SdkDiscoveryEnv must not see the host environment"
    );

    let env = SdkDiscoveryEnv {
        dotnet_root: Some(std::path::PathBuf::from("/opt/dotnet")),
        ..Default::default()
    };
    assert!(env.build_environment.is_empty());
}

/// …while the process-derived environment does carry it, or production
/// evaluation would lose every environment-backed property.
///
/// Asserted as *correspondence* with the process snapshot, not as
/// non-emptiness: nothing guarantees this process has any variable set at all
/// (run the test binary under `env -i` and it has none), and an empty
/// `build_environment` is then the correct representation. The one value that
/// is *not* a process variable is `MSBuildUserExtensionsPath`: MSBuild computes
/// it for itself, so `from_process_env` seeds the derived value on top of the
/// snapshot (a genuine env var of that name still wins). Everything else must
/// be carried verbatim.
#[test]
fn process_env_carries_the_build_environment() {
    let raw: HashMap<String, String> = std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();

    let env = SdkDiscoveryEnv::from_process_env();

    // Every genuine process variable is carried verbatim. (A real
    // `MSBuildUserExtensionsPath` env var appears in `raw` and, per `or_insert`,
    // still wins over the computed default — so it is checked here too.)
    for (name, value) in &raw {
        assert_eq!(
            env.build_environment.get(name),
            Some(value),
            "process variable {name} must be carried into build_environment verbatim"
        );
    }

    // The only key `from_process_env` may add beyond the snapshot is the
    // computed `MSBuildUserExtensionsPath` (absent only when no home directory
    // could be derived, e.g. under `env -i`).
    let added: Vec<&String> = env
        .build_environment
        .keys()
        .filter(|key| !raw.contains_key(*key))
        .collect();
    assert!(
        added
            .iter()
            .all(|key| key.as_str() == "MSBuildUserExtensionsPath"),
        "from_process_env added unexpected keys beyond MSBuildUserExtensionsPath: {added:?}"
    );
}

/// A non-empty `MSBuildSDKsPath` in the build environment reroutes MSBuild's
/// resolution of an *unpinned* (in-box) SDK: `MSBuildSDKsPath=/nonexistent
/// dotnet msbuild` on a `<Project Sdk="Microsoft.NET.Sdk">` fails with MSB4236
/// (probed against dotnet 8.0.420 and 10.0.301). Where the redirect points is
/// version/resolver-dependent and we do not model it, so discovery must not
/// silently resolve through the installed roots as if the override were
/// absent — that would import a chain the real build never uses. It declines
/// with the "degrade, don't guess" signal instead.
#[test]
fn resolve_declines_when_build_environment_sets_msbuild_sdks_path() {
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet);
        e.build_environment = HashMap::from([(
            "MSBuildSDKsPath".to_string(),
            "/somewhere/else/Sdks".to_string(),
        )]);
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let err = disc.resolve("Microsoft.NET.Sdk").unwrap_err();
    assert!(
        matches!(err, SdkResolveError::UnsupportedLayout { .. }),
        "expected an UnsupportedLayout decline, got {err:?}"
    );
}

/// MSBuild treats an *empty* `MSBuildSDKsPath` as unset — it computes its own
/// (probed: `MSBuildSDKsPath= dotnet msbuild -getProperty:MSBuildSDKsPath`
/// reads back the real toolset directory and resolution succeeds). So an empty
/// value must not trip the decline; normal resolution stands.
#[test]
fn resolve_unaffected_by_empty_msbuild_sdks_path() {
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet.clone());
        e.build_environment = HashMap::from([("MSBuildSDKsPath".to_string(), String::new())]);
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    let paths = single_root(disc.resolve("Microsoft.NET.Sdk").expect("resolve"));
    assert!(paths.props.starts_with(dotnet.join("sdk").join("8.0.401")));
}

/// The decline is keyed case-insensitively. On Unix the real build only reads
/// the exact-case env var, so a variant spelling declining here is a
/// deliberate *over*-decline (unresolved, never a wrong commit) rather than a
/// divergence — the safe direction when a non-standard spelling is present.
#[test]
fn resolve_declines_for_case_variant_msbuild_sdks_path() {
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet);
        e.build_environment =
            HashMap::from([("msbuildsdkspath".to_string(), "/somewhere/else".to_string())]);
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert!(matches!(
        disc.resolve("Microsoft.NET.Sdk").unwrap_err(),
        SdkResolveError::UnsupportedLayout { .. }
    ));
}

/// A version pin does *not* exempt a name from the decline. MSBuild's default
/// resolver serves a pinned name straight from `MSBuildSDKsPath` when NuGet has
/// nothing restored for it — probed against dotnet 8.0.420: `Sdk="MySdk"` with
/// `msbuild-sdks={MySdk:1.2.3}` and `MySdk/Sdk` under `MSBuildSDKsPath` reads a
/// property the package there defines. Whether NuGet has the pin is runtime
/// state we do not reimplement the resolver cascade to check, so a pinned name
/// under the override declines too — even one we *could* resolve via a restored
/// package, which is an over-decline (unresolved, never a wrong commit).
#[test]
fn msbuild_sdks_pinned_resolution_declines_under_the_override() {
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");
    let nuget = tmp.path().join("nuget");
    let sdk_pkg_root = nuget.join("my.custom.sdk").join("1.2.3").join("Sdk");
    fs::create_dir_all(&sdk_pkg_root).unwrap();
    fs::write(sdk_pkg_root.join("Sdk.props"), "<Project/>").unwrap();
    fs::write(sdk_pkg_root.join("Sdk.targets"), "<Project/>").unwrap();

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(
        tmp.path().join("global.json"),
        r#"{ "msbuild-sdks": { "My.Custom.Sdk": "1.2.3" } }"#,
    )
    .unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet);
        e.nuget_packages_dir = Some(nuget.clone());
        e.build_environment = HashMap::from([(
            "MSBuildSDKsPath".to_string(),
            "/nonexistent/Sdks".to_string(),
        )]);
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert!(matches!(
        disc.resolve("My.Custom.Sdk").unwrap_err(),
        SdkResolveError::UnsupportedLayout { .. }
    ));
    // The per-import `Sdk="Name/Version"` pin — the other pinned form — declines
    // for the same reason.
    assert!(matches!(
        disc.resolve("My.Custom.Sdk/1.2.3").unwrap_err(),
        SdkResolveError::UnsupportedLayout { .. }
    ));
}

/// A workload locator is declined under the override too — no exemption.
/// MSBuild serves even a workload locator *from* `MSBuildSDKsPath` when it
/// holds a matching entry (probed against dotnet 10.0.301: a
/// `MSBuildSDKsPath/Microsoft.NET.SDK.WorkloadAutoImportPropsLocator/Sdk` is
/// imported in preference to the workload resolver's empty result), so the
/// locator's resolution depends on the override as well. Resolving it through
/// our workload resolver as if the override were absent could commit a
/// different import set than the real build, so we decline — an over-decline
/// when the override lacks the locator, but always sound. (Guards against
/// reintroducing the unsound exemption the SDK-resolution oracle surfaced.)
#[test]
fn workload_locator_resolution_declines_under_the_override() {
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

    let project = tmp.path().join("proj").join("App.fsproj");
    fs::create_dir_all(project.parent().unwrap()).unwrap();

    let env = env_with(|e| {
        e.dotnet_root = Some(dotnet);
        e.build_environment = HashMap::from([(
            "MSBuildSDKsPath".to_string(),
            "/nonexistent/Sdks".to_string(),
        )]);
    });
    let disc = SdkDiscovery::for_project(&project, &env).expect("discovery");

    assert!(matches!(
        disc.resolve("Microsoft.NET.SDK.WorkloadAutoImportPropsLocator"),
        Err(SdkResolveError::UnsupportedLayout { .. })
    ));
}

/// The override lookup must not be fooled by a colliding empty case variant. On
/// Unix both `MSBuildSDKsPath=/real` (what MSBuild reads) and an empty
/// `msbuildsdkspath=` can coexist; an arbitrary-entry scan could pick the empty
/// one and miss the real override. The helper must find the non-empty value
/// regardless of `HashMap` iteration order.
#[test]
fn sdks_path_override_finds_the_nonempty_case_variant() {
    let mixed = HashMap::from([
        ("MSBuildSDKsPath".to_string(), "/real/Sdks".to_string()),
        ("msbuildsdkspath".to_string(), String::new()),
    ]);
    assert_eq!(
        build_env_sdks_path_override(&mixed).as_deref(),
        Some("/real/Sdks")
    );

    // The reverse population order must give the same answer.
    let mixed_rev = HashMap::from([
        ("msbuildsdkspath".to_string(), String::new()),
        ("MSBuildSDKsPath".to_string(), "/real/Sdks".to_string()),
    ]);
    assert_eq!(
        build_env_sdks_path_override(&mixed_rev).as_deref(),
        Some("/real/Sdks")
    );

    // All-empty variants collapse to "unset".
    let all_empty = HashMap::from([
        ("MSBuildSDKsPath".to_string(), String::new()),
        ("msbuildsdkspath".to_string(), String::new()),
    ]);
    assert_eq!(build_env_sdks_path_override(&all_empty), None);
}
