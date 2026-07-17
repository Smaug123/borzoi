//! Project-level `Sdk="..."` resolution: props/targets ordering against
//! the body and `Directory.Build.*`, and explicit `<Import Sdk=...>`
//! item promotion.

use super::*;
use proptest::prelude::*;
use tempfile::TempDir;

// -------------------------------------------------------------------------
// Phase 7b-v0: SDK resolution
// -------------------------------------------------------------------------

#[test]
fn top_level_sdk_element_degrades_instead_of_reading_sdk_names_as_undefined() {
    // `<Sdk Name="X"/>` as a top-level element is the `Sdk` attribute's
    // sibling form: MSBuild imports X's `Sdk.props` before everything in
    // the file and `Sdk.targets` after, regardless of where the element
    // sits (probed on dotnet msbuild 10.0.301: `[$(UsingMicrosoftNETSdk)]`
    // evaluates to `[true]`). We don't model the element form, so the walk
    // must degrade — silently ignoring it would let names the SDK chain
    // defines read as exactly-undefined, committing `[]` in a clean,
    // non-partial result (a codex review caught precisely that).
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Sdk Name="Microsoft.NET.Sdk" />
  <PropertyGroup>
    <R>[$(UsingMicrosoftNETSdk)]</R>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.is_partial,
        "an unmodelled top-level <Sdk> element must degrade: {:?}",
        result.diagnostics
    );
    assert!(
        result.items_uncertain,
        "the un-imported SDK chain could contribute default items: {:?}",
        result.diagnostics
    );
    assert!(
        result.property_provenance_untrusted("R"),
        "a read of an SDK-defined name must not be committed as exact"
    );

    // Position-independence: the SDK chain splices around the whole file,
    // so an `<Sdk>` element *after* the read still makes it inexact.
    let trailing = write_at(
        tmp.path(),
        "Trailing.fsproj",
        r#"<Project>
  <PropertyGroup>
    <R>[$(UsingMicrosoftNETSdk)]</R>
  </PropertyGroup>
  <Sdk Name="Microsoft.NET.Sdk" />
</Project>"#,
    );
    let result = parse_file(&trailing);
    assert!(
        result.is_partial && result.property_provenance_untrusted("R"),
        "the element form is position-independent, so a trailing <Sdk> \
         must still degrade the earlier read: {:?}",
        result.diagnostics
    );
}

#[test]
fn project_sdk_resolves_and_props_seed_visible_to_body() {
    // The hello-world for phase 7b: `<Project Sdk="X">` resolves, the
    // SDK's `Sdk.props` runs before the body, and a property it sets
    // is visible to a `$(...)` reference in the body. Without this,
    // the SDK splice would be entirely silent.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <SdkSeed>fromprops</SdkSeed>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
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
    assert_eq!(paths_of(&result.items), vec![dir.join("fromprops.fs")]);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "$(SdkSeed) should resolve from Sdk.props, no UndefinedProperty expected: {:?}",
        result.diagnostics,
    );
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            d.kind,
            DiagnosticKind::SdkNotFound { .. } | DiagnosticKind::UnsupportedConstruct { .. }
        )),
        "SDK resolved cleanly, no SDK-shaped diagnostic expected: {:?}",
        result.diagnostics,
    );
}

#[test]
fn project_sdk_body_reference_to_sdk_property_is_case_insensitive() {
    // MSBuild property names are case-insensitive (ASCII): the SDK
    // declares `<CNh>` and the body references `$(CnH)` — same
    // property. The body reference must resolve to the SDK's value
    // and must NOT emit `UndefinedProperty`. This locks the contract
    // independently of the proptest, whose generator otherwise
    // happens to draw matching casings most of the time.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <CNh>fromprops</CNh>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="$(CnH).fs" />
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
    assert_eq!(paths_of(&result.items), vec![dir.join("fromprops.fs")]);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "$(CnH) should match the SDK's <CNh> case-insensitively: {:?}",
        result.diagnostics,
    );
}

#[test]
fn project_sdk_targets_run_after_body() {
    // `Sdk.targets` is imported AFTER the project body. Its writes
    // therefore see properties the body set, *and* its writes do not
    // affect substitutions inside the body. The clearest probe:
    // `Sdk.targets` substitutes a `$(...)` against a property the
    // *body* defined — if targets ran before the body, the reference
    // would emit UndefinedProperty.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <PropertyGroup>
    <Echoed>echo-$(BodyValue)</Echoed>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup>
    <BodyValue>hello</BodyValue>
  </PropertyGroup>
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
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "Sdk.targets ran after body so $(BodyValue) should be defined: {:?}",
        result.diagnostics,
    );
}

