//! The read-set soundness oracle ([`crate::observed::soundness`]) over
//! generated worlds that reach every kind of query an evaluation makes:
//! SDK resolution against a dotnet root (its version listing and `Sdk.props`
//! probes), the `Directory.Build.*` walk, `Exists()` conditions, a
//! conditional and a wildcard `<Import>`, `GetDirectoryNameOfFileAbove`, a
//! Compile glob, and the `@(ProjectReference->Exists())` item function.
//!
//! Every helper in [`super`] runs the same sweep over its test's tree; this
//! module adds the worlds those trees never build, and the non-vacuity
//! check that the perturbations of each rule moved some evaluation.

use proptest::prelude::*;
use proptest::strategy::ValueTree as _;
use tempfile::TempDir;

use super::*;
use crate::observed::soundness::{Edit, Perturbation, Tally, Why, sweep};
use crate::observed::{Observation, Observer, ReadSet};
use crate::{resolve_sdk, workloads};

/// Which optional parts of the world exist.
#[derive(Clone, Debug)]
struct World {
    /// The SDK versions installed under `dotnet/sdk`.
    sdk_versions: Vec<&'static str>,
    /// Whether the project names the SDK.
    uses_sdk: bool,
    repo_props: bool,
    src_props: bool,
    src_targets: bool,
    shared_props: bool,
    /// Files under `src/imports`, which the wildcard import takes.
    imports: Vec<&'static str>,
    marker_in_repo: bool,
    marker_in_app: bool,
    /// Source files beside the project and below it.
    sources: Vec<&'static str>,
    lib_project: bool,
}

fn world() -> impl Strategy<Value = World> {
    (
        proptest::sample::subsequence(vec!["8.0.100", "9.0.200", "10.0.100"], 0..=3),
        any::<bool>(),
        any::<(bool, bool, bool, bool)>(),
        proptest::sample::subsequence(vec!["a.props", "b.props"], 0..=2),
        any::<(bool, bool)>(),
        proptest::sample::subsequence(vec!["A.fs", "sub/C.fs", "sub/deep/D.fs"], 0..=3),
        any::<bool>(),
    )
        .prop_map(
            |(
                sdk_versions,
                uses_sdk,
                (repo_props, src_props, src_targets, shared_props),
                imports,
                (marker_in_repo, marker_in_app),
                sources,
                lib_project,
            )| World {
                sdk_versions,
                uses_sdk,
                repo_props,
                src_props,
                src_targets,
                shared_props,
                imports,
                marker_in_repo,
                marker_in_app,
                sources,
                lib_project,
            },
        )
}

/// An MSBuild file that writes `name` = `value`.
fn writes(name: &str, value: &str) -> String {
    format!("<Project><PropertyGroup><{name}>{value}</{name}></PropertyGroup></Project>\n")
}

fn sdk_files(root: &Path, version: &str) -> Vec<(PathBuf, String)> {
    let sdk = root.join(format!("dotnet/sdk/{version}/Sdks/Fake.Sdk/Sdk"));
    vec![
        (
            sdk.join("Sdk.props"),
            format!(
                "<Project><PropertyGroup><SdkSeen>{version}</SdkSeen></PropertyGroup>\
                 <Import Project=\"$(MSBuildThisFileDirectory)extra.props\" \
                 Condition=\"Exists('$(MSBuildThisFileDirectory)extra.props')\" /></Project>\n"
            ),
        ),
        (sdk.join("Sdk.targets"), writes("SdkTargets", version)),
    ]
}

const PROJECT: &str = r#"<Project{SDK}>
  <PropertyGroup>
    <MarkerDir>$([MSBuild]::GetDirectoryNameOfFileAbove($(MSBuildProjectDirectory), marker.txt))</MarkerDir>
  </PropertyGroup>
  <Import Project="../shared.props" Condition="Exists('../shared.props')" />
  <Import Project="../imports/*.props" />
  <ItemGroup>
    <Compile Include="A.fs" Condition="Exists('A.fs')" />
    <Compile Include="sub/**/*.fs" />
    <ProjectReference Include="../Lib/Lib.fsproj;../Missing/Missing.fsproj" />
    <ProjectReferenceNonExistent Include="@(ProjectReference)" Exclude="@(ProjectReference->Exists())" />
    <ProjectReference Remove="@(ProjectReferenceNonExistent)" />
  </ItemGroup>
