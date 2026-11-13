//! Stage 8 of `docs/nuget-dgspec-boundary-plan.md`: the restore-cone census.
//!
//! Half A of restore (project → dependency-graph spec) is MSBuild evaluation
//! plus the targets and tasks `GenerateRestoreGraphFile` runs. A Rust model of
//! it (A₁) would have to reproduce every one of those targets and tasks; a
//! deterministic .NET runtime (A₂) would run them as they are. This census
//! measures what actually runs, so the choice rests on data.
//!
//! The `restoreCone` op of `tools/msbuild-condition-oracle` runs the target
//! in-process under a logger and reports each target and task that executed,
//! the content hash of every file defining one of those targets, and each task
//! assembly's identity. It runs under exactly the global properties the LSP's
//! offline restore uses (`borzoi_oracle_harness::restore_globals`, which
//! `borzoi`'s `restore_cone_globals.rs` holds to `restore.rs`'s command line),
//! and in a child whose environment is scrubbed as every oracle child here is,
//! so the cone does not move with the host's environment. Two checks make the
//! census a census of A₀ rather than of something nearby:
//!
//! - **Same computation.** Under the SDK `dotnet` resolves for the project,
//!   `dotnet msbuild -t:GenerateRestoreGraphFile` under the same globals, with
//!   the oracle's logger attached, must execute exactly the targets and tasks
//!   the in-process build does, and write the same spec up to the scratch
//!   directory. The cone is compared, not only the spec, because a target that
//!   runs in one and not the other can leave the spec unchanged. Each oracle
//!   runs from its project's directory, as `restore.rs` runs `dotnet`, since
//!   MSBuild exposes that directory to the build (`MSBuildStartupDirectory`).
//! - **Same SDK.** The oracle is pinned to one installed SDK (`--msbuild-sdk`)
//!   and reports the SDK version its evaluation imported, which must be that
//!   one.
//!
//! Every SDK installed beside `dotnet` is measured, not only the one each
//! project resolves: a cone run under an SDK the project does not resolve is
//! labelled `forced`, and is a measurement of how the cone moves across SDK
//! releases rather than of what the project runs.
//!
//! [`pinned_projects`] (`#[ignore]`d; CI's `corpus-diff` job runs it over the
//! pinned project corpus) pins each corpus project's cone in
//! `tests/manifests/restore_cone_projects.txt`. [`fixtures`] does the same for
//! generated projects that reach what the corpus does not (a `net10.0` target,
//! multi-targeting with central package management and a project reference,
//! a user target hooked into the restore targets), pinned in
//! `tests/manifests/restore_cone_fixtures.txt`.
//!
//! Each cone also grades the item seam (stage A₁.2, [`seam`]): the
//! `_RestoreGraphEntry` items the build hands `WriteRestoreGraphTask`, put
//! through `borzoi_nuget`'s `write_restore_graph`, must give the cone's own
//! spec byte for byte, or decline. The verdict is pinned as the cone's `seam`
//! line.
//!
//! File spellings are host-independent: `sdk:` relative to the SDK directory,
//! `dotnet:` relative to the dotnet root, and `project:`/`fixture:` relative to
//! the tree the project lives in. A file anywhere else fails the census, which
//! has no spelling for it. Target files are identified by the leading
//! [`HASH_DIGITS`] hex digits of their SHA-256, which agree between the macOS
//! and Linux builds of one SDK. Task assemblies are identified by name, version
//! and informational version (which names the commit the SDK was built from):
//! their bytes and module MVIDs differ between those builds.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use borzoi_oracle_harness::corpus_key::corpus_relative;
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};
use borzoi_oracle_harness::restore_globals::{
    EMPTY_SOURCE, GENERATE_RESTORE_GRAPH_TARGET, generate_restore_graph_globals,
};
use borzoi_oracle_harness::{BoundedCommand, default_timeout};
use common::{Oracle, dotnet_root_from_env, scrub_oracle_env};
use serde_json::Value;

/// Hex digits of a target file's SHA-256 the manifest keeps.
const HASH_DIGITS: usize = 16;

