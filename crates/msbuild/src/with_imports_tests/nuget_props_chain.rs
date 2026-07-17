//! The real `Directory.Packages.props` import point, exercised through a
//! canonical-layout fake SDK whose `Sdk.props` imports a (trimmed)
//! `Microsoft.Common.props`, which imports a **verbatim** copy of the .NET
//! SDK's `NuGet.props`. Nothing in the walker special-cases these files:
//! the chain lights up because the walker seeds the toolset properties
//! (`MSBuildToolsPath` etc.) at SDK resolution and evaluates the two
//! property functions `NuGet.props` uses. The real-SDK differential
//! fixtures in `tests/fsproj_packageref_diff.rs` pin the same behaviour
//! against `dotnet msbuild`.

use super::*;
use tempfile::TempDir;

/// `Sdk.props` line 49 of the real `Microsoft.NET.Sdk`, minus the
/// `AlternateCommonProps` escape hatch.
const FAKE_SDK_PROPS: &str = r#"<Project>
  <Import Project="$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\Microsoft.Common.props" />
</Project>"#;

/// The `Directory.Build.props` and `NuGetPropsFile` fragments of the real
/// `Microsoft.Common.props` (10.0.300 lines 19, 26-34 and 122-127), minus
/// the `CustomBefore/After` extension imports and the
/// Visual-Studio-layout arm. Keeping the Directory.Build block matters:
/// MSBuild imports `Directory.Build.props` *here*, before the NuGet
/// block, so gates it sets (`ImportDirectoryPackagesProps`,
/// `DirectoryPackagesPropsPath`) are visible to `NuGet.props`.
const FAKE_COMMON_PROPS: &str = r#"<Project>
  <PropertyGroup>
    <ImportDirectoryBuildProps Condition="'$(ImportDirectoryBuildProps)' == ''">true</ImportDirectoryBuildProps>
  </PropertyGroup>
  <PropertyGroup Condition="'$(ImportDirectoryBuildProps)' == 'true' and '$(DirectoryBuildPropsPath)' == ''">
    <_DirectoryBuildPropsFile Condition="'$(_DirectoryBuildPropsFile)' == ''">Directory.Build.props</_DirectoryBuildPropsFile>
    <_DirectoryBuildPropsBasePath Condition="'$(_DirectoryBuildPropsBasePath)' == ''">$([MSBuild]::GetDirectoryNameOfFileAbove($(MSBuildProjectDirectory), '$(_DirectoryBuildPropsFile)'))</_DirectoryBuildPropsBasePath>
    <DirectoryBuildPropsPath Condition="'$(_DirectoryBuildPropsBasePath)' != '' and '$(_DirectoryBuildPropsFile)' != ''">$([System.IO.Path]::Combine('$(_DirectoryBuildPropsBasePath)', '$(_DirectoryBuildPropsFile)'))</DirectoryBuildPropsPath>
  </PropertyGroup>
  <Import Project="$(DirectoryBuildPropsPath)" Condition="'$(ImportDirectoryBuildProps)' == 'true' and exists('$(DirectoryBuildPropsPath)')"/>
  <PropertyGroup>
    <NuGetPropsFile Condition="'$(NuGetPropsFile)'==''">$(MSBuildToolsPath)\NuGet.props</NuGetPropsFile>
  </PropertyGroup>
  <Import Condition="Exists('$(NuGetPropsFile)')" Project="$(NuGetPropsFile)" />
</Project>"#;

