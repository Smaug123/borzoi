//! End-to-end test for Stage 8 of `docs/ifdef-plan.md`.
//!
//! Builds a tempdir-backed F# project whose `.fsproj` sets
//! `<DefineConstants>FOO</DefineConstants>`, and a source file with the
//! shape
//!
//! ```fsharp
//! #if FOO
//! let x = 1
//! #else
//! let y = "unterminated
//! #endif
//! ```
//!
//! With `FOO` defined the unterminated string lives on a dead branch and
//! must produce zero diagnostics. Dropping `FOO` from `DefineConstants`
//! flips the active branch and the diagnostic should appear.
//!
//! The test exercises the runtime data path: project lookup → cached
//! evaluation → symbol-set merge → `lex_with_symbols` → diagnostic
//! generation. It deliberately uses an SDK-less `<Project>` (no
//! `Sdk="..."` attribute) so the test runner doesn't need a working
//! `dotnet` SDK on PATH — Stage 8b will wire `SdkResolver` for real
//! user projects.

use std::fs;
use std::path::{Path, PathBuf};

use borzoi::diagnostics::diagnostics_for;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::workspace::Workspace;
use tempfile::TempDir;

const SOURCE_WITH_BAD_ELSE: &str = "\
#if FOO
let x = 1
#else
let y = \"unterminated
#endif
";

/// Variant of [`SOURCE_WITH_BAD_ELSE`] keyed on `FROM_LOCAL` so the
/// repo-local-SDK integration tests can't accidentally pass through a
/// stray `FOO` body define.
const SOURCE_WITH_BAD_ELSE_FROM_LOCAL: &str = "\
#if FROM_LOCAL
let x = 1
#else
let y = \"unterminated
#endif
";

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn fsproj(defines: &str) -> String {
    format!(
        "<Project>\n  <PropertyGroup>\n    <DefineConstants>{defines}</DefineConstants>\n  </PropertyGroup>\n</Project>\n"
    )
}

#[test]
fn active_branch_selection_suppresses_dead_branch_diagnostics() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("Sample.fsproj");
    let file = tmp.path().join("Lib.fs");
    write(&proj, &fsproj("FOO"));
    write(&file, SOURCE_WITH_BAD_ELSE);

    let mut ws = Workspace::new();
    let symbols = ws.symbols_for(&file);
    assert!(
        symbols.contains("FOO"),
        "FOO should be in the symbol set, got {symbols:?}"
    );

    let diags = diagnostics_for(SOURCE_WITH_BAD_ELSE, &symbols);
    assert!(
        diags.is_empty(),
        "expected no diagnostics with FOO defined (dead branch); got {diags:#?}"
    );
}

#[test]
fn dropping_define_exposes_dead_branch_diagnostic() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("Sample.fsproj");
    let file = tmp.path().join("Lib.fs");
    // No DefineConstants → `#if FOO` is inactive → `#else` arm is live.
    write(&proj, &fsproj(""));
    write(&file, SOURCE_WITH_BAD_ELSE);

    let mut ws = Workspace::new();
    let symbols = ws.symbols_for(&file);
    assert!(
        !symbols.contains("FOO"),
        "FOO should NOT be in the symbol set, got {symbols:?}"
    );

    let diags = diagnostics_for(SOURCE_WITH_BAD_ELSE, &symbols);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("unterminated string")),
        "expected an unterminated-string diagnostic from the live `#else` branch, got {diags:#?}"
    );
}

