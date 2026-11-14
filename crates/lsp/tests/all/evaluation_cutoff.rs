//! A project's parses and its assembly env depend on its evaluation only
//! through the values they read from it (`docs/incremental-lsp-plan.md`,
//! "Follow-up: cutoffs on the evaluation's consumed outputs"). An edit to a
//! project file runs the evaluation again; what it leaves standing is
//! decided by those values, not by the evaluation's identity.
//!
//! The property: over generated sequences of project-file edits, after each
//! edit a long-lived server state serves the fold of `App` in a new
//! generation, and a fresh state over the same disk is the oracle.
//!
//! - The env is the same allocation exactly when a fresh state's env would
//!   be built from the same thing: the same reference DLLs in the same order,
//!   the same completeness verdict, under the same served TFM.
//! - The fold is the same allocation exactly when the env is and the
//!   evaluation values the parses read (the Compile items in order, the
//!   `#if` symbols, the language version) are unchanged; a generation folds
//!   exactly when it is not.
//! - Whatever was kept, the long-lived state's fold is folded against the
//!   env its project's key serves now, which is built from the fresh
//!   state's DLLs with its completeness verdict, over parses whose trees
//!   are the fresh state's. The fold is a function of those (sema's own
//!   `incremental ≡ batch` differential holds it to that), so its answers
//!   are the fresh state's.
//!
//! The edits change each of those values on its own (a define, the Compile
//! order, the language version, the TFM, a project reference, a referenced
//! project's output name) and none of them (a comment, whitespace, a
//! property nothing reads, a referenced project's comment, a reference to a
//! project with no output).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi::project_deferral::FoldOutcome;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::ProjectId;
use borzoi::server::State;
use borzoi::vfs::{Clock, RACY_WINDOW};
use borzoi::workspace::{ServedTfm, Workspace};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::syntax::{AstNode as _, SyntaxNode};
use borzoi_msbuild::ItemKind;
use proptest::prelude::*;
use tempfile::TempDir;

use crate::common::host_fs;

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A restored `App` naming the package `Foo` under `net10.0`, the only
/// target its assets file holds.
fn assets(root: &Path) -> String {
    serde_json::json!({
        "version": 3,
        "targets": { "net10.0": {
            "Foo/1.0.0": { "type": "package", "compile": { "lib/net10.0/Foo.dll": {} } },
        }},
        "libraries": { "Foo/1.0.0": { "type": "package", "path": "foo/1.0.0" } },
        "packageFolders": { root.join("nuget").to_str().unwrap(): {} },
        "project": {
            "frameworks": { "net10.0": { "frameworkReferences": { "Microsoft.NETCore.App": {} } } }
        }
    })
    .to_string()
}

/// The projects `App` may reference: `Lib`, built (its output a copy of the
/// real `System.Console.dll`), `Unbuilt`, which has no output, and the C#
/// project `Cs`, which the sidecar (not enabled here) would read: an unread
/// reference, which marks the env incomplete and adds no DLL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ref {
    Lib,
    Unbuilt,
    CSharp,
}

impl Ref {
    fn path(self) -> &'static str {
        match self {
            Ref::Lib => "../Lib/Lib.fsproj",
            Ref::Unbuilt => "../Unbuilt/Unbuilt.fsproj",
            Ref::CSharp => "../Cs/Cs.csproj",
        }
    }
}

/// `App.fsproj`, as a value every edit is a change of.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AppSpec {
    /// `net10.0` (restored) or `net9.0` (not).
    net9: bool,
    references: Vec<Ref>,
    defines: Vec<&'static str>,
    /// `A.fs` before `B.fs`, or after it.
    a_first: bool,
    lang_version: Option<&'static str>,
    /// What changes the file and none of its consumed values.
    comment: Option<u8>,
    indent: usize,
    description: Option<u8>,
}

