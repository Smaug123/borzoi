//! SDK-provenance for `package_references_uncertain`: the mirror-image of
//! `items_uncertain`. For the *Compile* set, the SDK tree is tolerated (its
//! default-item machinery is standard and doesn't drop hand-written sources).
//! For the *dependency* set, SDK item groups are evaluated normally: the SDK is
//! exactly where implicit `PackageReference`/`FrameworkReference` items live
//! (framework refs like `Microsoft.NETCore.App`, implicit `FSharp.Core`), so we
//! must capture them rather than blanket-ignore them. SDK provenance alone is
//! *not* an uncertainty: the multi-pass evaluator finalises every property
//! (SDK files included) before any item evaluates, so a cleanly-evaluated SDK
//! dependency item is captured exactly. Uncertainty survives only for concrete
//! constructs the walker genuinely cannot pin down — a condition or value
//! leaning on an undefined property, an unsupported expression, a structural
//! skip — each recorded as its own cause.

use super::*;
use proptest::prelude::*;
use tempfile::TempDir;

/// An SDK that injects a literal `FrameworkReference` (the
/// `Microsoft.NETCore.App` shape) is captured *with certainty*: the item
/// evaluates against the final property table like any user-authored item, and
/// nothing about it is unevaluable.
#[test]
fn sdk_injected_framework_reference_is_captured_and_certain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.NETCore.App" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        !result.package_references_uncertain,
        "a literal SDK dependency item evaluates exactly; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(result.package_reference_uncertainties.is_empty());
    // The project-authored reference is captured alongside SDK-injected
    // dependency items.
    assert!(
        result
            .package_references
            .iter()
            .any(|p| p.id == "Newtonsoft.Json"),
        "explicit references are still captured"
    );
    assert!(
        result
            .framework_references
            .iter()
            .any(|f| f.name == "Microsoft.NETCore.App"),
        "SDK framework references are captured"
    );
    // Contrast: the Compile set is untouched (the SDK carries no Compile risk
    // here), so this is genuinely a package-only distinction.
    assert!(!result.items_uncertain);
}

/// MSBuild evaluates all properties, including the project body, before item
/// metadata — and so does this walker's item pass, SDK files included.
#[test]
fn sdk_package_reference_captures_final_metadata_and_stays_certain() {
    // The SDK-declared PackageReference's Version metadata evaluates in the
    // item pass against the final table, so the project's clean 2.0 override
    // is captured exactly — and, both writes being clean, trusted.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <ImplicitPkgVersion>1.0</ImplicitPkgVersion>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(ImplicitPkgVersion)" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup>
    <ImplicitPkgVersion>2.0</ImplicitPkgVersion>
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("SDK PackageReference is still captured");
    assert_eq!(
        package.version.as_deref(),
        Some("2.0"),
        "the item pass reads the final property table"
    );
    assert!(
        !result.package_references_uncertain,
        "clean SDK item + clean project override evaluate exactly; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

/// A clean literal SDK default can still be wrong for a project-body package
/// item: MSBuild evaluates all project properties before all project items, so a
/// later body property write changes metadata for an earlier PackageReference.
#[test]
fn package_metadata_reads_final_value_of_sdk_seeded_property() {
    // The SDK seeds ImplicitPkgVersion=1.0; the project overrides it to 2.0
    // after the item's document position. The item pass reads the final
    // table, so the capture is 2.0 — and the project's clean overwrite makes
    // the value trustworthy regardless of SDK modelling fidelity.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <ImplicitPkgVersion>1.0</ImplicitPkgVersion>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(ImplicitPkgVersion)" />
  </ItemGroup>
  <PropertyGroup>
    <ImplicitPkgVersion>2.0</ImplicitPkgVersion>
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("project PackageReference is still captured");
    assert_eq!(
        package.version.as_deref(),
        Some("2.0"),
        "the item pass reads the final property table"
    );
    assert!(
        !result.package_references_uncertain,
        "a clean project overwrite is trustworthy; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// A project package item reading a cleanly-written SDK property is exact:
/// the property pass computes the same final value MSBuild would, so plain
/// SDK provenance must not degrade the set.
#[test]
fn clean_sdk_property_used_by_project_package_metadata_stays_certain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <PkgVersion>1.0</PkgVersion>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(PkgVersion)" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert_eq!(result.package_references[0].version.as_deref(), Some("1.0"));
    assert!(
        !result.package_references_uncertain,
        "clean SDK provenance should not mark package metadata uncertain; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

/// An SDK helper item whose identity comes from a cleanly-written SDK property
/// is exact for the same reason; a project package reference consuming the
/// helper list stays certain.
#[test]
fn clean_sdk_property_used_by_helper_identity_stays_certain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <PkgId>Foo</PkgId>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="$(PkgId)" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert!(
        !result.package_references_uncertain,
        "clean SDK helper identities should not be tainted; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

#[test]
fn sdk_tainted_skipped_property_group_marks_package_metadata_read_uncertain() {
    // `Gate` is SDK-written under a condition we cannot pin down (it reads
    // `TargetFramework`, carved out of undefined-read exactness because the
    // real build defines it), so the real build's Gate could differ, flipping
    // the gated PkgVersion write. Skipping the write under a taint-suspect
    // gate therefore taints PkgVersion itself, and the item pass's read of
    // `$(PkgVersion)` degrades the set to uncertain.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Gate>false</Gate>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup>
    <PkgVersion>1.0</PkgVersion>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(PkgVersion)" />
  </ItemGroup>
  <PropertyGroup Condition="'$(Gate)' == 'true'">
    <PkgVersion>2.0</PkgVersion>
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("project PackageReference is still captured");
    assert_eq!(
        package.version.as_deref(),
        Some("1.0"),
        "our evaluation skips the gated write; the taint records that the \
         real build might not"
    );
    assert!(
        result.package_references_uncertain,
        "a taint-suspect skipped write must degrade the set; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result.package_reference_uncertainties.iter().any(|cause| {
            matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )
        }),
        "expected the tainted-property read to degrade the set, got: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn sdk_tainted_skipped_property_condition_marks_package_metadata_read_uncertain() {
    // `Gate` is SDK-written under a condition we cannot pin down (it reads
    // `TargetFramework`, carved out of undefined-read exactness because the
    // real build defines it), so the real build's Gate could differ, flipping
    // the gated PkgVersion write. Skipping the write under a taint-suspect
    // gate therefore taints PkgVersion itself, and the item pass's read of
    // `$(PkgVersion)` degrades the set to uncertain.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Gate>false</Gate>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup>
    <PkgVersion>1.0</PkgVersion>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(PkgVersion)" />
  </ItemGroup>
  <PropertyGroup>
    <PkgVersion Condition="'$(Gate)' == 'true'">2.0</PkgVersion>
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("project PackageReference is still captured");
    assert_eq!(
        package.version.as_deref(),
        Some("1.0"),
        "our evaluation skips the gated write; the taint records that the \
         real build might not"
    );
    assert!(
        result.package_references_uncertain,
        "a taint-suspect skipped write must degrade the set; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result.package_reference_uncertainties.iter().any(|cause| {
            matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )
        }),
        "expected the tainted-property read to degrade the set, got: {:?}",
        result.package_reference_uncertainties
    );
}

/// SDK properties can feed package metadata declared later in the entry
/// project. If the SDK property assignment itself depended on a condition we
/// cannot pin down (`TargetFramework` is carved out of undefined-read
/// exactness), the expanded metadata value is useful but not trustworthy: a
/// real build, where the SDK defines it, could produce a different version.
#[test]
fn sdk_conditioned_property_used_by_project_package_metadata_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <PkgVersion>1.0</PkgVersion>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(PkgVersion)" />
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("project PackageReference is still captured");
    assert_eq!(
        package.version.as_deref(),
        Some("1.0"),
        "document the current single-pass capture; callers must consult uncertainty"
    );
    assert!(
        result.package_references_uncertain,
        "SDK-conditioned property metadata must make package references uncertain; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )),
        "expected SDK property-pass package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
}

/// SDK taint must follow supported property functions too. The evaluator can
/// reduce this `TrimStart` expression, but the value still depends on an SDK
/// property written under an uncertain condition.
#[test]
fn sdk_conditioned_property_function_used_by_project_package_metadata_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <TargetFrameworkVersion>v8.0</TargetFrameworkVersion>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(TargetFrameworkVersion.TrimStart('vV'))" />
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("project PackageReference is still captured");
    assert_eq!(package.version.as_deref(), Some("8.0"));
    assert!(
        result.package_references_uncertain,
        "SDK-conditioned property function metadata must make package references uncertain; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )),
        "expected SDK property-pass package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
}

/// The CPM opt-in gate is itself package-affecting. If its group condition
/// depends on an SDK-tainted property, a cleanly false result from the
/// document-order walk is not enough to report versioned package refs certain:
/// MSBuild's property pass could make the group run before items are evaluated.
#[test]
fn sdk_tainted_cpm_group_condition_marks_package_uncertain_when_false() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <EnableCentralPackageVersions>false</EnableCentralPackageVersions>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup Condition="'$(EnableCentralPackageVersions)' == 'true'">
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="1.0" />
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
        result.package_references_uncertain,
        "SDK-tainted CPM conditions must make package refs uncertain; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )),
        "expected SDK property-pass package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn sdk_tainted_import_condition_marks_package_uncertain_when_false() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <ImportImplicitDeps>false</ImportImplicitDeps>
  </PropertyGroup>
  <Import Project="ImplicitDeps.props" Condition="'$(ImportImplicitDeps)' == 'true'" />
</Project>"#,
        "<Project/>",
        &[(
            "ImplicitDeps.props",
            r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.AspNetCore.App" />
  </ItemGroup>
</Project>"#,
        )],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "SDK-tainted import gates can hide dependency imports; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )),
        "expected SDK property-pass package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
    assert!(
        !result.items_uncertain,
        "SDK-tainted import-gate package uncertainty must not reintroduce Compile uncertainty"
    );
}