</Project>
"#;

/// Materialise `world` under `root`; returns the project's path and source.
fn build(root: &Path, world: &World) -> (PathBuf, String) {
    for version in &world.sdk_versions {
        for (path, text) in sdk_files(root, version) {
            write_at(
                root,
                path.strip_prefix(root).unwrap().to_str().unwrap(),
                &text,
            );
        }
    }
    let repo = root.join("repo");
    let optional = [
        (
            world.repo_props,
            "Directory.Build.props",
            writes("RepoProp", "1"),
        ),
        (
            world.src_props,
            "src/Directory.Build.props",
            writes("SrcProp", "1"),
        ),
        (
            world.src_targets,
            "src/Directory.Build.targets",
            writes("SrcTargets", "1"),
        ),
        (
            world.shared_props,
            "src/shared.props",
            writes("Shared", "1"),
        ),
        (world.marker_in_repo, "marker.txt", String::new()),
        (world.marker_in_app, "src/App/marker.txt", String::new()),
        (
            world.lib_project,
            "src/Lib/Lib.fsproj",
            "<Project />".to_string(),
        ),
    ];
    for (present, name, text) in optional {
        if present {
            write_at(&repo, name, &text);
        }
    }
    for name in &world.imports {
        write_at(
            &repo,
            &format!("src/imports/{name}"),
            &writes("Imported", name),
        );
    }
    for name in &world.sources {
        write_at(&repo, &format!("src/App/{name}"), "module M\n");
    }
    let source = PROJECT.replace(
        "{SDK}",
        if world.uses_sdk {
            " Sdk=\"Fake.Sdk\""
        } else {
            ""
        },
    );
    let project = write_at(&repo, "src/App/App.fsproj", &source);
    (canon(&project), source)
}

/// The world's own perturbations: a newer SDK installed, and an `extra.props`
/// beside every installed SDK's `Sdk.props`.
fn extras(root: &Path, world: &World) -> Vec<Perturbation> {
    let mut out = vec![Perturbation {
        why: Why::Extra,
        edits: sdk_files(root, "11.0.100")
            .into_iter()
            .map(|(path, text)| Edit::Write(path, text.into_bytes()))
            .collect(),
    }];
    for version in &world.sdk_versions {
        out.push(Perturbation {
            why: Why::Extra,
            edits: vec![Edit::Write(
                root.join(format!(
                    "dotnet/sdk/{version}/Sdks/Fake.Sdk/Sdk/extra.props"
                )),
                writes("Extra", version).into_bytes(),
            )],
        });
    }
    out
}

fn evaluate(root: &Path, project: &Path, source: &str, through: Through<'_>) -> ParsedProject {
    let dotnet = root.join("dotnet");
    let resolver = |name: &str, observer: &Observer| {
        resolve_sdk(
            &dotnet,
            None,
            name,
            None,
            None,
            &workloads::WorkloadEnvironment {
                user_dotnet_root: None,
                overrides_present: false,
                global_json_pins_workload_set: false,
            },
            observer,
        )
    };
    let glob: &GlobResolver<'_> = &crate::glob_resolver::resolve;
    through
        .evaluate(
            source,
            project,
            &HashMap::new(),
            &HashMap::new(),
            Some(&resolver),
            Some(glob),
        )
        .expect("well-formed XML parses")
}

