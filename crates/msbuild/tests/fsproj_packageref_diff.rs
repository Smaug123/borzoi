//! Differential tests: our `<PackageReference>` / `<FrameworkReference>`
//! capture vs `dotnet msbuild -getItem:PackageReference,FrameworkReference`
//! for the same project. Ground-truth validation of the *capture semantics*
//! — the `Version`/`VersionOverride`/`*Assets` metadata (attribute or child
//! element), `Include="A;B"` splitting, `$(…)` expansion, and `Condition`
//! evaluation — against a real MSBuild evaluation.
//!
//! Scope: the always-on fixtures are bare `<Project>` files, so MSBuild
//! injects no implicit packages (e.g. FSharp.Core) and both sides see exactly
//! the project's own references. The ignored `sdk_style_*` fixtures are the
//! future oracle for SDK-injected package/framework references; they are
//! expected to fail until SDK dependency items are modelled directly.
//! `Update` items *are* diffed here: MSBuild's item view collapses Include +
//! matching `Update` (a prior `Include`'s metadata is overwritten per key by
//! each following `Update`, and a lone `Update` matching no prior `Include` is
//! dropped), and our capture now folds Updates the same way, so the effective
//! `PackageReference` set is compared directly.
//!
//! Each fixture spawns `dotnet msbuild`, so these run under `nix develop`
//! (offline SDK) like the sibling `fsproj_msbuild_diff` tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod common;

use borzoi_msbuild::{
    DiagnosticKind, GlobalPackageReference, PackageReference, PackageReferenceUncertaintyCauseKind,
    PackageVersion, parse_fsproj_with_imports, resolve_sdk, workloads,
};
use borzoi_oracle_harness::BoundedCommand;
use serde::Deserialize;

/// One MSBuild item as `-getItem` serialises it: `Identity` plus whatever
/// metadata is set (only present keys appear).
#[derive(Deserialize)]
struct MsbuildItem {
    #[serde(rename = "Identity")]
    identity: String,
    #[serde(flatten)]
    metadata: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Items {
    #[serde(rename = "PackageReference", default)]
    package_reference: Vec<MsbuildItem>,
    #[serde(rename = "PackageVersion", default)]
    package_version: Vec<MsbuildItem>,
    #[serde(rename = "GlobalPackageReference", default)]
    global_package_reference: Vec<MsbuildItem>,
    #[serde(rename = "FrameworkReference", default)]
    framework_reference: Vec<MsbuildItem>,
}

#[derive(Deserialize)]
struct GetItemOutput {
    #[serde(rename = "Items")]
    items: Items,
}

/// The metadata keys we model, in a stable order, for a package reference —
/// pulled from an MSBuild item's metadata map (absent → `None`).
fn msbuild_pkg_fields(item: &MsbuildItem) -> (String, Vec<(&'static str, Option<String>)>) {
    let g = |k: &str| item.metadata.get(k).filter(|v| !v.is_empty()).cloned();
    (
        item.identity.clone(),
        vec![
            ("Version", g("Version")),
            ("VersionOverride", g("VersionOverride")),
            ("IncludeAssets", g("IncludeAssets")),
            ("ExcludeAssets", g("ExcludeAssets")),
            ("PrivateAssets", g("PrivateAssets")),
        ],
    )
}

fn ours_pkg_fields(pr: &PackageReference) -> (String, Vec<(&'static str, Option<String>)>) {
    (
        pr.id.clone(),
        vec![
            ("Version", pr.version.clone()),
            ("VersionOverride", pr.version_override.clone()),
            ("IncludeAssets", pr.include_assets.clone()),
            ("ExcludeAssets", pr.exclude_assets.clone()),
            ("PrivateAssets", pr.private_assets.clone()),
        ],
    )
}

fn msbuild_package_version_fields(item: &MsbuildItem) -> (String, Option<String>) {
    (
        item.identity.clone(),
        item.metadata
            .get("Version")
            .filter(|v| !v.is_empty())
            .cloned(),
    )
}

fn ours_package_version_fields(pv: &PackageVersion) -> (String, Option<String>) {
    (pv.id.clone(), pv.version.clone())
}

fn msbuild_global_pkg_fields(item: &MsbuildItem) -> (String, Vec<(&'static str, Option<String>)>) {
    msbuild_pkg_fields(item)
}

fn ours_global_pkg_fields(
    pr: &GlobalPackageReference,
) -> (String, Vec<(&'static str, Option<String>)>) {
    (
        pr.id.clone(),
        vec![
            ("Version", pr.version.clone()),
            ("VersionOverride", pr.version_override.clone()),
            ("IncludeAssets", pr.include_assets.clone()),
            ("ExcludeAssets", pr.exclude_assets.clone()),
            ("PrivateAssets", pr.private_assets.clone()),
        ],
    )
}

/// Budget for one `dotnet msbuild` evaluation. A cold one restores packages and
/// walks the whole SDK import chain, which is legitimately minutes, so the bound
/// is far above the harness's per-request default: it is there to stop an
/// evaluation that has *stalled* — blocked on a NuGet lock held by a concurrent
/// run in a sibling worktree, say — from hanging the suite forever, not to police
/// a slow one.
const MSBUILD_TIMEOUT: Duration = Duration::from_secs(1800);

fn run_get_item(proj: &Path, extras: &[(&str, &str)]) -> GetItemOutput {
    run_get_item_set(proj, extras, "PackageReference,FrameworkReference")
}

fn run_get_item_set(proj: &Path, extras: &[(&str, &str)], item_set: &str) -> GetItemOutput {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    // Same environment-stripping rationale as fsproj_msbuild_diff::run_msbuild:
    // every inherited env var is an MSBuild initial property, so a stray
    // `Configuration=…` etc. would flip which items evaluate. Keep only what
    // dotnet needs to find its runtime and packages.
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
    cmd.args(["msbuild", "-nologo"]);
    cmd.arg(format!("-getItem:{item_set}"));
    for (k, v) in extras {
        cmd.arg(format!("-p:{k}={v}"));
    }
    cmd.arg(proj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", proj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("parse -getItem JSON for {}: {e}\n{stdout}", proj.display()))
}

/// Write `xml` to a temp `.proj`, evaluate it both ways, assert the
/// package/framework-reference sets agree field-for-field.
fn assert_captures_match(xml: &str, extras: &[(&str, &str)]) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let proj = dir.path().join("Diff.proj");
    std::fs::write(&proj, xml).expect("write project");

    let props = extras
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &props,
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));
    assert!(
        !parsed.package_references_uncertain,
        "fixture unexpectedly flagged uncertain: {xml}"
    );

    let msbuild = run_get_item(&proj, extras);

    let ours_pkg: Vec<_> = parsed
        .package_references
        .iter()
        .map(ours_pkg_fields)
        .collect();
    let theirs_pkg: Vec<_> = msbuild
        .items
        .package_reference
        .iter()
        .map(msbuild_pkg_fields)
        .collect();
    assert_eq!(
        ours_pkg, theirs_pkg,
        "PackageReference mismatch for:\n{xml}"
    );

    let ours_fw: Vec<&str> = parsed
        .framework_references
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let theirs_fw: Vec<&str> = msbuild
        .items
        .framework_reference
        .iter()
        .map(|i| i.identity.as_str())
        .collect();
    assert_eq!(
        ours_fw, theirs_fw,
        "FrameworkReference mismatch for:\n{xml}"
    );
}

/// Same differential check as `assert_captures_match`, but for CPM item types.
/// These items intentionally keep `package_references_uncertain=true` until
/// effective CPM application lands, so this oracle compares only the captured
/// item data.
fn assert_cpm_items_match(xml: &str, extras: &[(&str, &str)]) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let proj = dir.path().join("Diff.proj");
    std::fs::write(&proj, xml).expect("write project");

    let props = extras
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &props,
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));

