//! The half-A oracle of `docs/nuget-dgspec-boundary-plan.md` (stage 2):
//! [`restore_graph`] obtains the dependency-graph spec an offline restore of a
//! project reads, either by generating it (`GenerateRestoreGraphFile`, A₀) or
//! by taking the one the restore itself writes.
//!
//! The generated spec is only a reference for a future producer if it *is*
//! restore's input, so the gate here is that the two agree exactly, up to the
//! scratch directories each was written under. Both come from one command
//! builder in `restore.rs`, so they share the SDK, environment and global
//! properties by construction; what this checks is that MSBuild computes the
//! same spec down both routes. The stage-1 reader
//! (`borzoi_nuget::dgspec::parse_dgspec`) must commit to the fixtures here.
//!
//! Stage 3 then checks that the split loses nothing: NuGet's own restore,
//! handed the generated spec (B₀ after A₀: `RestoreRunner` in
//! `tools/nuget-oracle`'s `restoreDgSpec` op, running the `NuGet.Commands` of
//! the SDK the restore ran under), computes exactly the assets file the restore
//! wrote. Whatever MSBuild knows that bears on resolution and the spec does not
//! carry would show up there as a difference.
//!
//! Stage 4 then puts the spawn-free half B to the same restore: B₁
//! (`borzoi_nuget::dgspec::resolve_dgspec`), handed the spec and the identity
//! of the NuGet that graded it, either declines or computes each framework's
//! package closure exactly as the restore's assets file lists it
//! ([`spawn_free`]).
//!
//! [`pinned_projects`] (`#[ignore]`d; CI runs it in the `corpus-diff` job over
//! the pinned project corpus) does all of this for every project in
//! `BORZOI_PROJECT_LIST`, restoring against the packages folder
//! `BORZOI_PROJECT_PACKAGES` the corpus was restored into, and pins what the
//! reader makes of each spec, each target of each assets file, B₁'s verdict on
//! each project (the stage-4 census: which unmodelled fields block it), and the
//! identity of the `NuGet.Commands` that graded them in an exact manifest
//! (`tests/manifests/restore_graph_projects.txt`).
//!
//! Requires the .NET SDK + the vendored NuGet cache — the Nix devShell provides
//! both.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use borzoi::restore::{
    RestoreGraph, RestoreGraphSource, ScratchDir, restore_environment, restore_graph,
};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi_nuget::NuGetFramework;
use borzoi_nuget::dgspec::{
    DependencyGraphSpec, DgSpecDecline, DgSpecResolveDecline, EnvironmentVariable, NuGetIdentity,
    PathSegment, ResolvedFramework, environment_variable, parse_dgspec, resolve_dgspec,
};
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};
use borzoi_oracle_harness::nuget_oracle::{EnvironmentChange, Oracle, sdk_dir_for};
use serde_json::{Value, json};

use crate::common::checkout_key;

fn dotnet_root() -> PathBuf {
    std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset — run under `nix develop`")
}

/// `text` as JSON, with every occurrence of the scratch directory in a key or a
/// string replaced by `<scratch>`.
fn normalised(text: &str, scratch: &Path) -> Value {
    fn walk(value: Value, scratch: &str) -> Value {
        match value {
            Value::String(s) => Value::String(s.replace(scratch, "<scratch>")),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| walk(v, scratch)).collect())
            }
            Value::Object(members) => Value::Object(
                members
                    .into_iter()
                    .map(|(k, v)| (k.replace(scratch, "<scratch>"), walk(v, scratch)))
                    .collect(),
            ),
            other => other,
        }
    }
    let value: Value = serde_json::from_str(text).expect("MSBuild writes a JSON dgspec");
    let scratch = scratch.to_str().expect("scratch path is UTF-8");
    walk(value, scratch)
}

/// The first place two JSON values differ: `a` is what came down the generated
/// route, `b` what came down the restored one.
fn first_difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<_> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(v), Some(w)) => first_difference(v, w, &format!("{path}.{k}")),
                (v, w) => Some(format!("{path}.{k}: generated {v:?} / restored {w:?}")),
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (v, w))| first_difference(v, w, &format!("{path}[{i}]"))),
        _ if a == b => None,
        _ => Some(format!("{path}: generated {a} / restored {b}")),
    }
}

/// Both routes' outcome for one project.
enum Graphs {
    /// Both wrote a spec, and the specs agree.
    Agree {
        /// The generated spec.
        text: String,
        /// The directory the generated spec names, still in place.
        generated: ScratchDir,
        /// The directory restore wrote its spec and assets file into.
        restored: ScratchDir,
    },
    /// Both declined.
    Declined,
}