/// Verbatim body of the .NET SDK's `NuGet.props` (10.0.300), comment
/// header elided. This is the file the whole feature exists to evaluate
/// faithfully — do not "simplify" it.
const REAL_NUGET_PROPS: &str = r#"<Project>

  <PropertyGroup>
    <ImportDirectoryPackagesProps Condition="'$(ImportDirectoryPackagesProps)' == ''">true</ImportDirectoryPackagesProps>
  </PropertyGroup>

  <PropertyGroup Condition="'$(ImportDirectoryPackagesProps)' == 'true' and '$(DirectoryPackagesPropsPath)' == ''">
    <_DirectoryPackagesPropsFile Condition="'$(_DirectoryPackagesPropsFile)' == ''">Directory.Packages.props</_DirectoryPackagesPropsFile>
    <_DirectoryPackagesPropsBasePath Condition="'$(_DirectoryPackagesPropsBasePath)' == ''">$([MSBuild]::GetDirectoryNameOfFileAbove('$(MSBuildProjectDirectory)', '$(_DirectoryPackagesPropsFile)'))</_DirectoryPackagesPropsBasePath>
    <DirectoryPackagesPropsPath Condition="'$(_DirectoryPackagesPropsBasePath)' != '' and '$(_DirectoryPackagesPropsFile)' != ''">$([MSBuild]::NormalizePath('$(_DirectoryPackagesPropsBasePath)', '$(_DirectoryPackagesPropsFile)'))</DirectoryPackagesPropsPath>
  </PropertyGroup>

  <Import Project="$(DirectoryPackagesPropsPath)" Condition="'$(ImportDirectoryPackagesProps)' == 'true' and '$(DirectoryPackagesPropsPath)' != '' and Exists('$(DirectoryPackagesPropsPath)')"/>

  <PropertyGroup Condition="'$(ImportDirectoryPackagesProps)' == 'true' and '$(DirectoryPackagesPropsPath)' != '' and Exists('$(DirectoryPackagesPropsPath)')">
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
  </PropertyGroup>

</Project>"#;

/// Materialise `sdk/9.9.9/{Current/Microsoft.Common.props, NuGet.props,
/// Sdks/Fake.Sdk/Sdk/Sdk.{props,targets}}` under `tmp` and return the
/// resolver-visible paths. The layout matches the canonical .NET SDK
/// shape that [`super::super::evaluator`] recognises when seeding the
/// toolset properties.
fn write_canonical_sdk(tmp: &Path) -> SdkPaths {
    write_canonical_sdk_at(tmp, "9.9.9")
}

/// [`write_canonical_sdk`] at a caller-chosen SDK version. The version
/// directory names the *toolset*, and one toolset behaviour we model is
/// version-specific (whether an environment-supplied `MSBuildExtensionsPath`
/// survives — see [`super::super::evaluator::toolset_honours_env_extensions_path`]),
/// so those tests need to choose it.
fn write_canonical_sdk_at(tmp: &Path, version: &str) -> SdkPaths {
    let version_dir = tmp.join("dotnet/sdk").join(version);
    write_at(
        &version_dir,
        "Current/Microsoft.Common.props",
        FAKE_COMMON_PROPS,
    );
    write_at(&version_dir, "NuGet.props", REAL_NUGET_PROPS);
    let root = version_dir.join("Sdks/Fake.Sdk/Sdk");
    let props = write_at(&root, "Sdk.props", FAKE_SDK_PROPS);
    let targets = write_at(&root, "Sdk.targets", "<Project/>");
    SdkPaths {
        root,
        props,
        targets,
    }
}

/// A decoy toolset tree holding its own `Current/Microsoft.Common.props`,
/// which sets a marker the SDK's copy never sets. `Sdk.props` imports
/// `$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\Microsoft.Common.props`,
/// so an evaluation that lets an environment-supplied `MSBuildExtensionsPath`
/// stand imports *this* file, and one that overwrites it with the toolset's
/// own directory never reaches it. That makes `$(CommonPropsFrom)` a direct
/// read-out of which chain the walk actually followed.
fn write_decoy_extensions_path(tmp: &Path) -> PathBuf {
    let dir = tmp.join("decoy");
    write_at(
        &dir,
        "Current/Microsoft.Common.props",
        r#"<Project>
  <PropertyGroup>
    <CommonPropsFrom>decoy</CommonPropsFrom>
  </PropertyGroup>
</Project>"#,
    );
    canon(&dir)
}

/// Probes both the property and the import it steers.
const EXTENSIONS_PATH_PROBE: &str = r#"<Project Sdk="Fake.Sdk">
  <PropertyGroup>
    <ProbeExtensionsPath>$(MSBuildExtensionsPath)</ProbeExtensionsPath>
    <ProbeCommonPropsFrom>$(CommonPropsFrom)</ProbeCommonPropsFrom>
  </PropertyGroup>