/// The contrast to the tainted import gate above: an SDK import gated on a
/// *literal* SDK property evaluates exactly in the property pass, so the
/// cleanly-skipped import neither contributes items nor degrades the set.
#[test]
fn clean_sdk_import_condition_stays_certain_when_false() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <ImportImplicitDeps>false</ImportImplicitDeps>
  </PropertyGroup>
  <Import Project="ImplicitDeps.props" Condition="'$(ImportImplicitDeps)' == 'true'" />
</Project>"#,
        "<Project/>",
        &[(
            "ImplicitDeps.props",
            r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.AspNetCore.App" />
  </ItemGroup>
</Project>"#,
        )],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        !result.package_references_uncertain,
        "a literal SDK import gate is evaluated exactly by the property pass; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result.framework_references.is_empty(),
        "the cleanly false import should still be skipped"
    );
    assert!(!result.items_uncertain);
}

#[test]
fn uncertain_project_property_override_preserves_prior_sdk_package_taint() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <PkgVersion>1.0</PkgVersion>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup Condition="'$(VisualStudioVersion)' == ''">
    <PkgVersion>2.0</PkgVersion>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(PkgVersion)" />
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
    let package = result
        .package_references
        .iter()
        .find(|package| package.id == "Foo")
        .expect("project PackageReference is still captured");
    assert_eq!(package.version.as_deref(), Some("2.0"));
    assert!(
        result.package_references_uncertain,
        "an untrusted project override must not clear prior SDK package taint; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )),
        "expected SDK property-pass package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
}

/// An SDK that injects nothing is not a dependency uncertainty by itself. Any
/// uncertainty must be tied to a concrete package/framework construct in the
/// followed files.
#[test]
fn empty_sdk_does_not_mark_package_set_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        !result.package_references_uncertain,
        "entering an SDK subtree is not itself uncertain; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(result.package_reference_uncertainties.is_empty());
}