/// The sweep over `world`, with the reused entry point held to the reference
/// at every step, as every helper's is ([`super::sound`]).
fn sweep_world(world: &World) -> Tally {
    let tmp = TempDir::new().unwrap();
    let root = canon(tmp.path());
    let (project, source) = build(&root, world);
    let documents = RefCell::new(ImportDocuments::default());
    let read_a_file = std::cell::Cell::new(false);
    let tally = sweep(
        &root,
        &|| {
            let reused = same_as_reference(
                evaluate(&root, &project, &source, Through::Reference),
                evaluate(&root, &project, &source, Through::Reusing(&documents)),
            );
            // The world holds no workload manifests, so every file an
            // evaluation reads whole is an MSBuild file it reads through
            // `read_document`, and every one is well-formed: each is kept,
            // parsed from the bytes its read records.
            let documents = documents.borrow();
            for read in reused.read_set.reads() {
                if let Observation::Text(Ok(digest)) = &read.observation {
                    read_a_file.set(true);
                    assert_eq!(
                        documents.kept_digest(&read.path),
                        Some(*digest),
                        "{} is not kept as it was read",
                        read.path.display()
                    );
                }
            }
            reused
        },
        &extras(&root, world),
    );
    // The sweep evaluates the world many times over a tree it changes one
    // file at a time, so once any evaluation reads a file the comparison is
    // vacuous unless some later one reused its document.
    let documents = documents.into_inner();
    if read_a_file.get() {
        assert!(
            documents.reused() > 0,
            "the sweep's evaluations reused no document: {documents:?}"
        );
    }
    tally
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    /// Sound, and (the world spelling each file one way) precise.
    #[test]
    fn an_evaluation_whose_read_set_stands_is_unchanged(world in world()) {
        let tally = sweep_world(&world);
        prop_assert_eq!(tally.imprecise, 0, "{:#?}", tally);
    }
}

/// Every rule of the sweep moves some evaluation over a fixed sample of
/// worlds, so none of them is vacuous: a rewrite, a same-size rewrite and a
/// delete of an imported file, a created file where the read set recorded an
/// absence, a new entry in a listed directory, and a newly installed SDK.
#[test]
fn every_perturbation_rule_moves_some_evaluation() {
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let mut total = Tally::default();
    for _ in 0..8 {
        let world = world().new_tree(&mut runner).unwrap().current();
        total.absorb(&sweep_world(&world));
    }
    for why in [
        Why::Rewrite,
        Why::RewriteSameSize,
        Why::Delete,
        Why::CreateAbsent,
        Why::GrowListing,
        Why::Extra,
    ] {
        assert!(
            total.moved(why) > 0,
            "no {why:?} moved an evaluation: {total:#?}"
        );
    }
    assert!(
        total.untouched > 0,
        "no perturbation left the read set untouched, so precision was never checked: {total:#?}"
    );
    assert_eq!(
        total.imprecise, 0,
        "a change the read set does not name moved one of its answers: {total:#?}"
    );
}

/// The read set of a world with everything in it holds a query of every
/// kind, absences included.
#[test]
fn a_full_world_records_every_kind_of_query() {
    let world = World {
        sdk_versions: vec!["9.0.200", "10.0.100"],
        uses_sdk: true,
        repo_props: true,
        src_props: true,
        src_targets: true,
        shared_props: false,
        imports: vec!["a.props"],
        marker_in_repo: true,
        marker_in_app: false,
        sources: vec!["A.fs", "sub/C.fs"],
        lib_project: true,
    };
    let tmp = TempDir::new().unwrap();
    let root = canon(tmp.path());
    let (project, source) = build(&root, &world);
    let parsed = evaluate(
        &root,
        &project,
        &source,
        Through::Reusing(&RefCell::default()),
    );
    let read_set: &ReadSet = &parsed.read_set;
    let has =
        |pred: &dyn Fn(&Observation) -> bool| read_set.reads().iter().any(|r| pred(&r.observation));
    assert!(has(&|o| matches!(o, Observation::Text(Ok(_)))));
    assert!(has(&|o| matches!(o, Observation::Kind(Ok(_)))));
    assert!(has(
        &|o| matches!(o, Observation::Kind(Err(f)) if f.is_absent())
    ));
    assert!(has(&|o| matches!(o, Observation::LinkKind(_))));
    assert!(has(&|o| matches!(o, Observation::Listing(Ok(_)))));
    assert!(has(&|o| matches!(o, Observation::Canonical(Ok(_)))));
    let counts = parsed.read_counts;
    assert!(
        counts.opens > 0 && counts.stats > 0 && counts.listings > 0 && counts.canonicalizations > 0
    );
    assert!(
        read_set.len() as u64
            <= counts.opens + counts.stats + counts.listings + counts.canonicalizations,
        "the read set holds distinct queries, never more than were made"
    );
}