impl AppSpec {
    fn render(&self) -> String {
        let pad = " ".repeat(self.indent);
        let mut out = String::from("<Project>\n");
        if let Some(n) = self.comment {
            out.push_str(&format!("{pad}<!-- edit {n} -->\n"));
        }
        out.push_str(&format!("{pad}<PropertyGroup>\n"));
        let tfm = if self.net9 { "net9.0" } else { "net10.0" };
        out.push_str(&format!(
            "{pad}  <TargetFramework>{tfm}</TargetFramework>\n"
        ));
        if !self.defines.is_empty() {
            out.push_str(&format!(
                "{pad}  <DefineConstants>$(DefineConstants);{}</DefineConstants>\n",
                self.defines.join(";")
            ));
        }
        if let Some(version) = self.lang_version {
            out.push_str(&format!("{pad}  <LangVersion>{version}</LangVersion>\n"));
        }
        if let Some(n) = self.description {
            out.push_str(&format!("{pad}  <Description>edit {n}</Description>\n"));
        }
        out.push_str(&format!("{pad}</PropertyGroup>\n{pad}<ItemGroup>\n"));
        let (first, second) = if self.a_first {
            ("A.fs", "B.fs")
        } else {
            ("B.fs", "A.fs")
        };
        for file in [first, second] {
            out.push_str(&format!("{pad}  <Compile Include=\"{file}\" />\n"));
        }
        for reference in &self.references {
            out.push_str(&format!(
                "{pad}  <ProjectReference Include=\"{}\" />\n",
                reference.path()
            ));
        }
        out.push_str(&format!("{pad}</ItemGroup>\n</Project>\n"));
        out
    }
}

/// `Lib.fsproj`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibSpec {
    /// `<TargetName>Lib</TargetName>`, which names the output on disk, or
    /// another name, which names nothing.
    renamed: bool,
    comment: Option<u8>,
}