/// SDK dependency items still participate in package uncertainty. A condition
/// we cannot evaluate may hide or include an implicit dependency, so it is not
/// tolerated the way SDK Compile machinery is.
#[test]
fn sdk_dependency_condition_uncertainty_is_not_suppressed() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup Condition="'@(_Unmodelled)' == 'x'">
    <FrameworkReference Include="Microsoft.NETCore.App" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "an unevaluable SDK dependency condition should make the dependency set uncertain"
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                (&cause.kind, &cause.origin),
                (
                    PackageReferenceUncertaintyCauseKind::Diagnostic(
                        DiagnosticKind::UnsupportedCondition { .. }
                    ),
                    DiagnosticOrigin::Imported
                )
            )),
        "expected imported condition uncertainty cause, got: {:?}",
        result.package_reference_uncertainties
    );
}

// Cases 1 and 2 lean on `TargetFramework`, which is carved out of
// undefined-read exactness (the real build supplies it), so both gates stay
// untrustworthy rather than being decided exactly.
fn sdk_helper_group_condition(condition_case: u8) -> &'static str {
    match condition_case {
        0 => "'@(_Unmodelled)' == 'x'",
        1 => "'$(TargetFramework)' == ''",
        2 => "'$(TargetFramework)' == 'net8.0'",
        _ => unreachable!("condition_case strategy is 0..3"),
    }
}

