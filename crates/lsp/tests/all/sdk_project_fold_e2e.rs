//! Regression guard (3.3d round 19, codex): an ordinary
//! `<Project Sdk="Microsoft.NET.Sdk">` project — no exotic gates, no
//! `<LangVersion>` — must fold (`parses_for_project` returns `Some`).
//!
//! The fold gate for cross-file resolution is an *intersection*:
//! `Parse::shape_depends_on_language_version` (a file whose token stream
//! straddles the F# 8 strict-indentation boundary) AND
//! `ParsedProject::property_provenance_untrusted("LangVersion")` (we can't pin
//! which version the real build uses). Either alone folds; only both together
//! refuse. These tests pin every corner of that intersection against a *real*
//! SDK:
//!
//! - [`plain_sdk_project_folds`] — no shape-dependence → folds.
//! - [`sdk_project_with_straddling_file_and_trusted_langversion_folds`] —
//!   shape-dependence but *trusted* `LangVersion` provenance → folds. With the
//!   server's `MSBuildUserExtensionsPath` seeding, a plain `net10.0` project
//!   evaluates through the whole SDK chain transparently, so `LangVersion` is
//!   knowable (`net10.0` ⇒ the F# 10 default, past the boundary) and the file
//!   parses at the version the compiler actually uses.
//! - [`sdk_project_with_version_straddling_file_refuses_the_fold`] —
//!   shape-dependence AND genuinely-untrusted `LangVersion` provenance (a write
//!   under an unpinnable gate) → refuses.
//!
//! [`ordinary_sdk_projects_have_trusted_langversion_provenance`] asserts the
//! evaluation fact behind the trusted corner directly, over a table of ordinary
//! project shapes: the SDK's own `LangVersion` writes sit under gates the
//! walker decides, so an ordinary project's provenance is trusted and an
//! untrusted one marks a `LangVersion` write the walker could not pin (in the
//! project itself or an import such as `Directory.Build.props`).
//!
//! A fold gate keyed on the provenance mark *alone* would disable cross-file
//! resolution for every file of an untrusted project, however version-invariant;
//! keyed on the shape flag alone it would flicker off during ordinary mid-edit
//! states. The intersection is what makes it precise.
//!
//! Requires the .NET SDK on PATH — the Nix devShell provides it.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::workspace::Workspace;

/// A unique fixture dir per run, in the system temp so `nix develop` sandboxes
/// and CI caches treat it as scratch.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-{tag}-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn folds(fsproj: &std::path::Path) -> bool {
    let mut workspace = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
    let mut sema = SemanticState::new();
    let docs = HashMap::new();
    sema.parses_for_project(fsproj, &mut workspace, &docs)
        .is_some()
}