/// The task that writes the spec: every cone that produced one ran it.
const WRITE_TASK: &str = "WriteRestoreGraphTask";

/// `dotnet <args>` from `dir`, under the oracle children's scrubbed
/// environment; its standard output.
fn dotnet(dir: &Path, args: &[&std::ffi::OsStr]) -> String {
    let mut cmd = Command::new(dotnet_root_from_env().join("dotnet"));
    cmd.current_dir(dir).args(args);
    scrub_oracle_env(&mut cmd);
    let output = BoundedCommand::new(cmd)
        .timeout(default_timeout())
        .run_ok(format_args!("dotnet {args:?} in {}", dir.display()));
    String::from_utf8(output.stdout).expect("dotnet writes UTF-8")
}

/// The versions of every SDK installed beside `dotnet`.
fn installed_sdks() -> Vec<String> {
    let listing = dotnet(Path::new("/"), &["--list-sdks".as_ref()]);
    let sdks: Vec<String> = listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    assert!(!sdks.is_empty(), "no SDK installed:\n{listing}");
    sdks
}

/// The SDK `dotnet` resolves for `project`: the one its directory's
/// `global.json` selects, as `restore.rs` resolves it by running there.
fn resolved_sdk(project: &Path) -> String {
    dotnet(project.parent().unwrap(), &["--version".as_ref()])
        .trim()
        .to_owned()
}

/// The package folder `restore.rs` pins: `$NUGET_PACKAGES`, else
/// `<home>/.nuget/packages`.
fn cache_root() -> PathBuf {
    let non_empty = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    non_empty("NUGET_PACKAGES")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".nuget/packages")))
        .expect("neither NUGET_PACKAGES nor HOME is set")
}

/// A scratch directory laid out as `restore.rs` lays out its own, by its real
/// path so that a file under it is recognised whichever spelling MSBuild
/// reports.
struct Scratch {
    _dir: tempfile::TempDir,
    path: PathBuf,
    output: PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        let dir = tempfile::tempdir().expect("scratch directory");
        let path = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(path.join(EMPTY_SOURCE)).unwrap();
        let output = path.join("restore-graph.dg.json");
        Scratch {
            _dir: dir,
            path,
            output,
        }
    }

    fn globals(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        generate_restore_graph_globals(&self.path, &cache_root(), &self.output)
    }

    /// `spec` as JSON, with this scratch directory replaced by `<scratch>`.
    fn normalised(&self, spec: &str) -> Value {
        let scratch = self.path.to_str().expect("UTF-8 scratch path");
        serde_json::from_str(&spec.replace(scratch, "<scratch>")).expect("the spec is JSON")
    }
}

/// What `dotnet msbuild -t:GenerateRestoreGraphFile` executes and writes for
/// `project` under the census globals, run from the project's directory as
/// `restore.rs` runs it, with the oracle's `ConeLogger` attached: the executed
/// cone (as [`Cone::executed`] spells it) and the normalised spec.
fn generated_by_cli(project: &Path) -> (Executed, Value) {
    let scratch = Scratch::new();
    let recorded = scratch.path.join("cli-cone.json");
    let mut logger = std::ffi::OsString::from("-logger:MsBuildConditionOracle.Program+ConeLogger,");
    logger.push(common::oracle_assembly());
    logger.push(";");
    logger.push(&recorded);
    let mut args: Vec<std::ffi::OsString> = vec![
        "msbuild".into(),
        project.into(),
        "-nologo".into(),
        format!("-t:{GENERATE_RESTORE_GRAPH_TARGET}").into(),
        logger,
    ];
    for (name, value) in scratch.globals() {
        let mut arg = std::ffi::OsString::from(format!("-p:{name}="));
        arg.push(value);
        args.push(arg);
    }
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    dotnet(project.parent().unwrap(), &args);
    let spec = std::fs::read_to_string(&scratch.output)
        .unwrap_or_else(|e| panic!("{}: dotnet msbuild wrote no spec: {e}", project.display()));
    let cone: Value =
        serde_json::from_str(&std::fs::read_to_string(&recorded).unwrap_or_else(|e| {
            panic!("{}: the CLI's logger wrote nothing: {e}", project.display())
        }))
        .expect("the logger writes JSON");
    assert_eq!(
        cone["errors"],
        serde_json::json!([]),
        "{}",
        project.display()
    );
    (executed(&cone), scratch.normalised(&spec))
}

