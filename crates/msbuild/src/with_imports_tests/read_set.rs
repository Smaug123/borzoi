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
use crate::observed::{Observer, ReadSet};
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

fn evaluate(root: &Path, project: &Path, source: &str) -> ParsedProject {
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
    parse_fsproj_with_imports(
        source,
        project,
        &HashMap::new(),
        &HashMap::new(),
        Some(&resolver),
        Some(glob),
    )
    .expect("well-formed XML parses")
}

fn sweep_world(world: &World) -> Tally {
    let tmp = TempDir::new().unwrap();
    let root = canon(tmp.path());
    let (project, source) = build(&root, world);
    sweep(
        &root,
        &|| evaluate(&root, &project, &source),
        &extras(&root, world),
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    #[test]
    fn an_evaluation_whose_read_set_stands_is_unchanged(world in world()) {
        sweep_world(&world);
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
    let parsed = evaluate(&root, &project, &source);
    let read_set: &ReadSet = &parsed.read_set;
    let has =
        |pred: &dyn Fn(&Observation) -> bool| read_set.reads().iter().any(|r| pred(&r.observation));
    use crate::observed::Observation;
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
