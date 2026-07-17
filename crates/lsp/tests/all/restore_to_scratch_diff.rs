//! Differential: the on-demand **scratch** restore vs a normal **`obj/`**
//! restore (nuget-restore-plan Slice 8). `restore_to_scratch_assemblies` runs
//! `dotnet restore` with its output redirected to a throwaway directory and its
//! sources cleared (offline); this pins that those two knobs do not change the
//! *result* — the compile DLL set it computes must be **identical** to what a
//! plain `dotnet restore` into the project's own `obj/` produces and the LSP
//! reads back. It also guards the invariant that the scratch restore never
//! writes the project's `obj/`.
//!
//! The vendored cache holds only `FSharp.Core` and the framework packs, so the
//! shapes are FSharp.Core-based: the default (implicit) reference and an
//! explicit pin. Richer package graphs need more vendored packages.
//!
//! Requires the .NET SDK + the vendored NuGet cache — the Nix devShell provides
//! both.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::project_assets::resolve_assemblies_for_tfm;
use borzoi::restore::{RestoreOutcome, restore_to_scratch_assemblies};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::workspace::Workspace;
use borzoi_spawn::BoundedCommand;

/// A version of `FSharp.Core` vendored in the devshell's `$NUGET_PACKAGES`.
const FSHARP_CORE_VERSION: &str = "10.1.204";

fn dotnet_root() -> PathBuf {
    std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset — run under `nix develop`")
}

/// A plain `dotnet restore` into the project's own `obj/` (the ordinary flow the
/// LSP reads back post-restore).
fn restore_to_obj(project: &Path) {
    let mut cmd = Command::new("dotnet");
    cmd.args(["restore", "-nologo"]).arg(project);
    BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(600))
        .run_ok(format_args!(
            "`dotnet restore` of {} (run under `nix develop`)",
            project.display()
        ));
}

/// Canonicalise a DLL list into a set, so the two sides are compared by the
/// on-disk files they name, not by incidental path spelling.
fn canonical_set(dlls: &[PathBuf]) -> BTreeSet<PathBuf> {
    dlls.iter()
        .map(|dll| std::fs::canonicalize(dll).unwrap_or_else(|_| dll.clone()))
        .collect()
}

/// The compile DLL set read from the project's own `obj/project.assets.json`
/// after a normal restore — what the LSP's assets-present path yields.
fn obj_dll_set(project: &Path, root: &Path, tfm: &str) -> BTreeSet<PathBuf> {
    let assets = project
        .parent()
        .expect("project has a parent")
        .join("obj")
        .join("project.assets.json");
    let resolved = resolve_assemblies_for_tfm(&assets, root, tfm)
        .expect("restored project.assets.json resolves");
    let mut dlls = resolved.package_dlls;
    dlls.extend(resolved.framework_dlls);
    canonical_set(&dlls)
}

/// Write `App.fsproj` (+ a source file) with the given `<ItemGroup>` body into a
/// fresh scratch dir, and return the project path.
fn write_project(tag: &str, item_group: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-scratch-diff-{tag}-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("App.fsproj"),
        format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
{item_group}
  </ItemGroup>
</Project>
"#
        ),
    )
    .unwrap();
    std::fs::write(root.join("App.fs"), "module App\n\nlet answer = 42\n").unwrap();
    root.join("App.fsproj")
}

/// Assert the scratch restore reproduces the `obj/` restore's DLL set exactly,
/// and never wrote the project's `obj/`.
fn assert_scratch_matches_obj(project: &Path) {
    let root = dotnet_root();
    let mut workspace = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
    let served = workspace.served_tfm_for_project(project);
    let tfm = served
        .as_deref()
        .expect("a net10.0 project has a served TFM")
        .to_string();

    // Scratch side first, so it runs against an un-restored project (no obj/).
    let scratch =
        match restore_to_scratch_assemblies(project, &root, &served, Some(workspace.env())) {
            RestoreOutcome::Resolved(resolved) => resolved,
            RestoreOutcome::Declined => panic!("the scratch restore declined a warm-cache project"),
            RestoreOutcome::TransientFailure => panic!("the scratch restore failed transiently"),
        };
    let mut scratch_dlls = scratch.package_dlls;
    scratch_dlls.extend(scratch.framework_dlls);
    let scratch_set = canonical_set(&scratch_dlls);
    assert!(
        !project.parent().unwrap().join("obj").exists(),
        "the scratch restore must not create the project's obj/"
    );

    // obj/ side: a real restore into the project, read the way the LSP reads it.
    restore_to_obj(project);
    let obj_set = obj_dll_set(project, &root, &tfm);
    assert!(
        !obj_set.is_empty(),
        "restore should have produced compile DLLs"
    );

    assert_eq!(
        scratch_set,
        obj_set,
        "scratch-restore DLL set must equal the obj/-restore set\n\
         only in scratch: {:?}\n only in obj: {:?}",
        scratch_set.difference(&obj_set).collect::<Vec<_>>(),
        obj_set.difference(&scratch_set).collect::<Vec<_>>(),
    );
}

/// Write a multi-project entry: `App.fsproj` (implicit FSharp.Core) with a
/// `<ProjectReference>` to a **package-less** `lib/Lib.fsproj`
/// (`DisableImplicitFSharpCoreReference`). Returns the entry path.
///
/// This is the shape that broke the naive single shared extensions path: with
/// `RestoreRecursive` on, the reference restores into the same
/// `project.assets.json` and its (FSharp.Core-less) result can be read as the
/// entry's — which the DLL-set comparison catches, since the entry has
/// FSharp.Core and the reference does not.
fn write_multi_project(tag: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-scratch-multi-{tag}-{unique}"));
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("App.fsproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
    <ProjectReference Include="lib/Lib.fsproj" />
  </ItemGroup>
</Project>
"#,
    )
    .unwrap();
    std::fs::write(root.join("App.fs"), "module App\n\nlet answer = 42\n").unwrap();
    std::fs::write(
        root.join("lib").join("Lib.fsproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>
  <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
</Project>
"#,
    )
    .unwrap();
    std::fs::write(root.join("lib").join("Lib.fs"), "module Lib\n\nlet v = 1\n").unwrap();
    root.join("App.fsproj")
}

#[test]
fn multi_project_entry_reads_its_own_assets_not_a_dependencys() {
    // Regression for the shared-extensions-path collision: the entry has
    // FSharp.Core, the referenced project does not, so reading the reference's
    // assets instead of the entry's would drop FSharp.Core and fail the match.
    let entry = write_multi_project("refs");
    assert_scratch_matches_obj(&entry);
    std::fs::remove_dir_all(entry.parent().unwrap()).ok();
}

#[test]
fn default_fsharp_project_matches_obj_restore() {
    // The common case: the SDK's implicit FSharp.Core reference, no explicit
    // package items.
    let project = write_project("default", "");
    assert_scratch_matches_obj(&project);
    std::fs::remove_dir_all(project.parent().unwrap()).ok();
}

#[test]
fn explicit_pinned_reference_matches_obj_restore() {
    let project = write_project(
        "explicit",
        &format!(
            "    <PackageReference Include=\"FSharp.Core\" Version=\"{FSHARP_CORE_VERSION}\" />"
        ),
    );
    let text = std::fs::read_to_string(&project).unwrap().replace(
        "<TargetFramework>net10.0</TargetFramework>",
        "<TargetFramework>net10.0</TargetFramework>\n    \
         <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>",
    );
    std::fs::write(&project, text).unwrap();
    assert_scratch_matches_obj(&project);
    std::fs::remove_dir_all(project.parent().unwrap()).ok();
}