/// An executed cone as a multiset: each target as `(name, file)` and each task
/// as `(name, containing target, assembly)`, sorted. Order is not compared, since
/// the CLI may build on several nodes.
#[derive(Debug, PartialEq, Eq)]
struct Executed {
    targets: Vec<(String, String)>,
    tasks: Vec<(String, String, String)>,
}

fn executed(answer: &Value) -> Executed {
    let list = |field: &str| {
        answer[field]
            .as_array()
            .unwrap_or_else(|| panic!("{field}"))
    };
    let mut targets: Vec<_> = list("targets")
        .iter()
        .map(|t| (string(t, "name"), string(t, "file")))
        .collect();
    let mut tasks: Vec<_> = list("tasks")
        .iter()
        .map(|t| {
            (
                string(t, "name"),
                string(t, "target"),
                string(t, "assembly"),
            )
        })
        .collect();
    targets.sort();
    tasks.sort();
    Executed { targets, tasks }
}

/// The first place two JSON values differ: `in-process` is `a`, `cli` is `b`.
fn first_difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<_> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(v), Some(w)) => first_difference(v, w, &format!("{path}.{k}")),
                (v, w) => Some(format!("{path}.{k}: in-process {v:?} / cli {w:?}")),
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (v, w))| first_difference(v, w, &format!("{path}[{i}]"))),
        _ if a == b => None,
        _ => Some(format!("{path}: in-process {a} / cli {b}")),
    }
}

/// Keys whose objects are keyed by name (a project path, a framework alias, a
/// package id), generalised to `*` in a spec drift path so it names the place in
/// the schema rather than the item.
const NAME_KEYED: &[&str] = &[
    "projects",
    "frameworks",
    "dependencies",
    "centralPackageVersions",
    "packagesToPrune",
    "downloadDependencies",
    "frameworkReferences",
    "projectReferences",
    "sources",
];

/// Every schema place where `a` and `b` differ: a leaf or member present in
/// one and not the other, or two leaves that differ. Name-keyed members are
/// generalised to `*` ([`NAME_KEYED`], and the document's top-level `restore`,
/// which is keyed by project path where a project's own `restore` is not) and
/// array elements to `[]`.
fn drift(
    a: Option<&Value>,
    b: Option<&Value>,
    path: &str,
    parent: &str,
    out: &mut BTreeSet<String>,
) {
    match (a, b) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            for key in x.keys().chain(y.keys()).collect::<BTreeSet<_>>() {
                let child = if NAME_KEYED.contains(&parent) || path == "$.restore" {
                    format!("{path}.*")
                } else {
                    format!("{path}.{key}")
                };
                drift(x.get(key), y.get(key), &child, key, out);
            }
        }
        (Some(Value::Array(x)), Some(Value::Array(y))) if x.len() == y.len() => {
            for (v, w) in x.iter().zip(y) {
                drift(Some(v), Some(w), &format!("{path}[]"), "", out);
            }
        }
        (a, b) if a == b => {}
        _ => {
            out.insert(path.to_owned());
        }
    }
}

/// One target that executed.
struct TargetRun {
    name: String,
    file: PathBuf,
}

/// One task that executed.
struct TaskRun {
    name: String,
    target: String,
    assembly: PathBuf,
}

/// A `restoreCone` answer, checked.
struct Cone {
    sdk_dir: PathBuf,
    targets: Vec<TargetRun>,
    tasks: Vec<TaskRun>,
    /// Each target file's SHA-256, hex.
    files: BTreeMap<PathBuf, String>,
    /// Each task assembly's identity, rendered.
    assemblies: BTreeMap<PathBuf, String>,
    /// Every assembly this subject's oracle process loaded while its build ran,
    /// rendered as name and informational version.
    loaded: BTreeMap<PathBuf, String>,
    /// The spec the build wrote, normalised.
    spec: Value,
    /// The executed targets and tasks, for comparison with the CLI's.
    executed: Executed,
    /// The item seam's verdict on this cone ([`seam`]).
    seam: String,
}