</Project>"#;

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn canonical_sdk_resolver(
    paths: SdkPaths,
) -> impl Fn(&str) -> Result<SdkResolution, SdkResolveError> {
    move |name: &str| {
        if name == "Fake.Sdk" {
            Ok(SdkResolution::Single(paths.clone()))
        } else {
            Err(SdkResolveError::NotFound)
        }
    }
}

fn has_directory_packages_props_cause(result: &ParsedProject) -> bool {
    result.package_reference_uncertainties.iter().any(|cause| {
        matches!(
            cause.kind,
            PackageReferenceUncertaintyCauseKind::DirectoryPackagesProps { .. }
        )
    })
}

#[test]
fn central_versions_flow_through_canonical_sdk_chain() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    write_at(
        tmp.path(),
        "repo/Directory.Packages.props",
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
        "repo/src/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));

    // The central file was genuinely imported: its PackageVersion items
    // are captured and the versionless reference received its effective
    // version through the ordinary inline-CPM application.
    assert_eq!(
        result
            .package_versions
            .iter()
            .map(|pv| (pv.id.as_str(), pv.version.as_deref()))
            .collect::<Vec<_>>(),
        vec![("Newtonsoft.Json", Some("13.0.1"))],
        "diags: {:?}",
        result.diagnostics
    );
    assert_eq!(result.package_references.len(), 1);
    assert_eq!(
        result.package_references[0].version.as_deref(),
        Some("13.0.1"),
        "uncertainties: {:?}",
        result.package_reference_uncertainties
    );
    // The blanket "detected but not folded in" cause is discharged —
    // whatever uncertainty remains must be more precise than that.
    assert!(
        !has_directory_packages_props_cause(&result),
        "causes: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn missing_directory_build_props_leaves_no_rediscovery_noise() {
    // The overwhelmingly common shape: no Directory.Build.props anywhere.
    // Microsoft.Common.props' discovery group then leaves
    // `DirectoryBuildPropsPath` unset and its `<Import
    // Project="$(DirectoryBuildPropsPath)" Condition="…exists(…)">` is a
    // clean skip in MSBuild — the walker must not manufacture an
    // UndefinedProperty diagnostic (and package uncertainty) out of it.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "repo/src/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name }
                if name.eq_ignore_ascii_case("DirectoryBuildPropsPath")
                    || name.eq_ignore_ascii_case("DirectoryBuildTargetsPath")
        )),
        "diags: {:?}",
        result.diagnostics
    );
}

#[test]
fn import_gate_off_in_directory_build_props_leaves_conservative_uncertainty() {
    // `Directory.Build.props` runs before `NuGet.props`, so a repo-level
    // `ImportDirectoryPackagesProps=false` must keep the central file
    // unimported. The detected-but-not-walked file then keeps its
    // conservative cause (a real refinement would recognise the clean
    // opt-out as certain; this slice does not).
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    write_at(
        tmp.path(),
        "repo/Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <ImportDirectoryPackagesProps>false</ImportDirectoryPackagesProps>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "repo/Directory.Packages.props",
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
        "repo/src/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    assert!(
        result.package_versions.is_empty(),
        "the gated-off central file must not contribute items; got {:?}",
        result.package_versions
    );
    assert_eq!(result.package_references[0].version, None);
    assert!(result.package_references_uncertain);
    assert!(has_directory_packages_props_cause(&result));
}

