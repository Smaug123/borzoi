//! Custom SDK entry points (e.g. `Sdk.Web.props`): resolution and property
//! expansion in the imported project. How `Project` paths resolve (`..`,
//! absolute paths, separators) is graded against MSBuild by
//! `tests/sdk_import_project_path_diff.rs`.

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
fn explicit_import_with_empty_project_declines() {
    // MSBuild fails the evaluation on an empty or missing `Project`
    // (MSB4035) and on a whitespace one (MSB4019: it names the SDK
    // directory). Probed with `dotnet msbuild` 10.0.401. The import is
    // declined, not silently skipped.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk_with_extras(tmp.path(), "MySdk", "<Project/>", "<Project/>", &[]);
    for attribute in [r#"Project="""#, "", r#"Project="  ""#] {
        let body = format!(
            r#"<Project>
  <Import Sdk="MySdk" {attribute} />
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
            "{attribute:?}: {:?}",
            result.diagnostics,
        );
        assert!(
            result.package_reference_uncertainties.iter().any(|cause| matches!(
                &cause.kind,
                crate::PackageReferenceUncertaintyCauseKind::Structural(
                    crate::StructuralPackageReferenceUncertainty::SdkImportProjectUnresolved { .. }
                )
            )),
            "{attribute:?}: {:?}",
            result.package_reference_uncertainties,
        );
    }
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
fn explicit_import_subdir_custom_entry_point_resolves() {
    // Some SDKs lay out extra entry points under a subdirectory of
    // their `Sdk/` root.
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

#[test]
fn explicit_import_resolves_against_a_relative_sdk_root() {
    // `SdkPaths` admits a relative root (a relative `DOTNET_ROOT` yields
    // one). MSBuild's `NormalizePath` resolves it against the working
    // directory before collapsing `..`, so a root spelt with leading `..`
    // segments still names its own directory; dropping the segments `..`
    // cannot pop would name another one.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        "<Project/>",
        "<Project/>",
        &[(
            "Extra.props",
            r#"<Project>
  <PropertyGroup>
    <Seed>fromextra</Seed>
  </PropertyGroup>
</Project>"#,
        )],
    );
    // `root` spelt relative to the working directory: up to `/`, then down.
    let cwd = std::env::current_dir().unwrap();
    let relative_root = cwd
        .components()
        .skip(1)
        .map(|_| std::path::Component::ParentDir.as_os_str())
        .chain(root.components().skip(1).map(|c| c.as_os_str()))
        .collect::<PathBuf>();
    assert!(relative_root.starts_with(".."), "{relative_root:?}");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Extra.props" />
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: relative_root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(
        result.properties.get("Seed").map(String::as_str),
        Some("fromextra")
    );
}