#[test]
fn project_sdk_with_failing_resolver_emits_sdk_not_found() {
    // Resolver was supplied but couldn't locate the SDK. Diagnostic
    // surface flips from UnsupportedConstruct (no-resolver phase 7a
    // behaviour) to the more specific SdkNotFound. We also assert
    // *neither* Sdk.props nor Sdk.targets ran, by probing for any
    // property either might have set.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Unknown.Sdk">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |_| Err(SdkResolveError::NotFound));
    let sdk_not_found = result
        .diagnostics
        .iter()
        .filter_map(|d| match &d.kind {
            DiagnosticKind::SdkNotFound { name } => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sdk_not_found,
        ["Unknown.Sdk"],
        "expected exactly one SdkNotFound for Unknown.Sdk, got: {:?}",
        result.diagnostics,
    );
    // No UnsupportedConstruct for the SDK either: SdkNotFound replaces
    // it, doesn't duplicate it.
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedConstruct { element } if element.contains("Sdk")
        )),
        "SdkNotFound and UnsupportedConstruct should be mutually exclusive: {:?}",
        result.diagnostics,
    );
    assert!(result.is_partial);
}

#[test]
fn project_sdk_resolved_still_walks_directory_build_props() {
    // The real `Microsoft.NET.Sdk` chain pulls `Directory.Build.props`
    // itself from inside `Microsoft.Common.props`. The walker owns that
    // implicit import point directly rather than depending on the deeper SDK
    // chain to rediscover the file, so it must keep its splice live alongside
    // the SDK. We witness that: define a property in `Directory.Build.props`,
    // reference it from the body, and check no `UndefinedProperty` fires.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <DirBuildSeed>fromdirbuild</DirBuildSeed>
  </PropertyGroup>
</Project>"#,
    );
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="$(DirBuildSeed).fs" />
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
    assert_eq!(paths_of(&result.items), vec![dir.join("fromdirbuild.fs")]);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "$(DirBuildSeed) should resolve from Directory.Build.props even with SDK resolved: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_directory_build_props_rediscovery_import_is_suppressed() {
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromProps.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildPropsPath>$(MSBuildProjectDirectory)/Directory.Build.props</DirectoryBuildPropsPath>
  </PropertyGroup>
  <Import Project="$(DirectoryBuildPropsPath)" Condition="Exists('$(DirectoryBuildPropsPath)')" />
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
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
        vec![dir.join("FromProps.fs")],
        "Directory.Build.props should be walked by the explicit splice only; diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_directory_build_targets_rediscovery_import_is_suppressed() {
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromTargets.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <Import Project="$(DirectoryBuildTargetsPath)" Condition="Exists('$(DirectoryBuildTargetsPath)')" />
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
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
        vec![dir.join("FromTargets.fs")],
        "Directory.Build.targets should be walked by the explicit splice only; diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_custom_directory_build_targets_path_import_is_not_suppressed() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildTargetsPath>SdkCustom.targets</DirectoryBuildTargetsPath>
  </PropertyGroup>
  <Import Project="$(DirectoryBuildTargetsPath)" />