#[test]
fn custom_absolute_directory_packages_props_path_is_honoured() {
    // A repo that points `DirectoryPackagesPropsPath` at a custom file
    // (from `Directory.Build.props`, before `NuGet.props` runs) gets
    // *that* file imported — the nearest-ancestor discovery group is
    // skipped, exactly as the real `NuGet.props` conditions say.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let custom = write_at(
        tmp.path(),
        "elsewhere/Central.props",
        r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="9.9.9" />
  </ItemGroup>
</Project>"#,
    );
    let custom = canon(&custom);
    write_at(
        tmp.path(),
        "repo/Directory.Build.props",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <DirectoryPackagesPropsPath>{}</DirectoryPackagesPropsPath>
  </PropertyGroup>
</Project>"#,
            custom.display()
        ),
    );
    // A nearest-ancestor file that must NOT be imported (the override
    // wins). Its central version differs so a mix-up is visible.
    write_at(
        tmp.path(),
        "repo/Directory.Packages.props",
        r#"<Project>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "repo/src/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    assert_eq!(
        result
            .package_versions
            .iter()
            .map(|pv| (pv.id.as_str(), pv.version.as_deref()))
            .collect::<Vec<_>>(),
        vec![("Newtonsoft.Json", Some("9.9.9"))],
        "diags: {:?}",
        result.diagnostics
    );
    assert_eq!(
        result.package_references[0].version.as_deref(),
        Some("9.9.9")
    );
    // The un-imported nearest-ancestor file is not part of the real
    // build either (the override replaced it), and NuGet's own
    // central-file marker proves a central file WAS imported — so the
    // blanket cause must discharge; keeping it would make this exact
    // dependency set read as untrusted forever.
    assert!(
        !has_directory_packages_props_cause(&result),
        "causes: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn toolset_properties_are_seeded_for_canonical_sdk_layout() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let version_dir = canon(&tmp.path().join("dotnet/sdk/9.9.9"));
    let project_path = write_at(
        tmp.path(),
        "repo/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <PropertyGroup>
    <ProbeToolsVersion>$(MSBuildToolsVersion)</ProbeToolsVersion>
    <ProbeToolsPath>$(MSBuildToolsPath)</ProbeToolsPath>
    <ProbeBinPath>$(MSBuildBinPath)</ProbeBinPath>
    <ProbeExtensionsPath>$(MSBuildExtensionsPath)</ProbeExtensionsPath>
    <ProbeSdksPath>$(MSBuildSDKsPath)</ProbeSdksPath>
    <ProbeRuntimeType>$(MSBuildRuntimeType)</ProbeRuntimeType>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    let get = |name: &str| result.properties.get(name).map(String::as_str);
    assert_eq!(get("ProbeToolsVersion"), Some("Current"));
    assert_eq!(get("ProbeToolsPath"), Some(&*version_dir.to_string_lossy()));
    assert_eq!(get("ProbeBinPath"), Some(&*version_dir.to_string_lossy()));
    // MSBuildExtensionsPath carries a trailing separator in real MSBuild
    // (verified against dotnet 10.0.300); faithful concatenation like
    // `$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\…` depends on it.
    assert_eq!(
        get("ProbeExtensionsPath"),
        Some(&*format!("{}/", version_dir.display()))
    );
    assert_eq!(
        get("ProbeSdksPath"),
        Some(&*version_dir.join("Sdks").to_string_lossy())
    );
    assert_eq!(get("ProbeRuntimeType"), Some("Core"));
}

/// MSBuild ≤ 17 (SDK ≤ 9) promotes an environment `MSBuildExtensionsPath` and
/// then *overwrites* it with the toolset's own directory before evaluating the
/// project — probed one SDK at a time against `dotnet msbuild`:
/// `MSBuildExtensionsPath=/SPOOF dotnet msbuild -getProperty:MSBuildExtensionsPath`
/// reads the SDK version directory under 8.0.420 (MSBuild 17.11) and 9.0.315,
/// and `/SPOOF` under 10.0.301 (MSBuild 18.6). So the environment value must
/// lose here, and the SDK's own `Microsoft.Common.props` — not the decoy the
/// environment points at — must be the one imported.
#[test]
fn env_extensions_path_loses_to_a_pre_msbuild_18_toolset() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk_at(tmp.path(), "9.0.315");
    let version_dir = canon(&tmp.path().join("dotnet/sdk/9.0.315"));
    let decoy = write_decoy_extensions_path(tmp.path());
    let project_path = write_at(tmp.path(), "repo/Demo.fsproj", EXTENSIONS_PATH_PROBE);

    let result = parse_file_with_sdk_env(
        &project_path,
        canonical_sdk_resolver(sdk),
        HashMap::new(),
        env_of(&[("MSBuildExtensionsPath", &format!("{}/", decoy.display()))]),
    );

    let get = |name: &str| result.properties.get(name).map(String::as_str);
    assert_eq!(
        get("ProbeExtensionsPath"),
        Some(&*format!("{}/", version_dir.display())),
        "the toolset overwrites the environment value on MSBuild 17"
    );
    assert_eq!(
        get("ProbeCommonPropsFrom"),
        Some(""),
        "the decoy Microsoft.Common.props must never be imported: {:?}",
        result.diagnostics
    );
}