/// Obtain `project`'s spec down both routes, restoring under `env`, and
/// require them to agree.
fn both_routes(project: &Path, env: &SdkDiscoveryEnv) -> Result<Graphs, String> {
    let root = dotnet_root();
    let generated = restore_graph(project, &root, env, RestoreGraphSource::Generated);
    let restored = restore_graph(project, &root, env, RestoreGraphSource::Restored);
    match (generated, restored) {
        (
            RestoreGraph::Written {
                text: generated,
                scratch: generated_scratch,
            },
            RestoreGraph::Written {
                text: restored,
                scratch: restored_scratch,
            },
        ) => {
            let a = normalised(&generated, generated_scratch.path());
            let b = normalised(&restored, restored_scratch.path());
            match first_difference(&a, &b, "$") {
                None => Ok(Graphs::Agree {
                    text: generated,
                    generated: generated_scratch,
                    restored: restored_scratch,
                }),
                Some(difference) => Err(format!(
                    "{}: the generated spec is not the one restore read: {difference}",
                    project.display()
                )),
            }
        }
        (RestoreGraph::Declined, RestoreGraph::Declined) => Ok(Graphs::Declined),
        (RestoreGraph::TransientFailure, _) | (_, RestoreGraph::TransientFailure) => Err(format!(
            "{}: an invocation failed transiently",
            project.display()
        )),
        (RestoreGraph::Written { .. }, RestoreGraph::Declined) => Err(format!(
            "{}: generated a spec, but restore wrote none",
            project.display()
        )),
        (RestoreGraph::Declined, RestoreGraph::Written { .. }) => Err(format!(
            "{}: restore wrote a spec, but generating one declined",
            project.display()
        )),
    }
}

/// The resident oracle for each SDK directory, spawned on first use: one per
/// SDK, since each runs its own SDK's NuGet.
static ORACLES: Mutex<BTreeMap<PathBuf, Oracle>> = Mutex::new(BTreeMap::new());

/// The home every restore here runs under: a folder of the suite's own, with
/// no user config (restore writes its default there).
static SUITE_HOME: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::tempdir().expect("temp dir"));

/// The environment every restore here runs under: the process's, with every
/// variable B₁ has a verdict on removed from the build environment, no
/// `DOTNET_CLI_HOME`, and the home at [`SUITE_HOME`]. `restore.rs` puts the
/// discovered packages folder back. Without it, the caller's own home and
/// NuGet variables decide what B₁ may commit to, and so the census.
fn controlled_env() -> SdkDiscoveryEnv {
    let mut env = SdkDiscoveryEnv::from_process_env();
    env.build_environment
        .retain(|name, _| environment_variable(OsStr::new(name)) == EnvironmentVariable::Unread);
    env.dotnet_cli_home = None;
    env.home_dir = Some(SUITE_HOME.path().to_owned());
    env
}

/// The changes that give an oracle child [`controlled_env`]'s NuGet-visible
/// environment, as it inherits this process's: each variable B₁ has a verdict
/// on removed, then the controlled ones set.
fn oracle_changes() -> Vec<(String, Option<String>)> {
    let controlled = restore_environment(&controlled_env());
    let mut changes: Vec<(String, Option<String>)> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| environment_variable(OsStr::new(name)) != EnvironmentVariable::Unread)
        .map(|name| (name, None))
        .collect();
    changes.extend(
        controlled
            .iter()
            .filter(|(name, _)| environment_variable(name) != EnvironmentVariable::Unread)
            .map(|(name, value)| {
                (
                    name.to_str()
                        .expect("a NuGet variable's name is UTF-8")
                        .to_owned(),
                    Some(
                        value
                            .to_str()
                            .expect("a controlled value is UTF-8")
                            .to_owned(),
                    ),
                )
            }),
    );
    changes
}

/// What the stage-3 check found for one project.
struct Decomposition {
    /// The `AssemblyInformationalVersion` of the `NuGet.Commands` that ran.
    nuget_commands: String,
    /// Whether the restore succeeded.
    success: bool,
    /// The agreed assets file, normalised.
    assets: Value,
}