</Project>"#,
    );
    write_at(
        targets.parent().unwrap(),
        "SdkCustom.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromSdkCustomTargets.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
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
        vec![dir.join("FromSdkCustomTargets.fs")],
        "custom SDK import via DirectoryBuildTargetsPath should be followed; diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_custom_directory_build_props_path_import_is_not_suppressed() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildPropsPath>SdkCustom.props</DirectoryBuildPropsPath>
  </PropertyGroup>
  <Import Project="$(DirectoryBuildPropsPath)" Condition="Exists('$(DirectoryBuildPropsPath)')" />
</Project>"#,
    );
    write_at(
        targets.parent().unwrap(),
        "SdkCustom.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromSdkCustomProps.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
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
        vec![dir.join("FromSdkCustomProps.fs")],
        "custom SDK import via DirectoryBuildPropsPath should be followed; diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_directory_build_props_rediscovery_ignores_path_retargeted_by_sdk_props() {
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromFallbackProps.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Retargeted.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromRetargetedProps.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildPropsPath>$(MSBuildProjectDirectory)/Retargeted.props</DirectoryBuildPropsPath>
  </PropertyGroup>
  <Import Project="$(DirectoryBuildPropsPath)" Condition="Exists('$(DirectoryBuildPropsPath)')" />
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
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
        vec![dir.join("FromRetargetedProps.fs")],
        "SDK rediscovery should not pre-import a DirectoryBuildPropsPath retargeted before the explicit props splice; diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_directory_build_targets_rediscovery_ignores_path_retargeted_by_spliced_file() {
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildTargetsPath>$(MSBuildProjectDirectory)/Retargeted.targets</DirectoryBuildTargetsPath>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="FromTargets.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Retargeted.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromRetargetedTargets.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <Import Project="$(DirectoryBuildTargetsPath)" Condition="Exists('$(DirectoryBuildTargetsPath)')" />
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
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
        vec![dir.join("FromTargets.fs")],
        "SDK rediscovery should not re-import a DirectoryBuildTargetsPath retargeted by the spliced file; diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn failing_sdk_resolver_falls_back_to_directory_build_splice() {
    // Companion: when the resolver returns None, the walker still
    // walks Directory.Build.* — the splice has always been live, but
    // we keep this as a regression witness for the no-SDK path. The
    // deliberately-missing inner import is the witness: it fires
    // exactly once if and only if Directory.Build.props was imported.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <Import Project="never-exists.props" />
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Unknown.Sdk">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |_| Err(SdkResolveError::NotFound));
    let inner_import_failures = result
        .diagnostics
        .iter()
        .filter(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { .. }))
        .count();
    assert_eq!(
        inner_import_failures, 1,
        "expected the fallback Directory.Build.props to be walked exactly once, got: {:?}",
        result.diagnostics,
    );
}

#[test]
fn sdk_props_runs_before_directory_build_props() {
    // MSBuild's effective order is Sdk.props → Directory.Build.props
    // → body → Directory.Build.targets → Sdk.targets. The relevant
    // divergence risk: a `Directory.Build.props` conditioned on a
    // property the SDK defines (e.g. `$(UsingMicrosoftNETSdk)`) would
    // be silently skipped or emit UndefinedProperty if we ran
    // Directory.Build.props first. We witness the ordering by having
    // Sdk.props define a property and Directory.Build.props consume
    // it inside an `<ItemGroup>`. If the order is wrong, the item
    // either fails to materialise or `$(SdkSeed)` substitutes to "".
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <SdkSeed>fromsdk</SdkSeed>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="Body.fs" />
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
        vec![dir.join("fromsdk.fs"), dir.join("Body.fs")],
        "Directory.Build.props's $(SdkSeed) must resolve from Sdk.props \
         (which has to run first); diagnostics: {:?}",
        result.diagnostics,
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "no UndefinedProperty expected when Sdk.props runs before Directory.Build.props: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_only_sdk_props_runs_before_directory_build_props() {
    // Same divergence as `sdk_props_runs_before_directory_build_props`,
    // but with MSBuild's explicit form: the project has *no* root
    // `Sdk="X"` attribute and instead opens with
    // `<Import Sdk="X" Project="Sdk.props"/>`. MSBuild treats this as
    // equivalent to the root-Sdk shorthand, so the effective order is
    // still Sdk.props → Directory.Build.props → body → ... .
    // The body pre-scan slice (7b-v1c continued) promotes the first
    // unconditional explicit `Sdk.props` import to root-equivalent
    // status; without it, Directory.Build.props would run first and
    // `$(SdkSeed)` would be empty.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <SdkSeed>fromsdk</SdkSeed>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <ItemGroup>
    <Compile Include="Body.fs" />
  </ItemGroup>
  <Import Sdk="MySdk" Project="Sdk.targets" />
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
        vec![dir.join("fromsdk.fs"), dir.join("Body.fs")],
        "Directory.Build.props's $(SdkSeed) must resolve from Sdk.props \
         (promoted from the body Import) — diagnostics: {:?}",
        result.diagnostics,
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "no UndefinedProperty expected when the explicit Sdk.props import \
         is promoted before Directory.Build.props: {:?}",
        result.diagnostics,
    );
    // The explicit imports must not be double-walked once the same file
    // is spliced at the top: no ImportFailed of any kind may surface.
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { .. })),
        "explicit Sdk.props/Sdk.targets imports must be skipped during \
         body walk once promoted to top: {:?}",
        result.diagnostics,
    );
}

