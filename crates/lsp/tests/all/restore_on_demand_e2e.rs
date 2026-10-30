//! End-to-end for the on-demand restore (nuget-restore-plan Slice 8): a real
//! `net10.0` project with **no** `obj/project.assets.json` must still get its
//! package + framework reference assemblies into the assembly env — obtained by
//! running a bounded, offline, scratch-redirected `dotnet restore` and reading
//! its result (see [`borzoi::restore`]), never touching the project's
//! `obj/`.
//!
//! The project pins `FSharp.Core` to a version vendored in the devshell's
//! `$NUGET_PACKAGES` (and disables the implicit reference so the pinned one is
//! the whole package set), so the restore is deterministic and offline.
//!
//! Requires the .NET SDK + the vendored NuGet cache — the Nix devShell provides
//! both.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use borzoi::server::State;

/// `FSharp.Core` version present in the devshell's vendored `$NUGET_PACKAGES`
/// (`fsharp.core/10.1.204/`). Pinned explicitly so the resolve is deterministic.
const FSHARP_CORE_VERSION: &str = "10.1.204";

fn namespace(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// Write an un-restored `net10.0` project pinning `FSharp.Core` to a
/// cache-vendored version (implicit reference disabled so the pin is the whole
/// package set). Returns the project directory (unique per `tag`); the caller
/// removes it. Asserts the precondition that no `project.assets.json` exists, so
/// the tests genuinely exercise the on-demand restore path.
fn write_unrestored_project(tag: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-nuget-{tag}-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("App.fsproj"),
        format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>
  <ItemGroup>
    <!-- `ExcludeAssets="contentFiles"` mirrors the SDK's own implicit
         FSharp.Core reference; the real restore handles it, and we read the
         result — this just keeps the fixture close to a real F# project. -->
    <PackageReference Include="FSharp.Core" Version="{FSHARP_CORE_VERSION}" ExcludeAssets="contentFiles" />
    <Compile Include="App.fs" />
  </ItemGroup>
</Project>
"#
        ),
    )
    .unwrap();
    std::fs::write(root.join("App.fs"), "module App\n\nlet answer = 42\n").unwrap();
    assert!(
        !root.join("obj").join("project.assets.json").exists(),
        "fixture must have no project.assets.json"
    );
    root
}