/// Stage 3: NuGet's restore, handed the generated spec, computes exactly the
/// assets file the restore wrote. NuGet is the `NuGet.Commands` of the SDK the
/// restore ran under: the one the muxer selects from the project's directory,
/// as `restore.rs` runs it.
fn decomposes_losslessly(
    project: &Path,
    text: &str,
    generated: &ScratchDir,
    restored: &ScratchDir,
) -> Result<Decomposition, String> {
    let directory = project.parent().expect("a project is in a directory");
    let sdk = sdk_dir_for(&dotnet_root().join("dotnet"), directory);
    let response = {
        let mut oracles = ORACLES.lock().unwrap_or_else(|poison| poison.into_inner());
        oracles
            .entry(sdk.clone())
            .or_insert_with(|| {
                let changes = oracle_changes();
                let changes: Vec<EnvironmentChange<'_>> = changes
                    .iter()
                    .map(|(name, value)| match value {
                        Some(value) => EnvironmentChange::Set(name, value),
                        None => EnvironmentChange::Unset(name),
                    })
                    .collect();
                Oracle::spawn_on_with_changes(&sdk, &changes)
            })
            .request(&json!({"op": "restoreDgSpec", "text": text}))
    };
    if response["ok"] == false {
        return Err(format!(
            "{}: NuGet threw restoring the generated spec: {}",
            project.display(),
            response["message"]
        ));
    }
    let identity = &response["nugetCommands"];
    let expected = sdk.join("NuGet.Commands.dll");
    if identity["path"].as_str() != expected.to_str() {
        return Err(format!(
            "{}: the oracle ran {identity}, not {}",
            project.display(),
            expected.display()
        ));
    }
    let nuget_commands = identity["informationalVersion"]
        .as_str()
        .expect("the identity names a version")
        .to_owned();
    let results = response["results"]
        .as_array()
        .expect("restoreDgSpec reports its results");
    let [result] = results.as_slice() else {
        return Err(format!(
            "{}: an entry-only spec restores one project, not {}",
            project.display(),
            results.len()
        ));
    };
    let ours = comparable_assets(normalised(
        result["assets"].as_str().expect("each result has assets"),
        generated.path(),
    ));
    let assets_path = restored.path().join("project.assets.json");
    let theirs = match std::fs::read_to_string(&assets_path) {
        Ok(text) => comparable_assets(normalised(&text, restored.path())),
        Err(error) => {
            return Err(format!(
                "{}: restore wrote no assets file ({error})",
                project.display()
            ));
        }
    };
    match first_difference(&ours, &theirs, "$") {
        None => Ok(Decomposition {
            nuget_commands,
            success: result["success"]
                .as_bool()
                .expect("each result says whether it succeeded"),
            assets: theirs,
        }),
        Some(difference) => Err(format!(
            "{}: NuGet over the generated spec is not the restore: {difference}",
            project.display()
        )),
    }
}

/// `assets` with its `logs` in a canonical order. Their order is not part of
/// what restore computed: a missing package's message is built per element of
/// the target graph's unresolved set (`UnresolvedMessages.LogAsync`), which is
/// iterated in hash order, and .NET randomises string hashes per process. So two
/// runs of one restore list the same messages in different orders.
fn comparable_assets(mut assets: Value) -> Value {
    if let Some(logs) = assets.get_mut("logs").and_then(Value::as_array_mut) {
        logs.sort_by_cached_key(|message| message.to_string());
    }
    assets
}

/// What B₁ made of a spec whose restore it was checked against.
#[derive(Debug)]
enum SpawnFree {
    Committed(Vec<ResolvedFramework>),
    Declined(DgSpecResolveDecline),
}

impl SpawnFree {
    /// The census label: the frameworks committed, or the decline's class
    /// (for unmodelled fields, every one present).
    fn label(&self) -> String {
        match self {
            SpawnFree::Committed(frameworks) => {
                format!("b1: committed frameworks={}", frameworks.len())
            }
            SpawnFree::Declined(decline) => {
                let class = match decline {
                    DgSpecResolveDecline::Unmodelled { fields } => fields
                        .iter()
                        .map(|field| field.name())
                        .collect::<Vec<_>>()
                        .join("+"),
                    DgSpecResolveDecline::Resolve { decline, .. } => {
                        let debug = format!("{decline:?}");
                        let variant = debug.split(['(', ' ', '{']).next().unwrap_or_default();
                        format!("resolve:{variant}")
                    }
                    DgSpecResolveDecline::UnverifiedNuGet { .. } => "unverified-nuget".to_owned(),
                    DgSpecResolveDecline::ProjectNotRestored { .. } => "not-restored".to_owned(),
                    DgSpecResolveDecline::PackageNamedLikeProject { .. } => {
                        "named-like-project".to_owned()
                    }
                    DgSpecResolveDecline::DownloadUnavailable { id, .. } => {
                        format!("download-unavailable:{}", id.as_str().to_ascii_lowercase())
                    }
                    DgSpecResolveDecline::PromotedWarning { code, .. } => match code {
                        Some(code) => format!("promoted-warning:{code}"),
                        None => "promoted-warning:unpredicted".to_owned(),
                    },
                    DgSpecResolveDecline::CompatibilityUnproven { package, .. } => {
                        format!("compatibility-unproven:{}", package_label(package))
                    }
                    DgSpecResolveDecline::PackageRead { .. } => "package-read".to_owned(),
                    DgSpecResolveDecline::PackageMetadata { package, .. } => {
                        format!("package-metadata:{}", package_label(package))
                    }
                    // Host-dependent, so a manifest line with it would differ
                    // between machines: the census expects none.
                    DgSpecResolveDecline::Environment { decline } => {
                        format!("environment ({decline})")
                    }
                };
                format!("b1: declined {class}")
            }
        }
    }
}