impl LibSpec {
    fn render(&self) -> String {
        let name = if self.renamed { "Renamed" } else { "Lib" };
        let comment = self
            .comment
            .map(|n| format!("  <!-- edit {n} -->\n"))
            .unwrap_or_default();
        format!(
            "<Project>\n{comment}  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n    <TargetName>{name}</TargetName>\n  </PropertyGroup>\n</Project>\n"
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct World {
    app: AppSpec,
    lib: LibSpec,
}

fn initial() -> World {
    World {
        app: AppSpec {
            net9: false,
            references: vec![Ref::Lib],
            defines: Vec::new(),
            a_first: true,
            lang_version: None,
            comment: None,
            indent: 2,
            description: None,
        },
        lib: LibSpec {
            renamed: false,
            comment: None,
        },
    }
}

/// One edit, as the change of one field.
#[derive(Clone, Debug)]
enum Edit {
    Tfm,
    Reference(Ref),
    Define(&'static str),
    Order,
    LangVersion(Option<&'static str>),
    Comment(u8),
    Indent(usize),
    Description(u8),
    LibRenamed,
    LibComment(u8),
}

impl Edit {
    fn apply(&self, world: &mut World) {
        let app = &mut world.app;
        match self {
            Edit::Tfm => app.net9 = !app.net9,
            Edit::Reference(r) => match app.references.iter().position(|x| x == r) {
                Some(i) => {
                    app.references.remove(i);
                }
                None => app.references.push(*r),
            },
            Edit::Define(d) => match app.defines.iter().position(|x| x == d) {
                Some(i) => {
                    app.defines.remove(i);
                }
                None => app.defines.push(d),
            },
            Edit::Order => app.a_first = !app.a_first,
            Edit::LangVersion(v) => app.lang_version = *v,
            Edit::Comment(n) => app.comment = Some(*n),
            Edit::Indent(n) => app.indent = *n,
            Edit::Description(n) => app.description = Some(*n),
            Edit::LibRenamed => world.lib.renamed = !world.lib.renamed,
            Edit::LibComment(n) => world.lib.comment = Some(*n),
        }
    }
}

fn edit_strategy() -> impl Strategy<Value = Edit> {
    prop_oneof![
        1 => Just(Edit::Tfm),
        2 => prop_oneof![Just(Ref::Lib), Just(Ref::Unbuilt), Just(Ref::CSharp)].prop_map(Edit::Reference),
        2 => prop_oneof![Just("FOO"), Just("BAR")].prop_map(Edit::Define),
        1 => Just(Edit::Order),
        1 => prop_oneof![Just(None), Just(Some("8.0")), Just(Some("9.0"))].prop_map(Edit::LangVersion),
        3 => any::<u8>().prop_map(Edit::Comment),
        2 => (1usize..5).prop_map(Edit::Indent),
        2 => any::<u8>().prop_map(Edit::Description),
        1 => Just(Edit::LibRenamed),
        2 => any::<u8>().prop_map(Edit::LibComment),
    ]
}

/// The fixture: a stub dotnet root whose framework pack holds the real
/// `System.Runtime.dll`, the package `Foo` (a copy of the real
/// `System.Collections.dll`, or, when `junk_package`, bytes that are not an
/// assembly, which the env skips and which mark it incomplete whatever its
/// references say), `Lib` built, `Unbuilt`, `Cs`, and `App` with two sources
/// that read `System.Console` and hold `#if` blocks.
struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    app: PathBuf,
    lib: PathBuf,
}

fn fixture(world: &World, junk_package: bool) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let root = host_fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let runtime = crate::common::ensure_system_runtime_dll();
    write(
        &root.join("dotnet/packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0/System.Runtime.dll"),
        &host_fs::read(&runtime).unwrap(),
    );
    let package = root.join("nuget/foo/1.0.0");
    let package_dll = if junk_package {
        b"not an assembly".to_vec()
    } else {
        host_fs::read(runtime.with_file_name("System.Collections.dll")).unwrap()
    };
    write(&package.join("lib/net10.0/Foo.dll"), &package_dll);
    write(&package.join(".nupkg.metadata"), b"{}");
    write(
        &root.join("Lib/bin/Debug/net10.0/Lib.dll"),
        &host_fs::read(runtime.with_file_name("System.Console.dll")).unwrap(),
    );
    write(
        &root.join("Cs/Cs.csproj"),
        b"<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>\n",
    );
    write(
        &root.join("Unbuilt/Unbuilt.fsproj"),
        b"<Project><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>\n",
    );
    write(
        &root.join("App/obj/project.assets.json"),
        assets(&root).as_bytes(),
    );
    write(
        &root.join("App/A.fs"),
        b"module A\n\nlet out = System.Console.Out\n#if FOO\nlet foo = 1\n#endif\n",
    );
    write(
        &root.join("App/B.fs"),
        b"module B\n\nlet b = 2\n#if BAR\nlet bar = A.out\n#endif\n",
    );
    let fixture = Fixture {
        _tmp: tmp,
        app: root.join("App/App.fsproj"),
        lib: root.join("Lib/Lib.fsproj"),
        root,
    };
    write_world(&fixture, world);
    fixture
}

fn write_world(fixture: &Fixture, world: &World) {
    write(&fixture.app, world.app.render().as_bytes());
    write(&fixture.lib, world.lib.render().as_bytes());
}

/// A server state over the fixture, whose clock runs ahead of the files'
/// timestamps so that no stamp of a project file is racy.
fn state(fixture: &Fixture) -> State {
    let mut state = State::new();
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(fixture.root.join("dotnet")),
        nuget_packages_dir: Some(fixture.root.join("global-packages")),
        home_dir: Some(fixture.root.join("home")),
        ..SdkDiscoveryEnv::default()
    });
    state.clock = Clock::HostAhead(RACY_WINDOW * 30);
    state
}

/// What a fresh env is built from: the reference DLLs in fold order and the
/// completeness verdict, under the served TFM.
#[derive(Debug, PartialEq, Eq)]
struct EnvValue {
    tfm: ServedTfm,
    dlls: Vec<PathBuf>,
    incomplete: bool,
}

/// What the parses read from the evaluation, through the workspace's
/// public accessors.
#[derive(Debug, PartialEq, Eq)]
struct ParseValue {
    items: Vec<PathBuf>,
    symbols: std::collections::BTreeSet<String>,
    lang: LanguageVersion,
}

fn env_value(state: &mut State, project: &Path) -> EnvValue {
    let root = state.workspace.dotnet_root_for_project(project);
    let tfm = state.workspace.served_tfm_for_project(project);
    let (dlls, retryable) = state.semantic.env_reference_dlls_for_project(
        project,
        root.as_deref(),
        &tfm,
        &state.workspace,
    );
    assert!(!retryable);
    let env =
        state
            .semantic
            .assembly_env_for_project(project, root.as_deref(), &tfm, &state.workspace);
    EnvValue {
        tfm,
        dlls,
        incomplete: env.identities_incomplete(),
    }
}

fn parse_value(state: &mut State, project: &Path) -> ParseValue {
    let items = state
        .workspace
        .project(project)
        .expect("App evaluates")
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ItemKind::Compile | ItemKind::CompileBefore | ItemKind::CompileAfter
            )
        })
        .map(|item| item.include.clone())
        .collect();
    ParseValue {
        items,
        symbols: state
            .workspace
            .symbols_for_project(project)
            .into_iter()
            .collect(),
        lang: state.workspace.lang_version_for_project(project),
    }
}

