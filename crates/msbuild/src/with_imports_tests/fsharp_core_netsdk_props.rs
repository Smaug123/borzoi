//! The F# SDK's `Microsoft.FSharp.Core.NetSdk.props`, evaluated **verbatim**.
//!
//! This is the motivating consumer of the whole property-expression parser
//! (`docs/completed/property-expression-plan.md`): its `FSharpCoreMaximumMajorVersion`
//! derivation
//!
//! ```text
//! $([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)
//! ```
//!
//! chains a nested string-yielding `Split(char-set)[index]` inside a
//! `Version::Parse` argument, then reads `.Major` — every Stage-3 evaluator at
//! once. The file also uses `[MSBuild]::EnsureTrailingSlash('$(…)')` and a
//! `$(FSCorePackageVersion.Contains('{'))` guard in a condition. Nothing here
//! is special-cased: the file evaluates because the parser + pinned dispatch
//! model exactly what MSBuild does (each shape pinned against dotnet msbuild
//! 10.0.300; see the `property_expr_diff` differential). Do **not** "simplify"
//! the embedded body — it is a byte-for-byte copy of the shipped file.

use super::*;
use tempfile::TempDir;

/// Verbatim body of `FSharp/Microsoft.FSharp.Core.NetSdk.props` from the .NET
/// SDK 10.0.203 (the comment header elided). The `FSCorePackageVersion` value
/// is whatever the shipped file carries — here `10.1.203`.
const REAL_FSHARP_CORE_NETSDK_PROPS: &str = r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">

  <PropertyGroup>
    <MSBuildAllProjects>$(MSBuildAllProjects);$(MSBuildThisFileFullPath)</MSBuildAllProjects>
  </PropertyGroup>

  <PropertyGroup Condition="'$(FSCorePackageVersionSet)' != 'true'">
    <FSCorePackageVersionSet>true</FSCorePackageVersionSet>
    <FSCorePackageVersion>10.1.203</FSCorePackageVersion>
    <_FSharpCoreLibraryPacksFolder Condition="'$(_FSharpCoreLibraryPacksFolder)' == ''">$([MSBuild]::EnsureTrailingSlash('$(MSBuildThisFileDirectory)'))library-packs</_FSharpCoreLibraryPacksFolder>
  </PropertyGroup>

  <PropertyGroup Condition="'$(FSCorePackageVersionSet)' == 'true' and '$(FSCorePackageVersion)' != '' and !$(FSCorePackageVersion.Contains('{'))">
    <FSharpCoreMaximumMajorVersion Condition="'$(FSharpCoreMaximumMajorVersion)' == ''">$([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)</FSharpCoreMaximumMajorVersion>
  </PropertyGroup>

</Project>"#;

/// Import the verbatim props file from a project and evaluate the chain. The
/// props file is written to disk (so `MSBuildThisFileDirectory` resolves to a
/// real path for `EnsureTrailingSlash`) and pulled in with an ordinary
/// `<Import>`.
fn parse_importing_fsharp_core_props(tmp: &Path) -> ParsedProject {
    write_at(
        tmp,
        "FSharp/Microsoft.FSharp.Core.NetSdk.props",
        REAL_FSHARP_CORE_NETSDK_PROPS,
    );
    let project_path = write_at(
        tmp,
        "Demo.fsproj",
        r#"<Project>
  <Import Project="FSharp/Microsoft.FSharp.Core.NetSdk.props" />
</Project>"#,
    );
    parse_file(&project_path)
}

#[test]
fn fsharp_core_maximum_major_version_derives_exactly() {
    let tmp = TempDir::new().unwrap();
    let result = parse_importing_fsharp_core_props(tmp.path());
    let get = |name: &str| result.properties.get(name).map(String::as_str);

    // The `FSCorePackageVersionSet != 'true'` group ran once, setting the
    // version, and the derivation group parsed `10.1.203`, split off the
    // pre-release tail (none here), and read `.Major`.
    assert_eq!(get("FSCorePackageVersionSet"), Some("true"));
    assert_eq!(get("FSCorePackageVersion"), Some("10.1.203"));
    assert_eq!(
        get("FSharpCoreMaximumMajorVersion"),
        Some("10"),
        "the whole point of the feature — diags: {:?}",
        result.diagnostics
    );

    // The `EnsureTrailingSlash('$(MSBuildThisFileDirectory)')library-packs`
    // derivation also reduced (a Stage-3 static function with a nested bare
    // ref): its value ends in the appended segment, not a residual `$(`. This
    // limb is unix-only — `EnsureTrailingSlash` declines on Windows (separator
    // semantics unverified against the oracle), so gate it; the version
    // derivation above is platform-independent and stays unconditional.
    #[cfg(not(windows))]
    {
        let packs = get("_FSharpCoreLibraryPacksFolder").expect("packs folder set");
        assert!(
            packs.ends_with("/library-packs") && !packs.contains("$("),
            "unexpected _FSharpCoreLibraryPacksFolder: {packs:?}"
        );

        // No expression was left unsupported — the file evaluates cleanly,
        // exactly as a real build would, so it contributes no partiality.
        assert!(
            !result.diagnostics.iter().any(|d| matches!(
                &d.kind,
                DiagnosticKind::UnsupportedPropertyExpression { .. }
            )),
            "diags: {:?}",
            result.diagnostics
        );
        assert!(
            result.package_reference_uncertainties.is_empty(),
            "the props file has no PackageReferences and must add no uncertainty: {:?}",
            result.package_reference_uncertainties
        );
    }
}

#[test]
fn prerelease_version_splits_before_parsing() {
    // A pre-release `FSCorePackageVersion` exercises the `Split('-')[0]` limb:
    // the `-beta.1` tail is dropped before `Version::Parse`, so `.Major` is
    // still the numeric head. This is the shape that made the general
    // Split-then-Parse chain necessary (a bare `Parse('10.1.203-beta.1')`
    // would error). We can't override the verbatim body's value inline, so
    // drive the derivation directly with the property pre-seeded, matching the
    // file's expression byte-for-byte.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <FSCorePackageVersion>10.1.203-beta.1</FSCorePackageVersion>
    <FSharpCoreMaximumMajorVersion>$([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)</FSharpCoreMaximumMajorVersion>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        result
            .properties
            .get("FSharpCoreMaximumMajorVersion")
            .map(String::as_str),
        Some("10"),
        "diags: {:?}",
        result.diagnostics
    );
}