/// A package in a census label: its id, lower-cased, and version.
fn package_label(package: &borzoi_nuget::PackageIdentity) -> String {
    format!(
        "{}/{}",
        package.id.as_str().to_ascii_lowercase(),
        package.version.to_normalized_string()
    )
}

/// Stage 4: B₁, handed the spec, the identity of the NuGet that restored it
/// and the environment it restored under (`restore.rs`'s, from `env`),
/// declines, or commits only where the restore succeeded and to exactly the
/// package closure each framework's target lists.
///
/// Targets are matched by framework, not by name: an assets file of version 4
/// names a target by its alias, one of version 3 by `NuGetFramework.ToString()`
/// (`.NETStandard,Version=v2.0`), and which version restore writes moves with
/// the spec's `SdkAnalysisLevel`.
fn spawn_free(
    project: &Path,
    spec: &DependencyGraphSpec,
    decomposition: &Decomposition,
    env: &SdkDiscoveryEnv,
) -> Result<SpawnFree, String> {
    let [entry] = spec.restore.as_slice() else {
        return Err(format!(
            "{}: an entry-only spec restores one project, not {}",
            project.display(),
            spec.restore.len()
        ));
    };
    let identity = NuGetIdentity::new(&decomposition.nuget_commands);
    let frameworks = match resolve_dgspec(&identity, spec, entry, &restore_environment(env)) {
        Ok(frameworks) => frameworks,
        Err(decline) => return Ok(SpawnFree::Declined(decline)),
    };
    if !decomposition.success {
        return Err(format!(
            "{}: B₁ committed, but the restore failed",
            project.display()
        ));
    }
    let key = |framework: &NuGetFramework| {
        framework
            .short_folder_name()
            .expect("a project framework has a short folder name")
    };
    let theirs: BTreeMap<String, BTreeSet<(String, String)>> = decomposition.assets["targets"]
        .as_object()
        .expect("an assets file has targets")
        .iter()
        .map(|(name, libraries)| {
            let framework = NuGetFramework::parse(name)
                .unwrap_or_else(|e| panic!("target {name:?} names a framework: {e}"));
            let packages = libraries
                .as_object()
                .expect("a target maps libraries")
                .iter()
                .filter(|(_, library)| library["type"] == "package")
                .map(|(library, _)| {
                    let (id, version) = library.split_once('/').expect("`id/version`");
                    (id.to_ascii_lowercase(), version.to_owned())
                })
                .collect();
            (key(&framework), packages)
        })
        .collect();
    let ours: BTreeMap<String, BTreeSet<(String, String)>> = frameworks
        .iter()
        .map(|resolved| {
            let packages = resolved
                .closure
                .packages
                .iter()
                .map(|package| {
                    (
                        package.identity.id.as_str().to_ascii_lowercase(),
                        package.identity.version.to_normalized_string(),
                    )
                })
                .collect();
            (key(&resolved.framework), packages)
        })
        .collect();
    if ours != theirs {
        return Err(format!(
            "{}: B₁'s closures are not the restore's:\nB₁: {ours:?}\nrestore: {theirs:?}",
            project.display()
        ));
    }
    // B₁'s predicted warnings, where it makes a prediction, are the codes the
    // restore logged.
    if let Some(predicted) = frameworks
        .iter()
        .map(|framework| framework.warnings.clone())
        .collect::<Option<Vec<_>>>()
    {
        let predicted: BTreeSet<String> = predicted
            .into_iter()
            .flatten()
            .map(|code| code.as_str().to_owned())
            .collect();
        let logged: BTreeSet<String> = decomposition.assets["logs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|log| log["code"].as_str().map(str::to_owned))
            .collect();
        if predicted != logged {
            return Err(format!(
                "{}: B₁ predicts warnings {predicted:?}, the restore logged {logged:?}",
                project.display()
            ));
        }
    }
    Ok(SpawnFree::Committed(frameworks))
}

/// One manifest label per target of an assets file: its library count, how
/// many of those are packages, and how many compile assets they contribute.
fn target_labels(assets: &Value) -> Vec<String> {
    let targets = assets["targets"]
        .as_object()
        .expect("an assets file has targets");
    targets
        .iter()
        .map(|(name, libraries)| {
            let libraries = libraries.as_object().expect("a target maps libraries");
            let packages = libraries
                .values()
                .filter(|l| l["type"] == "package")
                .count();
            let compile: usize = libraries
                .values()
                .filter_map(|l| l["compile"].as_object())
                .map(|c| c.len())
                .sum();
            format!(
                "assets {name}: libraries={} packages={packages} compile={compile}",
                libraries.len()
            )
        })
        .collect()
}

/// A fresh directory holding `files` (relative path, contents).
fn write_tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-restore-graph-{tag}-{unique}"));
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    // By its real path: a project named through a symlink declines, and the
    // temporary directory is one on macOS.
    std::fs::canonicalize(&root).unwrap()
}