#[test]
fn promoted_explicit_sdk_items_carry_body_import_span() {
    // Public contract: items/diagnostics from an imported file
    // collapse to the importing `<Import>` element's byte range. For
    // the explicit-SDK form, the user wrote `<Import Sdk="X"
    // Project="Sdk.props"/>` as a body element — items the
    // (promoted) Sdk.props contributes must point at *that* Import
    // node's span, not the whole `<Project>` element. This test
    // pins the contract for the promotion path.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromSdkProps.fs" />
  </ItemGroup>
</Project>"#,
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromSdkTargets.fs" />
  </ItemGroup>
</Project>"#,
    );
    let source = r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <ItemGroup>
    <Compile Include="Body.fs" />
  </ItemGroup>
  <Import Sdk="MySdk" Project="Sdk.targets" />
</Project>"#;
    let project_path = write_at(tmp.path(), "Demo.fsproj", source);
    let result = parse_with_sdk(&project_path, source, |name| {
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
    let props_import_start = source
        .find("<Import Sdk=\"MySdk\" Project=\"Sdk.props\"")
        .unwrap();
    let props_import_end =
        source[props_import_start..].find("/>").unwrap() + props_import_start + 2;
    let targets_import_start = source
        .find("<Import Sdk=\"MySdk\" Project=\"Sdk.targets\"")
        .unwrap();
    let targets_import_end =
        source[targets_import_start..].find("/>").unwrap() + targets_import_start + 2;
    let from_props = result
        .items
        .iter()
        .find(|i| {
            i.include
                .file_name()
                .is_some_and(|n| n == "FromSdkProps.fs")
        })
        .expect("FromSdkProps.fs should be in items");
    assert_eq!(
        from_props.span,
        props_import_start..props_import_end,
        "promoted Sdk.props's items must point at the body <Import \
         Sdk=... Project=Sdk.props/> element, not the project root",
    );
    let from_targets = result
        .items
        .iter()
        .find(|i| {
            i.include
                .file_name()
                .is_some_and(|n| n == "FromSdkTargets.fs")
        })
        .expect("FromSdkTargets.fs should be in items");
    assert_eq!(
        from_targets.span,
        targets_import_start..targets_import_end,
        "promoted Sdk.targets's items must point at the body <Import \
         Sdk=... Project=Sdk.targets/> element, not the project root",
    );
}

#[test]
fn explicit_sdk_promotion_skipped_when_property_group_precedes_props() {
    // MSBuild evaluates child elements in document order: a
    // `<PropertyGroup>` written *before* `<Import Sdk="X"
    // Project="Sdk.props"/>` would set properties that Sdk.props itself
    // (and the SDK chain's `Sdk.props`-driven imports of
    // Directory.Build.props) could observe. Promoting that body Import
    // to the OUTERMOST splice position silently reorders the
    // PropertyGroup *after* the SDK chain. The conservative behaviour
    // is to skip promotion entirely when anything precedes Sdk.props;
    // the body walk then handles the explicit import in its natural
    // (post-PropertyGroup) position.
    //
    // Witness: a property the project sets *before* the SDK import
    // would only be visible to the SDK chain in the promoted (wrong)
    // ordering. We name the SDK with a value that wouldn't otherwise
    // be referenced and check the resolver was never asked to look up
    // anything based on the post-PropertyGroup name — promotion was
    // skipped, so the resolver gets called only at body position.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(BeforeSdk).fs" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <BeforeSdk>set-by-project</BeforeSdk>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <ItemGroup>
    <Compile Include="Body.fs" />
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
    // Without promotion, Sdk.props walks at body position — after the
    // PropertyGroup wrote BeforeSdk — so its <Compile Include=
    // "$(BeforeSdk).fs"/> resolves to "set-by-project.fs". With
    // (wrong) promotion, Sdk.props runs before the PropertyGroup and
    // BeforeSdk substitutes to empty, producing ".fs" plus an
    // UndefinedProperty diagnostic.
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("set-by-project.fs"), dir.join("Body.fs")],
        "promotion must be skipped when a PropertyGroup precedes \
         Sdk.props; got items {:?} diagnostics {:?}",
        result.items,
        result.diagnostics,
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "no UndefinedProperty expected when promotion is correctly \
         skipped: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_sdk_promotion_skipped_when_property_group_follows_targets() {
    // Symmetric case for Sdk.targets: a `<PropertyGroup>` *after* the
    // explicit `<Import Sdk="X" Project="Sdk.targets"/>` would in
    // MSBuild see the SDK-supplied properties (Sdk.targets has
    // already run). Promoting Sdk.targets to the bottommost slot
    // reorders it *after* the trailing PropertyGroup, which is the
    // opposite of what MSBuild would do.
    //
    // We promote Sdk.props (it's still at the top), but Sdk.targets
    // stays in-body. Witness: a property the trailing PropertyGroup
    // sets must be visible to Directory.Build.targets, since both
    // walk after the body — i.e. the trailing PropertyGroup ran
    // before our Directory.Build.targets splice. (In MSBuild it
    // would run between the body and the Sdk.targets the explicit
    // import requested — both orderings differ from us, but the
    // important thing here is to verify we did *not* promote
    // Sdk.targets.)
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(TrailingProp).fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <ItemGroup>
    <Compile Include="Body.fs" />
  </ItemGroup>
  <Import Sdk="MySdk" Project="Sdk.targets" />
  <PropertyGroup>
    <TrailingProp>set-after-targets</TrailingProp>
  </PropertyGroup>
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
    // The body's Sdk.targets import must NOT have been promoted (a
    // PropertyGroup follows it). It runs at body position, and only
    // once — we never spliced Sdk.targets at the bottom, so there is
    // no duplicate to skip.
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { .. })),
        "no ImportFailed expected — targets import shouldn't be \
         double-walked because it wasn't promoted: {:?}",
        result.diagnostics,
    );
    // Body.fs comes from the body's <ItemGroup>; the trailing
    // PropertyGroup sets TrailingProp which our Directory.Build.targets
    // splice (post-body) sees, producing set-after-targets.fs.
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("Body.fs"), dir.join("set-after-targets.fs")],
        "items {:?} diagnostics {:?}",
        result.items,
        result.diagnostics,
    );
}