/// The syntax trees of the parses a fold reads, with their paths.
fn trees(state: &mut State, project: &Path) -> Vec<(PathBuf, SyntaxNode)> {
    let parses = state
        .semantic
        .parses_for_project(project, &mut state.workspace, &state.docs)
        .expect("App parses");
    parses
        .paths
        .iter()
        .zip(&parses.files)
        .map(|(path, file)| {
            let root = file
                .file
                .as_impl()
                .expect("an implementation file")
                .syntax()
                .clone();
            (path.clone(), root)
        })
        .collect()
}

fn same_trees(a: &[(PathBuf, SyntaxNode)], b: &[(PathBuf, SyntaxNode)]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((pa, ta), (pb, tb))| pa == pb && ta.structurally_eq(tb))
}

/// What the oracle, a fresh state over the disk as it is, serves.
struct Fresh {
    env: EnvValue,
    parses: ParseValue,
    trees: Vec<(PathBuf, SyntaxNode)>,
}

fn fresh(fixture: &Fixture) -> Fresh {
    let mut state = state(fixture);
    state.begin_generation();
    state
        .semantic
        .resolved_project_and_env_for(&fixture.app, &mut state.workspace, &state.docs)
        .expect("App folds");
    let env = env_value(&mut state, &fixture.app);
    let parses = parse_value(&mut state, &fixture.app);
    let trees = trees(&mut state, &fixture.app);
    state.end_generation();
    Fresh { env, parses, trees }
}

/// What one generation of the long-lived state served, and what it built.
struct Served {
    env: Arc<borzoi_sema::AssemblyEnv>,
    resolved: Arc<borzoi_sema::ResolvedProject>,
    /// The env the project's key serves now, which the fold must be folded
    /// against, and what it was built from.
    keyed_env: Arc<borzoi_sema::AssemblyEnv>,
    env_value: EnvValue,
    trees: Vec<(PathBuf, SyntaxNode)>,
    /// What the deferral report reads of the fold under the evaluation the
    /// workspace serves: that it succeeded.
    fold_succeeded: bool,
    folds: u64,
    envs_built: u64,
    resolutions: u64,
}

fn serve(state: &mut State, app: &Path) -> Served {
    state.begin_generation();
    let (resolved, env) = state
        .semantic
        .resolved_project_and_env_for(app, &mut state.workspace, &state.docs)
        .expect("App folds");
    let env_value = env_value(state, app);
    let root = state.workspace.dotnet_root_for_project(app);
    let tfm = state.workspace.served_tfm_for_project(app);
    let keyed_env =
        state
            .semantic
            .assembly_env_for_project(app, root.as_deref(), &tfm, &state.workspace);
    let trees = trees(state, app);
    let id = ProjectId::new(app);
    let fold_succeeded = matches!(
        state
            .semantic
            .fold_outcome(&id, state.workspace.evaluation_id(&id)),
        FoldOutcome::Succeeded
    );
    let generation = state.end_generation();
    Served {
        env,
        resolved,
        keyed_env,
        env_value,
        trees,
        fold_succeeded,
        folds: generation.folds,
        envs_built: generation.envs.built,
        resolutions: generation.envs.resolutions,
    }
}

/// How many steps of each kind the property met, for its non-vacuity check.
#[derive(Debug, Default)]
struct Census {
    env_kept: usize,
    env_rebuilt: usize,
    fold_kept: usize,
    parses_moved_env_kept: usize,
    env_moved_parses_kept: usize,
    incompleteness_moved: usize,
}

