//! End-to-end for fsproj stage 3.3b: an F# project that `<ProjectReference>`s a
//! sibling C# project gains that project's types in its runtime `AssemblyEnv`.
//!
//! Unlike `csharp_sidecar_bundled_e2e` (which drives the sidecar directly), this
//! exercises the *real runtime path*: `SemanticState::assembly_env_for_project`
//! reads the restored `project.assets.json`, discovers the `.csproj` reference,
//! spawns the sidecar via `SidecarManager` (C# project references opted into,
//! as a host that trusts the workspace would), and folds the emitted metadata
//! DLL into the env. We then assert the C# type is resolvable through the env.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::server::State;
use borzoi::uri::UriExt as _;
use borzoi::workspace::Workspace;
use borzoi_sema::AssemblyEnv;
use borzoi_spawn::BoundedCommand;
use lsp_types::{FileChangeType, FileEvent, Uri};

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
    // C# project references are read only when the host opts in.
    sema.set_csharp_project_references_enabled(true);
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

/// C# references keep their event-driven invalidation
/// (`docs/incremental-lsp-plan.md`, Stage 6b): the sidecar reads the C#
/// sources itself, so nothing records them. A `.cs` edit and its watch event
/// re-read the edited project's metadata into the env of the F# project
/// that references it, and leave the env of an F#-only project, which
/// depends on nothing the sidecar read, the same allocation.
#[test]
fn a_cs_edit_and_its_event_rebuild_only_the_env_with_a_csharp_edge() {
    let dotnet = find_dotnet();
    let (fsproj, csharp_dir) = make_fixture();
    let other = fsproj
        .parent()
        .and_then(Path::parent)
        .expect("the fixture root")
        .join("other/Other.fsproj");
    std::fs::create_dir_all(other.parent().unwrap()).expect("mkdir other");
    std::fs::write(
        &other,
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "  </PropertyGroup>\n",
            "  <ItemGroup>\n",
            "    <Compile Include=\"Other.fs\" />\n",
            "  </ItemGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write Other.fsproj");
    std::fs::write(
        other.with_file_name("Other.fs"),
        "module Other\n\nlet x = 1\n",
    )
    .expect("write Other.fs");
    for project in [&fsproj, &other] {
        let mut cmd = Command::new(&dotnet);
        cmd.arg("restore").arg(project);
        BoundedCommand::new(cmd)
            .timeout(Duration::from_secs(1800))
            .run_ok(format_args!("`dotnet restore` of {}", project.display()));
    }

    let mut state = State::new();
    if let Some(root) = std::env::var_os("DOTNET_ROOT").map(PathBuf::from) {
        state.workspace = Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(root),
            ..SdkDiscoveryEnv::default()
        });
    }
    state.semantic.set_csharp_project_references_enabled(true);
    let env_of = |state: &mut State, project: &Path| -> Arc<AssemblyEnv> {
        let root = state.workspace.dotnet_root_for_project(project);
        assert!(root.is_some(), "run inside `nix develop`");
        let tfm = state.workspace.served_tfm_for_project(project);
        state
            .semantic
            .assembly_env_for_project(project, root.as_deref(), &tfm, &state.workspace)
    };
    let app = env_of(&mut state, &fsproj);
    let fsharp_only = env_of(&mut state, &other);
    assert!(
        app.has_namespace(&["BundledE2E".to_string()]),
        "precondition: the C# reference's namespace is in App's env"
    );
    assert!(
        !app.has_namespace(&["BundledE2E".to_string(), "Edited".to_string()]),
        "precondition: the edited namespace is not there yet"
    );

    let source = csharp_dir.join("Lib.cs");
    let edited = format!(
        "{}\nnamespace BundledE2E.Edited\n{{\n    public sealed class Added {{ }}\n}}\n",
        std::fs::read_to_string(&source)
            .expect("read Lib.cs")
            .replacen("namespace BundledE2E;", "namespace BundledE2E\n{", 1)
            + "}\n"
    );
    std::fs::write(&source, edited).expect("rewrite Lib.cs");
    state.apply_watched_changes(&[FileEvent {
        uri: Uri::from_file_path(&source).expect("an absolute path"),
        typ: FileChangeType::CHANGED,
    }]);

    let fsharp_only_after = env_of(&mut state, &other);
    let app_after = env_of(&mut state, &fsproj);
    assert!(
        Arc::ptr_eq(&fsharp_only, &fsharp_only_after),
        "a `.cs` edit rebuilt the env of a project with no C# edge"
    );
    assert!(
        app_after.has_namespace(&["BundledE2E".to_string(), "Edited".to_string()]),
        "the edited C# source's new namespace is not in the referencing env"
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
