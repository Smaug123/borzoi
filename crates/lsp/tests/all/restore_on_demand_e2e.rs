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