/// A project behind `Exists('alias')`, where `alias` is a symlink to
/// `target`; `target_exists` decides whether the link dangles. The sweep must
/// accept both: deleting or creating `target` moves the answer about `alias`,
/// which the precision check has to see as touching it, and a dangling link
/// is not an absent path to create a file at.
#[cfg(unix)]
fn sweep_symlinked_exists(target_exists: bool) {
    let tmp = TempDir::new().unwrap();
    let root = canon(tmp.path());
    if target_exists {
        write_at(&root, "proj/target", "x");
    }
    std::os::unix::fs::symlink(root.join("proj/target"), {
        std::fs::create_dir_all(root.join("proj")).unwrap();
        root.join("proj/alias")
    })
    .unwrap();
    let source = r#"<Project>
  <PropertyGroup Condition="Exists('alias')"><Linked>yes</Linked></PropertyGroup>
</Project>"#;
    let project = write_at(&root, "proj/Demo.fsproj", source);
    let evaluate = || {
        parse_fsproj_with_imports(
            source,
            &project,
            &HashMap::new(),
            &HashMap::new(),
            None,
            None,
        )
        .expect("well-formed XML parses")
    };
    sweep(&root, &evaluate, &[]);
}

#[cfg(unix)]
#[test]
fn a_symlink_whose_target_moves_is_swept() {
    sweep_symlinked_exists(true);
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_swept() {
    sweep_symlinked_exists(false);
}

/// A query that climbs out of the swept root (`Exists('../outside/…')`) is
/// recorded, but the sweep plans no change outside its root.
#[test]
fn the_sweep_writes_only_under_its_root() {
    let tmp = TempDir::new().unwrap();
    let base = canon(tmp.path());
    let root = base.join("root");
    let source = r#"<Project>
  <PropertyGroup Condition="Exists('../outside/marker.props')"><Out>yes</Out></PropertyGroup>
</Project>"#;
    let project = write_at(&root, "Demo.fsproj", source);
    let evaluate = || {
        parse_fsproj_with_imports(
            source,
            &project,
            &HashMap::new(),
            &HashMap::new(),
            None,
            None,
        )
        .expect("well-formed XML parses")
    };
    assert!(
        evaluate()
            .read_set
            .reads()
            .iter()
            .any(|read| read.path.ends_with("outside/marker.props"))
    );
    let planned = crate::observed::soundness::perturbations(&root, &evaluate().read_set);
    for perturbation in &planned {
        for edit in &perturbation.edits {
            let (Edit::Write(path, _) | Edit::Delete(path)) = edit;
            let lexical: PathBuf = path.components().fold(PathBuf::new(), |mut out, c| {
                match c {
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => out.push(other),
                }
                out
            });
            assert!(
                lexical.starts_with(&root),
                "{perturbation:?} leaves the root"
            );
        }
    }
    sweep(&root, &evaluate, &[]);
}

/// An unreadable import is left alone, and a read-only one is put back with
/// its permissions.
#[cfg(unix)]
#[test]
fn the_sweep_keeps_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = TempDir::new().unwrap();
    let root = canon(tmp.path());
    let unreadable = write_at(&root, "proj/unreadable.props", &writes("U", "1"));
    let read_only = write_at(&root, "proj/read_only.props", &writes("R", "1"));
    let mode = |path: &Path| {
        crate::observed::host_fs::metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o444)).unwrap();
    let source = r#"<Project>
  <Import Project="unreadable.props" />
  <Import Project="read_only.props" />
</Project>"#;
    let project = write_at(&root, "proj/Demo.fsproj", source);
    let evaluate = || {
        parse_fsproj_with_imports(
            source,
            &project,
            &HashMap::new(),
            &HashMap::new(),
            None,
            None,
        )
        .expect("well-formed XML parses")
    };
    sweep(&root, &evaluate, &[]);
    assert_eq!(mode(&unreadable), 0o000);
    assert_eq!(mode(&read_only), 0o444);
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
}