#[test]
fn sdk_helper_group_condition_reads_final_project_override_of_sdk_property() {
    // The SDK seeds UseImplicitPackage=false and the project cleanly
    // overrides it to true; the helper group's condition evaluates in the
    // item pass against the final value, so the helper item exists and the
    // consumer captures it — and the project's clean overwrite clears the
    // SDK-write taint, so the set stays certain.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <UseImplicitPackage>false</UseImplicitPackage>
  </PropertyGroup>
  <ItemGroup Condition="'$(UseImplicitPackage)' == 'true'">
    <ImplicitPackage Include="Foo" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <PropertyGroup>
    <UseImplicitPackage>true</UseImplicitPackage>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert!(
        !result.package_references_uncertain,
        "the helper gate reads the final (project-written) value; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

#[test]
fn sdk_helper_identity_uses_final_project_property_value() {
    // The SDK helper item evaluates in the item pass, so its identity reads
    // the FINAL ImplicitPackageId=Bar (the project's write after the import
    // site), and the consuming `@(ImplicitPackage)` sees exactly that —
    // matching MSBuild's property-pass-then-item-pass ordering.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <ImplicitPackage Include="$(ImplicitPackageId)" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <ImplicitPackageId>Foo</ImplicitPackageId>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <PropertyGroup>
    <ImplicitPackageId>Bar</ImplicitPackageId>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Bar");
    assert!(
        !result.package_references_uncertain,
        "helper and consumer both read final properties; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

#[test]
fn explicit_sdk_helper_group_and_child_conditions_read_final_values() {
    // Both the helper group's and the helper item's own conditions evaluate
    // in the item pass against final properties — the project's writes after
    // the import site flip them true, so the helper item exists and the
    // consumer captures it (matching MSBuild).
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup Condition="'$(UseGroup)' == 'true'">
    <ImplicitPackage Include="Foo" Condition="'$(UseChild)' == 'true'" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <UseGroup>false</UseGroup>
    <UseChild>false</UseChild>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <PropertyGroup>
    <UseGroup>true</UseGroup>
    <UseChild>true</UseChild>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert!(
        !result.package_references_uncertain,
        "group and child conditions read final properties; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

#[test]
fn sdk_helper_metadata_uses_final_project_property_value() {
    // The SDK helper's Version metadata evaluates in the item pass, so it
    // reads the FINAL ImplicitPackageVersion=2.0 (the project's write after
    // the import site) and the consumer inherits exactly that.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Version="$(ImplicitPackageVersion)" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <ImplicitPackageVersion>1.0</ImplicitPackageVersion>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <PropertyGroup>
    <ImplicitPackageVersion>2.0</ImplicitPackageVersion>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert_eq!(result.package_references[0].version.as_deref(), Some("2.0"));
    assert!(
        !result.package_references_uncertain,
        "helper metadata reads final properties; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

fn assert_has_item_definition_default_uncertainty(result: &ParsedProject) {
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault),
        "expected item-definition default uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn sdk_helper_item_definition_default_marks_inherited_package_metadata_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemDefinitionGroup>
    <ImplicitPackage>
      <PrivateAssets>all</PrivateAssets>
    </ImplicitPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert!(
        result.package_references_uncertain,
        "SDK helper item-definition defaults must remain uncertain; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault),
        "expected item-definition default uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn item_definition_default_on_uncaptured_package_metadata_is_inert() {
    // The F# SDK's `Microsoft.FSharp.NetSdk.props` sets a default
    // `<PackageReference><GeneratePathProperty>true</GeneratePathProperty></…>`
    // on every PackageReference. `GeneratePathProperty` is not one of the
    // metadata we capture (id / Version / VersionOverride / *Assets), so the
    // default cannot perturb any captured field — the set stays exact and no
    // uncertainty follows. (An item definition only sets metadata; it never
    // adds or removes items, so identity is unaffected too.)
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemDefinitionGroup>
    <PackageReference>
      <GeneratePathProperty>true</GeneratePathProperty>
    </PackageReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Newtonsoft.Json");
    assert_eq!(
        result.package_references[0].version.as_deref(),
        Some("13.0.1")
    );
    assert!(
        !result.package_references_uncertain,
        "a default on uncaptured metadata cannot change the captured set; causes: {:?}",
        result.package_reference_uncertainties
    );
    assert!(
        !result
            .package_reference_uncertainties
            .iter()
            .any(|cause| cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault),
        "no ItemDefinitionDefault cause expected: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn item_definition_default_on_captured_package_metadata_still_marks_uncertain() {
    // A default on a metadata we *do* capture (`Version`) could change the
    // captured version of any PackageReference that lacks its own — we do not
    // thread item-definition defaults into the capture, so the set must stay
    // uncertain. This guards the boundary of the uncaptured-metadata carve-out.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemDefinitionGroup>
    <PackageReference>
      <Version>1.2.3</Version>
    </PackageReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a default on captured Version metadata must keep the set uncertain; causes: {:?}",
        result.package_reference_uncertainties
    );
    assert_has_item_definition_default_uncertainty(&result);
}

#[test]
fn item_definition_default_on_framework_reference_metadata_is_inert() {
    // We capture only the *identity* of a FrameworkReference, no metadata, so
    // any item-definition default on it is inert regardless of the name.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemDefinitionGroup>
    <FrameworkReference>
      <PrivateAssets>all</PrivateAssets>
    </FrameworkReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.NETCore.App" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.framework_references.len(), 1);
    assert!(
        !result.package_references_uncertain,
        "a FrameworkReference default touches no captured metadata; causes: {:?}",
        result.package_reference_uncertainties
    );
}

// ------------------------------------------------------------------
// Inert `Update` items (the net10.0 AspNetCore residual, Stage C).
//
// `Microsoft.NET.Sdk.DefaultItems.Shared.targets` carries
//   <PackageReference Update="Microsoft.AspNetCore.App">
//     <PrivateAssets Condition="'%(PackageReference.Version)' == ''">all</…>
//   </PackageReference>
// An `Update` modifies only the *prior* `Include`s that share its identity
// (probed dotnet 10.0.301: an `Update` declared before its `Include` does not
// apply; a same-*spec* duplicate like `Update="A;A"` is the sole exception —
// it goes through MSBuild's position-independent dictionary path and can reach
// a *later* `Include`). So an `Update` whose literal identities are absent from
// the already-captured `Include` set — and contain no within-spec duplicate —
// changes nothing in the effective set. Its metadata (including any
// unsupported `%(…)`-conditioned child) can never reach a captured reference,
// so reading it would only manufacture a spurious uncertainty.
// ------------------------------------------------------------------

/// The exact SDK shape: an `Update` on an identity the project never
/// references, carrying an unsupported `%(PackageReference.Version)` condition.
/// It matches no prior `Include`, so it is inert and raises no uncertainty.
#[test]
fn update_matching_no_prior_include_is_inert_despite_unsupported_metadata_condition() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageReference Update="Microsoft.AspNetCore.App">
      <PrivateAssets Condition="'%(PackageReference.Version)' == ''">all</PrivateAssets>
    </PackageReference>
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Newtonsoft.Json");
    assert_eq!(
        result.package_references[0].version.as_deref(),
        Some("13.0.1")
    );
    assert!(
        !result.package_references_uncertain,
        "an Update matching no prior Include is inert; its unsupported metadata \
         condition must not surface; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// The boundary: the *same* unsupported metadata condition, but now the
/// `Update` matches a prior `Include`. Here the condition genuinely gates a
/// captured metadatum we cannot evaluate, so the set must stay uncertain.
#[test]
fn update_matching_a_prior_include_with_unsupported_metadata_condition_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageReference Update="Newtonsoft.Json">
      <PrivateAssets Condition="'%(PackageReference.Version)' == ''">all</PrivateAssets>
    </PackageReference>
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "an Update that matches a prior Include and carries an unevaluable \
         metadata condition must keep the set uncertain; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// A same-spec duplicate `Update` (`Update="A;A"`) declared *before* its
/// `Include` still reaches it through MSBuild's position-independent path, so
/// it is never inert — the existing duplicate-identity guard keeps the set
/// uncertain even though no *prior* Include matched at walk time.
#[test]
fn same_spec_duplicate_update_before_include_is_not_treated_as_inert() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Update="Dup;Dup">
      <PrivateAssets Condition="'%(PackageReference.Version)' == ''">all</PrivateAssets>
    </PackageReference>
    <PackageReference Include="Dup" Version="1.0" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a same-spec duplicate Update can modify a later Include, so it is not \
         inert; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// Our `Update`→`Include` matching is ASCII-case-insensitive, but MSBuild
/// matches item identities with full Unicode `OrdinalIgnoreCase` (probed
/// dotnet 10.0.301: `Update="ångström"` applies to `Include="Ångström"`). A
/// non-ASCII `Update` identity is a matching hazard we cannot resolve
/// faithfully, so it must decline rather than risk a certain-but-wrong
/// capture — both when its metadata carries an unevaluable condition …
#[test]
fn non_ascii_update_identity_with_unsupported_condition_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        "<Project>\n  <ItemGroup>\n    \
         <PackageReference Include=\"\u{c5}ngstr\u{f6}m\" Version=\"1.0\" />\n    \
         <PackageReference Update=\"\u{e5}ngstr\u{f6}m\">\n      \
         <PrivateAssets Condition=\"'%(PackageReference.Version)' == ''\">all</PrivateAssets>\n    \
         </PackageReference>\n  </ItemGroup>\n</Project>\n",
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a non-ASCII Update identity cannot be matched faithfully (ASCII vs \
         OrdinalIgnoreCase), so it must not be proven inert; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// … and when it writes a captured metadatum with no condition at all — the
/// case that would otherwise slip through `finalize_package_references`' own
/// ASCII match and publish a stale version with `package_references_uncertain
/// == false`.
#[test]
fn non_ascii_update_identity_writing_captured_metadata_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        "<Project>\n  <ItemGroup>\n    \
         <PackageReference Include=\"\u{c5}ngstr\u{f6}m\" Version=\"1.0\" />\n    \
         <PackageReference Update=\"\u{e5}ngstr\u{f6}m\" Version=\"2.0\" />\n  \
         </ItemGroup>\n</Project>\n",
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a non-ASCII Update writing captured metadata could match under \
         OrdinalIgnoreCase where our ASCII compare misses it; must decline; \
         causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// MSBuild matches item identities as *normalized paths* (probed dotnet
/// 10.0.301: `Update="./A"`, `".\A"`, `"Sub/../A"` all apply to `Include="A"`),
/// but our matching compares raw identities. A path-syntax-bearing `Update`
/// identity is a hazard our raw compare can miss, so it must decline — both for
/// the inert shortcut and (via the same guard) the finalize merge.
#[test]
fn path_like_update_identity_with_condition_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Aardvark" Version="1.0" />
    <PackageReference Update="./Aardvark">
      <PrivateAssets Condition="'%(PackageReference.Version)' == ''">all</PrivateAssets>
    </PackageReference>
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a path-like Update identity (`./Aardvark`) MSBuild normalizes to match \
         `Aardvark` must not be proven inert; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// The finalize-path twin: a path-like `Update` writing captured metadata with
/// no condition would slip through `finalize_package_references`' own raw match
/// and publish a stale version certainly.
#[test]
fn path_like_update_identity_writing_captured_metadata_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Aardvark" Version="1.0" />
    <PackageReference Update="Sub/../Aardvark" Version="2.0" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a path-like Update writing captured metadata could match under path \
         normalization where our raw compare misses it; must decline; \
         causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// A decoded trailing space (`Update="A%20"` → identity `"A "`, the escape
/// surviving `spec_fragments`' trim of the *escaped* text) is trimmed by
/// `Path.GetFullPath` on Windows, so MSBuild there matches `Include="A"`. Our
/// raw compare keeps the space; the identity is therefore not
/// normalization-stable across platforms and must decline. (This is why the
/// inert shortcut is gated on a positive package-id-shape allow-list rather
/// than a deny-list of known-bad characters.)
#[test]
fn decoded_trailing_space_update_identity_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Aardvark" Version="1.0" />
    <PackageReference Update="Aardvark%20" Version="2.0" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a decoded trailing-space identity is Windows-trimmable and not \
         match-faithful, so it must decline; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// The SDK's `DisableImplicitFrameworkReferences` shape:
/// `<PackageReference Update="@(PackageReference)" AllowExplicitVersion="true"/>`.
/// The self-referential target would resolve as unevaluable, but the Update
/// writes only `AllowExplicitVersion` — a metadatum we do not capture — so it
/// cannot change the captured set and must be screened out before its identity
/// is ever resolved.
#[test]
fn update_writing_only_uncaptured_metadata_is_inert_even_when_self_referential() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageReference Update="@(PackageReference)" AllowExplicitVersion="true" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Newtonsoft.Json");
    assert_eq!(
        result.package_references[0].version.as_deref(),
        Some("13.0.1")
    );
    assert!(
        !result.package_references_uncertain,
        "an Update writing only uncaptured metadata cannot perturb the set; \
         causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// The boundary of the uncaptured-metadata carve-out: an `Update` that *does*
/// write a captured metadatum (`Version`) to a matching prior `Include` — even
/// via a self-referential `@(PackageReference)` — must keep the set uncertain
/// (we do not thread `Update`s through the untracked-list identity path).
#[test]
fn update_writing_captured_metadata_self_referentially_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageReference Update="@(PackageReference)" Version="99.0.0" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "an Update writing a captured metadatum via an untracked list must stay \
         uncertain; causes: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn explicit_sdk_property_gated_helper_default_marks_inherited_metadata_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemDefinitionGroup Condition="'$(UseDefaults)' == 'true'">
    <ImplicitPackage>
      <PrivateAssets>all</PrivateAssets>
    </ImplicitPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Version="1.0" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <UseDefaults>false</UseDefaults>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <PropertyGroup>
    <UseDefaults>true</UseDefaults>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert_eq!(result.package_references[0].version.as_deref(), Some("1.0"));
    assert_eq!(result.package_references[0].private_assets, None);
    assert!(
        result.package_references_uncertain,
        "a later project property can enable explicit-SDK helper defaults MSBuild applies before package items; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert_has_item_definition_default_uncertainty(&result);
}

fn sdk_helper_identity_risk_fixture(risk_case: u8) -> (&'static str, &'static str) {
    match risk_case {
        0 => (
            r#"<PropertyGroup>
    <UseImplicitPackage>false</UseImplicitPackage>
  </PropertyGroup>
  <ItemGroup Condition="'$(UseImplicitPackage)' == 'true'">
    <ImplicitPackage Include="Foo" />
  </ItemGroup>"#,
            r#"<PropertyGroup>
    <UseImplicitPackage>true</UseImplicitPackage>
  </PropertyGroup>"#,
        ),
        1 => (
            r#"<PropertyGroup>
    <UseImplicitPackage>false</UseImplicitPackage>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Condition="'$(UseImplicitPackage)' == 'true'" />
  </ItemGroup>"#,
            r#"<PropertyGroup>
    <UseImplicitPackage>true</UseImplicitPackage>
  </PropertyGroup>"#,
        ),
        2 => (
            r#"<PropertyGroup>
    <ImplicitPackageId>Foo</ImplicitPackageId>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="$(ImplicitPackageId)" />
  </ItemGroup>"#,
            r#"<PropertyGroup>
    <ImplicitPackageId>Bar</ImplicitPackageId>
  </PropertyGroup>"#,
        ),
        _ => unreachable!("risk_case strategy is 0..3"),
    }
}

fn sdk_helper_metadata_risk_fixture(risk_case: u8) -> (&'static str, &'static str) {
    match risk_case {
        0 => (
            r#"<PropertyGroup>
    <ImplicitPackageVersion>1.0</ImplicitPackageVersion>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Version="$(ImplicitPackageVersion)" />
  </ItemGroup>"#,
            r#"<PropertyGroup>
    <ImplicitPackageVersion>2.0</ImplicitPackageVersion>
  </PropertyGroup>"#,
        ),
        1 => (
            r#"<PropertyGroup>
    <ImplicitPackageVersion>1.0</ImplicitPackageVersion>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo">
      <Version>$(ImplicitPackageVersion)</Version>
    </ImplicitPackage>
  </ItemGroup>"#,
            r#"<PropertyGroup>
    <ImplicitPackageVersion>2.0</ImplicitPackageVersion>
  </PropertyGroup>"#,
        ),
        2 => (
            r#"<PropertyGroup>
    <UseNewImplicitPackageVersion>false</UseNewImplicitPackageVersion>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Version="1.0">
      <Version Condition="'$(UseNewImplicitPackageVersion)' == 'true'">2.0</Version>
    </ImplicitPackage>
  </ItemGroup>"#,
            r#"<PropertyGroup>
    <UseNewImplicitPackageVersion>true</UseNewImplicitPackageVersion>
  </PropertyGroup>"#,
        ),
        _ => unreachable!("risk_case strategy is 0..3"),
    }
}

fn sdk_modelled_item_list_risk_fixture(risk_case: u8, item_case: u8) -> (String, &'static str) {
    let (item_type, include, item_ref) = match item_case {
        0 => (
            "ProjectReference",
            "../Lib/Lib.fsproj",
            "@(ProjectReference)",
        ),
        1 => ("Compile", "Generated.fs", "@(Compile)"),
        _ => unreachable!("item_case strategy is 0..2"),
    };
    let sdk_body = match risk_case {
        0 => format!(
            r#"<ItemGroup Condition="'$(UseModelledItem)' == 'true'">
    <{item_type} Include="{include}" />
  </ItemGroup>"#
        ),
        1 => format!(
            r#"<ItemGroup>
    <{item_type} Include="{include}" Condition="'$(UseModelledItem)' == 'true'" />
  </ItemGroup>"#
        ),
        _ => unreachable!("risk_case strategy is 0..2"),
    };
    (sdk_body, item_ref)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        ..ProptestConfig::default()
    })]

    /// Property: if an SDK helper item list is gated by a condition this
    /// evaluator cannot trust, any later dependency item that consumes the
    /// exact helper list must make the package/framework set uncertain. The
    /// helper may have been skipped, over-included, or included only because
    /// an inexact read (here the carved-out `TargetFramework`) substituted
    /// as empty.
    #[test]
    fn sdk_helper_group_gate_uncertainty_flows_to_project_dependency_consumer(
        condition_case in 0u8..3,
        use_framework_reference in proptest::bool::ANY,
    ) {
        let tmp = TempDir::new().unwrap();
        let condition = sdk_helper_group_condition(condition_case);
        let sdk_props = format!(
            r#"<Project>
  <ItemGroup Condition="{condition}">
    <ImplicitPackage Include="Foo" />
  </ItemGroup>
</Project>"#,
        );
        let dependency_item = if use_framework_reference {
            r#"<FrameworkReference Include="@(ImplicitPackage)" />"#
        } else {
            r#"<PackageReference Include="@(ImplicitPackage)" Version="1.0" />"#
        };
        let (root, props, targets) =
            write_synthetic_sdk(tmp.path(), "MySdk", &sdk_props, "<Project/>");
        let project = format!(
            r#"<Project Sdk="MySdk">
  <ItemGroup>
    {dependency_item}
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(tmp.path(), "Demo.fsproj", &project);
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
        prop_assert!(
            result.package_references_uncertain,
            "SDK helper list gate {condition:?} must taint dependency consumers; causes: {:?}; diags: {:?}",
            result.package_reference_uncertainties,
            result.diagnostics
        );
        let expected_cause = PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
            value: "@(ImplicitPackage)".to_string(),
        };
        prop_assert!(
            result
                .package_reference_uncertainties
                .iter()
                .any(|cause| cause.kind == expected_cause),
            "expected helper-list identity uncertainty, got: {:?}",
            result.package_reference_uncertainties
        );
    }

    /// Property: SDK helper item *identity* uncertainty must stay attached to
    /// the helper list until a dependency item consumes it. The stale-property
    /// risk can enter through the helper group's condition, the helper item's
    /// condition, or the helper item's identity expression.
    #[test]
    fn sdk_helper_identity_property_pass_uncertainty_flows_to_dependency_consumer(
        risk_case in 0u8..3,
        use_framework_reference in proptest::bool::ANY,
    ) {
        let tmp = TempDir::new().unwrap();
        let (sdk_body, project_properties) = sdk_helper_identity_risk_fixture(risk_case);
        let sdk_props = format!("<Project>\n  {sdk_body}\n</Project>");
        let dependency_item = if use_framework_reference {
            r#"<FrameworkReference Include="@(ImplicitPackage)" />"#
        } else {
            r#"<PackageReference Include="@(ImplicitPackage)" Version="9.0" />"#
        };
        let (root, props, targets) =
            write_synthetic_sdk(tmp.path(), "MySdk", &sdk_props, "<Project/>");
        let project = format!(
            r#"<Project Sdk="MySdk">
  {project_properties}
  <ItemGroup>
    {dependency_item}
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(tmp.path(), "Demo.fsproj", &project);
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
        // Every risk case resolves exactly in the item pass: the helper's
        // group/child conditions and identity read FINAL properties (the
        // project's writes), so the consumer captures the real MSBuild set.
        let expected_identity = match risk_case {
            0 | 1 => "Foo",
            2 => "Bar",
            _ => unreachable!("risk_case strategy is 0..3"),
        };
        if use_framework_reference {
            prop_assert_eq!(result.framework_references.len(), 1);
            prop_assert_eq!(
                result.framework_references[0].name.as_str(),
                expected_identity
            );
        } else {
            prop_assert_eq!(result.package_references.len(), 1);
            prop_assert_eq!(result.package_references[0].id.as_str(), expected_identity);
        }
        prop_assert!(
            !result.package_references_uncertain,
            "risk case {} evaluates exactly in the item pass; causes: {:?}; diags: {:?}",
            risk_case,
            result.package_reference_uncertainties,
            result.diagnostics
        );
    }

    /// Property: inherited SDK helper `Version` metadata is captured exactly
    /// (the item pass reads final properties), and a local
    /// `PackageReference Version=...` override always wins over it.
    #[test]
    fn sdk_helper_metadata_property_pass_uncertainty_flows_when_inherited(
        risk_case in 0u8..3,
        local_version_override in proptest::bool::ANY,
    ) {
        let tmp = TempDir::new().unwrap();
        let (sdk_body, project_properties) = sdk_helper_metadata_risk_fixture(risk_case);
        let sdk_props = format!("<Project>\n  {sdk_body}\n</Project>");
        let package_reference = if local_version_override {
            r#"<PackageReference Include="@(ImplicitPackage)" Version="9.0" />"#
        } else {
            r#"<PackageReference Include="@(ImplicitPackage)" />"#
        };
        let (root, props, targets) =
            write_synthetic_sdk(tmp.path(), "MySdk", &sdk_props, "<Project/>");
        let project = format!(
            r#"<Project Sdk="MySdk">
  {project_properties}
  <ItemGroup>
    {package_reference}
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(tmp.path(), "Demo.fsproj", &project);
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
        prop_assert_eq!(result.package_references.len(), 1);
        prop_assert_eq!(result.package_references[0].id.as_str(), "Foo");
        if local_version_override {
            prop_assert_eq!(result.package_references[0].version.as_deref(), Some("9.0"));
            prop_assert!(
                !result.package_references_uncertain,
                "local package metadata overrides stale helper metadata; causes: {:?}; diags: {:?}",
                result.package_reference_uncertainties,
                result.diagnostics
            );
        } else {
            // Inherited helper metadata evaluates in the item pass against
            // final properties, so every risk case captures the real
            // MSBuild value (2.0) exactly.
            prop_assert_eq!(
                result.package_references[0].version.as_deref(),
                Some("2.0")
            );
            prop_assert!(
                !result.package_references_uncertain,
                "risk case {} evaluates exactly in the item pass; causes: {:?}; diags: {:?}",
                risk_case,
                result.package_reference_uncertainties,
                result.diagnostics
            );
        }
    }

    /// Property: a dependency item consuming a *modelled* item list
    /// (`@(Compile)` / `@(ProjectReference)`) is untrusted — modelled lists
    /// are not resolvable as helper lists, so the identity is unevaluable
    /// regardless of how its gating property was written.
    #[test]
    fn sdk_modelled_item_gate_later_write_flows_to_dependency_consumer(
        risk_case in 0u8..2,
        item_case in 0u8..2,
    ) {
        let tmp = TempDir::new().unwrap();
        let (sdk_body, item_ref) = sdk_modelled_item_list_risk_fixture(risk_case, item_case);
        let sdk_props = format!("<Project>\n  {sdk_body}\n</Project>");
        let (root, props, targets) =
            write_synthetic_sdk(tmp.path(), "MySdk", &sdk_props, "<Project/>");
        let project = format!(
            r#"<Project>
  <PropertyGroup>
    <UseModelledItem>false</UseModelledItem>
  </PropertyGroup>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <PropertyGroup>
    <UseModelledItem>true</UseModelledItem>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="{item_ref}" Version="9.0" />
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(tmp.path(), "Demo.fsproj", &project);
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
        prop_assert!(
            result.package_references_uncertain,
            "SDK modelled item risk case {risk_case}/{item_case} must taint dependency consumers; causes: {:?}; diags: {:?}",
            result.package_reference_uncertainties,
            result.diagnostics
        );
        let expected_cause = PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
            value: item_ref.to_string(),
        };
        prop_assert!(
            result
                .package_reference_uncertainties
                .iter()
                .any(|cause| cause.kind == expected_cause),
            "expected modelled-list identity uncertainty, got: {:?}",
            result.package_reference_uncertainties
        );
        prop_assert!(
            !result.items_uncertain,
            "SDK modelled item package uncertainty must not reintroduce Compile uncertainty"
        );
    }
}

#[test]
fn clean_sdk_helper_item_consumed_by_project_package_stays_certain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <ImplicitPackage Include="Foo" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
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
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(result.package_references[0].id, "Foo");
    assert!(
        !result.package_references_uncertain,
        "a clean literal SDK helper list should not recreate blanket SDK uncertainty; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
}

/// A CPM item declared inside an SDK file is exactly as unapplied as one in a
/// user file: `<PackageVersion>`/`<GlobalPackageReference>` contribute
/// versions/packages this walker does not fold into the effective dependency
/// set unless the inline-CPM pass proves it can, so a clean SDK-authored one
/// must still record its conservative cause rather than ride the (removed)
/// blanket SDK envelope.
#[test]
fn sdk_authored_cpm_items_still_mark_package_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <PackageVersion Include="Foo" Version="1.0" />
    <GlobalPackageReference Include="MinVer" Version="4.3.0" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "unapplied SDK CPM items must degrade the set; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    for kind in [
        PackageReferenceUncertaintyCauseKind::PackageVersion,
        PackageReferenceUncertaintyCauseKind::GlobalPackageReference,
    ] {
        assert!(
            result
                .package_reference_uncertainties
                .iter()
                .any(|cause| cause.kind == kind
                    && matches!(cause.origin, DiagnosticOrigin::Imported)),
            "expected an imported {kind:?} cause, got: {:?}",
            result.package_reference_uncertainties
        );
    }
    assert!(
        !result.items_uncertain,
        "SDK CPM package uncertainty must not reintroduce Compile uncertainty"
    );
}

/// A structural SDK construct we cannot decide can hide dependency items:
/// the `When` gate reads `TargetFramework` (carved out of undefined-read
/// exactness), so the Choose is not exactly decidable and its subtree is
/// dropped. Compile uncertainty is still tolerated in SDK files, but the
/// package set must not be reported as certain when a dependency-bearing
/// subtree is dropped.
#[test]
fn sdk_choose_hiding_dependency_marks_package_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <Choose>
    <When Condition="'$(TargetFramework)' == 'net8.0'">
      <ItemGroup>
        <FrameworkReference Include="Microsoft.AspNetCore.App" />
      </ItemGroup>
    </When>
  </Choose>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "an SDK <Choose> can hide dependency items; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                (&cause.kind, &cause.origin),
                (
                    PackageReferenceUncertaintyCauseKind::Structural(
                        StructuralPackageReferenceUncertainty::UnsupportedChoose
                    ),
                    DiagnosticOrigin::Imported
                )
            )),
        "expected imported structural Choose package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
    assert!(
        !result.items_uncertain,
        "SDK structural package uncertainty must not reintroduce Compile uncertainty"
    );
}

/// The counterpart to [`sdk_choose_hiding_dependency_marks_package_uncertain`]:
/// when every reached `When` gate evaluates cleanly, the Choose is decided
/// exactly (docs/completed/sdk-chain-exactness-plan.md Stage A), the chosen branch's
/// dependency items are captured, and the set stays *certain*.
#[test]
fn sdk_choose_with_clean_gates_keeps_package_set_certain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup><UseAspNet>true</UseAspNet></PropertyGroup>
  <Choose>
    <When Condition="'$(UseAspNet)' == 'true'">
      <ItemGroup>
        <FrameworkReference Include="Microsoft.AspNetCore.App" />
      </ItemGroup>
    </When>
    <Otherwise>
      <ItemGroup>
        <FrameworkReference Include="Wrong.Framework" />
      </ItemGroup>
    </Otherwise>
  </Choose>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
    assert_eq!(
        result
            .framework_references
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["Microsoft.AspNetCore.App"],
        "diags: {:?}",
        result.diagnostics
    );
    assert!(
        !result.package_references_uncertain,
        "a cleanly-decided SDK Choose leaves the dependency set certain; causes: {:?}",
        result.package_reference_uncertainties
    );
}

/// An SDK import skipped because its condition is outside our supported subset
/// could have carried dependency items in the imported file. That skip is
/// package-affecting even though SDK Compile machinery stays tolerated.
#[test]
fn sdk_import_condition_uncertainty_marks_package_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <Import Project="ImplicitDeps.props" Condition="'@(_Unmodelled)' == 'x'" />
</Project>"#,
        "<Project/>",
        &[(
            "ImplicitDeps.props",
            r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.AspNetCore.App" />
  </ItemGroup>
</Project>"#,
        )],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "an unevaluable SDK import condition can hide dependency items"
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                (&cause.kind, &cause.origin),
                (
                    PackageReferenceUncertaintyCauseKind::Diagnostic(
                        DiagnosticKind::UnsupportedCondition { .. }
                    ),
                    DiagnosticOrigin::Imported
                )
            )),
        "expected imported unsupported-condition package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
    assert!(
        !result.items_uncertain,
        "SDK import-condition package uncertainty must not reintroduce Compile uncertainty"
    );
}

