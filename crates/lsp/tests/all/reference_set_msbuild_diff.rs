//! Live MSBuild oracle for the fsproj stage-3.3 reference set.
//!
//! Stage 3.3 resolves an entry F# project's references from four sources:
//! the assets file's package + framework DLLs (3.3 base), F#
//! `<ProjectReference>` output DLLs (3.3a), and C# `<ProjectReference>`
//! metadata DLLs via the sidecar (3.3b), under the entry-TFM selection policy
//! (3.3c); the project-reference *edges* driving the last two come from the
//! parsed `<ProjectReference>` graph, not the assets file (plan E1).
//! Each source has its own unit / e2e coverage, but nothing pinned the
//! *composed set* against ground truth. This test does: it builds synthetic
//! mixed-language project trees in a tempdir and diffs
//! [`SemanticState::reference_dlls_for_project`] — the exact DLL list the
//! runtime assembly env is built from — against MSBuild's own resolved
//! reference set (`dotnet msbuild -t:ResolveReferences -getItem:ReferencePath`)
//! for the same entry project.
//!
//! ## Comparison currency: assembly simple names
//!
//! The two sides intentionally disagree on *paths*: a C# reference resolves on
//! our side to the sidecar's content-addressed metadata DLL
//! (`obj/borzoi/csharp-sidecar/<hash>.dll` — the file stem is a hash,
//! not a name), while MSBuild points at the referenced project's build output.
//! What must agree is *which assemblies* the entry sees, so each side is
//! projected to the set of assembly **simple names**: ours by reading each
//! DLL's manifest identity ([`Ecma335Assembly::parse`] → `identity().name`),
//! MSBuild's from the item's `Filename` (MSBuild outputs are named after their
//! assembly). Compared as a *set*, not a multiset — "is assembly X referenced"
//! is the question; the env build tolerates a name reached via two paths.
//!
//! ## What a divergence means
//!
//! - **MSBuild-only names**: under-resolution — a reference the binder should
//!   see but won't (a missing project-ref fold, a dropped assets section).
//!   D5 tolerates this *degradation* at runtime, but in this oracle every
//!   fixture is fully restored and built, so there is nothing legitimate to
//!   degrade about: it is a bug.
//! - **Ours-only names**: fabrication — worse; the env claims an assembly the
//!   real build never referenced.
//!
//! ## Hermetic invocation
//!
//! Like `glob_msbuild_diff`, the `dotnet` children run with a stripped
//! environment (only `PATH`/`HOME`/`TMPDIR` and `DOTNET_*`/`NUGET_*`), and the
//! fixtures live under `CARGO_TARGET_TMPDIR` with no `global.json` /
//! `Directory.Build.*` above them, so MSBuild uses the host SDK and the
//! vendored NuGet feed. Requires the .NET SDK — run under `nix develop`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::workspace::Workspace;
use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_spawn::BoundedCommand;
use serde::Deserialize;

#[derive(Deserialize)]
struct MsbuildOutput {
    #[serde(rename = "Items")]
    items: MsbuildItems,
}

#[derive(Deserialize, Default)]
struct MsbuildItems {
    #[serde(default, rename = "ReferencePath")]
    reference_path: Vec<MsbuildItem>,
}

#[derive(Deserialize)]
struct MsbuildItem {
    #[serde(rename = "Filename")]
    filename: String,
}

/// Budget for one `dotnet` invocation here (a build, or an MSBuild evaluation
/// that restores and resolves references).
///
/// A cold run fetches packages and runs a compiler, which is legitimately
/// minutes, so the bound sits far above the driver's per-child default: it is
/// there to stop a run that has *stalled* — blocked on a NuGet lock held by a
/// concurrent run in a sibling worktree, say — from hanging the suite forever,
/// not to police a slow one.
const MSBUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// A `dotnet` command in `dir` with the stripped, reproducible environment.
fn dotnet_command(dir: &Path) -> Command {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(dir);
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
    cmd
}