/// The other side of the same probe: MSBuild 18 (SDK 10) leaves the
/// environment value standing, and `Sdk.props` then imports
/// `Microsoft.Common.props` from *there*. Committing the toolset directory
/// here would be just as wrong as committing the environment value above.
#[test]
fn env_extensions_path_survives_an_msbuild_18_toolset() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk_at(tmp.path(), "10.0.301");
    let decoy = write_decoy_extensions_path(tmp.path());
    let project_path = write_at(tmp.path(), "repo/Demo.fsproj", EXTENSIONS_PATH_PROBE);

    let result = parse_file_with_sdk_env(
        &project_path,
        canonical_sdk_resolver(sdk),
        HashMap::new(),
        env_of(&[("MSBuildExtensionsPath", &format!("{}/", decoy.display()))]),
    );

    let get = |name: &str| result.properties.get(name).map(String::as_str);
    assert_eq!(
        get("ProbeExtensionsPath"),
        Some(&*format!("{}/", decoy.display()))
    );
    assert_eq!(
        get("ProbeCommonPropsFrom"),
        Some("decoy"),
        "the environment-supplied path must steer the Sdk.props import: {:?}",
        result.diagnostics
    );
}

/// A caller global wins on *every* toolset — probed against 8.0.420, where
/// `-p:MSBuildExtensionsPath=/SPOOF` redirects the `Sdk.props` import even
/// though an environment variable of the same name would have been
/// overwritten. So the version-specific rule must key on the *environment* as
/// the value's source, not on the name.
#[test]
fn global_extensions_path_beats_the_environment_on_a_pre_msbuild_18_toolset() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk_at(tmp.path(), "8.0.420");
    let decoy = write_decoy_extensions_path(tmp.path());
    let project_path = write_at(tmp.path(), "repo/Demo.fsproj", EXTENSIONS_PATH_PROBE);

    let result = parse_file_with_sdk_env(
        &project_path,
        canonical_sdk_resolver(sdk),
        env_of(&[("MSBuildExtensionsPath", &format!("{}/", decoy.display()))]),
        env_of(&[("MSBuildExtensionsPath", "/ignored/env")]),
    );

    let get = |name: &str| result.properties.get(name).map(String::as_str);
    assert_eq!(
        get("ProbeExtensionsPath"),
        Some(&*format!("{}/", decoy.display()))
    );
    assert_eq!(get("ProbeCommonPropsFrom"), Some("decoy"));
}

/// An SDK whose version directory we cannot read a major version from tells us
/// nothing about which of the two toolset behaviours applies. Committing
/// either one would be a guess, so the property is left *undefined* and the
/// read declines — the environment value is not silently promoted.
#[test]
fn env_extensions_path_declines_when_the_toolset_version_is_unreadable() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk_at(tmp.path(), "banana");
    let decoy = write_decoy_extensions_path(tmp.path());
    let project_path = write_at(tmp.path(), "repo/Demo.fsproj", EXTENSIONS_PATH_PROBE);

    let result = parse_file_with_sdk_env(
        &project_path,
        canonical_sdk_resolver(sdk),
        HashMap::new(),
        env_of(&[("MSBuildExtensionsPath", &format!("{}/", decoy.display()))]),
    );

    assert_eq!(
        result
            .properties
            .get("ProbeExtensionsPath")
            .map(String::as_str),
        Some("")
    );
    assert!(
        result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name }
                if name.eq_ignore_ascii_case("MSBuildExtensionsPath")
        )),
        "the read must decline: {:?}",
        result.diagnostics
    );
}