#[test]
fn explicit_sdk_promotion_skipped_when_props_is_conditional() {
    // A `Condition` attribute on the body Import is a deliberate gate:
    // the project might be opting into a particular SDK variant only
    // under certain configurations. Promoting silently bypasses the
    // condition (the splice runs the SDK chain unconditionally).
    // Conservative behaviour: leave conditional imports to the body
    // walk, where `evaluate_condition` handles them faithfully.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <Who>dbp</Who>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <SdkSeed>fromsdk</SdkSeed>
    <Who>sdk</Who>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.props" Condition="'true' == 'true'" />
  <ItemGroup>
    <Compile Include="Body.fs" />
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
    // Promotion skipped → the property pass runs Directory.Build.props
    // (before-body splice) *before* the conditional Sdk.props import at its
    // body position, so the last write to `Who` is the SDK's. Promotion
    // would splice Sdk.props outermost-first and Directory.Build.props
    // would win instead.
    assert_eq!(
        result.properties.get("Who").map(String::as_str),
        Some("sdk"),
        "conditional Sdk.props must NOT be promoted; diagnostics: {:?}",
        result.diagnostics,
    );
    // The item pass runs after the whole property pass, so the
    // Directory.Build.props item sees the SDK-defined `SdkSeed` even
    // though the SDK props walk later — matching MSBuild.
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("fromsdk.fs"), dir.join("Body.fs")],
        "diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_sdk_promotion_skipped_when_props_wrapped_in_import_group() {
    // The body pre-scan only looks at *direct* <Import> children of
    // the project root. An `<ImportGroup>` wrapper — even
    // unconditional — is not a position MSBuild's root-Sdk shorthand
    // expands to, so we conservatively decline to promote. The body
    // walk handles the wrapped import normally.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <Who>dbp</Who>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
  </ItemGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <SdkSeed>fromsdk</SdkSeed>
    <Who>sdk</Who>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ImportGroup>
    <Import Sdk="MySdk" Project="Sdk.props" />
  </ImportGroup>
  <ItemGroup>
    <Compile Include="Body.fs" />
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
    // Same detection as the conditional-import test: promotion skipped →
    // Directory.Build.props runs first in the property pass, so the wrapped
    // Sdk.props import's write to `Who` lands last.
    assert_eq!(
        result.properties.get("Who").map(String::as_str),
        Some("sdk"),
        "ImportGroup-wrapped Sdk.props must NOT be promoted; \
         diagnostics: {:?}",
        result.diagnostics,
    );
    // And the item pass sees the final property table regardless of where
    // in the property pass the SDK props ran.
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("fromsdk.fs"), dir.join("Body.fs")],
        "diagnostics: {:?}",
        result.diagnostics,
    );
}