/// `dotnet build` the entry project (implicit restore included): materialises
/// `obj/project.assets.json` for every project in the graph and the referenced
/// projects' output DLLs — the state stage 3.3 is specified against (a
/// restored, built tree; un-restored trees are the `RestoreStale` diagnostic's
/// domain, not this oracle's).
fn build_entry(fsproj: &Path) {
    let mut cmd = dotnet_command(fsproj.parent().unwrap());
    cmd.args(["build", "-nologo", "-v:q"]).arg(fsproj);
    BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!(
            "`dotnet build` of {} (run under `nix develop`)",
            fsproj.display()
        ));
}

/// Build the entry (implicit restore + the `Build` target) *and* read MSBuild's
/// resolved reference set, as assembly simple names, in a single `dotnet`
/// invocation.
///
/// `-restore -t:Build` does exactly what [`build_entry`] does — materialises
/// `obj/project.assets.json` and the referenced projects' output DLLs — while
/// `-getItem:ReferencePath` reports the set `ResolveReferences` (a `Build`
/// dependency) populated. Reporting it costs nothing extra: the resolve has
/// already run inside `Build`, so the separate `-t:ResolveReferences` evaluation
/// the entry used to pay was pure process overhead (a whole second `dotnet`
/// startup per fixture). The item JSON is written to a result file
/// (`-getResultOutputFile`) rather than stdout, so the interleaved build log
/// can't corrupt it. Leaves the tree built for [`our_reference_names`].
///
/// `ReferencePath` (populated by `ResolveReferences`) is the item fsc's command
/// line is derived from — `ReferencePathWithRefAssemblies` merely swaps each item
/// for its reference assembly where one exists, preserving membership and
/// filenames, so it suffices for a name-set comparison.
fn build_and_msbuild_reference_names(fsproj: &Path) -> BTreeSet<String> {
    let dir = fsproj.parent().unwrap();
    let result_file = dir.join("borzoi-reference-path.json");
    let mut cmd = dotnet_command(dir);
    cmd.args([
        "msbuild",
        "-nologo",
        "-restore",
        "-t:Build",
        "-getItem:ReferencePath",
    ])
    .arg(format!("-getResultOutputFile:{}", result_file.display()))
    .arg(fsproj);
    BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!(
            "`dotnet msbuild -restore -t:Build -getItem:ReferencePath` of {} \
             (run under `nix develop`)",
            fsproj.display()
        ));
    let json = std::fs::read_to_string(&result_file).unwrap_or_else(|e| {
        panic!(
            "read reference-path result file {}: {e}",
            result_file.display()
        )
    });
    let parsed: MsbuildOutput = serde_json::from_str(&json).unwrap_or_else(|e| {
        panic!(
            "could not parse msbuild getItem JSON for {}: {e}\n--- json ---\n{json}",
            fsproj.display()
        )
    });
    parsed
        .items
        .reference_path
        .into_iter()
        .map(|i| i.filename)
        .collect()
}

/// Our resolved reference set for the entry, as assembly simple names: the
/// real runtime composition ([`SemanticState::reference_dlls_for_project`] —
/// assets package/framework DLLs + F# project-ref outputs + C# sidecar
/// metadata DLLs), each DLL projected to its manifest assembly name. The
/// wiring (workspace env, `dotnet_root`, entry-TFM selection) mirrors the
/// server's own request path, so this measures what the LSP actually serves.
fn our_reference_names(fsproj: &Path) -> BTreeSet<String> {
    let mut workspace = match std::env::var_os("DOTNET_ROOT").map(PathBuf::from) {
        Some(root) => Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(root),
            ..SdkDiscoveryEnv::default()
        }),
        None => Workspace::with_env(SdkDiscoveryEnv::from_process_env()),
    };
    let dotnet_root = workspace.dotnet_root_for_project(fsproj);
    assert!(
        dotnet_root.is_some(),
        "test needs a resolvable dotnet_root (DOTNET_ROOT or `dotnet` on PATH) — run under `nix develop`"
    );
    let target_framework = workspace.served_tfm_for_project(fsproj);

    let mut sema = SemanticState::new();
    let dlls = sema.reference_dlls_for_project(
        fsproj,
        dotnet_root.as_deref(),
        &target_framework,
        &workspace,
    );
    assert!(
        !dlls.is_empty(),
        "our reference set for {} is empty — the fixture should at least resolve \
         the framework pack",
        fsproj.display()
    );
    dlls.iter()
        .map(|dll| {
            let bytes = std::fs::read(dll)
                .unwrap_or_else(|e| panic!("read referenced DLL {}: {e}", dll.display()));
            match Ecma335Assembly::parse(&bytes) {
                Ok(asm) => asm.identity().name.clone(),
                // A DLL our reader can't parse still *is* a composed reference;
                // name it by file stem so it stays visible to the diff rather
                // than vanishing (its stem equals its assembly name for every
                // non-sidecar DLL, and sidecar DLLs — the hash-named ones — are
                // Roslyn-emitted and always parse).
                Err(_) => dll
                    .file_stem()
                    .expect("reference DLL has a file stem")
                    .to_string_lossy()
                    .into_owned(),
            }
        })
        .collect()
}

