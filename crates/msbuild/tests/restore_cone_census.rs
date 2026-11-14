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
//! line. Only a cone of the SDK's targets alone is graded: a user target can
//! run anything around the writer.
//!
//! Each cone under the SDK its project resolves also gets a read census
//! (stage A₁.3a, [`reads`]). Every property and item type its targets
//! reference is read at the end of our evaluation and at the end of MSBuild's.
//! The verdicts are pinned in `tests/manifests/restore_cone_reads_projects.txt`
//! and `restore_cone_reads_fixtures.txt`.
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

use borzoi_msbuild::observed::host_fs;
use host_fs::HostPath;
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
        let path = host_fs::canonicalize(dir.path()).unwrap();
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
    let spec = host_fs::read_to_string(&scratch.output)
        .unwrap_or_else(|e| panic!("{}: dotnet msbuild wrote no spec: {e}", project.display()));
    let cone: Value =
        serde_json::from_str(&host_fs::read_to_string(&recorded).unwrap_or_else(|e| {
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
    /// The project the target ran in.
    project: PathBuf,
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
    /// The `restoreCone` answer, which [`seam`] grades the item seam on.
    answer: Value,
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
            project: string(t, "project").into(),
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
    Cone {
        sdk_dir: sdk_dir.to_path_buf(),
        targets,
        tasks,
        files,
        assemblies,
        loaded,
        spec,
        executed,
        answer,
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
/// Graded only on a cone of the SDK's targets alone (the census checks), so
/// the one writer invocation is `GenerateRestoreGraphFile`'s. The entries are
/// its own input, as the census build logged it, after `RemoveDuplicates`. The
/// NuGet identity is that of the `NuGet.Commands` the census build loaded,
/// and the environment is the oracle's scrubbed one.
fn seam(project: &Path, answer: &Value, loaded: &BTreeMap<PathBuf, String>) -> String {
    use borzoi_nuget::dgspec::{
        NuGetIdentity, RestoreGraphItem, RestoreItemsDecline, write_restore_graph,
    };

    let writer = &answer["writer"];
    if writer.is_null() {
        panic!(
            "{}: no single writer invocation wrote the spec in a cone of SDK targets",
            project.display()
        );
    }
    let items: Vec<RestoreGraphItem> = writer["entries"]
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
    // The invocation's own bool parameter: unset is the task's default, false.
    let recursive = match writer["restoreRecursive"].as_str() {
        None => false,
        Some(v) if v.eq_ignore_ascii_case("true") => true,
        Some(v) if v.eq_ignore_ascii_case("false") => false,
        Some(other) => panic!("RestoreRecursive is {other:?}, which the census does not convert"),
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

/// What the cone's own targets reference and write, read from their XML: the
/// read census's subjects (stage A₁.3a of the plan). Each map is keyed by the
/// lowercase name and holds the least spelling the targets use, so the
/// manifest's spelling does not depend on which file was read first.
#[derive(Default)]
struct ConeText {
    /// Every `$(Name)` in a target's attributes and text.
    properties: BTreeMap<String, String>,
    /// Every `@(Type)`, and the `Type` of every qualified `%(Type.Name)`.
    items: BTreeMap<String, String>,
    /// Property names a target writes: a `PropertyGroup` child, or a task
    /// `Output`'s `PropertyName`.
    written_properties: BTreeSet<String>,
    /// Item types a target writes: an `ItemGroup` child, or a task `Output`'s
    /// `ItemName`.
    written_items: BTreeSet<String>,
}

fn note(map: &mut BTreeMap<String, String>, name: &str) {
    let slot = map
        .entry(name.to_ascii_lowercase())
        .or_insert_with(|| name.to_owned());
    if name < slot.as_str() {
        *slot = name.to_owned();
    }
}

/// The item types `text` references: `@(Type…)`, and `Type` in `%(Type.Name)`.
/// An over-report is harmless here (it costs a census line), so this scans
/// for the openers alone rather than parsing the expression around them.
fn item_references(text: &str) -> Vec<&str> {
    fn name_at(text: &str, start: usize) -> Option<(&str, usize)> {
        let bytes = text.as_bytes();
        let mut i = start;
        while bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        let begin = i;
        // A `-` is a name character, except where it opens a transform's `->`.
        while bytes.get(i).is_some_and(|b| {
            b.is_ascii_alphanumeric()
                || *b == b'_'
                || (*b == b'-' && bytes.get(i + 1) != Some(&b'>'))
        }) {
            i += 1;
        }
        let first = *bytes.get(begin)?;
        (i > begin && (first.is_ascii_alphabetic() || first == b'_')).then(|| (&text[begin..i], i))
    }
    let mut out = Vec::new();
    for (opener, qualified_only) in [("@(", false), ("%(", true)] {
        let mut from = 0;
        while let Some(at) = text[from..].find(opener) {
            let start = from + at + opener.len();
            if let Some((name, end)) = name_at(text, start)
                && (!qualified_only || text.as_bytes().get(end) == Some(&b'.'))
            {
                out.push(name);
            }
            from = start;
        }
    }
    out
}

/// What the cone's executed targets reference and write, from the definition
/// of each in the file MSBuild reported it from (the last definition of that
/// name there, as MSBuild keeps the last).
fn cone_text(cone: &Cone, key: &str) -> ConeText {
    use borzoi_msbuild::test_support::property_references;

    let mut text = ConeText::default();
    let mut sources: BTreeMap<&Path, String> = BTreeMap::new();
    let defined: BTreeSet<(&str, &Path)> = cone
        .targets
        .iter()
        .map(|t| (t.name.as_str(), t.file.as_path()))
        .collect();
    for (name, file) in defined {
        let source = sources.entry(file).or_insert_with(|| {
            host_fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("{key}: read {}: {e}", file.display()))
        });
        let doc = roxmltree::Document::parse(source)
            .unwrap_or_else(|e| panic!("{key}: parse {}: {e}", file.display()));
        let target = doc
            .descendants()
            .rfind(|n| {
                n.is_element()
                    && n.tag_name().name() == "Target"
                    && n.attribute("Name")
                        .is_some_and(|n| n.trim().eq_ignore_ascii_case(name))
            })
            .unwrap_or_else(|| {
                panic!(
                    "{key}: {} defines no target {name}, which MSBuild ran from it",
                    file.display()
                )
            });
        for node in target.descendants() {
            let mut scan = |raw: &str| {
                for property in property_references(raw) {
                    note(&mut text.properties, property);
                }
                for item in item_references(raw) {
                    note(&mut text.items, item);
                }
            };
            if node.is_text() {
                scan(node.text().unwrap_or_default());
                continue;
            }
            if !node.is_element() {
                continue;
            }
            for attribute in node.attributes() {
                scan(attribute.value());
            }
            let element = node.tag_name().name();
            match node.parent_element().map(|p| p.tag_name().name()) {
                Some("PropertyGroup") => {
                    text.written_properties.insert(element.to_ascii_lowercase());
                }
                Some("ItemGroup") => {
                    text.written_items.insert(element.to_ascii_lowercase());
                }
                _ => {}
            }
            if element == "Output" {
                if let Some(name) = node.attribute("PropertyName") {
                    text.written_properties
                        .insert(name.trim().to_ascii_lowercase());
                }
                if let Some(name) = node.attribute("ItemName") {
                    text.written_items.insert(name.trim().to_ascii_lowercase());
                }
            }
        }
    }
    text
}

/// The item types `borzoi-msbuild` keeps in buckets of their own rather than in
/// the generic item tables, with how many items each bucket holds.
fn dedicated_bucket(parsed: &borzoi_msbuild::ParsedProject, item_type: &str) -> Option<usize> {
    Some(match item_type.to_ascii_lowercase().as_str() {
        "compile" => parsed.items.len(),
        _ => return None,
    })
}

/// How our captured items' metadata compares with MSBuild's, item by item, for
/// a list whose identities agree with MSBuild's in order.
///
/// A metadatum we hold and do not mark uncertain must be MSBuild's value (an
/// absent one reads as ""), or the census panics: a target batching over the
/// list would read it. A name is counted as agreeing when every item agrees
/// on it, uncertain when we mark it so on some item, and missing when some
/// item lacks a value MSBuild gives it.
fn metadata_verdict(
    key: &str,
    item_type: &str,
    rows: &[borzoi_msbuild::test_support::ItemRow],
    theirs: &[&Value],
) -> String {
    let mut agree = BTreeSet::new();
    let mut uncertain = BTreeSet::new();
    let mut missing = BTreeSet::new();
    for (row, item) in rows.iter().zip(theirs) {
        let msbuild: BTreeMap<String, &str> = item["metadata"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str().unwrap()))
            .collect();
        let names: BTreeSet<&String> = msbuild.keys().chain(row.metadata.keys()).collect();
        for name in names {
            let expected = msbuild.get(name).copied().unwrap_or_default();
            if row.uncertain_metadata.contains(name) {
                uncertain.insert(name.clone());
                continue;
            }
            match row.metadata.get(name) {
                Some(ours) => {
                    assert!(
                        ours == expected,
                        "{key}: {item_type} item {:?} has %({name}) {ours:?} in our tables, \
                         {expected:?} in MSBuild's",
                        row.identity
                    );
                    agree.insert(name.clone());
                }
                None if !row.metadata_complete => {
                    uncertain.insert(name.clone());
                }
                None if expected.is_empty() => {
                    agree.insert(name.clone());
                }
                None => {
                    missing.insert(name.clone());
                }
            }
        }
    }
    agree.retain(|name| !uncertain.contains(name) && !missing.contains(name));
    let mut verdict = format!(", metadata agrees on {} names", agree.len());
    if !uncertain.is_empty() {
        let names: Vec<&str> = uncertain.iter().map(String::as_str).collect();
        verdict.push_str(&format!(", uncertain {}", names.join(" ")));
    }
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(String::as_str).collect();
        verdict.push_str(&format!(", missing {}", names.join(" ")));
    }
    verdict
}

/// A short, host-independent spelling of why a value is untrusted.
fn cause(cause: &borzoi_msbuild::UntrustedCause) -> String {
    use borzoi_msbuild::UntrustedCause::*;
    match cause {
        UndefinedProperty { name, hidden_by } => match hidden_by {
            Some(hidden) => format!(
                "read $({name}) undefined, hidden by {}",
                self::cause(hidden)
            ),
            None => format!("read $({name}) undefined"),
        },
        Unmodelled(_) => "unmodelled text".to_owned(),
        SdkPackageInput => "an SDK package input".to_owned(),
        Unentered(_) => "content not entered".to_owned(),
    }
}

/// The read census of one cone (stage A₁.3a of the plan): every property and
/// item type the cone's targets reference, read at the end of our evaluation
/// of the project under the census globals and at the end of MSBuild's, as
/// manifest items keyed by `prefix`.
///
/// A property value we hold and trust must be MSBuild's, or the census panics:
/// that is the value a target would read. Everything else is a verdict: unset
/// where MSBuild sets it, untrusted, or inexact, which is A₁.3's worklist. An
/// item type's verdict says how many items MSBuild's evaluation holds, whether
/// our tables hold the same identities untainted, and how their metadata
/// compares ([`metadata_verdict`]).
///
/// Our evaluation resolves the SDK the cone ran under, at its real path, as
/// the oracle does.
fn reads(
    oracle: &mut Oracle,
    subject: &Subject,
    cone: &Cone,
    sdk: &str,
    prefix: &str,
) -> Vec<(String, String)> {
    use borzoi_msbuild::test_support::{
        ExecutionProbe, ExecutionRead, InexactUnset, PropertyRead, ReadProbe, parse_fsproj_reads,
    };
    use borzoi_msbuild::{SdkVersion, VersionSpec, glob_resolver, resolve_sdk, workloads};

    let text = cone_text(cone, &subject.key);
    let scratch = Scratch::new();
    let globals = scratch.globals();
    let properties: Vec<&str> = text.properties.values().map(String::as_str).collect();
    let items: Vec<&str> = text.items.values().map(String::as_str).collect();
    // The targets MSBuild ran in this project itself, not in one it references.
    let canonical =
        |path: &Path| host_fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let own_project = canonical(&subject.project);
    let own_targets: Vec<&str> = cone
        .targets
        .iter()
        .filter(|run| canonical(&run.project) == own_project)
        .map(|run| run.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let msbuild = oracle.evaluated(
        &subject.project,
        &globals,
        &properties,
        &items,
        &own_targets,
    );

    let dotnet_root = cone.sdk_dir.parent().and_then(Path::parent).unwrap();
    let spec = VersionSpec::with_version(
        SdkVersion::parse(sdk).unwrap_or_else(|| panic!("SDK version {sdk}")),
        borzoi_msbuild::RollForward::Disable,
        false,
    );
    let (user_dotnet_root, overrides_present) = common::workload_env_from_process();
    let resolver = |name: &str, observer: &borzoi_msbuild::observed::Observer| {
        resolve_sdk(
            dotnet_root,
            None,
            name,
            Some(&spec),
            None,
            &workloads::WorkloadEnvironment {
                user_dotnet_root: user_dotnet_root.as_deref(),
                overrides_present,
                global_json_pins_workload_set: false,
            },
            observer,
        )
    };
    let extra: std::collections::HashMap<String, String> = globals
        .iter()
        .map(|(name, value)| {
            (
                (*name).to_owned(),
                value.to_str().expect("UTF-8 global").to_owned(),
            )
        })
        .collect();
    let source = host_fs::read_to_string(&subject.project).unwrap();
    let probe = ReadProbe {
        properties: properties.iter().map(|p| (*p).to_owned()).collect(),
        item_types: items.iter().map(|i| (*i).to_owned()).collect(),
        targets: own_targets.iter().map(|name| (*name).to_owned()).collect(),
        // Where the executor stops on the cone's entry target: the next
        // construct A₁.3c has to model.
        execute: Some(ExecutionProbe {
            targets: vec!["GenerateRestoreGraphFile".to_owned()],
            ..ExecutionProbe::default()
        }),
    };
    let (parsed, ours) = parse_fsproj_reads(
        &source,
        &subject.project,
        &extra,
        &common::oracle_environment(),
        Some(&resolver),
        Some(&glob_resolver::resolve as &borzoi_msbuild::GlobResolver<'_>),
        &probe,
    )
    .unwrap_or_else(|e| panic!("{}: our evaluation failed: {e}", subject.key));
    // The probe reads the walk's end state and changes nothing in it.
    let unprobed = common::parse_fsproj_with_imports(
        &source,
        &subject.project,
        &extra,
        &common::oracle_environment(),
        Some(&resolver),
        Some(&glob_resolver::resolve as &borzoi_msbuild::GlobResolver<'_>),
    )
    .unwrap();
    assert!(
        parsed == unprobed,
        "{}: probing the walk changed its result",
        subject.key
    );

    let mut out = Vec::new();
    for name in &properties {
        let theirs = &msbuild["properties"][*name];
        let set = theirs.as_str().is_some_and(|v| !v.is_empty());
        let set_word = if set { "set" } else { "unset" };
        // Expanded against the file each element came from, so no one value
        // stands for the evaluation.
        if name.to_ascii_lowercase().starts_with("msbuildthisfile") {
            out.push((
                format!("{prefix} reads property {name}"),
                "per file".to_owned(),
            ));
            continue;
        }
        let verdict = match &ours.properties[*name] {
            PropertyRead::Value(value) => {
                assert!(
                    theirs.as_str().unwrap_or_default() == value,
                    "{}: we read $({name}) as {value:?} at the end of evaluation, MSBuild as {theirs}",
                    subject.key
                );
                if set {
                    "value agrees".to_owned()
                } else {
                    "unset agrees".to_owned()
                }
            }
            PropertyRead::Unset if set => "unset here, set in MSBuild".to_owned(),
            PropertyRead::Unset => "unset agrees".to_owned(),
            PropertyRead::UnsetInexact(why) => {
                let why = match why {
                    InexactUnset::Refused => "a refused write".to_owned(),
                    InexactUnset::Environment => "the environment".to_owned(),
                    InexactUnset::Toolset => "the toolset".to_owned(),
                    InexactUnset::Opaque(c) => format!("opacity: {}", cause(c)),
                    InexactUnset::ConsumerCarveOut => "a consumer carve-out".to_owned(),
                };
                format!("unset, inexact ({why}); {set_word} in MSBuild")
            }
            PropertyRead::Untrusted(c) => {
                format!("untrusted ({}); {set_word} in MSBuild", cause(c))
            }
        };
        let written = if text.written_properties.contains(&name.to_ascii_lowercase()) {
            "; the cone writes it"
        } else {
            ""
        };
        out.push((
            format!("{prefix} reads property {name}"),
            format!("{verdict}{written}"),
        ));
    }
    for item_type in &items {
        let theirs: Vec<&Value> = msbuild["items"][*item_type]
            .as_array()
            .unwrap_or_else(|| panic!("evaluated: no items for {item_type}"))
            .iter()
            .collect();
        let read = &ours.items[*item_type];
        let mut verdict = format!("MSBuild {}", theirs.len());
        if let Some(count) = dedicated_bucket(&parsed, item_type) {
            verdict.push_str(&format!("; ours a dedicated bucket of {count}"));
        } else {
            match &read.items {
                None => verdict.push_str("; ours none"),
                Some(rows) => {
                    let identities: Vec<&str> = rows.iter().map(|r| r.identity.as_str()).collect();
                    let msbuild_identities: Vec<&str> = theirs
                        .iter()
                        .map(|i| i["identity"].as_str().unwrap())
                        .collect();
                    // An exact list is a claim about every identity, in order.
                    assert!(
                        !read.exact || identities == msbuild_identities,
                        "{}: our exact {item_type} list {identities:?} is not MSBuild's \
                         {msbuild_identities:?}",
                        subject.key
                    );
                    let agreement = if identities == msbuild_identities {
                        "identities agree"
                    } else {
                        "identities differ"
                    };
                    verdict.push_str(&format!("; ours {}, {agreement}", rows.len()));
                    // A list that is not exact makes no claim: two declarations may
                    // share an identity and differ in metadata.
                    if identities == msbuild_identities && read.exact {
                        verdict.push_str(&metadata_verdict(&subject.key, item_type, rows, &theirs));
                    }
                }
            }
            if read.tainted {
                verdict.push_str(", tainted");
            }
            if read.untracked {
                verdict.push_str(", untracked");
            }
            if !read.exact && !read.tainted && !read.untracked {
                verdict.push_str(", not exact");
            }
        }
        if text.written_items.contains(&item_type.to_ascii_lowercase()) {
            verdict.push_str("; the cone writes it");
        }
        out.push((format!("{prefix} reads item {item_type}"), verdict));
    }
    // An exact read of a target must be the definition MSBuild's evaluation
    // keeps, the one its build ran.
    let mut retained = 0;
    let mut inexact = 0;
    for name in &own_targets {
        match common::check_target(&ours.targets[*name], &msbuild["targets"][*name]) {
            Ok(true) => retained += 1,
            Ok(false) => inexact += 1,
            Err(why) => panic!("{}: target {name}: {why}", subject.key),
        }
    }
    out.push((
        format!("{prefix} retains its targets"),
        format!("{retained} as MSBuild ran them, {inexact} not exact"),
    ));
    let execution = match ours.execution.as_ref().expect("the census asks for a run") {
        ExecutionRead::Ran { targets, .. } => format!("ran {} targets", targets.len()),
        ExecutionRead::Declined(why) => format!("declines: {why}"),
    };
    out.push((format!("{prefix} runs GenerateRestoreGraphFile"), execution));
    out
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
            host_fs::canonicalize(&installed).unwrap(),
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
fn census(subjects: &[Subject]) -> (Manifest, Manifest, String) {
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
    let mut read_items = Vec::new();
    // Each subject's spec under the previous SDK measured, for drift.
    let mut previous: Vec<Option<(String, Value)>> = subjects.iter().map(|_| None).collect();
    let mut report = String::new();
    for sdk in &sdks {
        // By its real path, which is how the muxer reaches it: under the other
        // spelling MSBuild records paths into the SDK (the runtime identifier
        // graph) that `dotnet msbuild` does not.
        let sdk_dir = host_fs::canonicalize(dotnet_root_from_env().join("sdk").join(sdk))
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
            // The seam is graded only on a cone of the SDK's targets alone: a user
            // target can run anything around the writer (edit its entries, run it
            // again, change its flags afterwards), and A₁ declines such a cone.
            let user_target = cone
                .targets
                .iter()
                .any(|t| !shipped(&subject.spell(&cone, &t.file)));
            let verdict = if user_target {
                "decline: a user target ran in the cone".to_owned()
            } else {
                seam(&subject.project, &cone.answer, &cone.loaded)
            };
            items.push((format!("{prefix} seam"), verdict));
            if how == "resolved" {
                read_items.extend(reads(&mut oracle, subject, &cone, sdk, &prefix));
            }
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
    let reads = Manifest::from_keyed(read_items).unwrap_or_else(|e| panic!("reads manifest: {e}"));
    (manifest, reads, report)
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
                .find(|d| d.join(".git").host_exists())
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
    let (manifest, reads, report) = census(&subjects);
    println!("{report}");
    check(
        &manifest_path("restore_cone_projects.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 BORZOI_PROJECT_LIST=… nix develop -c cargo test -p borzoi-msbuild \
             --test restore_cone_census pinned_projects -- --ignored"
        ),
    );
    check(
        &manifest_path("restore_cone_reads_projects.txt"),
        &reads,
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
    <!-- An escaped `;` in metadata the writer writes: the item seam must see it
         as the task does, unescaped. -->
    <PackageReference Include="Fake.Package" Version="1.0.0" Aliases="x%3By" />
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
  <Target Name="UserHookBeforeRestoreGraphEntry" BeforeTargets="_GenerateRestoreGraphProjectEntry"
          Condition="'$(MSBuildStartupDirectory)' == '$(MSBuildProjectDirectory)'">
    <Message Text="a user target in the restore cone" />
  </Target>
</Project>
"#;

/// Generated projects that reach what the corpus does not, and a user target
/// hooked into restore, which the census must see.
#[test]
fn fixtures() {
    let dir = tempfile::tempdir().unwrap();
    let root = host_fs::canonicalize(dir.path()).unwrap();
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
    let (manifest, reads, report) = census(&subjects);
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
    check(
        &manifest_path("restore_cone_reads_fixtures.txt"),
        &reads,
        &format!(
            "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-msbuild --test restore_cone_census \
             fixtures"
        ),
    );
}