    let msbuild = run_get_item_set(&proj, extras, "PackageVersion,GlobalPackageReference");

    let ours_versions: Vec<_> = parsed
        .package_versions
        .iter()
        .map(ours_package_version_fields)
        .collect();
    let theirs_versions: Vec<_> = msbuild
        .items
        .package_version
        .iter()
        .map(msbuild_package_version_fields)
        .collect();
    assert_eq!(
        ours_versions, theirs_versions,
        "PackageVersion mismatch for:\n{xml}"
    );

    let ours_global: Vec<_> = parsed
        .global_package_references
        .iter()
        .map(ours_global_pkg_fields)
        .collect();
    let theirs_global: Vec<_> = msbuild
        .items
        .global_package_reference
        .iter()
        .map(msbuild_global_pkg_fields)
        .collect();
    assert_eq!(
        ours_global, theirs_global,
        "GlobalPackageReference mismatch for:\n{xml}"
    );
}

/// SDK-style F# projects need a real SDK resolver on our side and a `.fsproj`
/// extension on both sides so the F# SDK targets import. These fixtures are
/// ignored today because the real SDK chain still trips concrete uncertainty
/// causes — chiefly hook-point imports gated on undefined properties
/// (`AlternateCommonProps`, `CustomBeforeDirectoryBuildProps`, …) — even
/// though clean SDK dependency items are now trusted; later slices should
/// enable them one by one as those remaining paths become exact.
fn assert_sdk_captures_match(xml: &str, extras: &[(&str, &str)]) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let proj = dir.path().join("Diff.fsproj");
    std::fs::write(&proj, xml).expect("write project");

    let props = extras
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let dotnet_root = dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        // The fixture tempdir has no global.json above it.
        global_json_pins_workload_set: false,
    };
    let resolver = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &props,
        &common::oracle_environment(),
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));
    assert!(
        !parsed.package_references_uncertain,
        "SDK fixture still flagged package-reference uncertainty: {xml}\ncauses: {:#?}",
        parsed.package_reference_uncertainties
    );

    let msbuild = run_get_item(&proj, extras);

    let ours_pkg: Vec<_> = parsed
        .package_references
        .iter()
        .map(ours_pkg_fields)
        .collect();
    let theirs_pkg: Vec<_> = msbuild
        .items
        .package_reference
        .iter()
        .map(msbuild_pkg_fields)
        .collect();
    assert_eq!(
        ours_pkg, theirs_pkg,
        "PackageReference mismatch for SDK fixture:\n{xml}"
    );

    let ours_fw: Vec<&str> = parsed
        .framework_references
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let theirs_fw: Vec<&str> = msbuild
        .items
        .framework_reference
        .iter()
        .map(|i| i.identity.as_str())
        .collect();
    assert_eq!(
        ours_fw, theirs_fw,
        "FrameworkReference mismatch for SDK fixture:\n{xml}"
    );
}