#[test]
fn plain_sdk_project_folds() {
    let root = scratch_dir("sdk-fold");
    let fsproj = root.join("App.fsproj");
    std::fs::write(
        &fsproj,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Lib.fs" />
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>
"#,
    )
    .unwrap();
    std::fs::write(root.join("Lib.fs"), "module Lib\n\nlet answer = 42\n").unwrap();
    std::fs::write(
        root.join("Main.fs"),
        "module Main\n\nlet double = Lib.answer * 2\n",
    )
    .unwrap();

    assert!(
        folds(&fsproj),
        "a plain SDK project must fold; if this regressed, a fold gate is \
         keyed on a signal the SDK machinery trips for every real project"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// A shape-dependent file (`match x with` — the F# 8 strict-indentation
/// boundary) in a project whose `LangVersion` provenance is *trusted* folds:
/// the version is knowable, so there is no ambiguity to guard against. This is
/// the plain `net10.0` case, which the `MSBuildUserExtensionsPath` seeding lets
/// the walker evaluate through the whole SDK chain.
#[test]
fn sdk_project_with_straddling_file_and_trusted_langversion_folds() {
    let root = scratch_dir("sdk-fold-straddle-trusted");
    let fsproj = root.join("App.fsproj");
    std::fs::write(
        &fsproj,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    )
    .unwrap();
    std::fs::write(root.join("A.fs"), "match x with\n").unwrap();

    assert!(
        folds(&fsproj),
        "a version-shape-dependent file must fold when LangVersion is trusted \
         (net10.0 is knowable through the seeded SDK chain)"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The evaluation fact the folding cases above rest on, asserted directly
/// rather than through the fold: an ordinary SDK project's `LangVersion`
/// provenance is **trusted**. The SDK's own `LangVersion` writes sit under
/// gates the walker evaluates — `Microsoft.FSharp.NetSdk.props` writes `11`
/// only for a preview SDK 11 (`_NETCoreSdkIsPreview`), and
/// `Microsoft.NET.Sdk.Common.targets` writes `Preview` only under
/// `EnablePreviewFeatures` on the latest TFM — so walking the SDK chain does
/// not by itself mark the provenance. The generic seam still marks dozens of
/// *other* properties on these same projects (`DefineConstants`, `OutputPath`,
/// `TargetFrameworkMoniker`, …); `LangVersion` is not among them.
///
/// Each row also pins the evaluated value, so a row whose fixture stopped
/// meaning what its tag says fails here rather than passing vacuously: the
/// `preview-features` row's `Preview` can only come from the SDK's gated write,
/// which shows the walk really reached and decided it. The last row is the
/// control: a write under an unpinnable gate (an item reference in a condition)
/// must read untrusted, or the assertion on the other rows proves nothing.
#[test]
fn ordinary_sdk_projects_have_trusted_langversion_provenance() {
    // (tag, PropertyGroup body, expected evaluated LangVersion, expected untrusted)
    let rows: &[(&str, &str, Option<&str>, bool)] = &[
        (
            "plain-net10",
            "<TargetFramework>net10.0</TargetFramework>",
            None,
            false,
        ),
        (
            "exe-net10",
            "<TargetFramework>net10.0</TargetFramework><OutputType>Exe</OutputType>",
            None,
            false,
        ),
        (
            "net8",
            "<TargetFramework>net8.0</TargetFramework>",
            None,
            false,
        ),
        (
            "netstandard20",
            "<TargetFramework>netstandard2.0</TargetFramework>",
            None,
            false,
        ),
        (
            "multi-target",
            "<TargetFrameworks>net8.0;net10.0</TargetFrameworks>",
            None,
            false,
        ),
        (
            "body-pinned",
            "<TargetFramework>net10.0</TargetFramework><LangVersion>8.0</LangVersion>",
            Some("8.0"),
            false,
        ),
        (
            "preview-features",
            "<TargetFramework>net10.0</TargetFramework>\
             <EnablePreviewFeatures>true</EnablePreviewFeatures>",
            Some("Preview"),
            false,
        ),
        (
            "control-unpinnable-gate",
            "<TargetFramework>net10.0</TargetFramework>\
             <LangVersion Condition=\"'@(NoSuchItem)' == 'yes'\">7.0</LangVersion>",
            None,
            true,
        ),
    ];
    let mut failures = Vec::new();
    for &(tag, props, expected_value, expected_untrusted) in rows {
        let root = scratch_dir(&format!("sdk-langversion-trust-{tag}"));
        let fsproj = root.join("App.fsproj");
        std::fs::write(
            &fsproj,
            format!(
                r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    {props}
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#
            ),
        )
        .unwrap();
        std::fs::write(root.join("A.fs"), "module A\n").unwrap();
        let mut workspace = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
        let parsed = workspace
            .project(&fsproj)
            .unwrap_or_else(|| panic!("{tag}: the project must evaluate"));
        let value = parsed.lang_version.as_deref();
        let untrusted = parsed.property_provenance_untrusted("LangVersion");
        if value != expected_value || untrusted != expected_untrusted {
            failures.push(format!(
                "{tag}: LangVersion {value:?} (untrusted: {untrusted}), \
                 expected {expected_value:?} (untrusted: {expected_untrusted})"
            ));
        }
        std::fs::remove_dir_all(&root).ok();
    }
    assert!(
        failures.is_empty(),
        "LangVersion provenance moved on an ordinary SDK project:\n{}",
        failures.join("\n")
    );
}

/// The narrow counterpart: the same shape-dependent file, but `LangVersion` is
/// written under an *unpinnable* gate (an item reference in a condition —
/// permanently unsupported by design), so its provenance is genuinely
/// untrusted. The version we'd fold at could mis-shape the tree, so the fold
/// must refuse. Together with the two folding cases this pins the intersection
/// gate exactly: untrusted provenance × version-dependent shape does not fold;
/// drop either and it does.
#[test]
fn sdk_project_with_version_straddling_file_refuses_the_fold() {
    let root = scratch_dir("sdk-fold-straddle-untrusted");
    let fsproj = root.join("App.fsproj");
    std::fs::write(
        &fsproj,
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <LangVersion Condition="'@(NoSuchItem)' == 'yes'">7.0</LangVersion>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    )
    .unwrap();
    std::fs::write(root.join("A.fs"), "match x with\n").unwrap();

    assert!(
        !folds(&fsproj),
        "an unknowable LangVersion (written under an unpinnable gate) with a \
         version-shape-dependent file must refuse the fold"
    );

    std::fs::remove_dir_all(&root).ok();
}