const APP: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
  </ItemGroup>
</Project>
"#;

const APP_WITH_LIB: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
    <ProjectReference Include="lib/Lib.fsproj" />
  </ItemGroup>
</Project>
"#;

const LIB: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Lib.fs" />
  </ItemGroup>
</Project>
"#;

/// The agreed spec of `project`, read by the stage-1 reader, which must commit.
/// The decomposition must be lossless on it too.
fn committed_spec(project: &Path) -> DependencyGraphSpec {
    committed_spec_and_b1(project).0
}

/// [`committed_spec`], with B₁'s verdict on it, which must agree with the
/// restore.
fn committed_spec_and_b1(project: &Path) -> (DependencyGraphSpec, SpawnFree) {
    let env = controlled_env();
    let (text, generated, restored) = match both_routes(project, &env) {
        Ok(Graphs::Agree {
            text,
            generated,
            restored,
        }) => (text, generated, restored),
        Ok(Graphs::Declined) => panic!("{}: both routes declined", project.display()),
        Err(failure) => panic!("{failure}"),
    };
    let decomposition = decomposes_losslessly(project, &text, &generated, &restored)
        .unwrap_or_else(|failure| panic!("{failure}"));
    assert!(
        !project.parent().unwrap().join("obj").exists(),
        "neither route may write the project's obj/"
    );
    let spec =
        parse_dgspec(&text).unwrap_or_else(|decline| panic!("the reader declined: {decline}"));
    let b1 = spawn_free(project, &spec, &decomposition, &env)
        .unwrap_or_else(|failure| panic!("{failure}"));
    (spec, b1)
}