fn dotnet_root_from_env() -> PathBuf {
    std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("DOTNET_ROOT is not set; run under nix develop"))
}

/// The workload context of the test process — the same environment the
/// `dotnet msbuild` oracle child inherits, so both sides consult the
/// same user-local workload roots.
fn workload_env_from_process() -> (Option<PathBuf>, bool) {
    // Empty home-ish values count as unset (`string.IsNullOrEmpty`
    // in .NET's CliFolderPathCalculatorCore) — match what the oracle
    // child's dotnet host does.
    let non_empty = |var: &str| std::env::var_os(var).filter(|value| !value.is_empty());
    let user_dotnet_root = non_empty("DOTNET_CLI_HOME")
        .or_else(|| non_empty("HOME"))
        .map(|home| PathBuf::from(home).join(".dotnet"));
    // Per-variable effective-value semantics, mirroring the LSP's
    // `SdkDiscoveryEnv::from_process_env` (PACK_ROOTS goes through
    // IsNullOrEmpty upstream; the other two are presence checks).
    let overrides_present = std::env::var_os("DOTNETSDK_WORKLOAD_MANIFEST_ROOTS").is_some()
        || std::env::var_os("DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS").is_some()
        || non_empty("DOTNETSDK_WORKLOAD_PACK_ROOTS").is_some();
    (user_dotnet_root, overrides_present)
}

#[test]
fn sdk_style_netcoreapp_fsharp_implicit_dependencies_match_msbuild() {
    assert_sdk_captures_match(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

/// The F# SDK's `Microsoft.FSharp.Core.NetSdk.props` derives
/// `FSharpCoreMaximumMajorVersion` /`_FSharpCoreLibraryPacksFolder` with the
/// property functions Stage 3 of `docs/completed/property-expression-plan.md` pinned
/// (`$([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)`,
/// `$([MSBuild]::EnsureTrailingSlash(...))`, and a `Contains('{')` condition
/// guard). Before Stage 3 those were `Unsupported`, poisoning the reference set
/// with property-function-shaped causes; now the whole real `net10.0` SDK chain
/// yields *no* uncertainty cause whose expression/condition is one of those
/// shapes.
///
/// The full fixture
/// ([`sdk_style_netcoreapp_fsharp_implicit_dependencies_match_msbuild`]) now
/// passes outright; this remains as a focused sentinel that pins the Stage-3
/// property-function win specifically, so a regression there is diagnosed
/// without decoding the whole capture diff.
#[test]
fn sdk_style_netcoreapp_fsharp_property_functions_leave_no_cause() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let proj = dir.path().join("Diff.fsproj");
    let xml = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>"#;
    std::fs::write(&proj, xml).expect("write project");

    let dotnet_root = dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        // The fixture tempdir has no global.json above it.
        global_json_pins_workload_set: false,
    };
    let resolver = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &Default::default(),
        &common::oracle_environment(),
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));

    // The F# props file uses exactly these property-function fragments; if any
    // still failed to evaluate it would surface as an `UnsupportedProperty
    // Expression`/`UnsupportedCondition` cause quoting the fragment.
    const FSCORE_FUNCTION_MARKERS: &[&str] = &[
        "System.Version]::Parse",
        "FSCorePackageVersion.Split",
        "FSCorePackageVersion.Contains",
        "EnsureTrailingSlash",
    ];
    let offending: Vec<&str> = parsed
        .package_reference_uncertainties
        .iter()
        .filter_map(|cause| match &cause.kind {
            PackageReferenceUncertaintyCauseKind::Diagnostic(
                DiagnosticKind::UnsupportedPropertyExpression { expression },
            ) => Some(expression.as_str()),
            PackageReferenceUncertaintyCauseKind::Diagnostic(
                DiagnosticKind::UnsupportedCondition { condition },
            ) => Some(condition.as_str()),
            _ => None,
        })
        .filter(|text| FSCORE_FUNCTION_MARKERS.iter().any(|m| text.contains(m)))
        .collect();
    assert!(
        offending.is_empty(),
        "F# SDK property-function expressions should evaluate cleanly after \
         Stage 3, but these remain as uncertainty causes: {offending:#?}"
    );
}

