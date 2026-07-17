//! Multi-root (locator-style) SDK resolution through the walker:
//! `<Import Project="P" Sdk="S"/>` where the resolver returns
//! [`SdkResolution::Roots`] imports `P` against every root in order,
//! zero roots is an exact no-op, and the `<Project Sdk="…">` shorthand
//! cannot be backed by a locator (docs/completed/sdk-chain-exactness-plan.md,
//! Stage B / D1).

use super::*;
use crate::StructuralPackageReferenceUncertainty;
use tempfile::TempDir;

fn roots_resolver(
    locator_name: &'static str,
    roots: Vec<PathBuf>,
) -> impl Fn(&str) -> Result<SdkResolution, SdkResolveError> {
    move |name: &str| {
        if name == locator_name {
            Ok(SdkResolution::Roots(roots.clone()))
        } else {
            Err(SdkResolveError::NotFound)
        }
    }
}

/// A multi-root resolution walks `Project` against every root, in
/// order, all sharing one property table — the second root's write to
/// the overlapping name wins, the first root's non-overlapping write
/// survives.
#[test]
fn import_sdk_with_multiple_roots_imports_each_in_order() {
    let tmp = TempDir::new().unwrap();
    let r1 = tmp.path().join("m1");
    let r2 = tmp.path().join("m2");
    write_at(
        &r1,
        "W.targets",
        r#"<Project>
  <PropertyGroup><P>one</P><Q>one</Q></PropertyGroup>
</Project>"#,
    );
    write_at(
        &r2,
        "W.targets",
        r#"<Project>
  <PropertyGroup><P>two</P></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="W.targets" Sdk="My.Locator" />
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(
        &project_path,
        roots_resolver("My.Locator", vec![r1.clone(), r2.clone()]),
    );
    assert_eq!(result.properties.get("P").map(String::as_str), Some("two"));
    assert_eq!(result.properties.get("Q").map(String::as_str), Some("one"));
    assert!(
        result.diagnostics.is_empty(),
        "diags: {:?}",
        result.diagnostics
    );
    assert!(!result.package_references_uncertain);
}

/// Zero roots is MSBuild's empty workload-resolver result: the import
/// cleanly contributes nothing and every certainty axis is untouched.
#[test]
fn import_sdk_with_zero_roots_is_an_exact_noop() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="AutoImport.props" Sdk="My.Locator" />
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
  </ItemGroup>
</Project>"#,
    );
    let result =
        parse_file_with_sdk_resolution(&project_path, roots_resolver("My.Locator", Vec::new()));
    assert!(
        result.diagnostics.is_empty(),
        "diags: {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
    assert!(!result.package_references_uncertain);
    assert_eq!(result.package_references.len(), 1);
}

/// Dependency items inside locator-resolved files are captured through
/// the ordinary machinery, and the roots count as SDK machinery for the
/// Compile-tolerance set.
#[test]
fn locator_resolved_file_contributes_dependency_items() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("manifest");
    write_at(
        &manifest,
        "WorkloadManifest.targets",
        r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.Fake.Workload" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="WorkloadManifest.targets" Sdk="My.Locator" />
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(
        &project_path,
        roots_resolver("My.Locator", vec![manifest.clone()]),
    );
    assert_eq!(
        result
            .framework_references
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["Microsoft.Fake.Workload"],
        "diags: {:?}",
        result.diagnostics
    );
    assert!(!result.package_references_uncertain);
}

/// The `<Project Sdk="…">` shorthand needs the canonical entry points a
/// locator-style resolution doesn't have — degrade, don't invent them.
#[test]
fn project_sdk_attribute_resolving_to_roots_degrades() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="My.Locator">
  <ItemGroup><Compile Include="A.fs" /></ItemGroup>
</Project>"#,
    );
    let result =
        parse_file_with_sdk_resolution(&project_path, roots_resolver("My.Locator", Vec::new()));
    assert!(result.is_partial);
    assert!(result.items_uncertain);
    assert!(result.package_references_uncertain);
}

/// `UnsupportedLayout` surfaces as `SdkResolutionUnsupported` and marks
/// the dependency set uncertain — the skipped import may have carried
/// dependency items.
#[test]
fn unsupported_layout_marks_package_uncertain() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="WorkloadManifest.targets" Sdk="My.Locator" />
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, |name: &str| {
        if name == "My.Locator" {
            Err(SdkResolveError::UnsupportedLayout {
                reason: "workload set present".to_string(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(
        result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::SdkResolutionUnsupported { name, .. } if name == "My.Locator"
        )),
        "diags: {:?}",
        result.diagnostics
    );
    assert!(result.package_references_uncertain);
}

/// Path vetting applies to locator imports exactly as to custom
/// single-root SDK entry points: `..` cannot escape the roots.
#[test]
fn locator_import_project_path_is_vetted() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("manifest");
    write_at(&manifest, "WorkloadManifest.targets", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="../escape.targets" Sdk="My.Locator" />
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(
        &project_path,
        roots_resolver("My.Locator", vec![manifest.clone()]),
    );
    assert!(
        result
            .package_reference_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                crate::PackageReferenceUncertaintyCauseKind::Structural(
                    StructuralPackageReferenceUncertainty::SdkImportProjectRejected { .. }
                )
            )),
        "causes: {:?}",
        result.package_reference_uncertainties
    );
    assert!(result.is_partial);
}

/// A file-level symlink-merge layout (Nix's combined dotnet tree, but any
/// store-of-symlinks qualifies) makes a locator-resolved workload manifest's
/// reach path and its *canonical* path land under different roots: the merge
/// directory is real, but the `WorkloadManifest.targets` inside it is a symlink
/// back to the originating store. A canonical-only Compile-tolerance check then
/// scores this SDK-internal manifest as a user file, so its `<ImportGroup>`
/// condition on an undefined property flips `items_uncertain` and refuses the
/// project fold for every real SDK project (regression guarded end-to-end by
/// `crates/lsp/tests/all/sdk_project_fold_e2e.rs`; this reproduces it hermetically).
#[test]
#[cfg(unix)]
fn locator_manifest_symlinked_out_of_its_root_is_still_sdk_tolerated() {
    let tmp = TempDir::new().unwrap();
    // The real manifest lives in a separate "store" directory and gates a
    // `<Compile>` on an undefined property — the shape a real workload manifest
    // uses to gate platform-specific machinery.
    let store = tmp.path().join("store");
    let real_manifest = write_at(
        &store,
        "WorkloadManifest.targets",
        r#"<Project>
  <ItemGroup Condition="'$(TargetPlatformIdentifier)' == 'android'">
    <Compile Include="Android.fs" />
  </ItemGroup>
</Project>"#,
    );
    // The "merge" directory the locator points at is a real directory whose
    // `WorkloadManifest.targets` is a per-file symlink into the store — so the
    // file canonicalises out of the merge root while its own directory does not.
    let merge = tmp.path().join("merge");
    std::fs::create_dir_all(&merge).unwrap();
    std::os::unix::fs::symlink(&real_manifest, merge.join("WorkloadManifest.targets")).unwrap();

    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="WorkloadManifest.targets" Sdk="My.Locator" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(
        &project_path,
        roots_resolver("My.Locator", vec![merge.clone()]),
    );
    assert!(
        !result.items_uncertain,
        "a locator-resolved workload manifest reached through a symlink-merge root \
         is SDK machinery and must not flip items_uncertain; diags: {:?}",
        result.diagnostics
    );
    assert!(result.compile_condition_uncertainties.is_empty());
}