#[test]
fn sdk_supplied_define_constants_suppress_dead_branch_diagnostics() {
    // Stage 8b.1 end-to-end: an `<Sdk="...">` whose stubbed
    // Sdk.props contributes `FOO` to `DefineConstants` must suppress
    // the unterminated string in the dead `#else` arm, exactly as a
    // body-level `<DefineConstants>FOO</DefineConstants>` does
    // (covered by `active_branch_selection_suppresses_dead_branch_diagnostics`).
    //
    // The SDK is faked in a tempdir; the test injects the stub path
    // via `SdkDiscoveryEnv.dotnet_root` so we don't depend on a real
    // .NET install. Pre-Stage 8b.1 this test fails because
    // `Workspace` would call `parse_fsproj_with_imports` with
    // `sdk_resolver=None`, leaving `FOO` undefined.
    let tmp = TempDir::new().unwrap();
    let dotnet = tmp.path().join("dotnet");
    let sdk_root = dotnet
        .join("sdk")
        .join("8.0.401")
        .join("Sdks")
        .join("Test.Sdk")
        .join("Sdk");
    fs::create_dir_all(&sdk_root).unwrap();
    fs::write(
        sdk_root.join("Sdk.props"),
        "<Project><PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup></Project>",
    )
    .unwrap();
    fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();

    let project_dir = tmp.path().join("proj");
    let proj = project_dir.join("Sample.fsproj");
    let file = project_dir.join("Lib.fs");
    write(&proj, r#"<Project Sdk="Test.Sdk"></Project>"#);
    write(&file, SOURCE_WITH_BAD_ELSE);

    let env = SdkDiscoveryEnv {
        dotnet_root: Some(dotnet),
        ..SdkDiscoveryEnv::default()
    };
    let mut ws = Workspace::with_env(env);
    let symbols = ws.symbols_for(&file);
    assert!(
        symbols.contains("FOO"),
        "SDK should have contributed FOO, got {symbols:?}"
    );

    let diags = diagnostics_for(SOURCE_WITH_BAD_ELSE, &symbols);
    assert!(
        diags.is_empty(),
        "expected no diagnostics — SDK-supplied FOO should kill the dead-branch error; got {diags:#?}"
    );
}

/// Build the shared fixture for the `sdk.paths` integration tests:
///
/// ```text
/// <tmp>/
///   global.json                    ← sdk.paths value = `paths_json`
///   dotnet/                        ← env DOTNET_ROOT (empty dir)
///   dotnet-local/                  ← repo-local SDK contributing FROM_LOCAL
///     sdk/8.0.401/Sdks/Test.Sdk/Sdk/Sdk.{props,targets}
///   proj/
///     Sample.fsproj                ← <Project Sdk="Test.Sdk"/>
///     Lib.fs                       ← SOURCE_WITH_BAD_ELSE_FROM_LOCAL
/// ```
///
/// `paths_json` is the JSON array literal for `sdk.paths` (e.g.
/// `r#"["./dotnet-local", "$host$"]"#` or `"[]"`). The empty
/// `<tmp>/dotnet` directory is a valid root: `locate_dotnet_sdk`
/// returns `NotFound` from a directory with no `sdk/` subtree, so
/// `$host$` falls through cleanly to the next entry rather than
/// short-circuiting resolution.
fn build_paths_fixture(tmp: &TempDir, paths_json: &str) -> (PathBuf, SdkDiscoveryEnv) {
    let local_sdk_root = tmp
        .path()
        .join("dotnet-local")
        .join("sdk")
        .join("8.0.401")
        .join("Sdks")
        .join("Test.Sdk")
        .join("Sdk");
    fs::create_dir_all(&local_sdk_root).unwrap();
    fs::write(
        local_sdk_root.join("Sdk.props"),
        "<Project><PropertyGroup>\
         <DefineConstants>FROM_LOCAL</DefineConstants>\
         </PropertyGroup></Project>",
    )
    .unwrap();
    fs::write(local_sdk_root.join("Sdk.targets"), "<Project/>").unwrap();

    let host = tmp.path().join("dotnet");
    fs::create_dir_all(&host).unwrap();

    write(
        &tmp.path().join("global.json"),
        &format!(r#"{{ "sdk": {{ "paths": {paths_json} }} }}"#),
    );

    let project_dir = tmp.path().join("proj");
    let proj = project_dir.join("Sample.fsproj");
    let file = project_dir.join("Lib.fs");
    write(&proj, r#"<Project Sdk="Test.Sdk"></Project>"#);
    write(&file, SOURCE_WITH_BAD_ELSE_FROM_LOCAL);

    let env = SdkDiscoveryEnv {
        dotnet_root: Some(host),
        ..SdkDiscoveryEnv::default()
    };
    (file, env)
}

#[test]
fn sdk_paths_resolves_repo_local_sdk_for_defines() {
    // Stage 8b.2b end-to-end: a `global.json` `sdk.paths` list naming
    // a repo-local install before `$host$` must drive
    // `Microsoft.NET.Sdk`-style imports through *that* root, so a
    // `<DefineConstants>FROM_LOCAL</DefineConstants>` baked into the
    // repo-local SDK's `Sdk.props` reaches `lex_with_symbols` and
    // suppresses the bad `#else` arm.
    //
    // The companion test `sdk_paths_empty_list_drops_repo_local_define`
    // pins the inverse — flipping `paths` to `[]` removes
    // `FROM_LOCAL` and re-exposes the diagnostic.
    let tmp = TempDir::new().unwrap();
    let (file, env) = build_paths_fixture(&tmp, r#"["./dotnet-local", "$host$"]"#);

    let mut ws = Workspace::with_env(env);
    let symbols = ws.symbols_for(&file);
    assert!(
        symbols.contains("FROM_LOCAL"),
        "repo-local SDK should have contributed FROM_LOCAL, got {symbols:?}"
    );

    let diags = diagnostics_for(SOURCE_WITH_BAD_ELSE_FROM_LOCAL, &symbols);
    assert!(
        diags.is_empty(),
        "expected no diagnostics — FROM_LOCAL should kill the `#else` arm; got {diags:#?}"
    );
}

#[test]
fn sdk_paths_empty_list_drops_repo_local_define() {
    // Same fixture as `sdk_paths_resolves_repo_local_sdk_for_defines`
    // but with `paths: []` — the explicit opt-out from
    // `docs/ifdef-plan.md`'s "Empty-roots semantics". The resolver
    // returns `NotFound` for every lookup, so the SDK's
    // `<DefineConstants>FROM_LOCAL</DefineConstants>` never reaches
    // the project; the body has no defines of its own; the file's
    // `#if FROM_LOCAL` arm goes inactive and the bad `#else` arm
    // surfaces as one unterminated-string diagnostic.
    let tmp = TempDir::new().unwrap();
    let (file, env) = build_paths_fixture(&tmp, "[]");

    let mut ws = Workspace::with_env(env);
    let symbols = ws.symbols_for(&file);
    assert!(
        !symbols.contains("FROM_LOCAL"),
        "paths: [] must opt out of every SDK root, so FROM_LOCAL must be absent; got {symbols:?}"
    );

    let diags = diagnostics_for(SOURCE_WITH_BAD_ELSE_FROM_LOCAL, &symbols);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("unterminated string")),
        "expected an unterminated-string diagnostic from the live `#else` branch, got {diags:#?}"
    );
}

#[test]
fn file_outside_any_project_uses_default_symbol_set() {
    // A file with no owning `.fsproj` gets only the implicit defines
    // (`{COMPILED, EDITING}`) — never `FOO`. The `#if FOO` arm is therefore
    // inactive and the bad `#else` is live.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("Lib.fs");
    write(&file, SOURCE_WITH_BAD_ELSE);

    let mut ws = Workspace::new();
    let symbols = ws.symbols_for(&file);
    assert!(
        !symbols.contains("FOO"),
        "FOO should NOT be defined outside any project, got {symbols:?}"
    );

    let diags = diagnostics_for(SOURCE_WITH_BAD_ELSE, &symbols);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("unterminated string")),
        "{diags:#?}"
    );
}