fn run(edits: &[Edit], census: &mut Census) -> Result<(), TestCaseError> {
    let mut world = initial();
    let fixture = fixture(&world, false);
    let mut state = state(&fixture);
    let mut before = serve(&mut state, &fixture.app);
    let Fresh {
        env: mut env_before,
        parses: mut parses_before,
        ..
    } = fresh(&fixture);
    for edit in edits {
        let old = world.clone();
        edit.apply(&mut world);
        if world == old {
            continue;
        }
        write_world(&fixture, &world);
        let Fresh {
            env: env_now,
            parses: parses_now,
            trees: trees_now,
        } = fresh(&fixture);
        let after = serve(&mut state, &fixture.app);
        let env_kept = Arc::ptr_eq(&before.env, &after.env);
        let fold_kept = Arc::ptr_eq(&before.resolved, &after.resolved);
        let env_same = env_now == env_before;
        let parses_same = parses_now == parses_before;
        let context = format!(
            "after {edit:?}\nworld {world:#?}\nenv before {env_before:?}\nenv now {env_now:?}\nparses before {parses_before:?}\nparses now {parses_now:?}"
        );
        // Sound: whatever was kept, the fold reads what a fresh one does.
        prop_assert!(
            Arc::ptr_eq(&after.env, &after.keyed_env),
            "the fold's env is not the one its key serves\n{}",
            context
        );
        prop_assert_eq!(&after.env_value, &env_now, "{}", context);
        prop_assert_eq!(
            after.env.identities_incomplete(),
            env_now.incomplete,
            "{}",
            context
        );
        prop_assert!(
            same_trees(&after.trees, &trees_now),
            "the fold's parses differ from a fresh state's\n{}",
            context
        );
        prop_assert!(
            after.fold_succeeded,
            "the fold's outcome is not reported\n{}",
            context
        );
        // Exact: kept exactly when what it is built from is unchanged.
        prop_assert_eq!(env_kept, env_same, "env kept\n{}", context);
        prop_assert_eq!(env_kept, after.envs_built == 0, "envs built\n{}", context);
        // A rebuilt env is built from the resolution that found it moved,
        // not from a second one.
        prop_assert!(
            after.resolutions <= 1 && (env_kept || after.resolutions == 1),
            "{} resolutions\n{}",
            after.resolutions,
            context
        );
        prop_assert_eq!(fold_kept, env_same && parses_same, "fold kept\n{}", context);
        prop_assert_eq!(fold_kept, after.folds == 0, "folds\n{}", context);
        if env_kept {
            census.env_kept += 1;
        } else {
            census.env_rebuilt += 1;
        }
        if fold_kept {
            census.fold_kept += 1;
        }
        if env_kept && !parses_same {
            census.parses_moved_env_kept += 1;
        }
        if !env_kept && parses_same {
            census.env_moved_parses_kept += 1;
        }
        if env_now.incomplete != env_before.incomplete && env_now.dlls == env_before.dlls {
            census.incompleteness_moved += 1;
        }
        before = after;
        env_before = env_now;
        parses_before = parses_now;
    }
    Ok(())
}

#[test]
fn the_env_and_the_fold_are_kept_exactly_when_what_they_read_is() {
    let census = std::cell::RefCell::new(Census::default());
    // Sequences that reach every kind of step the census asks for, so that
    // its check holds whatever the generator draws: the generated cases add
    // to them.
    let covering = [
        vec![Edit::Comment(1), Edit::Define("FOO")],
        vec![Edit::Reference(Ref::CSharp), Edit::Reference(Ref::Lib)],
    ];
    for edits in &covering {
        run(edits, &mut census.borrow_mut()).unwrap();
    }
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases: 24,
        ..ProptestConfig::default()
    });
    runner
        .run(&prop::collection::vec(edit_strategy(), 1..8), |edits| {
            run(&edits, &mut census.borrow_mut())
        })
        .unwrap();
    let census = census.into_inner();
    assert!(
        census.env_kept > 0
            && census.env_rebuilt > 0
            && census.fold_kept > 0
            && census.parses_moved_env_kept > 0
            && census.env_moved_parses_kept > 0
            && census.incompleteness_moved > 0,
        "{census:?}"
    );
}

