//! Where `Directory.Build.targets` lands relative to the entry SDK's
//! `Sdk.targets`.
//!
//! MSBuild does not splice this file after the body: `Microsoft.Common.targets`
//! imports it, and the SDK's `Sdk.targets` chain reaches that — *after*
//! `Microsoft.NET.TargetFrameworkInference.targets` has computed
//! `TargetFrameworkIdentifier` / `TargetFrameworkVersion`. So the walker lets
//! the chain place the import, and splices explicitly only when the chain did
//! not.
//!
//! The tests here are about **the gate**, not about MSBuild's semantics: they
//! use synthetic SDKs, which the differential
//! (`tests/fsproj_derived_tfm_diff.rs`) structurally cannot reach — the oracle
//! has no resolver hook and would fail to resolve the name. Anything that is a
//! claim about *MSBuild* belongs there, where MSBuild adjudicates it; anything
//! that is a claim about *our* fallback belongs here.

use super::*;
use tempfile::TempDir;

/// A `Directory.Build.targets` that **appends** a record of what it could see
/// when it ran, in the style of the real gates this ordering feeds. Appending
/// rather than assigning is what makes a second walk visible.
const TRACE: &str = r#"<Project>
  <PropertyGroup>
    <Trace>$(Trace);dbt=[$(FromSdkTargets)]</Trace>
  </PropertyGroup>
</Project>"#;

#[test]
fn the_sdk_chains_own_import_places_the_file_once_after_what_sdk_targets_wrote() {
    // `Sdk.targets` writes a property and *then* imports
    // `Directory.Build.targets`, the shape `Microsoft.Common.targets` has.
    //
    // The trace **appends**, so it pins position and multiplicity in one
    // assertion, and both halves are load-bearing. Asserting only the final
    // witness value would pass on the pre-fix walker for the wrong reason: with
    // our copy spliced before `Sdk.targets` this fixture walks the file
    // *twice*, and the second walk — the chain's, at the right position —
    // overwrites the first with the right answer. Position is only really
    // pinned once "exactly once" is pinned with it.
    let tmp = TempDir::new().unwrap();
    let (sdk_dir, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "Chain.Sdk",
        "<Project></Project>",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <FromSdkTargets>written</FromSdkTargets>
  </PropertyGroup>
  <Import Project="{}/Directory.Build.targets" />
</Project>"#,
            tmp.path().display()
        ),
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Chain.Sdk">
  <PropertyGroup>
    <Trace>body</Trace>
  </PropertyGroup>
</Project>"#,
    );
    write_at(tmp.path(), "Directory.Build.targets", TRACE);

    let result = parse_file_with_sdk(&project_path, |_name| {
        Ok(SdkPaths {
            root: sdk_dir.clone(),
            props: props.clone(),
            targets: targets.clone(),
        })
    });
    assert_eq!(
        result.properties.get("Trace").map(String::as_str),
        Some("body;dbt=[written]"),
        "the chain imported the file itself, so it must run exactly once, at \
         the chain's position, seeing what `Sdk.targets` had already written"
    );
}

#[test]
fn an_sdk_that_never_imports_the_file_still_gets_the_explicit_splice() {
    // The fallback. An SDK whose targets do not reach
    // `Microsoft.Common.targets` would otherwise lose `Directory.Build.targets`
    // entirely — a silent loss of the user's own build logic, which is worse
    // than placing it late.
    let tmp = TempDir::new().unwrap();
    let (sdk_dir, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "Inert.Sdk",
        "<Project></Project>",
        r#"<Project>
  <PropertyGroup>
    <FromSdkTargets>written</FromSdkTargets>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Inert.Sdk">
  <PropertyGroup>
    <Trace>body</Trace>
  </PropertyGroup>
</Project>"#,
    );
    write_at(tmp.path(), "Directory.Build.targets", TRACE);

    let result = parse_file_with_sdk(&project_path, |_name| {
        Ok(SdkPaths {
            root: sdk_dir.clone(),
            props: props.clone(),
            targets: targets.clone(),
        })
    });
    assert_eq!(
        result.properties.get("Trace").map(String::as_str),
        Some("body;dbt=[written]"),
        "the chain never imported it, so the explicit splice must — once, and \
         after `Sdk.targets`, so it still sees what that wrote"
    );
}

#[test]
fn an_sdkless_project_still_gets_the_explicit_splice() {
    // No SDK at all: there is no chain to place the import, so the splice is
    // the only route. (Whether MSBuild imports it for a *bare* `<Project>` is a
    // separate question — it does not, and that gap is recorded in
    // `tests/fsproj_derived_tfm_diff.rs`. This test pins the walker's existing
    // behaviour, which this change does not touch.)
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <Trace>body</Trace>
    <FromSdkTargets>from-body</FromSdkTargets>
  </PropertyGroup>
</Project>"#,
    );
    write_at(tmp.path(), "Directory.Build.targets", TRACE);

    let result = parse_file(&project_path);
    assert_eq!(
        result.properties.get("Trace").map(String::as_str),
        Some("body;dbt=[from-body]")
    );
}