/// The Stage-C `[System.String]::IsNullOrEmpty` keystone (2026-07-14): the real
/// `net10.0` chain's *first* `walk_opaque` latch was the
/// `Microsoft.WorkflowBuildExtensions.targets` import gate, whose condition
/// `(… and !$([System.String]::IsNullOrEmpty('$(TargetFrameworkVersion)')) …)`
/// we could not evaluate, so it degraded to `UnsupportedCondition` — latching
/// opacity and turning ~30 downstream undefined-property *reads*, 5
/// SDK-property taints, and 4 structural import skips into cascade causes.
///
/// With `IsNullOrEmpty` modelled the gate resolves exactly (to `Skip`), the
/// latch never fires, and the whole cascade collapses. This asserts the shape
/// of that win directly: the surviving causes carry **no** `UndefinedProperty`
/// read, **no** `Structural` skip, and **no** `SdkDependencyItemPropertyEvaluation`
/// taint — the three families that were pure collateral of the latch. The full
/// fixture now passes; this remains as a focused sentinel for the keystone.
#[test]
fn sdk_style_netcoreapp_workflow_gate_no_longer_cascades() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let proj = dir.path().join("Diff.fsproj");
    let xml = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>"#;
    std::fs::write(&proj, xml).expect("write project");

    let dotnet_root = dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        global_json_pins_workload_set: false,
    };
    let resolver = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &Default::default(),
        &common::oracle_environment(),
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));

    let cascade_causes: Vec<String> = parsed
        .package_reference_uncertainties
        .iter()
        .filter(|cause| {
            matches!(
                cause.kind,
                PackageReferenceUncertaintyCauseKind::Diagnostic(
                    DiagnosticKind::UndefinedProperty { .. }
                ) | PackageReferenceUncertaintyCauseKind::Structural(_)
                    | PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
            )
        })
        .map(|cause| format!("{:?}", cause.kind))
        .collect();
    assert!(
        cascade_causes.is_empty(),
        "the WorkflowBuildExtensions import gate should no longer latch \
         `walk_opaque`, so no undefined-read / structural-skip / SDK-taint cause \
         should survive; found: {cascade_causes:#?}"
    );
}

/// The F# SDK's `Microsoft.FSharp.NetSdk.props` declares an
/// `<ItemDefinitionGroup>` setting `<PackageReference><GeneratePathProperty>`
/// true on every package reference. That was the real net10.0 chain's third
/// residual uncertainty cause (`ItemDefinitionDefault`) — over-conservative,
/// because `GeneratePathProperty` is not one of the metadata we capture
/// (id / Version / VersionOverride / *Assets), so the default cannot perturb
/// the captured set. With item-definition defaults now screened against the
/// captured-metadata set, the real chain yields no `ItemDefinitionDefault`
/// cause. The full fixture now passes; this remains as a focused sentinel.
#[test]
fn sdk_style_netcoreapp_fsharp_path_property_default_is_inert() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let proj = dir.path().join("Diff.fsproj");
    let xml = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>"#;
    std::fs::write(&proj, xml).expect("write project");

    let dotnet_root = dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        global_json_pins_workload_set: false,
    };
    let resolver = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &Default::default(),
        &common::oracle_environment(),
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));

    assert!(
        !parsed
            .package_reference_uncertainties
            .iter()
            .any(|cause| cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault),
        "the F# SDK GeneratePathProperty item-definition default touches no \
         captured metadata and must raise no ItemDefinitionDefault cause; \
         causes: {:#?}",
        parsed.package_reference_uncertainties
    );
}