#[test]
fn directory_build_targets_runs_before_sdk_targets() {
    // Symmetric to `sdk_props_runs_before_directory_build_props`:
    // after the body, MSBuild walks `Directory.Build.targets` first
    // (it's pulled in by the SDK chain's targets) and then the rest
    // of `Sdk.targets`. We witness by writing a property in
    // Directory.Build.targets and consuming it in Sdk.targets — if
    // the order were reversed, Sdk.targets would emit
    // UndefinedProperty.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup>
    <DirTargetsSeed>fromdirtargets</DirTargetsSeed>
  </PropertyGroup>
</Project>"#,
    );
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <PropertyGroup>
    <SdkTargetsEcho>echo-$(DirTargetsSeed)</SdkTargetsEcho>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="A.fs" />
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
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "no UndefinedProperty expected when Directory.Build.targets runs before Sdk.targets: {:?}",
        result.diagnostics,
    );
}

#[test]
fn imported_file_with_sdk_root_without_resolver_emits_unsupported_construct() {
    // Without an SDK resolver in hand, an imported file's
    // `<Project Sdk="...">` root cannot be resolved any more than the
    // entry project's can — the splice machinery routes both through
    // `resolve_project_sdk`, which emits `UnsupportedConstruct` when
    // `sdk_resolver` is `None`. The `Imported` origin distinguishes
    // the diagnostic from the entry-project case so LSP-style
    // callers can choose to suppress it.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project Sdk="Inner.Sdk">
  <PropertyGroup>
    <FromDirBuild>seen</FromDirBuild>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let imported_sdk_diags: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| {
            matches!(
                &d.kind,
                DiagnosticKind::UnsupportedConstruct { element }
                    if element.contains("Inner.Sdk")
            ) && d.origin == DiagnosticOrigin::Imported
        })
        .collect();
    assert_eq!(
        imported_sdk_diags.len(),
        1,
        "expected one UnsupportedConstruct flagging the imported file's Sdk root, got: {:?}",
        result.diagnostics,
    );
    assert!(result.is_partial);
}

#[test]
fn explicit_import_sdk_props_runs_resolver() {
    // `<Import Sdk="X" Project="Sdk.props" />` is MSBuild's explicit
    // form of the SDK import (the shorthand expands to this plus
    // `Sdk.targets`). The resolver wires it to the same on-disk file
    // as the shorthand would. We probe by depending on a property the
    // synthetic Sdk.props defines.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <Stem>fromsdkpropsfile</Stem>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <ItemGroup>
    <Compile Include="$(Stem).fs" />
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
        vec![dir.join("fromsdkpropsfile.fs")]
    );
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            d.kind,
            DiagnosticKind::UnsupportedConstruct { .. } | DiagnosticKind::SdkNotFound { .. }
        )),
        "explicit Sdk import should resolve cleanly, got: {:?}",
        result.diagnostics,
    );
}

#[test]
fn explicit_import_sdk_with_failing_resolver_emits_sdk_not_found() {
    // Resolver supplied but it doesn't know this SDK; behaviour
    // matches the project-root case: SdkNotFound rather than
    // UnsupportedConstruct.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="Unknown.Sdk" Project="Sdk.props" />
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |_| Err(SdkResolveError::NotFound));
    let sdk_not_found: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|d| match &d.kind {
            DiagnosticKind::SdkNotFound { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sdk_not_found,
        ["Unknown.Sdk"],
        "got: {:?}",
        result.diagnostics,
    );
}