fn string(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("restoreCone answer has no string {field}: {value}"))
        .to_owned()
}

/// Run the census op on `project` under SDK `sdk` (installed at `sdk_dir`, the
/// one `oracle` is pinned to).
fn cone(oracle: &mut Oracle, project: &Path, sdk: &str, sdk_dir: &Path) -> Cone {
    let scratch = Scratch::new();
    let answer = oracle.restore_cone(project, &scratch.globals());
    assert_eq!(
        answer["succeeded"],
        Value::Bool(true),
        "{}: GenerateRestoreGraphFile failed under SDK {sdk}: {}",
        project.display(),
        answer["errors"]
    );
    assert_eq!(
        answer["errors"],
        serde_json::json!([]),
        "{}",
        project.display()
    );
    assert_eq!(
        string(&answer, "sdkVersion"),
        sdk,
        "{}: the evaluation imported another SDK than the oracle was pinned to",
        project.display()
    );
    assert_eq!(
        PathBuf::from(string(&answer, "msbuildDirectory")),
        sdk_dir,
        "the oracle runs another MSBuild than the one it was pinned to"
    );
    let list = |field: &str| {
        answer[field]
            .as_array()
            .unwrap_or_else(|| panic!("{field}"))
    };
    let targets = list("targets")
        .iter()
        .map(|t| TargetRun {
            name: string(t, "name"),
            file: string(t, "file").into(),
        })
        .collect();
    let tasks = list("tasks")
        .iter()
        .map(|t| TaskRun {
            name: string(t, "name"),
            target: string(t, "target"),
            assembly: string(t, "assembly").into(),
        })
        .collect();
    let object = |field: &str| {
        answer[field]
            .as_object()
            .unwrap_or_else(|| panic!("{field}"))
    };
    let files = object("files")
        .iter()
        .map(|(path, hash)| (PathBuf::from(path), hash.as_str().unwrap().to_owned()))
        .collect();
    let assemblies = object("assemblies")
        .iter()
        .map(|(path, id)| {
            let rendered = format!(
                "{} {} {}",
                string(id, "name"),
                string(id, "version"),
                string(id, "informationalVersion")
            );
            (PathBuf::from(path), rendered)
        })
        .collect();
    let loaded = list("loadedDuringBuilds")
        .iter()
        .map(|a| {
            let rendered = format!(
                "{} {}",
                string(a, "name"),
                string(a, "informationalVersion")
            );
            (PathBuf::from(string(a, "path")), rendered)
        })
        .collect();
    let spec = scratch.normalised(&string(&answer, "dgspec"));
    let executed = executed(&answer);
    let seam = seam(project, &answer, &loaded);
    Cone {
        sdk_dir: sdk_dir.to_path_buf(),
        targets,
        tasks,
        files,
        assemblies,
        loaded,
        spec,
        executed,
        seam,
    }
}