#[test]
fn a_single_project_generates_the_spec_restore_reads() {
    let root = write_tree("single", &[("App.fsproj", APP), ("App.fs", "module App\n")]);
    let spec = committed_spec(&root.join("App.fsproj"));
    // `write_tree` returns the real path; on Windows that is in verbatim form
    // (`\\?\C:\…`), which MSBuild is handed without the prefix.
    let app = root.join("App.fsproj");
    let app = app.to_str().unwrap();
    let app = app.strip_prefix(r"\\?\").unwrap_or(app);
    assert_eq!(
        spec.restore,
        vec![app.to_owned()],
        "the entry, by its real path"
    );
    assert_eq!(spec.projects.len(), 1);
    let framework = &spec.projects[0].frameworks[0];
    assert!(
        framework
            .dependencies
            .iter()
            .any(|d| d.id.as_str() == "FSharp.Core"),
        "the implicit FSharp.Core reference is in the spec"
    );
}

/// A `net10.0` project's spec carries the SDK's pruning list, a few hundred
/// entries, which B₁ applies; its closure is exactly the restore's.
#[test]
fn b1_resolves_a_net10_project_with_its_pruning_list() {
    let root = write_tree(
        "b1-net10",
        &[("App.fsproj", APP), ("App.fs", "module App\n")],
    );
    let (spec, b1) = committed_spec_and_b1(&root.join("App.fsproj"));
    assert!(
        spec.projects[0].frameworks[0].packages_to_prune.len() > 100,
        "the SDK writes its pruning list"
    );
    match b1 {
        SpawnFree::Committed(_) => {}
        SpawnFree::Declined(decline) => panic!("B₁ must commit, but declined: {decline}"),
    }
}

/// A `netstandard2.0` project has no pruning list and no targeting-pack
/// download, so B₁ resolves it, implicit references and asset-target fallback
/// list included, to exactly the restore's closure.
#[test]
fn b1_resolves_a_netstandard_project_as_restore_does() {
    let library = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>netstandard2.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Lib.fs" />
  </ItemGroup>
</Project>
"#;
    let root = write_tree(
        "b1-netstandard",
        &[("Lib.fsproj", library), ("Lib.fs", "module Lib\n")],
    );
    match committed_spec_and_b1(&root.join("Lib.fsproj")).1 {
        SpawnFree::Committed(frameworks) => {
            let ids: BTreeSet<String> = frameworks[0]
                .closure
                .packages
                .iter()
                .map(|package| package.identity.id.as_str().to_ascii_lowercase())
                .collect();
            assert!(ids.contains("fsharp.core"), "{ids:?}");
            assert!(ids.contains("netstandard.library"), "{ids:?}");
        }
        SpawnFree::Declined(decline) => panic!("B₁ must commit, but declined: {decline}"),
    }
}

#[test]
fn a_project_reference_brings_its_project_into_the_spec() {
    let root = write_tree(
        "multi",
        &[
            ("App.fsproj", APP_WITH_LIB),
            ("App.fs", "module App\n"),
            ("lib/Lib.fsproj", LIB),
            ("lib/Lib.fs", "module Lib\n"),
        ],
    );
    let spec = committed_spec(&root.join("App.fsproj"));
    assert_eq!(spec.projects.len(), 2, "the entry and its reference");
    let entry = spec
        .projects
        .iter()
        .find(|p| p.restore.project_unique_name.ends_with("App.fsproj"))
        .expect("the entry is in the spec");
    let references = &entry.restore.frameworks[0].project_references;
    assert_eq!(references.len(), 1);
    assert!(references[0].project_unique_name.ends_with("Lib.fsproj"));
    assert!(!root.join("lib/obj").exists(), "nor its reference's obj/");
}

/// The stage-3 check can fail: a spec that has lost a dependency on its way to
/// NuGet does not reproduce the restore. Without this, a comparison that
/// silently compared nothing would pass every fixture.
#[test]
fn a_spec_that_loses_a_dependency_does_not_decompose() {
    let root = write_tree("lossy", &[("App.fsproj", APP), ("App.fs", "module App\n")]);
    let project = root.join("App.fsproj");
    let Ok(Graphs::Agree {
        text,
        generated,
        restored,
    }) = both_routes(&project, &controlled_env())
    else {
        panic!("{}: both routes must agree", project.display());
    };
    decomposes_losslessly(&project, &text, &generated, &restored)
        .unwrap_or_else(|failure| panic!("{failure}"));

    let mut spec: Value = serde_json::from_str(&text).unwrap();
    let projects = spec["projects"].as_object_mut().unwrap();
    let [(_, entry)] = projects.iter_mut().collect::<Vec<_>>().try_into().unwrap();
    let frameworks = entry["frameworks"].as_object_mut().unwrap();
    let [(_, framework)] = frameworks
        .iter_mut()
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let dependencies = framework["dependencies"].as_object_mut().unwrap();
    assert!(
        dependencies.remove("FSharp.Core").is_some(),
        "the fixture depends on FSharp.Core"
    );
    let lossy = serde_json::to_string(&spec).unwrap();
    match decomposes_losslessly(&project, &lossy, &generated, &restored) {
        Err(failure) => assert!(failure.contains("not the restore"), "{failure}"),
        Ok(_) => panic!("a spec without FSharp.Core must not reproduce the restore"),
    }
}

/// A project whose directory is reached through a symlink declines down both
/// routes. MSBuild runs a project's targets with the process's working
/// directory set to the project's directory, which the OS reports by its real
/// path, so such a project is spelt two ways in one evaluation and NuGet drops
/// its `ProjectReference`s from the spec restore reads: the routes would
/// disagree. A project *file* that is a symlink, in a real directory, is spelt
/// one way: MSBuild evaluates it beside the link, so it is a project of its
/// own, and it keeps the reference that resolves there.
#[cfg(unix)]
#[test]
fn a_project_in_a_symlinked_directory_declines() {
    let root = write_tree(
        "symlinked",
        &[
            ("real/App.fsproj", APP_WITH_LIB),
            ("real/App.fs", "module App\n"),
            ("real/lib/Lib.fsproj", LIB),
            ("real/lib/Lib.fs", "module Lib\n"),
            ("elsewhere/App.fs", "module App\n"),
            ("elsewhere/lib/Lib.fsproj", LIB),
            ("elsewhere/lib/Lib.fs", "module Lib\n"),
        ],
    );
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    std::os::unix::fs::symlink(
        root.join("real/App.fsproj"),
        root.join("elsewhere/App.fsproj"),
    )
    .unwrap();

    match both_routes(&root.join("link").join("App.fsproj"), &controlled_env()) {
        Ok(Graphs::Declined) => {}
        Ok(Graphs::Agree { .. }) => panic!("a project in a symlinked directory must decline"),
        Err(failure) => panic!("{failure}"),
    }
    // By its real path, and as a symlinked file beside its own `lib/`, the
    // project keeps its reference, resolved beside the spelling.
    for directory in ["real", "elsewhere"] {
        let spec = committed_spec(&root.join(directory).join("App.fsproj"));
        let entry = spec
            .projects
            .iter()
            .find(|p| p.restore.project_unique_name.ends_with("App.fsproj"))
            .expect("the entry is in the spec");
        let references = &entry.restore.frameworks[0].project_references;
        assert_eq!(references.len(), 1, "{directory}");
        let expected = root.join(directory).join("lib").join("Lib.fsproj");
        assert_eq!(
            references[0].project_unique_name,
            expected.to_str().unwrap(),
            "{directory}"
        );
    }
}

/// A project conditioned on a global `dotnet restore` injects
/// (`NuGetInteractive`) must generate the spec restore reads: the generated
/// route has to evaluate under the same globals, or the implicit `FSharp.Core`
/// reference appears down one route only.
#[test]
fn a_project_conditioned_on_restores_globals_generates_alike() {
    let conditioned = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference Condition="'$(NuGetInteractive)' == 'false'">true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
  </ItemGroup>
</Project>
"#;
    let root = write_tree(
        "interactive",
        &[("App.fsproj", conditioned), ("App.fs", "module App\n")],
    );
    let spec = committed_spec(&root.join("App.fsproj"));
    assert!(
        spec.projects[0].frameworks[0].dependencies.is_empty(),
        "under restore's globals the implicit FSharp.Core reference is disabled"
    );
}

/// The MSBuild command line the `dotnet` CLI forwards for `verb`, read from the
/// first line of its diagnostic output, as a set of arguments. The line does not
/// quote, so `root` (which may contain spaces) is replaced by `<root>` before it
/// is split.
fn forwarded_arguments(
    root: &Path,
    project: &Path,
    verb: &[&str],
) -> std::collections::BTreeSet<String> {
    let mut cmd = std::process::Command::new(dotnet_root().join("dotnet"));
    cmd.current_dir(project.parent().unwrap())
        .args(verb)
        .arg(project)
        .args(["-nologo", "-v:diag", "-p:NuGetAudit=false"]);
    let output = borzoi_spawn::BoundedCommand::new(cmd)
        .timeout(std::time::Duration::from_secs(300))
        .run_ok(format_args!("dotnet {verb:?} -v:diag"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.contains("--target:"))
        .unwrap_or_else(|| panic!("no forwarded MSBuild command line for {verb:?}:\n{stdout}"));
    line.replace(root.to_str().expect("UTF-8 root"), "<root>")
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

/// What `dotnet restore` forwards to MSBuild beyond its target is exactly
/// `NuGetInteractive=false`, which the shared command builder in `restore.rs`
/// passes for every verb. An SDK that starts forwarding another global fails
/// here by name, rather than making the generated spec quietly diverge from
/// the one restore reads.
#[test]
fn restore_forwards_no_global_the_generated_route_lacks() {
    let root = write_tree(
        "forwarded",
        &[("App.fsproj", APP), ("App.fs", "module App\n")],
    );
    let project = root.join("App.fsproj");
    let output = root.join("graph.dg.json");
    let restored = forwarded_arguments(&root, &project, &["restore"]);
    let generated = forwarded_arguments(
        &root,
        &project,
        &[
            "msbuild",
            "-t:GenerateRestoreGraphFile",
            &format!("-p:RestoreGraphOutputPath={}", output.display()),
        ],
    );
    let only_restored: Vec<&String> = restored.difference(&generated).collect();
    let only_generated: Vec<&String> = generated.difference(&restored).collect();
    assert_eq!(
        only_restored,
        ["--property:NuGetInteractive=false", "--target:Restore"],
        "what `dotnet restore` forwards beyond `dotnet msbuild`"
    );
    assert_eq!(
        only_generated,
        [
            "--property:RestoreGraphOutputPath=<root>/graph.dg.json",
            "--target:GenerateRestoreGraphFile",
        ],
        "what `dotnet msbuild` forwards beyond `dotnet restore`"
    );
}

#[test]
fn a_locked_project_declines_down_both_routes() {
    let root = write_tree(
        "locked",
        &[
            ("App.fsproj", APP),
            ("App.fs", "module App\n"),
            (
                "packages.lock.json",
                "{\"version\": 1, \"dependencies\": {}}",
            ),
        ],
    );
    match both_routes(&root.join("App.fsproj"), &controlled_env()) {
        Ok(Graphs::Declined) => {}
        Ok(Graphs::Agree { .. }) => panic!("a locked project must decline"),
        Err(failure) => panic!("{failure}"),
    }
}

/// Keys whose objects are keyed by name (a project path, an alias, a package
/// id), blanked in a decline's manifest spelling so it names the place in the
/// schema rather than a host path.
const NAME_KEYED: &[&str] = &[
    "restore",
    "projects",
    "frameworks",
    "dependencies",
    "centralPackageVersions",
    "packagesToPrune",
    "frameworkReferences",
    "projectReferences",
    "sources",
    "suppressedAdvisories",
];

fn decline_label(decline: &DgSpecDecline) -> String {
    let mut shape = String::from("$");
    let mut previous = "";
    for segment in decline.path.segments() {
        match segment {
            PathSegment::Key(key) => {
                if NAME_KEYED.contains(&previous) {
                    shape.push_str(".*");
                } else {
                    shape.push('.');
                    shape.push_str(key);
                }
                previous = key;
            }
            PathSegment::Index(_) => shape.push_str("[]"),
        }
    }
    let reason = format!("{:?}", decline.reason);
    let reason = reason.split(['(', ' ', '{']).next().unwrap_or_default();
    format!("declined {reason} at {shape}")
}

/// What the reader makes of a committed spec, in counts that move with the
/// spec's content but not with the host.
fn committed_label(spec: &DependencyGraphSpec) -> String {
    let frameworks: usize = spec.projects.iter().map(|p| p.frameworks.len()).sum();
    let dependencies: usize = spec
        .projects
        .iter()
        .flat_map(|p| &p.frameworks)
        .map(|f| f.dependencies.len())
        .sum();
    let pruned: usize = spec
        .projects
        .iter()
        .flat_map(|p| &p.frameworks)
        .map(|f| f.packages_to_prune.len())
        .sum();
    format!(
        "committed projects={} frameworks={frameworks} dependencies={dependencies} pruned={pruned}",
        spec.projects.len()
    )
}

#[test]
#[ignore = "stages 2 to 4 over the project corpus; run through tools/ci/project-corpus-gate.sh restore-graph"]
fn pinned_projects() {
    let Some(list) = std::env::var_os("BORZOI_PROJECT_LIST") else {
        eprintln!("BORZOI_PROJECT_LIST unset; skipping. See the module docs.");
        return;
    };
    let projects: Vec<PathBuf> = std::env::split_paths(&list).collect();
    assert!(!projects.is_empty(), "BORZOI_PROJECT_LIST is empty");
    // The corpus was restored into a packages folder of its own, which the
    // offline restore must read: the devshell's NuGet store holds only what
    // Borzoi's own builds need, so a corpus project's restore there fails, and
    // the gate would grade NuGet's failures rather than its resolutions.
    let packages = std::env::var_os("BORZOI_PROJECT_PACKAGES").expect(
        "BORZOI_PROJECT_PACKAGES must name the packages folder the corpus was restored into",
    );
    let env = SdkDiscoveryEnv {
        nuget_packages_dir: Some(PathBuf::from(packages)),
        ..controlled_env()
    };
    let mut lines = Vec::new();
    let mut identities = BTreeSet::new();
    let mut failures = Vec::new();
    for project in &projects {
        let key = checkout_key(project);
        match both_routes(project, &env) {
            Ok(Graphs::Agree {
                text,
                generated,
                restored,
            }) => {
                let spec = parse_dgspec(&text);
                lines.push(match &spec {
                    Ok(spec) => format!("{key} spec: {}", committed_label(spec)),
                    Err(decline) => format!("{key} spec: {}", decline_label(decline)),
                });
                match decomposes_losslessly(project, &text, &generated, &restored) {
                    Ok(decomposition) => {
                        if let Ok(spec) = &spec {
                            match spawn_free(project, spec, &decomposition, &env) {
                                Ok(b1) => lines.push(format!("{key} {}", b1.label())),
                                Err(failure) => failures.push(failure),
                            }
                        }
                        identities.insert(decomposition.nuget_commands);
                        let logs = decomposition.assets["logs"].as_array().map_or(0, Vec::len);
                        let outcome = if decomposition.success {
                            "succeeded"
                        } else {
                            "failed"
                        };
                        lines.push(format!("{key} restore: {outcome} logs={logs}"));
                        lines.extend(
                            target_labels(&decomposition.assets)
                                .into_iter()
                                .map(|label| format!("{key} {label}")),
                        );
                    }
                    Err(failure) => failures.push(failure),
                }
            }
            Ok(Graphs::Declined) => lines.push(format!("{key} not-restorable")),
            Err(failure) => failures.push(failure),
        }
    }
    assert!(
        failures.is_empty(),
        "the generated spec must be the one restore reads, NuGet over it must \
         compute the restore's assets file, and B₁ must decline or agree with it:\n{}",
        failures.join("\n")
    );
    lines.extend(
        identities
            .into_iter()
            .map(|identity| format!("graded by NuGet.Commands {identity}")),
    );
    let manifest = Manifest::from_entries(lines).unwrap_or_else(|e| panic!("manifest: {e}"));
    check(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/manifests/restore_graph_projects.txt"),
        &manifest,
        &format!("{UPDATE_ENV}=1 bash tools/ci/project-corpus-gate.sh restore-graph"),
    );
}
