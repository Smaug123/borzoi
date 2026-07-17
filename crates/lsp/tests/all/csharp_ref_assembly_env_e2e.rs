//! End-to-end for fsproj stage 3.3b: an F# project that `<ProjectReference>`s a
//! sibling C# project gains that project's types in its runtime `AssemblyEnv`.
//!
//! Unlike `csharp_sidecar_bundled_e2e` (which drives the sidecar directly), this
//! exercises the *real runtime path*: `SemanticState::assembly_env_for_project`
//! reads the restored `project.assets.json`, discovers the `.csproj` reference,
//! spawns the sidecar via `SidecarManager`, and folds the emitted metadata DLL
//! into the env. We then assert the C# type is resolvable through the env.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::workspace::Workspace;
use borzoi_spawn::BoundedCommand;

#[test]
fn assembly_env_includes_csharp_project_reference_types() {
    let dotnet = find_dotnet();
    let (fsproj, csharp_dir) = make_fixture();

    // Restore the F# entry: this walks the `<ProjectReference>` graph and lands
    // `obj/project.assets.json` for *both* the F# project (recording the C# ref
    // + its TFM) and the C# project (needed for the closure-TFM map).
    let mut cmd = Command::new(&dotnet);
    cmd.arg("restore").arg(&fsproj);
    // A cold restore fetches packages, which is legitimately minutes: the bound
    // is there to stop a *stalled* restore (blocked on a NuGet lock held by a
    // concurrent run in a sibling worktree, say) from hanging the suite forever,
    // not to police a slow one.
    BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(1800))
        .run_ok("`dotnet restore` of the fixture");
    let _ = csharp_dir; // restored transitively via the fsproj graph.

    // A workspace wired to the real SDK so `dotnet_root_for_project` resolves the
    // framework packs (and the sidecar's `dotnet`). Mirrors the hover e2e setup.
    let dotnet_root = std::env::var_os("DOTNET_ROOT").map(PathBuf::from);
    let mut workspace = match &dotnet_root {
        Some(root) => Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(root.clone()),
            ..SdkDiscoveryEnv::default()
        }),
        None => Workspace::with_env(SdkDiscoveryEnv::from_process_env()),
    };
    let resolved_root = workspace.dotnet_root_for_project(&fsproj);
    assert!(
        resolved_root.is_some(),
        "test needs a resolvable dotnet_root (DOTNET_ROOT or `dotnet` on PATH) — run inside `nix develop`"
    );

    let target_framework = workspace.served_tfm_for_project(&fsproj);
    let mut sema = SemanticState::new();
    let env = sema.assembly_env_for_project(
        &fsproj,
        resolved_root.as_deref(),
        &target_framework,
        &workspace,
    );

    assert!(
        env.has_namespace(&["BundledE2E".to_string()]),
        "expected the C# reference's `BundledE2E` namespace (via the sidecar) in \
         the assembly env; env len = {}",
        env.len()
    );
}

/// Materialises an `fsharp/App.fsproj` + `csharp/Lib.csproj` pair under the
/// target tmp dir, with a fresh nonce so the sidecar's content-addressed cache
/// misses. Returns `(fsproj_path, csharp_dir)`.
fn make_fixture() -> (PathBuf, PathBuf) {
    let nonce = unique_nonce();
    let root =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("csharp-ref-env-e2e-{nonce}"));
    let csharp = root.join("csharp");
    let fsharp = root.join("fsharp");
    std::fs::create_dir_all(&csharp).expect("mkdir csharp");
    std::fs::create_dir_all(&fsharp).expect("mkdir fsharp");

    std::fs::write(
        csharp.join("Lib.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>BundledE2E</RootNamespace>\n",
            "    <AssemblyName>BundledE2E.Lib</AssemblyName>\n",
            "    <Nullable>enable</Nullable>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write Lib.csproj");
    std::fs::write(
        csharp.join("Lib.cs"),
        format!(
            "// nonce {nonce}\nnamespace BundledE2E;\n\npublic sealed class Greeter\n{{\n    public string Hello() => \"hi\";\n}}\n",
        ),
    )
    .expect("write Lib.cs");

    std::fs::write(
        fsharp.join("App.fsproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>BundledE2E.App</RootNamespace>\n",
            "  </PropertyGroup>\n",
            "  <ItemGroup>\n",
            "    <Compile Include=\"App.fs\" />\n",
            "    <ProjectReference Include=\"..\\csharp\\Lib.csproj\" />\n",
            "  </ItemGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write App.fsproj");
    std::fs::write(
        fsharp.join("App.fs"),
        "module BundledE2E.App\n\nlet main () = ()\n",
    )
    .expect("write App.fs");

    (fsharp.join("App.fsproj"), csharp)
}

fn find_dotnet() -> PathBuf {
    let mut cmd = Command::new("dotnet");
    cmd.arg("--version");
    BoundedCommand::new(cmd)
        .run_ok("`dotnet --version` (the .NET SDK is required — run inside `nix develop`)");
    PathBuf::from("dotnet")
}

fn unique_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock running")
        .as_nanos()
}