/// The item seam on this cone (stage A₁.2 of the plan): the
/// `_RestoreGraphEntry` items the build hands `WriteRestoreGraphTask`, put
/// through `borzoi_nuget`'s `write_restore_graph`, must give exactly the spec
/// the cone wrote, or decline. A commit with any other text panics, since
/// under certain-implies-exact it is a bug. The verdict is `agree`, or the
/// decline's shape (without the item index or value, which are not
/// host-independent).
///
/// The entries are the task's own input, as the census build logged it: after
/// `RemoveDuplicates` and after any target hooked in before the task. The
/// NuGet identity is that of the `NuGet.Commands` the census build loaded,
/// and the environment is the oracle's scrubbed one.
fn seam(project: &Path, answer: &Value, loaded: &BTreeMap<PathBuf, String>) -> String {
    use borzoi_nuget::dgspec::{
        NuGetIdentity, RestoreGraphItem, RestoreItemsDecline, write_restore_graph,
    };

    let items: Vec<RestoreGraphItem> = answer["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|item| RestoreGraphItem {
            identity: string(item, "identity"),
            metadata: item["metadata"]
                .as_object()
                .expect("metadata")
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().expect("string metadata").to_owned()))
                .collect(),
        })
        .collect();
    // The task's bool parameter, as MSBuild converts the property.
    let recursive = match answer["restoreRecursive"].as_str().unwrap_or_default() {
        v if v.eq_ignore_ascii_case("true") => true,
        v if v.is_empty() || v.eq_ignore_ascii_case("false") => false,
        other => panic!("RestoreRecursive is {other:?}, which the census does not convert"),
    };
    let nuget = loaded
        .values()
        .find_map(|rendered| rendered.strip_prefix("NuGet.Commands "))
        .unwrap_or_else(|| panic!("{}: the build loaded no NuGet.Commands", project.display()));
    let environment = common::oracle_environment()
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    match write_restore_graph(&items, recursive, &NuGetIdentity::new(nuget), &environment) {
        Ok(file) => {
            let theirs = string(answer, "dgspec");
            assert!(
                file.text == theirs,
                "{}: the item seam wrote another spec than the build did\n--- ours\n{}\n--- the build's\n{theirs}",
                project.display(),
                file.text
            );
            "agree".to_owned()
        }
        Err(RestoreItemsDecline::Value { name, detail, .. }) => {
            format!("decline {name}: {detail}")
        }
        Err(RestoreItemsDecline::Vocabulary { detail, .. }) => format!("decline {detail}"),
        Err(RestoreItemsDecline::UnverifiedNuGet { identity }) => {
            format!("decline unverified NuGet {identity}")
        }
        Err(other) => format!("decline {other}"),
    }
}

/// One project in a census: its file, its manifest key, and the tree its own
/// files are spelt relative to (`(tag, root)`).
struct Subject {
    project: PathBuf,
    key: String,
    tag: &'static str,
    root: PathBuf,
}

impl Subject {
    /// `path`'s host-independent spelling under `cone`'s SDK.
    fn spell(&self, cone: &Cone, path: &Path) -> String {
        let slashed = |rel: &Path| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        };
        // The SDK's own installation, and the one `DOTNET_ROOT` names, which
        // may gather several SDKs' installations under links.
        let installed = dotnet_root_from_env();
        let dotnet_roots = [
            cone.sdk_dir
                .parent()
                .and_then(Path::parent)
                .unwrap()
                .to_path_buf(),
            std::fs::canonicalize(&installed).unwrap(),
            installed,
        ];
        if let Ok(rel) = path.strip_prefix(&cone.sdk_dir) {
            format!("sdk:{}", slashed(rel))
        } else if let Some(rel) = dotnet_roots.iter().find_map(|r| path.strip_prefix(r).ok()) {
            format!("dotnet:{}", slashed(rel))
        } else if path.starts_with(&self.root) {
            format!("{}:{}", self.tag, corpus_relative(&self.root, path))
        } else {
            panic!(
                "{}: {} is outside the SDK, the dotnet root and the project's tree, \
                 so the census has no host-independent spelling for it",
                self.key,
                path.display()
            )
        }
    }
}

/// Whether a spelling names a file shipped with the SDK.
fn shipped(spelling: &str) -> bool {
    spelling.starts_with("sdk:") || spelling.starts_with("dotnet:")
}

/// What one SDK's cones add up to across the subjects.
#[derive(Default)]
struct SdkCensus {
    resolved: usize,
    forced: usize,
    targets: BTreeMap<(String, String), BTreeSet<String>>,
    tasks: BTreeMap<(String, String), BTreeSet<String>>,
    /// The SDK's assemblies loaded while any census build ran.
    loaded: BTreeMap<String, String>,
}

