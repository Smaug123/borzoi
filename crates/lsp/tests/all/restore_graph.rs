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
//! [`pinned_projects`] (`#[ignore]`d; CI runs it in the `corpus-diff` job over
//! the pinned project corpus) does the same for every project in
//! `BORZOI_PROJECT_LIST`, and pins what the reader makes of each spec in an
//! exact manifest (`tests/manifests/restore_graph_projects.txt`).
//!
//! Requires the .NET SDK + the vendored NuGet cache — the Nix devShell provides
//! both.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use borzoi::restore::{RestoreGraph, RestoreGraphSource, restore_graph};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi_nuget::dgspec::{DependencyGraphSpec, DgSpecDecline, PathSegment, parse_dgspec};
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};
use serde_json::Value;

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

/// The first place two JSON values differ.
fn first_difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<_> = x.keys().chain(y.keys()).collect();
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
    /// Both wrote a spec, and the specs agree; the generated text.
    Agree(String),
    /// Both declined.
    Declined,
}

/// Obtain `project`'s spec down both routes and require them to agree.
fn both_routes(project: &Path) -> Result<Graphs, String> {
    let env = SdkDiscoveryEnv::from_process_env();
    let root = dotnet_root();
    let generated = restore_graph(project, &root, &env, RestoreGraphSource::Generated);
    let restored = restore_graph(project, &root, &env, RestoreGraphSource::Restored);
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
            let a = normalised(&generated, &generated_scratch);
            let b = normalised(&restored, &restored_scratch);
            match first_difference(&a, &b, "$") {
                None => Ok(Graphs::Agree(generated)),
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
    root
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
fn committed_spec(project: &Path) -> DependencyGraphSpec {
    let text = match both_routes(project) {
        Ok(Graphs::Agree(text)) => text,
        Ok(Graphs::Declined) => panic!("{}: both routes declined", project.display()),
        Err(failure) => panic!("{failure}"),
    };
    assert!(
        !project.parent().unwrap().join("obj").exists(),
        "neither route may write the project's obj/"
    );
    parse_dgspec(&text).unwrap_or_else(|decline| panic!("the reader declined: {decline}"))
}

#[test]
fn a_single_project_generates_the_spec_restore_reads() {
    let root = write_tree("single", &[("App.fsproj", APP), ("App.fs", "module App\n")]);
    let spec = committed_spec(&root.join("App.fsproj"));
    let app = std::fs::canonicalize(root.join("App.fsproj")).unwrap();
    assert_eq!(
        spec.restore,
        vec![app.to_str().unwrap().to_owned()],
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

/// A project named through a symlinked directory: restore runs in the
/// project's directory, which the OS reports by its real path, so the project
/// path and the working directory spell the directory differently. The
/// reference must survive that, in the spec restore itself reads.
#[cfg(unix)]
#[test]
fn a_project_named_through_a_symlink_keeps_its_project_reference() {
    let root = write_tree(
        "symlinked",
        &[
            ("real/App.fsproj", APP_WITH_LIB),
            ("real/App.fs", "module App\n"),
            ("real/lib/Lib.fsproj", LIB),
            ("real/lib/Lib.fs", "module Lib\n"),
        ],
    );
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    let project = root.join("link").join("App.fsproj");

    let env = SdkDiscoveryEnv::from_process_env();
    let RestoreGraph::Written { text, .. } =
        restore_graph(&project, &dotnet_root(), &env, RestoreGraphSource::Restored)
    else {
        panic!("restore wrote no spec");
    };
    let spec =
        parse_dgspec(&text).unwrap_or_else(|decline| panic!("the reader declined: {decline}"));
    let entry = spec
        .projects
        .iter()
        .find(|p| p.restore.project_unique_name.ends_with("App.fsproj"))
        .expect("the entry is in the spec");
    assert_eq!(
        entry.restore.frameworks[0].project_references.len(),
        1,
        "restore must see the entry's ProjectReference"
    );
    // And the generated spec is that one, as for any other project.
    committed_spec(&project);
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
    match both_routes(&root.join("App.fsproj")) {
        Ok(Graphs::Declined) => {}
        Ok(Graphs::Agree(_)) => panic!("a locked project must decline"),
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
#[ignore = "the half-A oracle over restored projects; set BORZOI_PROJECT_LIST and run with --ignored under nix develop"]
fn pinned_projects() {
    let Some(list) = std::env::var_os("BORZOI_PROJECT_LIST") else {
        eprintln!("BORZOI_PROJECT_LIST unset; skipping. See the module docs.");
        return;
    };
    let projects: Vec<PathBuf> = std::env::split_paths(&list).collect();
    assert!(!projects.is_empty(), "BORZOI_PROJECT_LIST is empty");
    let mut lines = Vec::new();
    let mut failures = Vec::new();
    for project in &projects {
        let key = checkout_key(project);
        match both_routes(project) {
            Ok(Graphs::Agree(text)) => lines.push(match parse_dgspec(&text) {
                Ok(spec) => format!("{key} {}", committed_label(&spec)),
                Err(decline) => format!("{key} {}", decline_label(&decline)),
            }),
            Ok(Graphs::Declined) => lines.push(format!("{key} not-restorable")),
            Err(failure) => failures.push(failure),
        }
    }
    assert!(
        failures.is_empty(),
        "the generated spec must be the one restore reads:\n{}",
        failures.join("\n")
    );
    let manifest = Manifest::from_entries(lines).unwrap_or_else(|e| panic!("manifest: {e}"));
    check(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/manifests/restore_graph_projects.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 BORZOI_PROJECT_LIST=… nix develop -c cargo test -p borzoi --test all \
             restore_graph::pinned_projects -- --ignored"
        ),
    );
}