/// Build the fixture's entry, then assert our composed reference set and
/// MSBuild's agree as assembly-name sets. `expected` are the fixture's own
/// library names: asserting they landed in the (already equal) sets keeps the
/// test non-vacuous — equality alone could also mean both sides missed a
/// fixture project, which would silently prove nothing about the ref folds.
fn assert_reference_sets_match(fsproj: &Path, expected: &[&str]) {
    let theirs = build_and_msbuild_reference_names(fsproj);
    let ours = our_reference_names(fsproj);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}\n\
         (ours: {} names, msbuild: {} names)",
        fsproj.display(),
        ours.len(),
        theirs.len(),
    );
    for name in expected {
        assert!(
            ours.contains(*name),
            "expected fixture reference {name:?} in the (matching) sets for {} — \
             the comparison would be vacuous for it",
            fsproj.display()
        );
    }
    // Every fixture is an SDK net10.0 project, so the framework pack and the
    // implicit FSharp.Core must both be present — a second vacuity guard.
    for name in ["FSharp.Core", "System.Runtime"] {
        assert!(ours.contains(name), "baseline reference {name:?} missing");
    }
}

/// A fresh fixture root under `CARGO_TARGET_TMPDIR`. Nonce-suffixed so reruns
/// (and the sidecar's per-workspace publish dirs) never collide across test
/// processes.
fn fixture_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock running")
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("refset-{label}-{nonce}"));
    std::fs::create_dir_all(&root).expect("mkdir fixture root");
    root
}

/// Write `<root>/<name>/<name>.fsproj` (TFM net10.0) with one `Compile` file
/// and the given `<ProjectReference>` includes (relative to the project dir).
fn write_fsproj(root: &Path, name: &str, refs: &[&str]) -> PathBuf {
    write_fsproj_ext(root, name, refs, "")
}

/// [`write_fsproj`] with extra raw `<PropertyGroup>` lines (already-indented
/// XML, e.g. an `<AssemblyName>` override).
fn write_fsproj_ext(root: &Path, name: &str, refs: &[&str], extra_props: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir project dir");
    let refs_xml: String = refs
        .iter()
        .map(|r| format!("    <ProjectReference Include=\"{r}\" />\n"))
        .collect();
    let fsproj = dir.join(format!("{name}.fsproj"));
    std::fs::write(
        &fsproj,
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
             \x20 <PropertyGroup>\n\
             \x20   <TargetFramework>net10.0</TargetFramework>\n\
             {extra_props}\
             \x20 </PropertyGroup>\n\
             \x20 <ItemGroup>\n\
             \x20   <Compile Include=\"{name}.fs\" />\n\
             {refs_xml}\
             \x20 </ItemGroup>\n\
             </Project>\n"
        ),
    )
    .expect("write fsproj");
    std::fs::write(
        dir.join(format!("{name}.fs")),
        format!("module {name}\n\nlet marker = 1\n"),
    )
    .expect("write fs source");
    fsproj
}