impl SdkCensus {
    fn render(&self, sdk: &str) -> String {
        let user_targets = self.targets.keys().filter(|(_, f)| !shipped(f)).count();
        let user_tasks = self.tasks.keys().filter(|(_, a)| !shipped(a)).count();
        let target_names: BTreeSet<&String> = self.targets.keys().map(|(n, _)| n).collect();
        let target_files: BTreeSet<&String> = self.targets.keys().map(|(_, f)| f).collect();
        let task_names: BTreeSet<&String> = self.tasks.keys().map(|(n, _)| n).collect();
        let assemblies: BTreeSet<&String> = self.tasks.keys().map(|(_, a)| a).collect();
        let mut out = format!(
            "SDK {sdk}: {} subjects resolve it, {} forced; {} distinct targets in {} files \
             ({user_targets} outside the SDK); {} distinct tasks from {} assemblies \
             ({user_tasks} outside the SDK)\n",
            self.resolved,
            self.forced,
            target_names.len(),
            target_files.len(),
            task_names.len(),
            assemblies.len()
        );
        for ((name, file), subjects) in &self.targets {
            out.push_str(&format!("  target {name} {file} ({})\n", subjects.len()));
        }
        for ((name, assembly), subjects) in &self.tasks {
            out.push_str(&format!("  task {name} {assembly} ({})\n", subjects.len()));
        }
        for (assembly, identity) in &self.loaded {
            out.push_str(&format!("  loaded {assembly} {identity}\n"));
        }
        out
    }

    fn summary(&self) -> String {
        let target_names: BTreeSet<&String> = self.targets.keys().map(|(n, _)| n).collect();
        let task_names: BTreeSet<&String> = self.tasks.keys().map(|(n, _)| n).collect();
        let assemblies: BTreeSet<&String> = self.tasks.keys().map(|(_, a)| a).collect();
        format!(
            "resolved={} forced={} targets={} target-files={} user-targets={} tasks={} \
             task-assemblies={} user-tasks={} loaded-sdk-assemblies={}",
            self.resolved,
            self.forced,
            target_names.len(),
            self.targets
                .keys()
                .map(|(_, f)| f)
                .collect::<BTreeSet<_>>()
                .len(),
            self.targets.keys().filter(|(_, f)| !shipped(f)).count(),
            task_names.len(),
            assemblies.len(),
            self.tasks.keys().filter(|(_, a)| !shipped(a)).count(),
            self.loaded.len(),
        )
    }
}

