//! Custom SDK entry points (e.g. `Sdk.Web.props`): resolution, path-
//! traversal rejection, and property expansion in the imported project.

use super::*;
use tempfile::TempDir;

// -------------------------------------------------------------------------
// Phase 7b-v1c: custom SDK entry points (e.g. `Sdk.Web.props`)
// -------------------------------------------------------------------------

#[test]
fn explicit_import_resolves_custom_sdk_entry_point() {
    // Web/Worker/Razor and third-party SDKs ship extra entry-point
    // files like `Sdk.Web.props`. With phase 7b-v1c the walker resolves
    // `<Import Sdk="X" Project="Sdk.Web.props" />` against the SDK
    // root, picking up properties the extra file sets the same way it
    // does for the well-known `Sdk.props` / `Sdk.targets` pair.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        "<Project/>",
        "<Project/>",
        &[(
            "Sdk.Web.props",
            r#"<Project>
  <PropertyGroup>
    <WebSeed>fromweb</WebSeed>
  </PropertyGroup>
</Project>"#,
        )],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.Web.props" />
  <ItemGroup>
    <Compile Include="$(WebSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("fromweb.fs")]);
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            d.kind,
            DiagnosticKind::UndefinedProperty { .. }
                | DiagnosticKind::UnsupportedConstruct { .. }
                | DiagnosticKind::ImportFailed { .. }
        )),
        "Sdk.Web.props should resolve cleanly under the SDK root: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_rejects_sdk_relative_path_traversal() {
    // Defence: a hostile or malformed fsproj must not be able to
    // exfiltrate or evaluate arbitrary files outside the SDK root by
    // putting `..` in the `Project` attribute. The walker should reject
    // the import without touching the filesystem.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk_with_extras(tmp.path(), "MySdk", "<Project/>", "<Project/>", &[]);
    // Place an escape target a real file system read could land on if
    // `..` traversal were permitted. The test passes regardless of
    // whether this file exists, but its presence makes a successful
    // exfiltration visible: an `<Properties/>` element here would add a
    // property, so if the property doesn't appear we know the walker
    // didn't read this file.
    write_at(
        tmp.path(),
        "escape.props",
        r#"<Project>
  <PropertyGroup>
    <Escaped>OOPS</Escaped>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="../../escape.props" />
  <ItemGroup>
    <Compile Include="$(Escaped).fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    // The traversal attempt is reported as UnsupportedConstruct (same
    // shape as a malformed import); no file outside the SDK root is
    // read, so `$(Escaped)` stays undefined.
    assert!(
        result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedConstruct { element } if element.contains("..")
        )),
        "expected UnsupportedConstruct mentioning '..': {:?}",
        result.diagnostics,
    );
    assert!(
        result.diagnostics.iter().any(
            |d| matches!(&d.kind, DiagnosticKind::UndefinedProperty { name } if name == "Escaped")
        ),
        "expected `$(Escaped)` to remain undefined: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_rejects_absolute_sdk_relative_project() {
    // Absolute paths in `Project` would also escape the SDK root.
    // Reject before walking.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk_with_extras(tmp.path(), "MySdk", "<Project/>", "<Project/>", &[]);
    let absolute = if cfg!(windows) {
        "C:\\Windows\\System32\\drivers\\etc\\hosts"
    } else {
        "/etc/hosts"
    };
    let body = format!(
        r#"<Project>
  <Import Sdk="MySdk" Project="{absolute}" />
</Project>"#
    );
    let project_path = write_at(tmp.path(), "Demo.fsproj", &body);
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(
        result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedConstruct { element } if element.contains(absolute)
        )),
        "expected UnsupportedConstruct mentioning the absolute path: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_well_known_stems_still_use_explicit_fields() {
    // Regression: when the resolver returns `props`/`targets` at paths
    // that *don't* live under `root`, the well-known stems must keep
    // using the explicit fields rather than the new `root.join(...)`
    // path. Lets a resolver carry a stub `Sdk.props` elsewhere — common
    // when the production resolver caches a downloaded SDK in a
    // separate directory.
    let tmp = TempDir::new().unwrap();
    let other_dir = tmp.path().join("elsewhere");
    let props = write_at(
        &other_dir,
        "Sdk.props",
        r#"<Project>
  <PropertyGroup>
    <ElsewhereSeed>fromelsewhere</ElsewhereSeed>
  </PropertyGroup>
</Project>"#,
    );
    let targets = write_at(&other_dir, "Sdk.targets", "<Project/>");
    // The declared `root` is empty — has neither file. If the walker
    // ever consults `root.join("Sdk.props")` for the well-known stem
    // it would IOError; the explicit field is what should be used.
    let root = tmp.path().join("empty-root");
    std::fs::create_dir_all(&root).unwrap();

    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="$(ElsewhereSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("fromelsewhere.fs")],
        "well-known stem must follow the explicit `props` field, not `root.join(\"Sdk.props\")`: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_missing_custom_entry_point_surfaces_import_failed() {
    // A custom entry point the resolver doesn't ship surfaces through
    // the existing IO path: `walk_external_file` produces an
    // `ImportFailed` diagnostic. We don't invent a new variant for
    // "custom entry point missing"; the user gets the same shape they'd
    // see for any other broken `<Import Project="..."/>`.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        "<Project/>",
        "<Project/>",
        // Sdk.Worker.props is NOT shipped.
        &[],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.Worker.props" />
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { reason, .. } if matches!(reason, ImportFailReason::NotFound))),
        "expected ImportFailed::NotFound for missing custom entry point: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_custom_entry_point_expands_properties_in_project() {
    // MSBuild expands `$(...)` in `Import` `Project` attributes before
    // resolving the path. The non-SDK import path already does this;
    // the SDK custom-entry path must agree, or projects that compose
    // an entry-point name from a property —
    // `Project="Sdk.$(Flavor).props"` — would 404 against a literal
    // `$(Flavor)`.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        "<Project/>",
        "<Project/>",
        &[(
            "Sdk.Web.props",
            r#"<Project>
  <PropertyGroup>
    <ExpandedSeed>fromexpanded</ExpandedSeed>
  </PropertyGroup>
</Project>"#,
        )],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <Flavor>Web</Flavor>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.$(Flavor).props" />
  <ItemGroup>
    <Compile Include="$(ExpandedSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("fromexpanded.fs")],
        "diagnostics: {:?}",
        result.diagnostics,
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::ImportFailed { .. })),
        "no ImportFailed expected after expansion: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_rejects_windows_drive_relative_sdk_project() {
    // On Windows, `C:..\escape.props` is *not* absolute by Rust's
    // `Path::is_absolute()` (which requires both a drive prefix and
    // a root separator). But `PathBuf::join("C:..")` replaces the SDK
    // root with the drive prefix, letting a hostile fsproj escape
    // `SdkPaths::root`. The guard must reject any component that
    // starts with a single ASCII letter followed by `:` — regardless
    // of host platform, since the parser may run on Linux against a
    // .fsproj authored on Windows. UNC prefixes (`\\server\share`)
    // are likewise rejected: after normalisation they begin with `/`
    // and would be Unix-absolute, but we make the check explicit so
    // the intent is documented.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk_with_extras(tmp.path(), "MySdk", "<Project/>", "<Project/>", &[]);
    for hostile in [
        r"C:..\escape.props",
        r"C:escape.props",
        "C:/escape.props",
        r"\\server\share\escape.props",
    ] {
        let body = format!(
            r#"<Project>
  <Import Sdk="MySdk" Project="{hostile}" />
</Project>"#
        );
        let project_path = write_at(tmp.path(), "Demo.fsproj", &body);
        let result = parse_file_with_sdk(&project_path, |name| {
            if name == "MySdk" {
                Ok(SdkPaths {
                    root: root.clone(),
                    props: props.clone(),
                    targets: targets.clone(),
                })
            } else {
                Err(SdkResolveError::NotFound)
            }
        });
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedConstruct { .. })),
            "expected UnsupportedConstruct for hostile path {hostile:?}: {:?}",
            result.diagnostics,
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { .. })),
            "no FS touch should occur for hostile path {hostile:?}: {:?}",
            result.diagnostics,
        );
    }
}

#[test]
fn explicit_import_subdir_custom_entry_point_resolves() {
    // Some SDKs lay out extra entry points under a subdirectory of
    // their `Sdk/` root. Forward-slash subdir traversal is allowed
    // (the check rejects `..` and absolute paths, not nested forward
    // navigation).
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        "<Project/>",
        "<Project/>",
        &[(
            "sub/Extra.props",
            r#"<Project>
  <PropertyGroup>
    <SubSeed>fromsub</SubSeed>
  </PropertyGroup>
</Project>"#,
        )],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="sub/Extra.props" />
  <ItemGroup>
    <Compile Include="$(SubSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("fromsub.fs")]);
}