#[test]
fn sdk_style_netstandard20_fsharp_implicit_dependencies_match_msbuild() {
    assert_sdk_captures_match(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>netstandard2.0</TargetFramework>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn sdk_style_netstandard21_fsharp_implicit_dependencies_match_msbuild() {
    assert_sdk_captures_match(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>netstandard2.1</TargetFramework>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn sdk_style_disable_implicit_framework_references_matches_msbuild() {
    assert_sdk_captures_match(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFrameworkReferences>true</DisableImplicitFrameworkReferences>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn sdk_style_disable_implicit_fsharp_core_reference_matches_msbuild() {
    assert_sdk_captures_match(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn version_attribute_and_metadata() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" PrivateAssets="all" />
    <PackageReference Include="Serilog" Version="3.1.1" ExcludeAssets="runtime" IncludeAssets="compile" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn case_variant_metadata_attributes_use_last_write() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" version="2.0" Version="1.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn version_child_element() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Serilog">
      <Version>3.1.1</Version>
      <PrivateAssets>all</PrivateAssets>
    </PackageReference>
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn semicolon_split_shares_metadata() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="A;B;C" Version="1.2.3" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn property_expansion_in_id_and_version() {
    assert_captures_match(
        r#"<Project>
  <PropertyGroup>
    <PkgId>Some.Package</PkgId>
    <PkgVer>4.5.6</PkgVer>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="$(PkgId)" Version="$(PkgVer)" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn condition_included_and_excluded() {
    let xml = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Always" Version="1.0" />
    <PackageReference Include="DebugOnly" Version="2.0"
                      Condition="'$(Configuration)' == 'Debug'" />
  </ItemGroup>
</Project>"#;
    assert_captures_match(xml, &[("Configuration", "Release")]);
    assert_captures_match(xml, &[("Configuration", "Debug")]);
}

#[test]
fn version_override_metadata() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" VersionOverride="2.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_reference_from_item_list_transfers_metadata() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="all" />
    <MIBCPackage Include="Beta">
      <Version>2.0</Version>
      <IncludeAssets>compile</IncludeAssets>
    </MIBCPackage>
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Before;@(MIBCPackage);After" Version="9.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_reference_from_item_list_ignores_unmodeled_helper_metadata_uncertainty() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" Foo="$(Missing)" />
    <MIBCPackage Include="Beta">
      <Version>2.0</Version>
      <PrivateAssets>all</PrivateAssets>
      <Foo Condition="'$(Missing)' == 'true'">ignored</Foo>
    </MIBCPackage>
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_reference_from_item_list_overrides_helper_metadata_uncertainty() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="$(Missing)" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" Version="1.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn framework_reference_from_item_list_ignores_helper_package_metadata_uncertainty() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <SharedFramework Include="Microsoft.AspNetCore.App" Version="$(Missing)" />
  </ItemGroup>
  <ItemGroup>
    <FrameworkReference Include="@(SharedFramework)" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_reference_from_item_list_after_helper_remove() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <MIBCPackage Include="Beta" Version="2.0" />
    <MIBCPackage Remove="alpha" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_reference_from_item_list_after_removing_bad_helper_metadata() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="$(Missing)" />
    <MIBCPackage Remove="Alpha" />
    <MIBCPackage Include="Beta" Version="2.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn cpm_items_from_literals_and_item_lists_match_msbuild() {
    assert_cpm_items_match(
        r#"<Project>
  <ItemGroup>
    <CentralVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <GlobalAnalyzer Include="Some.Analyzer" Version="1.0" PrivateAssets="all" />
  </ItemGroup>
  <ItemGroup>
    <PackageVersion Include="@(CentralVersion);Serilog" Version="3.1.1" />
    <GlobalPackageReference Include="@(GlobalAnalyzer)" IncludeAssets="compile" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn framework_reference_alongside_package() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <FrameworkReference Include="Microsoft.AspNetCore.App" />
    <FrameworkReference Include="Microsoft.WindowsDesktop.App" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn exclude_removes_matching_id() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="A;B;C" Exclude="B" Version="1.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn conditioned_metadata_child_matches_msbuild() {
    let xml = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X">
      <Version Condition="'$(Chan)' == 'stable'">1.0.0</Version>
      <Version Condition="'$(Chan)' != 'stable'">2.0.0-pre</Version>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    assert_captures_match(xml, &[("Chan", "stable")]);
    assert_captures_match(xml, &[("Chan", "beta")]);
}

#[test]
fn child_metadata_overrides_attribute() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0"><Version>2.0</Version></PackageReference>
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn exclude_matches_case_insensitively() {
    // MSBuild item identity is OrdinalIgnoreCase, so `Exclude="beta"` removes
    // `Beta`. Getting this wrong would over-resolve Beta (the cardinal sin).
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Alpha;Beta" Exclude="beta" Version="1.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

// ---------------------------------------------------------------------------
// `PackageReference Update` merge: MSBuild folds each `Update` onto every
// *prior* `Include` of the same (case-insensitive) id, overwriting each
// specified metadatum, and drops a lone `Update` matching no prior `Include`.
// These fixtures diff the effective (collapsed) set directly against MSBuild.
// ---------------------------------------------------------------------------

#[test]
fn update_after_include_merges_metadata() {
    // Version overwritten; Include-only IncludeAssets kept; Update-only
    // PrivateAssets added. Orphan `Update="Beta"` (no prior Include) dropped.
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Alpha" Version="1.0" IncludeAssets="all" />
    <PackageReference Update="Alpha" Version="2.0" PrivateAssets="compile" />
    <PackageReference Update="Beta" Version="9.9" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn update_before_include_does_not_apply() {
    // An `Update` preceding its `Include` modifies nothing: Gamma keeps
    // Version 1.0 and gains no PrivateAssets. Applying it would over-resolve.
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Update="Gamma" Version="7.7" PrivateAssets="all" />
    <PackageReference Include="Gamma" Version="1.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn multiple_updates_last_write_wins_per_key_case_insensitively() {
    // Cross-ItemGroup, case-insensitive id: later `Version` wins, the first
    // Update's PrivateAssets survives (not re-specified later).
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Update="ALPHA" PrivateAssets="all" />
    <PackageReference Update="alpha" Version="3.0" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn update_applies_to_all_prior_includes_of_id() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Dup" Version="1.0" />
    <PackageReference Include="Dup" Version="1.5" />
    <PackageReference Update="Dup" PrivateAssets="all" />
  </ItemGroup>
</Project>"#,
        &[],
    );
}

// ---------------------------------------------------------------------------
// Pass-ordering pins: MSBuild finalises every property before evaluating any
// item, so an item's Include / Condition / metadata may consume a property
// written *after* its own document position. These fixtures diff that
// behaviour directly against `dotnet msbuild`.
// ---------------------------------------------------------------------------

#[test]
fn item_condition_uses_final_property_value_not_document_position_value() {
    // Flag reads "true" at the ItemGroup's position but finishes "false":
    // MSBuild excludes the package. Including it would be an over-resolve.
    assert_captures_match(
        r#"<Project>
  <PropertyGroup>
    <Flag>true</Flag>
  </PropertyGroup>
  <ItemGroup Condition="'$(Flag)' == 'true'">
    <PackageReference Include="A" Version="1.0.0" />
  </ItemGroup>
  <PropertyGroup>
    <Flag>false</Flag>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_version_metadata_uses_property_defined_later_in_document() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="B" Version="$(BVer)" />
  </ItemGroup>
  <PropertyGroup>
    <BVer>2.1.0</BVer>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

#[test]
fn package_gated_on_property_defined_later_in_document_is_included() {
    assert_captures_match(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="C" Version="3.0.0"
                      Condition="'$(UseC)' == 'true'" />
  </ItemGroup>
  <PropertyGroup>
    <UseC>true</UseC>
  </PropertyGroup>
</Project>"#,
        &[],
    );
}

// ---------------------------------------------------------------------------
// Real NuGet.props chain: SDK-style fixtures with a Directory.Packages.props
// up-tree, diffed against `dotnet msbuild`. Unlike the `sdk_style_*` fixtures
// above these do NOT require the full SDK dependency model to be exact — they
// compare the captured CPM item data and targeted probe properties, both of
// which must already be exact for the central-package chain
// (Sdk.props → Microsoft.Common.props → NuGet.props → Directory.Packages.props)
// to count as genuinely evaluated.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GetItemsAndPropertiesOutput {
    #[serde(rename = "Items")]
    items: Items,
    #[serde(rename = "Properties")]
    properties: BTreeMap<String, String>,
}

fn run_get_items_and_properties(
    proj: &Path,
    item_set: &str,
    properties: &[&str],
) -> GetItemsAndPropertiesOutput {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
    cmd.args(["msbuild", "-nologo"]);
    cmd.arg(format!("-getItem:{item_set}"));
    cmd.arg(format!("-getProperty:{}", properties.join(",")));
    cmd.arg(proj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", proj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "parse -getItem/-getProperty JSON for {}: {e}\n{stdout}",
            proj.display()
        )
    })
}

/// Build `<canonical tempdir>/Directory.Packages.props` + `src/Diff.fsproj`,
/// parse ours with the real SDK resolver, and hand back both sides.
/// The tempdir is canonicalised up front so tempdir-rooted path property
/// values compare byte-for-byte between the two sides.
fn eval_cpm_fixture_both_ways(
    packages_props_xml: &str,
    project_xml: &str,
    item_set: &str,
    properties: &[&str],
) -> (
    borzoi_msbuild::ParsedProject,
    GetItemsAndPropertiesOutput,
    tempfile::TempDir,
) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    std::fs::write(root.join("Directory.Packages.props"), packages_props_xml)
        .expect("write packages props");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    let proj = root.join("src/Diff.fsproj");
    std::fs::write(&proj, project_xml).expect("write project");

    let dotnet_root = dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        // The fixture tempdir has no global.json above it.
        global_json_pins_workload_set: false,
    };
    let resolver = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let parsed = parse_fsproj_with_imports(
        project_xml,
        &proj,
        &std::collections::HashMap::new(),
        &common::oracle_environment(),
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));

    let msbuild = run_get_items_and_properties(&proj, item_set, properties);
    (parsed, msbuild, dir)
}

fn has_directory_packages_props_cause(parsed: &borzoi_msbuild::ParsedProject) -> bool {
    parsed.package_reference_uncertainties.iter().any(|cause| {
        matches!(
            cause.kind,
            borzoi_msbuild::PackageReferenceUncertaintyCauseKind::DirectoryPackagesProps { .. }
        )
    })
}

#[test]
fn sdk_style_cpm_central_versions_flow_through_real_nuget_props_chain() {
    let (parsed, msbuild, _dir) = eval_cpm_fixture_both_ways(
        r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageVersion Include="FSharp.Core" Version="8.0.400" />
  </ItemGroup>
</Project>"#,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
        "PackageVersion",
        &["CentralPackageVersionsFileImported"],
    );

    let ours: Vec<_> = parsed
        .package_versions
        .iter()
        .map(ours_package_version_fields)
        .collect();
    let theirs: Vec<_> = msbuild
        .items
        .package_version
        .iter()
        .map(msbuild_package_version_fields)
        .collect();
    assert_eq!(
        ours, theirs,
        "PackageVersion mismatch; our uncertainties: {:#?}",
        parsed.package_reference_uncertainties
    );
    assert_eq!(
        msbuild
            .properties
            .get("CentralPackageVersionsFileImported")
            .map(String::as_str),
        Some("true"),
        "oracle sanity: MSBuild itself must have imported the central file"
    );
    // The explicit reference is captured versionless. Central-version
    // *application* is deliberately not asserted here: the real SDK ships
    // evaluation-time `<PackageReference Update=…>` items
    // (Microsoft.NET.Sdk.DefaultItems.*), and the inline-CPM envelope
    // conservatively refuses to apply versions while any Update op is in
    // the captured set. Application through the identical NuGet.props
    // chain is pinned by the `nuget_props_chain` unit tests, whose fake
    // SDK has no Update items; refining the envelope per-id is a
    // follow-up slice.
    let ours_nj = parsed
        .package_references
        .iter()
        .find(|pr| pr.id == "Newtonsoft.Json")
        .expect("explicit reference captured");
    assert_eq!(ours_nj.op, borzoi_msbuild::PackageRefOp::Include);
    assert!(
        !has_directory_packages_props_cause(&parsed),
        "the walked central file must discharge the blanket cause: {:#?}",
        parsed.package_reference_uncertainties
    );
}

#[test]
fn sdk_style_cpm_probe_properties_match_msbuild() {
    // Pins the toolset-property seeding and both property functions
    // against ground truth. Probes are written by the project body (so
    // they land in `ParsedProject::properties`) and read back from
    // MSBuild via `-getProperty`. SDK-rooted paths are compared
    // canonicalised (the resolver and MSBuild may name the same file
    // through different symlink prefixes); tempdir-rooted and scalar
    // probes compare byte-for-byte.
    let (parsed, msbuild, _dir) = eval_cpm_fixture_both_ways(
        r#"<Project>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <ProbeToolsVersion>$(MSBuildToolsVersion)</ProbeToolsVersion>
    <ProbeRuntimeType>$(MSBuildRuntimeType)</ProbeRuntimeType>
    <ProbeExtensionsPath>$(MSBuildExtensionsPath)</ProbeExtensionsPath>
    <ProbeNuGetPropsFile>$(NuGetPropsFile)</ProbeNuGetPropsFile>
    <ProbeCentralPath>$(DirectoryPackagesPropsPath)</ProbeCentralPath>
    <ProbeMarker>$(CentralPackageVersionsFileImported)</ProbeMarker>
    <ProbeDirAbove>$([MSBuild]::GetDirectoryNameOfFileAbove('$(MSBuildProjectDirectory)', 'Directory.Packages.props'))</ProbeDirAbove>
    <ProbeNormalize>$([MSBuild]::NormalizePath('$(MSBuildProjectDirectory)', 'nested', '..', 'probe.txt'))</ProbeNormalize>
  </PropertyGroup>
</Project>"#,
        "PackageVersion",
        &[
            "ProbeToolsVersion",
            "ProbeRuntimeType",
            "ProbeExtensionsPath",
            "ProbeNuGetPropsFile",
            "ProbeCentralPath",
            "ProbeMarker",
            "ProbeDirAbove",
            "ProbeNormalize",
        ],
    );

    let ours = |name: &str| {
        parsed
            .properties
            .get(name)
            .unwrap_or_else(|| panic!("our side did not record {name}"))
            .as_str()
    };
    let theirs = |name: &str| {
        msbuild
            .properties
            .get(name)
            .unwrap_or_else(|| panic!("msbuild did not report {name}"))
            .as_str()
    };

    // Scalar and tempdir-rooted probes: byte-for-byte.
    for name in [
        "ProbeToolsVersion",
        "ProbeRuntimeType",
        "ProbeCentralPath",
        "ProbeMarker",
        "ProbeDirAbove",
        "ProbeNormalize",
    ] {
        assert_eq!(ours(name), theirs(name), "probe {name} diverges");
    }

    // SDK-rooted path: same file, possibly via different symlink prefixes.
    // MSBuild on unix also lazily converts backslashes in path-shaped
    // property values while this walker stores values verbatim and
    // normalises at use sites — so compare file identity, not string form.
    let name = "ProbeNuGetPropsFile";
    let a = std::fs::canonicalize(ours(name).replace('\\', "/"))
        .unwrap_or_else(|e| panic!("canonicalize ours {name}={}: {e}", ours(name)));
    let b = std::fs::canonicalize(theirs(name))
        .unwrap_or_else(|e| panic!("canonicalize theirs {name}={}: {e}", theirs(name)));
    assert_eq!(a, b, "probe {name} names different files");
    // MSBuildExtensionsPath is a directory and carries a trailing
    // separator on both sides.
    assert!(
        ours("ProbeExtensionsPath").ends_with('/'),
        "ours: {}",
        ours("ProbeExtensionsPath")
    );
    assert!(
        theirs("ProbeExtensionsPath").ends_with('/'),
        "theirs: {}",
        theirs("ProbeExtensionsPath")
    );
    let a = std::fs::canonicalize(ours("ProbeExtensionsPath")).expect("ours extensions path");
    let b = std::fs::canonicalize(theirs("ProbeExtensionsPath")).expect("theirs extensions path");
    assert_eq!(a, b, "ProbeExtensionsPath names different directories");
}

#[test]
fn sdk_style_cpm_version_override_matches_msbuild() {
    let (parsed, msbuild, _dir) = eval_cpm_fixture_both_ways(
        r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageVersion Include="Serilog" Version="3.1.1" />
  </ItemGroup>
</Project>"#,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
    <PackageReference Include="Serilog" VersionOverride="2.0.0" />
  </ItemGroup>
</Project>"#,
        "PackageVersion,PackageReference",
        &["CentralPackageVersionsFileImported"],
    );

    let ours_versions: Vec<_> = parsed
        .package_versions
        .iter()
        .map(ours_package_version_fields)
        .collect();
    let theirs_versions: Vec<_> = msbuild
        .items
        .package_version
        .iter()
        .map(msbuild_package_version_fields)
        .collect();
    assert_eq!(ours_versions, theirs_versions, "PackageVersion mismatch");

    let ours_by_id = |id: &str| {
        parsed
            .package_references
            .iter()
            .find(|pr| pr.id == id)
            .unwrap_or_else(|| panic!("{id} captured"))
    };
    let theirs_by_id = |id: &str| {
        msbuild
            .items
            .package_reference
            .iter()
            .find(|item| item.identity == id)
            .unwrap_or_else(|| panic!("{id} in msbuild output"))
    };

    // The `VersionOverride` metadata is captured verbatim and must match
    // MSBuild's evaluated item view exactly (Serilog carries it, Newtonsoft
    // does not).
    for id in ["Newtonsoft.Json", "Serilog"] {
        let g = |k: &str| {
            theirs_by_id(id)
                .metadata
                .get(k)
                .filter(|v| !v.is_empty())
                .cloned()
        };
        assert_eq!(
            ours_by_id(id).version_override,
            g("VersionOverride"),
            "captured VersionOverride for {id} diverges"
        );
    }

    // The *effective* `version` is our own CPM computation and deliberately
    // diverges from MSBuild's evaluated `-getItem:PackageReference` (which
    // shows no version — NuGet resolves central/override versions in a later
    // target, not during evaluation): the versionless include takes the
    // central `PackageVersion`, and `Serilog` takes its `VersionOverride`.
    assert_eq!(
        ours_by_id("Newtonsoft.Json").version.as_deref(),
        Some("13.0.1"),
        "central version not applied to versionless include"
    );
    assert_eq!(
        ours_by_id("Serilog").version.as_deref(),
        Some("2.0.0"),
        "VersionOverride not applied as effective version"
    );
}

#[test]
fn sdk_style_cpm_global_package_reference_capture_matches_msbuild() {
    // GlobalPackageReference blocks inline version application (that
    // envelope is deliberately conservative), but the *capture* of the
    // central file's items must still match MSBuild exactly.
    let (parsed, msbuild, _dir) = eval_cpm_fixture_both_ways(
        r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <GlobalPackageReference Include="MinVer" Version="4.3.0" />
  </ItemGroup>
</Project>"#,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#,
        "PackageVersion,GlobalPackageReference",
        &["CentralPackageVersionsFileImported"],
    );

    // `NuGet.targets` converts `GlobalPackageReference` items into
    // additional evaluation-time `PackageVersion` items via an
    // `Include="@(GlobalPackageReference)"` item-list reference, which
    // this walker does not model yet — so MSBuild's PackageVersion list
    // is a strict superset here. Ours must be exactly the declared items
    // and every one of them present in MSBuild's list.
    let ours_versions: Vec<_> = parsed
        .package_versions
        .iter()
        .map(ours_package_version_fields)
        .collect();
    let theirs_versions: Vec<_> = msbuild
        .items
        .package_version
        .iter()
        .map(msbuild_package_version_fields)
        .collect();
    assert_eq!(
        ours_versions,
        vec![("Newtonsoft.Json".to_string(), Some("13.0.1".to_string()))],
        "PackageVersion capture"
    );
    for declared in &ours_versions {
        assert!(
            theirs_versions.contains(declared),
            "msbuild lost a declared PackageVersion: {declared:?} vs {theirs_versions:?}"
        );
    }

    let ours_global: Vec<_> = parsed
        .global_package_references
        .iter()
        .map(ours_global_pkg_fields)
        .collect();
    let theirs_global: Vec<_> = msbuild
        .items
        .global_package_reference
        .iter()
        .map(msbuild_global_pkg_fields)
        .collect();
    assert_eq!(
        ours_global, theirs_global,
        "GlobalPackageReference mismatch"
    );
}