proptest! {
    /// Property: every body `$(Name)` reference whose `Name` was
    /// defined by the SDK's `Sdk.props` (case-insensitively, matching
    /// MSBuild's property-lookup semantics) substitutes to the SDK's
    /// value; every reference to a name the SDK *doesn't* define
    /// substitutes exactly to the empty string (since C.2b: MSBuild
    /// expands an undefined `$(Name)` to "", and this walk — a cleanly
    /// resolved SDK, no opacity, no carve-out names in the alphabet —
    /// proves the name undefined in the real build too). No
    /// `UndefinedProperty` is emitted for either population.
    ///
    /// The generator's alphabet (`[A-Z][A-Za-z]{2,5}`, 3–6 chars, no
    /// underscores) cannot collide with the exactness guard's toolset
    /// names (`MSBuild*` needs 7+ chars; `OS` needs 2; the rest are
    /// longer or contain `_`) or the consumer-contract carve-outs
    /// (`DefineConstants`, `TargetFramework` — both 15 chars), so every
    /// undefined draw is exact.
    ///
    /// The generator builds the SDK's PropertyGroup and the body's
    /// Include references from that alphabet independently — so on the
    /// same draw a given name is either:
    /// in the SDK only (defined, body uses → substitutes `v`),
    /// in the body only (used, undefined → substitutes ``),
    /// in both (defined and used → substitutes `v`), or
    /// in neither (irrelevant).
    /// Each iteration covers the full population in one go.
    ///
    /// Property names are compared case-insensitively (ASCII): the SDK
    /// might write `<CNh>` and the body reference `$(CnH)`, and MSBuild
    /// treats those as the same property. The oracle lowercases both
    /// sides before checking membership so the assertions reflect the
    /// implementation's contract rather than the random draw's casing.
    ///
    /// Why this catches bugs the unit tests miss: any wiring mistake
    /// that flips when N=0 vs N=1 vs N=many properties (e.g. an
    /// off-by-one in the splice loop, or a State.lookup miss that only
    /// shows up on the second property) will fail here on the random
    /// draws that exercise those sizes.
    #[test]
    fn sdk_props_defined_names_substitute_and_undefined_names_read_exactly_empty(
        sdk_defined in proptest::collection::hash_set("[A-Z][A-Za-z]{2,5}", 0..6),
        body_uses in proptest::collection::vec("[A-Z][A-Za-z]{2,5}", 0..6),
    ) {
        let tmp = TempDir::new().unwrap();
        let props_body = if sdk_defined.is_empty() {
            "<Project/>".to_string()
        } else {
            let mut s = String::from("<Project>\n  <PropertyGroup>\n");
            for name in &sdk_defined {
                s.push_str(&format!("    <{name}>v</{name}>\n"));
            }
            s.push_str("  </PropertyGroup>\n</Project>\n");
            s
        };
        let (root, props, targets) =
            write_synthetic_sdk(tmp.path(), "MySdk", &props_body, "<Project/>");

        let mut project_body = String::from("<Project Sdk=\"MySdk\">\n  <ItemGroup>\n");
        for (i, name) in body_uses.iter().enumerate() {
            project_body.push_str(&format!(
                "    <Compile Include=\"$({name})-{i}.fs\" />\n",
            ));
        }
        project_body.push_str("  </ItemGroup>\n</Project>\n");
        let project_path = write_at(tmp.path(), "Demo.fsproj", &project_body);

        let result = parse_with_sdk(&project_path, &project_body, |name| {
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

        let defined_lower: std::collections::HashSet<String> = sdk_defined
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();

        // No name in the alphabet is a toolset property or a carve-out and
        // the walk is not opaque, so every undefined read is exact: no
        // UndefinedProperty for anyone, and the parse is not partial.
        prop_assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
            "exact undefined reads must not diagnose: {:?}",
            result.diagnostics,
        );
        prop_assert!(!result.is_partial, "diags: {:?}", result.diagnostics);

        // Defined names substitute to the SDK's value; undefined names
        // substitute exactly to "".
        let dir = canon(tmp.path());
        let expected: Vec<std::path::PathBuf> = body_uses
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let value = if defined_lower.contains(&name.to_ascii_lowercase()) {
                    "v"
                } else {
                    ""
                };
                dir.join(format!("{value}-{i}.fs"))
            })
            .collect();
        prop_assert_eq!(
            paths_of(&result.items),
            expected,
            "diags: {:?}",
            result.diagnostics,
        );
    }
}