#[test]
fn on_demand_restore_populates_the_env_without_a_project_assets_json() {
    let root = write_unrestored_project("resolve");
    let fsproj = root.join("App.fsproj");

    let mut state = State::default();
    // On-demand restore is a trust opt-in, off by default; enable it for the test.
    state.semantic.set_on_demand_restore_enabled(true);
    let docs = HashMap::new();

    // Resolving the project builds and caches its assembly env; with no assets
    // file this runs the on-demand restore.
    let resolved = state
        .semantic
        .resolved_project_for(&fsproj, &mut state.workspace, &docs);
    assert!(
        resolved.is_some(),
        "a plain net10.0 project must fold (see sdk_project_fold_e2e)"
    );

    // Retrieve the env the resolve just cached (same key: project + dotnet_root
    // + served TFM). This `&Workspace` entry point is a pure cache read here.
    let dotnet_root = state.workspace.dotnet_root_for_project(&fsproj);
    assert!(
        dotnet_root.is_some(),
        "test needs a resolvable dotnet_root — run under `nix develop`"
    );
    let tfm = state.workspace.served_tfm_for_project(&fsproj);
    let env = state.semantic.assembly_env_for_project(
        &fsproj,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );

    assert!(
        env.has_namespace(&namespace(&["Microsoft", "FSharp", "Core"])),
        "on-demand restore must fold FSharp.Core's compile assembly (env len = {})",
        env.len()
    );
    assert!(
        env.has_namespace(&namespace(&["System"])),
        "on-demand restore must fold the shared-framework ref pack (env len = {})",
        env.len()
    );
    // The restore ran against a scratch dir — the project's own `obj/` must be
    // untouched, so a later real build/restore starts clean.
    assert!(
        !root.join("obj").join("project.assets.json").exists(),
        "on-demand restore must not write the project's obj/"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// Unlike the old in-house resolve (which needed `resolved_project_for`'s
/// `&mut Workspace` to read the cached evaluation), the on-demand restore works
/// from the project path and environment alone — so the `&Workspace`
/// `assembly_env_for_project` entry point that handlers call can drive it
/// *directly*, with no prior `resolved_project_for`. This pins that: a first,
/// standalone `&Workspace` lookup on an un-restored project resolves the env.
#[test]
fn the_workspace_entry_point_restores_on_its_own() {
    let root = write_unrestored_project("standalone");
    let fsproj = root.join("App.fsproj");

    let mut state = State::default();
    // On-demand restore is a trust opt-in, off by default; enable it for the test.
    state.semantic.set_on_demand_restore_enabled(true);
    let dotnet_root = state.workspace.dotnet_root_for_project(&fsproj);
    assert!(
        dotnet_root.is_some(),
        "test needs a resolvable dotnet_root — run under `nix develop`"
    );
    let tfm = state.workspace.served_tfm_for_project(&fsproj);

    // No `resolved_project_for` first: the `&Workspace` entry point is the only
    // thing that runs, and it must still restore + populate the env.
    let env = state.semantic.assembly_env_for_project(
        &fsproj,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );
    assert!(
        env.has_namespace(&namespace(&["Microsoft", "FSharp", "Core"])),
        "the &Workspace entry point must restore FSharp.Core on its own (env len = {})",
        env.len()
    );
    assert!(
        !root.join("obj").join("project.assets.json").exists(),
        "on-demand restore must not write the project's obj/"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn on_demand_restore_is_off_by_default() {
    // The trust gate: without opting in, an assets-absent project must NOT be
    // restored — it degrades to an empty env, and crucially no `dotnet restore`
    // (which would execute the project's MSBuild targets) is run.
    let root = write_unrestored_project("default-off");
    let fsproj = root.join("App.fsproj");

    let mut state = State::default(); // on-demand restore NOT enabled
    let docs = HashMap::new();
    state
        .semantic
        .resolved_project_for(&fsproj, &mut state.workspace, &docs);

    let dotnet_root = state.workspace.dotnet_root_for_project(&fsproj);
    let tfm = state.workspace.served_tfm_for_project(&fsproj);
    let env = state.semantic.assembly_env_for_project(
        &fsproj,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );
    assert!(
        !env.has_namespace(&namespace(&["Microsoft", "FSharp", "Core"])),
        "with the opt-in off, an assets-absent project must not restore (env len = {})",
        env.len()
    );

    std::fs::remove_dir_all(&root).ok();
}

/// An un-restored `App` whose only route to `FSharp.Core` is a
/// `<ProjectReference>` to `lib/Lib.fsproj`, which pins it: `App`'s env has
/// `Microsoft.FSharp.Core` exactly when the restore kept the reference. Laid out
/// under `<root>/real`, with `<root>/link` a symlink to it. Returns the real
/// `<root>`; the caller removes it.
#[cfg(unix)]
fn write_unrestored_pair_behind_a_symlink(tag: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-nuget-{tag}-{unique}"));
    let real = root.join("real");
    std::fs::create_dir_all(real.join("lib")).unwrap();
    // By its real path, so `<root>/real` is the only symlink-free spelling.
    let root = std::fs::canonicalize(&root).unwrap();
    let real = root.join("real");
    std::fs::write(
        real.join("App.fsproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
    <ProjectReference Include="lib/Lib.fsproj" />
  </ItemGroup>
</Project>
"#,
    )
    .unwrap();
    std::fs::write(real.join("App.fs"), "module App\n").unwrap();
    std::fs::write(
        real.join("lib").join("Lib.fsproj"),
        format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="FSharp.Core" Version="{FSHARP_CORE_VERSION}" />
    <Compile Include="Lib.fs" />
  </ItemGroup>
</Project>
"#
        ),
    )
    .unwrap();
    std::fs::write(real.join("lib").join("Lib.fs"), "module Lib\n").unwrap();
    std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
    root
}

/// Whether the env `state` serves for `fsproj` has (`System`, the framework)
/// and (`Microsoft.FSharp.Core`, flowed through the project reference).
fn framework_and_flowed(state: &mut State, fsproj: &std::path::Path) -> (bool, bool) {
    let dotnet_root = state.workspace.dotnet_root_for_project(fsproj);
    assert!(
        dotnet_root.is_some(),
        "test needs a resolvable dotnet_root — run under `nix develop`"
    );
    let tfm = state.workspace.served_tfm_for_project(fsproj);
    let env = state.semantic.assembly_env_for_project(
        fsproj,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );
    (
        env.has_namespace(&namespace(&["System"])),
        env.has_namespace(&namespace(&["Microsoft", "FSharp", "Core"])),
    )
}

/// A project named through a symlinked directory must not be served an env
/// that silently lacks what its `<ProjectReference>` brings. MSBuild runs a
/// project's targets with the process's working directory set to the project's
/// directory, which the OS reports by its real path, so `dotnet restore` of
/// that spelling resolves the reference by a spelling the evaluation never
/// used, and drops it: the restored framework is there, the flowed package is
/// not. Either the package is there too, or nothing is claimed.
#[cfg(unix)]
#[test]
fn a_symlink_spelt_project_never_loses_its_references_packages() {
    let root = write_unrestored_pair_behind_a_symlink("symlink-spelt");
    let mut state = State::default();
    state.semantic.set_on_demand_restore_enabled(true);

    let (framework, flowed) =
        framework_and_flowed(&mut state, &root.join("link").join("App.fsproj"));
    assert!(
        !framework || flowed,
        "an env with the framework but without the package `lib/Lib.fsproj` \
         brings has lost the project reference"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The control for the two symlink tests: by its real path, the fixture's
/// reference survives the restore and brings its package.
#[cfg(unix)]
#[test]
fn by_its_real_path_the_reference_brings_its_package() {
    let root = write_unrestored_pair_behind_a_symlink("symlink-control");
    let mut state = State::default();
    state.semantic.set_on_demand_restore_enabled(true);

    let (framework, flowed) =
        framework_and_flowed(&mut state, &root.join("real").join("App.fsproj"));
    assert!(
        framework && flowed,
        "framework: {framework}, flowed package: {flowed}"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// One file, two spellings: whatever the symlinked spelling is served must not
/// decide what the real one is. The real spelling restores with its reference
/// whichever spelling was asked about first.
#[cfg(unix)]
#[test]
fn the_real_spelling_keeps_its_reference_after_a_symlinked_one() {
    let root = write_unrestored_pair_behind_a_symlink("symlink-first");
    let mut state = State::default();
    state.semantic.set_on_demand_restore_enabled(true);

    framework_and_flowed(&mut state, &root.join("link").join("App.fsproj"));
    let (framework, flowed) =
        framework_and_flowed(&mut state, &root.join("real").join("App.fsproj"));
    assert!(
        framework && flowed,
        "by its real path the project restores with its reference \
         (framework: {framework}, flowed package: {flowed})"
    );

    std::fs::remove_dir_all(&root).ok();
}
