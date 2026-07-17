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
//!   shape-dependence but *trusted* `LangVersion` provenance → folds. Once the
//!   server seeds `MSBuildUserExtensionsPath`, a plain `net10.0` project
//!   evaluates through the whole SDK chain transparently, so `LangVersion` is
//!   knowable (`net10.0` ⇒ the F# 10 default, past the boundary) and the file
//!   parses at the version the compiler actually uses. Before that seeding the
//!   opaque walk left `LangVersion` untrusted for *every* SDK project, and this
//!   corner refused — the change is a strict improvement.
//! - [`sdk_project_with_version_straddling_file_refuses_the_fold`] —
//!   shape-dependence AND genuinely-untrusted `LangVersion` provenance (a write
//!   under an unpinnable gate) → refuses.
//!
//! A fold gate keyed on the provenance mark *alone* would disable cross-file
//! resolution for essentially every real project whose SDK trips generic
//! provenance marks; keyed on the shape flag alone it would flicker off during
//! ordinary mid-edit states. The intersection is what makes it precise.
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
/// the plain `net10.0` case the `MSBuildUserExtensionsPath` seeding makes
/// certain — the opaque walk used to leave it untrusted and refuse.
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