/// With no environment value in play there is nothing version-specific to
/// decide: every toolset computes `MSBuildExtensionsPath` from its own
/// directory, so an MSBuild-18 SDK seeds it exactly like the 9.x one that
/// `toolset_properties_are_seeded_for_canonical_sdk_layout` pins.
#[test]
fn toolset_properties_are_seeded_for_an_msbuild_18_sdk() {
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk_at(tmp.path(), "10.0.301");
    let version_dir = canon(&tmp.path().join("dotnet/sdk/10.0.301"));
    let project_path = write_at(tmp.path(), "repo/Demo.fsproj", EXTENSIONS_PATH_PROBE);

    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));

    assert_eq!(
        result
            .properties
            .get("ProbeExtensionsPath")
            .map(String::as_str),
        Some(&*format!("{}/", version_dir.display()))
    );
}

#[test]
fn toolset_properties_are_not_seeded_for_flat_sdk_layout() {
    // A custom resolver returning a self-contained root (no canonical
    // `sdk/<version>/Sdks/<name>/Sdk` shape) tells us nothing about
    // where a real toolset would live — seeding would be a guess.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "Flat.Sdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Flat.Sdk">
  <PropertyGroup>
    <ProbeToolsVersion>$(MSBuildToolsVersion)</ProbeToolsVersion>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, move |name| {
        if name == "Flat.Sdk" {
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
            .properties
            .get("ProbeToolsVersion")
            .map(String::as_str),
        Some("")
    );
}

#[test]
fn seeded_reserved_toolset_property_ignores_project_write() {
    // MSBuild raises MSB4004 for writes to reserved names; this walker's
    // established model is to drop the write silently. Either way the
    // seeded value must survive.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "repo/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <PropertyGroup>
    <MSBuildToolsVersion>hijacked</MSBuildToolsVersion>
    <ProbeToolsVersion>$(MSBuildToolsVersion)</ProbeToolsVersion>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    assert_eq!(
        result
            .properties
            .get("ProbeToolsVersion")
            .map(String::as_str),
        Some("Current")
    );
}

#[test]
fn seeded_overridable_toolset_property_respects_project_write() {
    // `MSBuildExtensionsPath` / `MSBuildSDKsPath` are well-known but NOT
    // reserved (verified against dotnet 10.0.300: a project write is
    // accepted). The seed must not shadow a later legitimate write.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "repo/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <PropertyGroup>
    <MSBuildSDKsPath>/custom/sdks</MSBuildSDKsPath>
    <ProbeSdksPath>$(MSBuildSDKsPath)</ProbeSdksPath>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    assert_eq!(
        result.properties.get("ProbeSdksPath").map(String::as_str),
        Some("/custom/sdks")
    );
}

#[test]
fn global_toolset_property_is_not_clobbered_by_seeding() {
    // A caller-supplied global (e.g. `-p:MSBuildSDKsPath=…`) wins over
    // the seed, exactly like every other global-vs-default interaction.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "repo/Demo.fsproj",
        r#"<Project Sdk="Fake.Sdk">
  <PropertyGroup>
    <ProbeSdksPath>$(MSBuildSDKsPath)</ProbeSdksPath>
  </PropertyGroup>
</Project>"#,
    );
    let canon_project = canon(&project_path);
    let source = std::fs::read_to_string(&project_path).unwrap();
    let mut extras = HashMap::new();
    extras.insert("MSBuildSDKsPath".to_string(), "/global/sdks".to_string());
    let resolver = canonical_sdk_resolver(sdk);
    let result = parse_fsproj_with_imports(
        &source,
        &canon_project,
        &extras,
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .expect("well-formed XML parses");
    assert_eq!(
        result.properties.get("ProbeSdksPath").map(String::as_str),
        Some("/global/sdks")
    );
}

#[test]
fn hand_set_marker_does_not_discharge_unwalked_central_props() {
    // A project can write `CentralPackageVersionsFileImported=true`
    // itself — that must not launder the conservative cause: a real
    // build's NuGet.props would still import the detected central file
    // (the import is gated on `ImportDirectoryPackagesProps` and the
    // path, not the marker), so its contents are genuinely missing from
    // a walk that never reached it.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Packages.props",
        r#"<Project>
  <ItemGroup>
    <GlobalPackageReference Include="MinVer" Version="4.3.0" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    // No SDK resolver: the chain cannot fire, so the central file is
    // never walked.
    let result = parse_file(&project_path);
    assert!(result.package_references_uncertain);
    assert!(
        has_directory_packages_props_cause(&result),
        "causes: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn pointing_the_path_at_an_incidentally_walked_file_does_not_discharge() {
    // `DirectoryPackagesPropsPath=$(MSBuildProjectFullPath)` names a file
    // that is in the walk for an entirely different reason (it IS the
    // entry project). Without a resolver the NuGet.props chain never
    // ran, so the detected central file was never imported — the cause
    // must survive any amount of final-property-state manipulation.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Packages.props",
        r#"<Project>
  <ItemGroup>
    <GlobalPackageReference Include="MinVer" Version="4.3.0" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <DirectoryPackagesPropsPath>$(MSBuildProjectFullPath)</DirectoryPackagesPropsPath>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        has_directory_packages_props_cause(&result),
        "causes: {:?}",
        result.package_reference_uncertainties
    );
}

#[test]
fn late_sdk_seeding_overrides_prior_project_write_to_reserved_toolset_property() {
    // A body write to a reserved toolset name *before* the first
    // canonical SDK resolves (SDK-less entry, explicit body `<Import
    // Sdk=…>` after a PropertyGroup) would be rejected outright by real
    // MSBuild — the name is reserved from process start. The late seed
    // must therefore replace the value, not defer to it: otherwise the
    // hijacked value redirects `Sdk.props`' Microsoft.Common.props
    // import and silently breaks the NuGet.props chain.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    let version_dir = canon(&tmp.path().join("dotnet/sdk/9.9.9"));
    let project_path = write_at(
        tmp.path(),
        "repo/Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <MSBuildToolsVersion>hijacked</MSBuildToolsVersion>
    <MSBuildToolsPath>/hijacked</MSBuildToolsPath>
  </PropertyGroup>
  <Import Sdk="Fake.Sdk" Project="Sdk.props" />
  <PropertyGroup>
    <ProbeToolsVersion>$(MSBuildToolsVersion)</ProbeToolsVersion>
    <ProbeToolsPath>$(MSBuildToolsPath)</ProbeToolsPath>
    <ProbeNuGetPropsFile>$(NuGetPropsFile)</ProbeNuGetPropsFile>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    let get = |name: &str| result.properties.get(name).map(String::as_str);
    assert_eq!(get("ProbeToolsVersion"), Some("Current"));
    assert_eq!(get("ProbeToolsPath"), Some(&*version_dir.to_string_lossy()));
    // The chain past Microsoft.Common.props stays intact.
    assert_eq!(
        get("ProbeNuGetPropsFile"),
        Some(&*format!("{}\\NuGet.props", version_dir.display()))
    );
}

#[test]
fn body_reached_nested_sdk_honours_directory_build_props_cpm_gate() {
    // The deferred (pass-2) shape: an SDK-less entry whose *body*
    // reaches the SDK. MSBuild still imports Directory.Build.props from
    // inside that SDK's Microsoft.Common.props — before NuGet.props —
    // so a repo-level `ImportDirectoryPackagesProps=false` must gate the
    // central import in this shape too, not just for an entry-level Sdk
    // attribute.
    let tmp = TempDir::new().unwrap();
    let sdk = write_canonical_sdk(tmp.path());
    write_at(
        tmp.path(),
        "repo/Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <ImportDirectoryPackagesProps>false</ImportDirectoryPackagesProps>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "repo/Directory.Packages.props",
        r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    // The nested file's *root* carries the Sdk attribute; reaching it
    // through a body `<Import>` is the shape the deferred second pass
    // exists for (no promotion applies — the import is not an
    // `<Import Sdk=…>` first element).
    write_at(
        tmp.path(),
        "repo/src/nested.proj",
        r#"<Project Sdk="Fake.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "repo/src/Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <BeforeSdk>yes</BeforeSdk>
  </PropertyGroup>
  <Import Project="nested.proj" />
</Project>"#,
    );
    let result = parse_file_with_sdk_resolution(&project_path, canonical_sdk_resolver(sdk));
    assert!(
        result.package_versions.is_empty(),
        "the gated-off central file must not contribute items; got {:?}",
        result.package_versions
    );
    assert_eq!(result.package_references[0].version, None);
    assert!(has_directory_packages_props_cause(&result));
}