/// The census of `subjects` under every installed SDK: the manifest, and a
/// readable report.
fn census(subjects: &[Subject]) -> (Manifest, String) {
    let resolved: Vec<String> = subjects.iter().map(|s| resolved_sdk(&s.project)).collect();
    let sdks = installed_sdks();
    for (subject, sdk) in subjects.iter().zip(&resolved) {
        assert!(
            sdks.contains(sdk),
            "{} resolves SDK {sdk}, which `dotnet --list-sdks` does not list",
            subject.key
        );
    }
    let mut items = Vec::new();
    let mut shared = BTreeSet::new();
    // Each subject's spec under the previous SDK measured, for drift.
    let mut previous: Vec<Option<(String, Value)>> = subjects.iter().map(|_| None).collect();
    let mut report = String::new();
    for sdk in &sdks {
        // By its real path, which is how the muxer reaches it: under the other
        // spelling MSBuild records paths into the SDK (the runtime identifier
        // graph) that `dotnet msbuild` does not.
        let sdk_dir = std::fs::canonicalize(dotnet_root_from_env().join("sdk").join(sdk))
            .unwrap_or_else(|e| panic!("SDK {sdk} is not installed under DOTNET_ROOT: {e}"));
        let mut per_sdk = SdkCensus::default();
        for ((subject, resolved), previous) in subjects.iter().zip(&resolved).zip(&mut previous) {
            let how = if resolved == sdk {
                "resolved"
            } else {
                "forced"
            };
            // One oracle per project, from its directory: MSBuild fixes the
            // startup directory once per process.
            let mut oracle = Oracle::spawn_for_sdk(&sdk_dir, subject.project.parent().unwrap());
            let cone = cone(&mut oracle, &subject.project, sdk, &sdk_dir);
            if let Some((before, spec)) = previous.replace((sdk.clone(), cone.spec.clone())) {
                let mut places = BTreeSet::new();
                drift(Some(&spec), Some(&cone.spec), "$", "", &mut places);
                let places = if places.is_empty() {
                    "none".to_owned()
                } else {
                    places.into_iter().collect::<Vec<_>>().join(" ")
                };
                items.push((
                    format!("spec drift {before}..{sdk} {}", subject.key),
                    places,
                ));
            }
            if how == "resolved" {
                per_sdk.resolved += 1;
                let (cli_cone, cli) = generated_by_cli(&subject.project);
                assert_eq!(
                    cone.executed, cli_cone,
                    "{}: the in-process build executed another cone than `dotnet msbuild` does",
                    subject.key
                );
                if let Some(difference) = first_difference(&cone.spec, &cli, "$") {
                    panic!(
                        "{}: the in-process build wrote another spec than `dotnet msbuild` \
                         does: {difference}",
                        subject.key
                    );
                }
            } else {
                per_sdk.forced += 1;
            }
            assert!(
                cone.targets
                    .iter()
                    .any(|t| t.name == GENERATE_RESTORE_GRAPH_TARGET),
                "{}: the census did not see {GENERATE_RESTORE_GRAPH_TARGET} run",
                subject.key
            );
            assert!(
                cone.tasks.iter().any(|t| t.name == WRITE_TASK),
                "{}: the census did not see {WRITE_TASK} run",
                subject.key
            );
            let prefix = format!("{sdk} {how} {}", subject.key);
            items.push((format!("{prefix} seam"), cone.seam.clone()));
            for target in &cone.targets {
                let file = subject.spell(&cone, &target.file);
                per_sdk
                    .targets
                    .entry((target.name.clone(), file.clone()))
                    .or_default()
                    .insert(subject.key.clone());
                items.push((format!("{prefix} target {}", target.name), file));
            }
            for task in &cone.tasks {
                let assembly = subject.spell(&cone, &task.assembly);
                per_sdk
                    .tasks
                    .entry((task.name.clone(), assembly.clone()))
                    .or_default()
                    .insert(subject.key.clone());
                items.push((
                    format!("{prefix} task {}", task.name),
                    format!("{assembly} in {}", task.target),
                ));
            }
            for (file, hash) in &cone.files {
                shared.insert((
                    format!("{sdk} file {}", subject.spell(&cone, file)),
                    format!("sha256:{}", &hash[..HASH_DIGITS]),
                ));
            }
            // Each subject has its own oracle process, so the SDK's set is the
            // union over subjects. Only the SDK's own assemblies: the runtime's
            // are the BCL any replacement runs on.
            per_sdk.loaded.extend(
                cone.loaded
                    .iter()
                    .map(|(path, identity)| (subject.spell(&cone, path), identity.clone()))
                    .filter(|(spelling, _)| spelling.starts_with("sdk:")),
            );
            for (assembly, identity) in &cone.assemblies {
                shared.insert((
                    format!("{sdk} assembly {}", subject.spell(&cone, assembly)),
                    identity.clone(),
                ));
            }
        }
        items.push((format!("{sdk} census"), per_sdk.summary()));
        for (assembly, identity) in &per_sdk.loaded {
            items.push((format!("{sdk} loaded {assembly}"), identity.clone()));
        }
        report.push_str(&per_sdk.render(sdk));
    }
    items.extend(shared);
    let manifest = Manifest::from_keyed(items).unwrap_or_else(|e| panic!("manifest: {e}"));
    (manifest, report)
}

fn manifest_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/manifests")
        .join(name)
}

#[test]
#[ignore = "the restore-cone census over restored projects; set BORZOI_PROJECT_LIST and run with --ignored under nix develop"]
fn pinned_projects() {
    let Some(list) = std::env::var_os("BORZOI_PROJECT_LIST") else {
        eprintln!("BORZOI_PROJECT_LIST unset; skipping. See the module docs.");
        return;
    };
    let subjects: Vec<Subject> = std::env::split_paths(&list)
        .map(|project| {
            let checkout = project
                .ancestors()
                .find(|d| d.join(".git").exists())
                .unwrap_or_else(|| panic!("{} is not inside a checkout", project.display()));
            let root = checkout.parent().unwrap().to_path_buf();
            Subject {
                key: corpus_relative(&root, &project),
                project,
                tag: "project",
                root,
            }
        })
        .collect();
    assert!(!subjects.is_empty(), "BORZOI_PROJECT_LIST is empty");
    let (manifest, report) = census(&subjects);
    println!("{report}");
    check(
        &manifest_path("restore_cone_projects.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 BORZOI_PROJECT_LIST=… nix develop -c cargo test -p borzoi-msbuild \
             --test restore_cone_census pinned_projects -- --ignored"
        ),
    );
}