/// An SDK import whose `Project` path cannot be reduced is another structural
/// skip: the skipped file may have contained implicit dependency items. The
/// path leans on `TargetFramework`, which is carved out of undefined-read
/// exactness, so the expansion stays unresolvable.
#[test]
fn sdk_import_unresolved_project_path_marks_package_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <Import Project="$(TargetFramework).props" />
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "an unresolved SDK import path can hide dependency items"
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                (&cause.kind, &cause.origin),
                (
                    PackageReferenceUncertaintyCauseKind::Structural(
                        StructuralPackageReferenceUncertainty::ImportProjectUnresolved { project }
                    ),
                    DiagnosticOrigin::Imported
                ) if project == "$(TargetFramework).props"
            )),
        "expected imported unresolved-import structural package uncertainty, got: {:?}",
        result.package_reference_uncertainties
    );
    assert!(
        !result.items_uncertain,
        "SDK unresolved-import package uncertainty must not reintroduce Compile uncertainty"
    );
}

/// A *resolvable* SDK import path is still package-uncertain if the property
/// that selected it was written under a gate we could not pin down: a real
/// build may supply the missing input and choose a different props file
/// containing dependency items, so the unpinned path is dropped structurally.
#[test]
fn sdk_tainted_import_project_path_marks_package_uncertain() {
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk_with_extras(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <DepsProps>EmptyDeps.props</DepsProps>
  </PropertyGroup>
  <Import Project="$(DepsProps)" />
</Project>"#,
        "<Project/>",
        &[
            ("EmptyDeps.props", "<Project/>"),
            (
                "ImplicitDeps.props",
                r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.AspNetCore.App" />
  </ItemGroup>
</Project>"#,
            ),
        ],
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
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
        result.package_references_uncertain,
        "an SDK-tainted import path can choose the wrong dependency props file; causes: {:?}; diags: {:?}",
        result.package_reference_uncertainties, result.diagnostics
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                (&cause.kind, &cause.origin),
                (
                    PackageReferenceUncertaintyCauseKind::Structural(
                        StructuralPackageReferenceUncertainty::ImportProjectUnresolved { project }
                    ),
                    DiagnosticOrigin::Imported
                ) if project == "$(DepsProps)"
            )),
        "expected the unpinned import path to be dropped structurally, got: {:?}",
        result.package_reference_uncertainties
    );
    assert!(
        !result.items_uncertain,
        "SDK-tainted import-path package uncertainty must not reintroduce Compile uncertainty"
    );
}