/// Write `<root>/<name>/<name>.csproj` (TFM net10.0, default compile globs)
/// with one C# source and the given `<ProjectReference>` includes.
fn write_csproj(root: &Path, name: &str, refs: &[&str]) -> PathBuf {
    write_csproj_ext(root, name, refs, "")
}

/// [`write_csproj`] with extra raw `<PropertyGroup>` lines.
fn write_csproj_ext(root: &Path, name: &str, refs: &[&str], extra_props: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir project dir");
    let refs_xml: String = refs
        .iter()
        .map(|r| {
            format!("  <ItemGroup>\n    <ProjectReference Include=\"{r}\" />\n  </ItemGroup>\n")
        })
        .collect();
    let csproj = dir.join(format!("{name}.csproj"));
    std::fs::write(
        &csproj,
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
             \x20 <PropertyGroup>\n\
             \x20   <TargetFramework>net10.0</TargetFramework>\n\
             {extra_props}\
             \x20 </PropertyGroup>\n\
             {refs_xml}\
             </Project>\n"
        ),
    )
    .expect("write csproj");
    std::fs::write(
        dir.join(format!("{name}.cs")),
        format!("namespace {name};\n\npublic sealed class Marker {{ public int X => 1; }}\n"),
    )
    .expect("write cs source");
    csproj
}

/// No references at all: pins the base composition — the framework pack and
/// the implicit FSharp.Core package — before any project-ref folding.
#[test]
fn plain_project_matches_msbuild() {
    let root = fixture_root("plain");
    let app = write_fsproj(&root, "App", &[]);
    assert_reference_sets_match(&app, &[]);
}

/// F# → F# → F# chain: pins 3.3a *including* the transitive edge — NuGet
/// flattens `<ProjectReference>` closures into the entry's assets, and the
/// .NET SDK makes project references transitive for compilation, so both
/// sides must surface `LibA` *and* `LibB`.
#[test]
fn fsharp_project_ref_chain_matches_msbuild() {
    let root = fixture_root("fs-chain");
    write_fsproj(&root, "LibB", &[]);
    write_fsproj(&root, "LibA", &["../LibB/LibB.fsproj"]);
    let app = write_fsproj(&root, "App", &["../LibA/LibA.fsproj"]);
    assert_reference_sets_match(&app, &["LibA", "LibB"]);
}

/// F# → C# → C# chain: pins 3.3b — the direct C# ref *and* its transitive C#
/// closure arrive via the sidecar's metadata DLLs, whose hash-named files
/// must still project to the right assembly names.
#[test]
fn csharp_project_ref_transitive_matches_msbuild() {
    let root = fixture_root("cs-chain");
    write_csproj(&root, "CsLibB", &[]);
    write_csproj(&root, "CsLibA", &["../CsLibB/CsLibB.csproj"]);
    let app = write_fsproj(&root, "App", &["../CsLibA/CsLibA.csproj"]);
    assert_reference_sets_match(&app, &["CsLibA", "CsLibB"]);
}

/// Mixed direct references: the F# fold (3.3a) and the C# sidecar fold (3.3b)
/// compose in one entry without dropping or duplicating either side.
#[test]
fn mixed_fsharp_and_csharp_refs_match_msbuild() {
    let root = fixture_root("mixed");
    write_fsproj(&root, "FsLib", &[]);
    write_csproj(&root, "CsLib", &[]);
    let app = write_fsproj(
        &root,
        "App",
        &["../FsLib/FsLib.fsproj", "../CsLib/CsLib.csproj"],
    );
    assert_reference_sets_match(&app, &["FsLib", "CsLib"]);
}

/// A C# ref overriding `<AssemblyName>`: the sidecar evaluates the real
/// project (Roslyn `MSBuildWorkspace`), so — unlike the F# locator below — the
/// override must resolve to the *overridden* assembly name, not the project
/// stem.
#[test]
fn csharp_assembly_name_override_matches_msbuild() {
    let root = fixture_root("cs-asmname");
    write_csproj_ext(
        &root,
        "CsLib",
        &[],
        "    <AssemblyName>RenamedCs</AssemblyName>\n",
    );
    let app = write_fsproj(&root, "App", &["../CsLib/CsLib.csproj"]);
    assert_reference_sets_match(&app, &["RenamedCs"]);
}