const NET10: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
  </ItemGroup>
</Project>
"#;

const CPM_PACKAGES: &str = r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
    <CentralPackageTransitivePinningEnabled>true</CentralPackageTransitivePinningEnabled>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="System.Text.Json" Version="8.0.5" />
  </ItemGroup>
</Project>
"#;

const CPM_APP: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
    <TreatWarningsAsErrors>true</TreatWarningsAsErrors>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
    <PackageReference Include="System.Text.Json" />
    <ProjectReference Include="../lib/Lib.fsproj" />
  </ItemGroup>
</Project>
"#;

const CPM_LIB: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFrameworks>net8.0;netstandard2.0</TargetFrameworks>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Lib.fs" />
  </ItemGroup>
</Project>
"#;

/// The user target [`HOOKED`] hooks into the restore targets. It runs only when
/// MSBuild starts from the project's directory, as `restore.rs` starts it, so
/// the census must start there too to see it.
const USER_HOOK: &str = "UserHookBeforeRestoreGraphEntry";

const HOOKED: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
  </ItemGroup>
  <ItemGroup>
    <!-- An escaped `;` in metadata the writer writes: the item seam must see it
         as the task does, unescaped. -->
    <PackageReference Include="Fake.Package" Version="1.0.0" Aliases="x%3By" />
  </ItemGroup>
  <Target Name="UserHookBeforeRestoreGraphEntry" BeforeTargets="_GenerateRestoreGraphProjectEntry"
          Condition="'$(MSBuildStartupDirectory)' == '$(MSBuildProjectDirectory)'">
    <Message Text="a user target in the restore cone" />
  </Target>
  <!-- Edits the entries after `_GenerateRestoreGraph` and before the writer runs:
       the item seam must be graded on what the writer is handed. -->
  <Target Name="UserHookEditsRestoreGraphEntries" BeforeTargets="GenerateRestoreGraphFile">
    <ItemGroup>
      <_RestoreGraphEntry Condition="'%(_RestoreGraphEntry.Type)' == 'ProjectSpec'">
        <Version>9.9.9</Version>
      </_RestoreGraphEntry>
    </ItemGroup>
  </Target>
</Project>
"#;

/// Generated projects that reach what the corpus does not, and a user target
/// hooked into restore, which the census must see.
#[test]
fn fixtures() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let files = [
        ("net10/App.fsproj", NET10),
        ("net10/App.fs", "module App\n"),
        ("cpm/Directory.Packages.props", CPM_PACKAGES),
        ("cpm/app/App.fsproj", CPM_APP),
        ("cpm/app/App.fs", "module App\n"),
        ("cpm/lib/Lib.fsproj", CPM_LIB),
        ("cpm/lib/Lib.fs", "module Lib\n"),
        ("hooked/App.fsproj", HOOKED),
        ("hooked/App.fs", "module App\n"),
    ];
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let subjects: Vec<Subject> = [
        "net10/App.fsproj",
        "cpm/app/App.fsproj",
        "hooked/App.fsproj",
    ]
    .into_iter()
    .map(|key| Subject {
        project: root.join(key),
        key: key.to_owned(),
        tag: "fixture",
        root: root.clone(),
    })
    .collect();
    let (manifest, report) = census(&subjects);
    println!("{report}");
    let hooked: Vec<&String> = manifest
        .entries()
        .iter()
        .filter(|e| e.contains(&format!(" hooked/App.fsproj target {USER_HOOK}\t")))
        .collect();
    assert!(
        !hooked.is_empty()
            && hooked
                .iter()
                .all(|e| e.ends_with("\tfixture:hooked/App.fsproj")),
        "the census must see the user target hooked into restore, in the project's file: {hooked:?}"
    );
    check(
        &manifest_path("restore_cone_fixtures.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-msbuild --test restore_cone_census \
             fixtures"
        ),
    );
}
