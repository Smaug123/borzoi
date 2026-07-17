//! End-to-end for the referenced-assembly file-watch class: a **sibling F#
//! project rebuild** is picked up by the entry project's assembly env once the
//! client reports the rewritten output DLL via
//! `workspace/didChangeWatchedFiles`.
//!
//! This is the loop the unit tests can't close: `server.rs` proves a watched
//! `.dll` event drops the env cache (Arc identity), and `semantic.rs` proves
//! the env is built from the DLLs on disk — this test proves the *composition*
//! against real binaries: prime the env over a built sibling, rebuild the
//! sibling with a changed public surface, deliver the DLL event, and observe
//! the new surface (and only the new surface) in the refreshed env.
//!
//! Requires the .NET SDK on PATH — run under `nix develop`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::server::State;
use borzoi_spawn::BoundedCommand;
use lsp_types::{FileChangeType, FileEvent, Url};

#[test]
fn sibling_rebuild_refreshes_the_assembly_env_after_watched_dll_change() {
    let (app, lib_source, lib_dll) = make_fixture();
    build(&app);

    let mut state = State::default();
    let dotnet_root = state.workspace.dotnet_root_for_project(&app);
    assert!(
        dotnet_root.is_some(),
        "test needs a resolvable dotnet_root (`dotnet` on PATH / DOTNET_ROOT) — run under `nix develop`"
    );
    let tfm = state.workspace.served_tfm_for_project(&app);

    // Prime: the entry's env sees the sibling's V1 surface.
    let env = state.semantic.assembly_env_for_project(
        &app,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );
    assert!(
        env.has_namespace(&["RefreshV1".to_string()]),
        "primed env must contain the sibling's V1 namespace (env len = {})",
        env.len()
    );

    // The sibling's public surface moves; a rebuild rewrites its output DLL.
    std::fs::write(&lib_source, lib_module("RefreshV2")).expect("rewrite sibling source");
    build(&app);
    assert!(lib_dll.is_file(), "rebuilt sibling output exists");

    // Without the watched change the env is (deliberately) still the cached V1
    // — this is the staleness the file-watch class exists to end.
    let stale = state.semantic.assembly_env_for_project(
        &app,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );
    assert!(
        stale.has_namespace(&["RefreshV1".to_string()]),
        "before the watched event the cached env still serves V1"
    );

    // The client reports the rewritten DLL.
    let republish = state.apply_watched_changes(&[FileEvent {
        uri: Url::from_file_path(&lib_dll).expect("dll path to URI"),
        typ: FileChangeType::CHANGED,
    }]);
    assert!(republish.is_empty(), "a DLL change republishes nothing");

    let refreshed = state.semantic.assembly_env_for_project(
        &app,
        dotnet_root.as_deref(),
        &tfm,
        &state.workspace,
    );
    assert!(
        refreshed.has_namespace(&["RefreshV2".to_string()]),
        "after the watched DLL change the env must see the rebuilt surface"
    );
    assert!(
        !refreshed.has_namespace(&["RefreshV1".to_string()]),
        "the old surface must be gone — a refresh that unions old and new \
         would mis-resolve"
    );
}

/// `App.fsproj` → `LibFs.fsproj` under a nonce'd tmp dir. Returns the entry
/// project, the sibling's source file (rewritten mid-test), and the sibling's
/// expected output DLL path.
fn make_fixture() -> (PathBuf, PathBuf, PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock running")
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("watch-refresh-{nonce}"));
    let lib_dir = root.join("LibFs");
    let app_dir = root.join("App");
    std::fs::create_dir_all(&lib_dir).expect("mkdir LibFs");
    std::fs::create_dir_all(&app_dir).expect("mkdir App");

    std::fs::write(
        lib_dir.join("LibFs.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
         <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n  \
         <ItemGroup>\n    <Compile Include=\"Lib.fs\" />\n  </ItemGroup>\n</Project>\n",
    )
    .expect("write LibFs.fsproj");
    let lib_source = lib_dir.join("Lib.fs");
    std::fs::write(&lib_source, lib_module("RefreshV1")).expect("write Lib.fs");

    std::fs::write(
        app_dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
         <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n  \
         <ItemGroup>\n    <Compile Include=\"App.fs\" />\n    \
         <ProjectReference Include=\"../LibFs/LibFs.fsproj\" />\n  </ItemGroup>\n</Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(app_dir.join("App.fs"), "module App\n\nlet main () = ()\n")
        .expect("write App.fs");

    let lib_dll = lib_dir.join("bin/Debug/net10.0/LibFs.dll");
    (app_dir.join("App.fsproj"), lib_source, lib_dll)
}

/// The sibling's source at a given namespace version: one public type under a
/// top-level namespace the test can probe with `AssemblyEnv::has_namespace`.
fn lib_module(ns: &str) -> String {
    format!("namespace {ns}\n\ntype Marker() =\n    member _.X = 1\n")
}

/// `dotnet build` (implicit restore) with Debug configuration — the config the
/// LSP's output locator serves ([`borzoi::BUILD_CONFIGURATION`]).
///
/// The deadline is generous: a cold build restores packages and compiles two
/// projects, which is legitimately minutes. It is there to stop a build that has
/// *stalled* — blocked on a NuGet lock held by a concurrent run in a sibling
/// worktree, say — from hanging the suite forever, not to police a slow one.
fn build(fsproj: &Path) {
    let mut cmd = Command::new("dotnet");
    cmd.args(["build", "-nologo", "-v:q", "-c", "Debug"])
        .arg(fsproj);
    BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(1800))
        .run_ok(format_args!(
            "`dotnet build` of {} (run under `nix develop`)",
            fsproj.display()
        ));
}