/// A `ReferenceOutputAssembly="false"` project reference is a build-order
/// dependency: MSBuild builds the target but keeps its output off
/// `ReferencePath`, and the compile-closure graph walk drops the edge — both
/// sides must therefore agree the tool project is *absent* while the normal
/// ref is present.
#[test]
fn fsharp_reference_output_assembly_false_matches_msbuild() {
    let root = fixture_root("fs-roa-false");
    write_fsproj(&root, "FsLib", &[]);
    write_fsproj(&root, "BuildTool", &[]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../FsLib/FsLib.fsproj\" />\n\
         \x20   <ProjectReference Include=\"../BuildTool/BuildTool.fsproj\" ReferenceOutputAssembly=\"false\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(ours.contains("FsLib"), "the normal ref must resolve");
    assert!(
        !ours.contains("BuildTool"),
        "a ReferenceOutputAssembly=false ref must not be a compile reference"
    );
}

/// `ReferenceOutputAssembly` is decided by the common targets'
/// `'%(...)'=='true'` (after empty defaults to `true`) — an MSBuild `==`, so
/// the boolean vocabulary counts as true while `"0"` falls through to string
/// comparison and is *stripped* despite looking truthy-adjacent. Pins the
/// probe (dotnet 10.0.301, 2026-07-10) live: `on` keeps the target on
/// `ReferencePath`, `0` removes it. Treating only literal `false` as
/// build-order-only would fabricate the `0` edge's DLL.
#[test]
fn fsharp_reference_output_assembly_vocabulary_matches_msbuild() {
    let root = fixture_root("fs-roa-vocab");
    write_fsproj(&root, "OnLib", &[]);
    write_fsproj(&root, "ZeroLib", &[]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../OnLib/OnLib.fsproj\" ReferenceOutputAssembly=\"on\" />\n\
         \x20   <ProjectReference Include=\"../ZeroLib/ZeroLib.fsproj\" ReferenceOutputAssembly=\"0\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(ours.contains("OnLib"), "`on` compares true to `true`");
    assert!(
        !ours.contains("ZeroLib"),
        "`0` is not an MSBuild boolean and must not be a compile reference"
    );
}

/// An `ExcludeAssets="compile"` project reference keeps the referenced
/// project's **own** output on `ReferencePath` (the build adds direct
/// `<ProjectReference>` outputs itself; the asset exclusion filters what
/// flows *through* the reference) while excluding the target's transitive
/// closure. Both sides must agree: `Mid` present, `Deep` absent.
#[test]
fn fsharp_exclude_assets_compile_keeps_direct_output_matches_msbuild() {
    let root = fixture_root("fs-excl-compile");
    write_fsproj(&root, "Deep", &[]);
    write_fsproj(&root, "Mid", &["../Deep/Deep.fsproj"]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../Mid/Mid.fsproj\" ExcludeAssets=\"compile\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(
        ours.contains("Mid"),
        "the excluded ref's own output must still be a compile reference"
    );
    assert!(
        !ours.contains("Deep"),
        "nothing flows through an ExcludeAssets=compile reference"
    );
}

/// `PrivateAssets="all"` on a **transitive** project reference: `Mid`
/// compiles against `Deep`, but nothing of `Deep` flows to `App` — following
/// the edge anyway would *fabricate* a reference the real build omits (the
/// worse divergence direction). Both sides must agree: `Mid` in, `Deep` out.
#[test]
fn fsharp_transitive_private_assets_matches_msbuild() {
    let root = fixture_root("fs-private-assets");
    write_fsproj(&root, "Deep", &[]);
    let mid_dir = root.join("Mid");
    std::fs::create_dir_all(&mid_dir).expect("mkdir Mid");
    std::fs::write(
        mid_dir.join("Mid.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"Mid.fs\" />\n\
         \x20   <ProjectReference Include=\"../Deep/Deep.fsproj\" PrivateAssets=\"all\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write Mid.fsproj");
    std::fs::write(mid_dir.join("Mid.fs"), "module Mid\n\nlet marker = 1\n").expect("write Mid.fs");
    let app = write_fsproj(&root, "App", &["../Mid/Mid.fsproj"]);
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(ours.contains("Mid"), "the direct ref must resolve");
    assert!(
        !ours.contains("Deep"),
        "a privately-referenced transitive project must not be fabricated"
    );
}

/// A `<ProjectReference Update>` writing `ReferenceOutputAssembly=false`
/// after the `Include`: MSBuild strips the reference; our evaluator doesn't
/// model item mutation, so it must refuse the un-mutated list (empty edge
/// set) rather than fold `Mid.dll` from it — both sides agree `Mid` is
/// absent.
#[test]
fn fsharp_project_reference_update_matches_msbuild() {
    let root = fixture_root("fs-ref-update");
    write_fsproj(&root, "Mid", &[]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../Mid/Mid.fsproj\" />\n\
         \x20   <ProjectReference Update=\"../Mid/Mid.fsproj\" ReferenceOutputAssembly=\"false\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(
        !ours.contains("Mid"),
        "an Update-disabled project reference must not be fabricated"
    );
}

/// `BuildReference="false"` and `Targets="Clean"` remove the target from
/// `ReferencePath` even with its DLL prebuilt (probed, dotnet 10). We don't
/// model either; the capture surfaces them as unmodelled significant
/// metadata and the walk drops the edge — both sides agree `Mid` is absent.
#[test]
fn fsharp_build_suppressing_reference_metadata_matches_msbuild() {
    for (label, metadata) in [
        ("fs-ref-buildref", "BuildReference=\"false\""),
        ("fs-ref-targets-clean", "Targets=\"Clean\""),
    ] {
        let root = fixture_root(label);
        let mid = write_fsproj(&root, "Mid", &[]);
        build_entry(&mid);
        let dir = root.join("App");
        std::fs::create_dir_all(&dir).expect("mkdir App");
        std::fs::write(
            dir.join("App.fsproj"),
            format!(
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
                 \x20 <PropertyGroup>\n\
                 \x20   <TargetFramework>net10.0</TargetFramework>\n\
                 \x20 </PropertyGroup>\n\
                 \x20 <ItemGroup>\n\
                 \x20   <Compile Include=\"App.fs\" />\n\
                 \x20   <ProjectReference Include=\"../Mid/Mid.fsproj\" {metadata} />\n\
                 \x20 </ItemGroup>\n\
                 </Project>\n"
            ),
        )
        .expect("write App.fsproj");
        std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
        let app = dir.join("App.fsproj");
        let theirs = build_and_msbuild_reference_names(&app);
        let ours = our_reference_names(&app);
        let missing: Vec<&String> = theirs.difference(&ours).collect();
        let extra: Vec<&String> = ours.difference(&theirs).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{metadata}: reference set diverges from MSBuild for {}\n\
             msbuild-only (we under-resolve): {missing:?}\n\
             ours-only (we fabricate): {extra:?}",
            app.display(),
        );
        assert!(
            !ours.contains("Mid"),
            "{metadata}: a build-suppressed reference must not be fabricated"
        );
    }
}

/// An **explicitly empty** `IncludeAssets=""` on a project reference is the
/// same as absent — the default (everything flows), NOT an empty allow-list
/// (probed, dotnet 10: the transitive Leaf still lands on `ReferencePath`).
/// Pins the `Known(None)` collapse of unset/empty/cleared in
/// `compile_edge_kind`: both sides must agree Mid AND Leaf are referenced.
#[test]
fn fsharp_empty_include_assets_matches_msbuild() {
    let root = fixture_root("fs-ref-empty-include-assets");
    write_fsproj(&root, "Leaf", &[]);
    write_fsproj(&root, "Mid", &["../Leaf/Leaf.fsproj"]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../Mid/Mid.fsproj\" IncludeAssets=\"\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(
        ours.contains("Mid") && ours.contains("Leaf"),
        "an explicitly empty IncludeAssets is the default, not an empty allow-list"
    );
}

/// An `<ItemDefinitionGroup>` default of `ReferenceOutputAssembly=false`:
/// MSBuild applies item-definition defaults to every `<ProjectReference>`
/// (pass 2 precedes pass 3) and strips the reference. We don't thread
/// defaults into captured items, so the evaluator must mark the list
/// uncertain and the walk must refuse it rather than fold `Mid.dll` — both
/// sides agree `Mid` is absent.
#[test]
fn fsharp_item_definition_default_matches_msbuild() {
    let root = fixture_root("fs-ref-item-definition");
    write_fsproj(&root, "Mid", &[]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemDefinitionGroup>\n\
         \x20   <ProjectReference>\n\
         \x20     <ReferenceOutputAssembly>false</ReferenceOutputAssembly>\n\
         \x20   </ProjectReference>\n\
         \x20 </ItemDefinitionGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../Mid/Mid.fsproj\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(
        !ours.contains("Mid"),
        "an item-definition-disabled project reference must not be fabricated"
    );
}

/// A `<ProjectReference Update>` writing `ReferenceOutputAssembly=false`
/// inside an `<ItemGroup>` whose `Condition` is a property function — legal
/// MSBuild (true in the real build), outside our condition grammar. MSBuild
/// executes the update and strips the reference; we skip the whole group,
/// so the evaluator must distrust the un-mutated Include rather than fold
/// `Mid.dll` from it — both sides agree `Mid` is absent.
#[test]
fn fsharp_condition_gated_reference_update_matches_msbuild() {
    let root = fixture_root("fs-ref-gated-update");
    write_fsproj(&root, "Mid", &[]);
    let dir = root.join("App");
    std::fs::create_dir_all(&dir).expect("mkdir App");
    std::fs::write(
        dir.join("App.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFramework>net10.0</TargetFramework>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"App.fs\" />\n\
         \x20   <ProjectReference Include=\"../Mid/Mid.fsproj\" />\n\
         \x20 </ItemGroup>\n\
         \x20 <ItemGroup Condition=\"$([MSBuild]::Add(1, 1)) == 2\">\n\
         \x20   <ProjectReference Update=\"../Mid/Mid.fsproj\" ReferenceOutputAssembly=\"false\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write App.fsproj");
    std::fs::write(dir.join("App.fs"), "module App\n\nlet marker = 1\n").expect("write App.fs");
    let app = dir.join("App.fsproj");
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "reference set diverges from MSBuild for {}\n\
         msbuild-only (we under-resolve): {missing:?}\n\
         ours-only (we fabricate): {extra:?}",
        app.display(),
    );
    assert!(
        !ours.contains("Mid"),
        "an update hidden behind an unsupported group condition must not be fabricated"
    );
}

/// A **restored, built** multi-targeted F# producer: producer-TFM recovery
/// seeds the graph walk and the output locator with NuGet's selection, so the
/// multi-TFM machinery (seeded walk + TFM-matched locate) composes to a set
/// that matches MSBuild exactly.
#[test]
fn fsharp_multi_target_ref_matches_msbuild() {
    let root = fixture_root("fs-multitfm");
    let dir = root.join("MultiLib");
    std::fs::create_dir_all(&dir).expect("mkdir MultiLib");
    std::fs::write(
        dir.join("MultiLib.fsproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n\
         \x20 <PropertyGroup>\n\
         \x20   <TargetFrameworks>net10.0;net8.0</TargetFrameworks>\n\
         \x20 </PropertyGroup>\n\
         \x20 <ItemGroup>\n\
         \x20   <Compile Include=\"MultiLib.fs\" />\n\
         \x20 </ItemGroup>\n\
         </Project>\n",
    )
    .expect("write MultiLib.fsproj");
    std::fs::write(
        dir.join("MultiLib.fs"),
        "module MultiLib\n\nlet marker = 1\n",
    )
    .expect("write MultiLib.fs");
    let app = write_fsproj(&root, "App", &["../MultiLib/MultiLib.fsproj"]);
    assert_reference_sets_match(&app, &["MultiLib"]);
}

/// **Pinned known gap**: an F# project referenced *by a C# project* breaks
/// that whole C# subtree out of our set. Two residuals compose here, both
/// under-resolutions (D5), neither fabrication:
///
/// - `CsMid` (pre-existing 3.3b limitation, unchanged by graph-sourced
///   edges — the sidecar drive's inputs are identical either way): the
///   sidecar pre-opens a Roslyn workspace for every project in the
///   closure-TFM map, and `MSBuildWorkspace` cannot load an `.fsproj` (no F#
///   language service), so the *boundary project's own* metadata build fails
///   and the ref is skipped.
/// - `FsLeaf` (graph-sourced edges): the parsed graph treats `.csproj` as a
///   terminal boundary (plan E3), so an F# project behind it is invisible to
///   the F# output-DLL fold. (Pre-graph, the flattened assets closure
///   happened to surface it.) MSBuild, by contrast, makes both transitive
///   `ReferencePath` items.
///
/// Closing this lives in the sidecar: skip (or metadata-reference) non-C#
/// closure members instead of hard-failing, and report them to the caller so
/// the F# locator can fold their outputs. When that lands, this flips to a
/// plain [`assert_reference_sets_match`].
#[test]
fn fsharp_behind_csharp_boundary_is_a_pinned_gap() {
    let root = fixture_root("fs-behind-cs");
    write_fsproj(&root, "FsLeaf", &[]);
    write_csproj(&root, "CsMid", &["../FsLeaf/FsLeaf.fsproj"]);
    let app = write_fsproj(&root, "App", &["../CsMid/CsMid.csproj"]);
    let theirs = build_and_msbuild_reference_names(&app);
    let ours = our_reference_names(&app);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&theirs).collect();
    assert_eq!(
        missing,
        vec![&"CsMid".to_string(), &"FsLeaf".to_string()],
        "the C#-boundary gap should under-resolve exactly the F# leaf and the \
         C# project referencing it — a different missing set means a residual \
         moved"
    );
    assert!(
        extra.is_empty(),
        "the gap must stay an *under*-resolution; ours-only names would be \
         fabrication: {extra:?}"
    );
}

/// An F# ref overriding `<AssemblyName>` (formerly the pinned 3.3a gap): the
/// locator resolves the producer's *evaluated* output assembly name from the
/// entry's assets file (NuGet records it as the project-kind entry's `compile`
/// asset, `bin/placeholder/<AssemblyName>.dll`), so the renamed output must
/// resolve like any other F# reference.
#[test]
fn fsharp_assembly_name_override_matches_msbuild() {
    let root = fixture_root("fs-asmname");
    write_fsproj_ext(
        &root,
        "FsLib",
        &[],
        "    <AssemblyName>RenamedFs</AssemblyName>\n",
    );
    let app = write_fsproj(&root, "App", &["../FsLib/FsLib.fsproj"]);
    assert_reference_sets_match(&app, &["RenamedFs"]);
}

/// `TargetName` (not `AssemblyName`) names the output file — MSBuild writes
/// `$(TargetName)$(TargetExt)` and `TargetName` merely *defaults* to
/// `AssemblyName` (probed, dotnet 10.0.301: `AssemblyName=Identity` +
/// `TargetName=FsFileName` builds `FsFileName.dll`, and fsc stamps the
/// manifest identity from the output path too — the built DLL's assembly
/// name is `FsFileName`, matching MSBuild's `FusionName` for the item).
/// NuGet's assets meanwhile record `bin/placeholder/Identity.dll`
/// (probed), so locating by the assets-recovered name would miss — only
/// the graph node's evaluated `target_name` finds the real file.
#[test]
fn fsharp_target_name_override_matches_msbuild() {
    let root = fixture_root("fs-targetname");
    write_fsproj_ext(
        &root,
        "FsLib",
        &[],
        "    <AssemblyName>Identity</AssemblyName>\n    <TargetName>FsFileName</TargetName>\n",
    );
    let app = write_fsproj(&root, "App", &["../FsLib/FsLib.fsproj"]);
    assert_reference_sets_match(&app, &["FsFileName"]);
}