/// A bare project with no SDK involvement and only fully-resolved explicit
/// references is the *one* case we can call certain — the contrast that keeps
/// the flag meaningful.
#[test]
fn sdkless_explicit_references_are_certain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |_name| Err(SdkResolveError::NotFound));
    assert!(
        !result.package_references_uncertain,
        "a bare project with resolved explicit refs is certain; diags: {:?}",
        result.diagnostics
    );
    assert_eq!(result.package_references.len(), 1);
}

/// A `Directory.Packages.props` up-tree is the CPM trigger — it holds the
/// central versions we don't yet fold in (slice 4b). Its mere presence must
/// flag the set, even though the with-imports walk doesn't follow it and the
/// project body itself looks fully-versioned.
#[test]
fn directory_packages_props_up_tree_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Packages.props",
        r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "a project under a Directory.Packages.props is CPM → uncertain; diags: {:?}",
        result.diagnostics
    );
    let expected_cpm_props = canon(&tmp.path().join("Directory.Packages.props"));
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                PackageReferenceUncertaintyCauseKind::DirectoryPackagesProps { path }
                    if path == &expected_cpm_props
            ))
    );
}

/// An `<ItemGroup>` gated on an unevaluable condition that contains only CPM
/// items must still be treated as package-affecting: skipping it silently
/// would drop central versions / global refs while reporting the set trusted.
#[test]
fn conditioned_group_of_only_cpm_items_marks_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup Condition="'@(_Unmodelled)' == 'x'">
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.package_references_uncertain,
        "an unevaluable condition on a CPM-item group → uncertain; diags: {:?}",
        result.diagnostics
    );
}