/// Each edit from the initial world, pinned: whether it rebuilds the env and
/// whether it folds again. Run again with a package DLL the env skips, so the
/// DLLs the env holds are not the DLLs it was built from: an edit nothing
/// reads keeps the env all the same.
#[test]
fn each_consumed_value_moves_only_what_reads_it() {
    // (edit, env rebuilt, folded)
    let table: &[(Edit, bool, bool)] = &[
        (Edit::Comment(1), false, false),
        (Edit::Indent(4), false, false),
        (Edit::Description(1), false, false),
        (Edit::LibComment(1), false, false),
        (Edit::Reference(Ref::Unbuilt), false, false),
        (Edit::Reference(Ref::CSharp), true, true),
        (Edit::Define("FOO"), false, true),
        (Edit::Order, false, true),
        (Edit::LangVersion(Some("8.0")), false, true),
        (Edit::Reference(Ref::Lib), true, true),
        (Edit::LibRenamed, true, true),
        (Edit::Tfm, true, true),
    ];
    let mut failures = Vec::new();
    let junk_table: Vec<(Edit, bool, bool)> = table
        .iter()
        .filter(|(_, env_rebuilt, folded)| !env_rebuilt && !folded)
        .cloned()
        .collect();
    let flavours = table
        .iter()
        .map(|row| (row.clone(), false))
        .chain(junk_table.into_iter().map(|row| (row, true)));
    for ((edit, env_rebuilt, folded), junk_package) in flavours {
        let (edit, env_rebuilt, folded) = (&edit, &env_rebuilt, &folded);
        let mut world = initial();
        let fixture = fixture(&world, junk_package);
        let mut state = state(&fixture);
        let before = serve(&mut state, &fixture.app);
        edit.apply(&mut world);
        write_world(&fixture, &world);
        let after = serve(&mut state, &fixture.app);
        let rebuilt = !Arc::ptr_eq(&before.env, &after.env);
        if (rebuilt, after.folds > 0) != (*env_rebuilt, *folded) {
            failures.push(format!(
                "{edit:?} (junk package: {junk_package}): env rebuilt {rebuilt} (want {env_rebuilt}), folded {} (want {folded})",
                after.folds > 0
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A change of the `LangVersion`'s provenance alone, the version staying
/// `7.0`: the version-boundary gate runs exactly when the provenance is
/// untrusted, and a file whose parse shape depends on the version (an
/// EOF-anchored `match`) refuses the fold then. So the parses must not
/// outlive the change in either direction, though the items, the symbols
/// and the version are all as they were.
#[test]
fn a_change_of_language_version_provenance_alone_reaches_the_fold() {
    let tmp = TempDir::new().unwrap();
    let root = host_fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("P/P.fsproj");
    let render = |gated: bool| {
        let condition = if gated {
            " Condition=\"'$(DefineConstants)' == ''\""
        } else {
            ""
        };
        format!(
            "<Project>\n  <PropertyGroup{condition}>\n    <LangVersion>7.0</LangVersion>\n  </PropertyGroup>\n  <ItemGroup>\n    <Compile Include=\"A.fs\" />\n  </ItemGroup>\n</Project>\n"
        )
    };
    write(&root.join("P/A.fs"), b"match x with\n");
    let mut state = State::new();
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(root.join("no-dotnet")),
        ..SdkDiscoveryEnv::default()
    });
    state.clock = Clock::HostAhead(RACY_WINDOW * 30);
    let folds = |state: &mut State| {
        state.begin_generation();
        let folds = state
            .semantic
            .parses_for_project(&project, &mut state.workspace, &state.docs)
            .is_some();
        let value = parse_value(state, &project);
        state.end_generation();
        (folds, value)
    };
    write(&project, render(false).as_bytes());
    let (trusted, before) = folds(&mut state);
    assert!(trusted, "a trusted LangVersion folds the straddling file");
    for gated in [true, false, true] {
        write(&project, render(gated).as_bytes());
        let (folded, value) = folds(&mut state);
        assert_eq!(
            value, before,
            "precondition: only the provenance moved (gated: {gated})"
        );
        assert_eq!(
            folded,
            !gated,
            "the fold under {} provenance",
            if gated { "untrusted" } else { "trusted" }
        );
    }
}
