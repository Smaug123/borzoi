//! A cached assembly env is valid while its resolution's read set still
//! validates (`docs/incremental-lsp-plan.md`, Stage 6b), so a change to a
//! file one env reads rebuilds that env and leaves every other alone.
//!
//! The fixture is two unrelated restored projects over a stub framework pack
//! holding the real `System.Runtime.dll`: `App`, which references the F#
//! project `Lib` (whose built output is a copy of the real
//! `System.Console.dll`) and the package `Foo` in the NuGet folder, and
//! `Other`, which references neither. "Rebuilding" `Lib` rewrites its output
//! DLL. Whether the client reports it (`workspace/didChangeWatchedFiles`) or
//! not, `App`'s env is built again and `Other`'s is the same allocation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::State;
use borzoi::uri::UriExt as _;
use borzoi::vfs::{Clock, RACY_WINDOW};
use borzoi::workspace::Workspace;
use borzoi_sema::AssemblyEnv;
use lsp_types::{FileChangeType, FileEvent, Uri};
use tempfile::TempDir;

use crate::common::host_fs;

const TFM: &str = "net10.0";

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn assets(root: &Path, packages: &[&str]) -> String {
    let mut targets = serde_json::Map::new();
    let mut libraries = serde_json::Map::new();
    for id in packages {
        let lower = id.to_ascii_lowercase();
        targets.insert(
            format!("{id}/1.0.0"),
            serde_json::json!({
                "type": "package",
                "compile": { format!("lib/{TFM}/{id}.dll"): {} },
            }),
        );
        libraries.insert(
            format!("{id}/1.0.0"),
            serde_json::json!({ "type": "package", "path": format!("{lower}/1.0.0") }),
        );
    }
    serde_json::json!({
        "version": 3,
        "targets": { TFM: targets },
        "libraries": libraries,
        "packageFolders": { root.join("nuget").to_str().unwrap(): {} },
        "project": {
            "frameworks": { TFM: { "frameworkReferences": { "Microsoft.NETCore.App": {} } } }
        }
    })
    .to_string()
}

/// The fixture's paths.
struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    app: PathBuf,
    other: PathBuf,
    lib_dll: PathBuf,
    /// A file inside the `Foo` package version `App` reads from, which `App`
    /// itself never reads.
    foo_doc: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    // Canonical, so the reads' spellings are the ones the events name.
    let root = host_fs::canonicalize(tmp.path()).unwrap();
    // NuGet reads its user settings from a home that exists.
    std::fs::create_dir_all(root.join("home")).unwrap();
    let runtime = crate::common::ensure_system_runtime_dll();
    let console = host_fs::read(runtime.with_file_name("System.Console.dll")).unwrap();
    write(
        &root.join(format!(
            "dotnet/packs/Microsoft.NETCore.App.Ref/10.0.0/ref/{TFM}/System.Runtime.dll"
        )),
        &host_fs::read(&runtime).unwrap(),
    );
    let package = root.join("nuget/foo/1.0.0");
    write(
        &package.join(format!("lib/{TFM}/Foo.dll")),
        b"not an assembly",
    );
    write(&package.join(".nupkg.metadata"), b"{}");
    let foo_doc = package.join(format!("lib/{TFM}/Foo.xml"));
    write(&foo_doc, b"<doc />");
    let lib = root.join("Lib/Lib.fsproj");
    write(
        &lib,
        format!(
            "<Project><PropertyGroup><TargetFramework>{TFM}</TargetFramework><TargetName>Lib</TargetName></PropertyGroup></Project>\n"
        )
        .as_bytes(),
    );
    let lib_dll = root.join(format!("Lib/bin/Debug/{TFM}/Lib.dll"));
    write(&lib_dll, &console);
    let app = root.join("App/App.fsproj");
    write(
        &app,
        format!(
            "<Project><PropertyGroup><TargetFramework>{TFM}</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include=\"../Lib/Lib.fsproj\" /></ItemGroup></Project>\n"
        )
        .as_bytes(),
    );
    write(
        &root.join("App/obj/project.assets.json"),
        assets(&root, &["Foo"]).as_bytes(),
    );
    let other = root.join("Other/Other.fsproj");
    write(
        &other,
        format!(
            "<Project><PropertyGroup><TargetFramework>{TFM}</TargetFramework></PropertyGroup></Project>\n"
        )
        .as_bytes(),
    );
    write(
        &root.join("Other/obj/project.assets.json"),
        assets(&root, &[]).as_bytes(),
    );
    Fixture {
        _tmp: tmp,
        root,
        app,
        other,
        lib_dll,
        foo_doc,
    }
}

/// A server state over the fixture: SDK discovery finds the stub install,
/// and nothing of the host's. NuGet's settings name a global packages folder
/// other than the package folder the assets files name (as the
/// whole-project corpus's restore does), so a package is trusted as
/// installed because the assets file names its folder. Its clock runs
/// ahead of the files' timestamps, so no stamp a generation takes of a
/// project file is racy.
fn state(fixture: &Fixture) -> State {
    let mut state = State::new();
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(fixture.root.join("dotnet")),
        nuget_packages_dir: Some(fixture.root.join("global-packages")),
        // NuGet's settings are read from the home directory; without one
        // they cannot be said, and no package is trusted as installed.
        home_dir: Some(fixture.root.join("home")),
        ..SdkDiscoveryEnv::default()
    });
    state.clock = Clock::HostAhead(RACY_WINDOW * 30);
    state
}

fn env_of(state: &mut State, project: &Path) -> Arc<AssemblyEnv> {
    let root = state.workspace.dotnet_root_for_project(project);
    let tfm = state.workspace.served_tfm_for_project(project);
    state
        .semantic
        .assembly_env_for_project(project, root.as_deref(), &tfm, &state.workspace)
}

/// A DLL's stamp taken within the racy window of its last change, by the
/// host's clock, never holds again, so a test that means an env to survive
/// waits that window out after writing what the env reads.
fn outwait_racy_stamps() {
    std::thread::sleep(
        borzoi_msbuild::observed::STAMP_RACY_WINDOW + std::time::Duration::from_millis(200),
    );
}

fn changed(path: &Path) -> FileEvent {
    FileEvent {
        uri: Uri::from_file_path(path).expect("an absolute path"),
        typ: FileChangeType::CHANGED,
    }
}

/// The plan's oracle: with a watch event reporting the rebuilt DLL, the env
/// of the project that references it is built again, and the other
/// project's env is the same allocation.
#[test]
fn a_watched_rebuild_rebuilds_only_the_env_that_reads_it() {
    let fixture = fixture();
    let mut state = state(&fixture);
    let app = env_of(&mut state, &fixture.app);
    let other = env_of(&mut state, &fixture.other);
    let (app_dlls, _) = {
        let root = state.workspace.dotnet_root_for_project(&fixture.app);
        let tfm = state.workspace.served_tfm_for_project(&fixture.app);
        state.semantic.env_reference_dlls_for_project(
            &fixture.app,
            root.as_deref(),
            &tfm,
            &state.workspace,
        )
    };
    assert!(
        app_dlls.contains(&fixture.lib_dll),
        "precondition: App's env folds Lib's output: {app_dlls:?}"
    );

    write(
        &fixture.lib_dll,
        b"a rebuilt Lib.dll that is not an assembly",
    );
    state.apply_watched_changes(&[changed(&fixture.lib_dll)]);

    let other_after = env_of(&mut state, &fixture.other);
    let app_after = env_of(&mut state, &fixture.app);
    assert!(
        Arc::ptr_eq(&other, &other_after),
        "a DLL Other never read rebuilt Other's env"
    );
    assert!(
        !Arc::ptr_eq(&app, &app_after),
        "the DLL App's env folds was rebuilt and reported, and App kept its env"
    );
    assert!(
        app_after.len() < app.len(),
        "App's rebuilt env still holds what the old Lib.dll defined ({} entities, {} before)",
        app_after.len(),
        app.len()
    );
}

/// The same rebuild with no watch event: in the next generation that reads
/// them, `App`'s env is confirmed against the disk and built again, and
/// `Other`'s is confirmed and kept. A generation with nothing changed builds
/// no env; once the first has asked each read again and taken its witnesses,
/// one confirms each env by `stat`s alone.
#[test]
fn an_unwatched_rebuild_rebuilds_only_the_env_that_reads_it() {
    let fixture = fixture();
    let mut state = state(&fixture);
    outwait_racy_stamps();
    let generation = |state: &mut State| {
        state.begin_generation();
        let app = env_of(state, &fixture.app);
        let other = env_of(state, &fixture.other);
        (app, other, state.end_generation())
    };
    let (app, other, first) = generation(&mut state);
    assert_eq!(first.envs.built, 2, "{first:?}");

    let (app_again, other_again, first_confirmation) = generation(&mut state);
    assert!(Arc::ptr_eq(&app, &app_again) && Arc::ptr_eq(&other, &other_again));
    assert_eq!(first_confirmation.envs.built, 0, "{first_confirmation:?}");
    let (app_again, other_again, unchanged) = generation(&mut state);
    assert!(Arc::ptr_eq(&app, &app_again) && Arc::ptr_eq(&other, &other_again));
    assert_eq!(unchanged.envs.built, 0, "{unchanged:?}");
    assert!(
        unchanged.envs.validation.stats > 0
            && unchanged.envs.validation.opens == 0
            && unchanged.envs.validation.listings == 0,
        "an unchanged env is confirmed by `stat`s alone: {unchanged:?}"
    );

    write(
        &fixture.lib_dll,
        b"a rebuilt Lib.dll that is not an assembly",
    );
    let (app_after, other_after, rebuilt) = generation(&mut state);
    assert!(
        Arc::ptr_eq(&other, &other_after),
        "a DLL Other never read rebuilt Other's env"
    );
    assert!(
        !Arc::ptr_eq(&app, &app_after),
        "the DLL App's env folds was rebuilt, and App kept its env"
    );
    assert!(app_after.len() < app.len());
    assert_eq!(rebuilt.envs.built, 1, "{rebuilt:?}");
}

/// What a watch event does to an env: one naming a path the env read drops
/// it at once; any event at all makes its next confirmation cover the
/// installed contents it trusts, whatever path the event names (another
/// case, a symlink, a file it never read), because an event is what covers
/// an in-place change to an installed version. Without an event, such a
/// change is the installed-version axiom's boundary, and goes unseen.
#[test]
fn an_event_widens_the_next_confirmation_to_installed_contents() {
    let fixture = fixture();
    let foo_dll = fixture
        .root
        .join(format!("nuget/foo/1.0.0/lib/{TFM}/Foo.dll"));
    let mut state = state(&fixture);
    outwait_racy_stamps();
    let generation = |state: &mut State| {
        state.begin_generation();
        let app = env_of(state, &fixture.app);
        let other = env_of(state, &fixture.other);
        state.end_generation();
        (app, other)
    };
    let (app, other) = generation(&mut state);

    // A file nobody read, inside an installed version or not: nothing moved.
    let elsewhere = fixture
        .root
        .join(format!("nuget/bar/2.0.0/lib/{TFM}/Bar.dll"));
    write(&elsewhere, b"not an assembly");
    state.apply_watched_changes(&[changed(&elsewhere), changed(&fixture.foo_doc)]);
    let (app_again, other_again) = generation(&mut state);
    assert!(Arc::ptr_eq(&app, &app_again) && Arc::ptr_eq(&other, &other_again));

    // An in-place change inside the package version App trusts, unwatched:
    // trusted, so not confirmed (the exemption is live).
    write(&foo_dll, b"rewritten in place, not an assembly either");
    outwait_racy_stamps();
    let (app_again, _) = generation(&mut state);
    assert!(
        Arc::ptr_eq(&app, &app_again),
        "precondition: an unwatched in-place change to an installed version is trusted away"
    );

    // Then any event, under a spelling that names nothing App read.
    state.apply_watched_changes(&[changed(&fixture.root.join("Elsewhere/NOTHING.dll"))]);
    let (app_after, other_after) = generation(&mut state);
    assert!(
        !Arc::ptr_eq(&app, &app_after),
        "an event left an in-place change to a trusted version unconfirmed"
    );
    assert!(Arc::ptr_eq(&other, &other_after));
}

/// What an env confirms by `stat` rather than trusts as installed: a
/// package version's contents are trusted only once its `.nupkg.metadata`
/// (which NuGet writes last) was there before the env was resolved, and a
/// framework pack version's whenever a read inside it got an answer. A
/// project's own files are always confirmed.
#[test]
fn an_env_trusts_a_package_only_once_it_was_committed() {
    for committed in [true, false] {
        let fixture = fixture();
        let foo_dll = fixture
            .root
            .join(format!("nuget/foo/1.0.0/lib/{TFM}/Foo.dll"));
        if !committed {
            std::fs::remove_file(fixture.root.join("nuget/foo/1.0.0/.nupkg.metadata")).unwrap();
        }
        let mut state = state(&fixture);
        outwait_racy_stamps();
        state.begin_generation();
        let root = state.workspace.dotnet_root_for_project(&fixture.app);
        let tfm = state.workspace.served_tfm_for_project(&fixture.app);
        let checks = state
            .semantic
            .env_confirmed_reads_for_project(&fixture.app, root.as_deref(), &tfm, &state.workspace)
            .expect("App's env is cached");
        state.end_generation();
        let confirms = |path: &Path| checks.iter().any(|read| read.path == path);
        assert_eq!(
            confirms(&foo_dll),
            !committed,
            "a package committed: {committed}, confirmed: {}",
            confirms(&foo_dll)
        );
        assert!(
            confirms(&fixture.lib_dll),
            "a project's output is confirmed"
        );
        assert!(
            !checks.iter().any(|read| read.path.starts_with(
                fixture
                    .root
                    .join("dotnet/packs/Microsoft.NETCore.App.Ref/10.0.0")
            ) && read.path
                != fixture
                    .root
                    .join("dotnet/packs/Microsoft.NETCore.App.Ref/10.0.0")),
            "the framework pack's contents are trusted: {checks:?}"
        );
    }
}

/// An env's checks are confirmed only through the workspace that made
/// them: the witnesses name that checker's directories. A state whose
/// workspace is replaced builds its envs again in its next generation.
#[test]
fn an_env_made_under_another_workspace_is_built_again() {
    let fixture = fixture();
    let mut state = state(&fixture);
    outwait_racy_stamps();
    let generation = |state: &mut State| {
        state.begin_generation();
        let env = env_of(state, &fixture.other);
        state.end_generation();
        env
    };
    let other = generation(&mut state);
    assert!(Arc::ptr_eq(&other, &generation(&mut state)));
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(fixture.root.join("dotnet")),
        ..SdkDiscoveryEnv::default()
    });
    assert!(!Arc::ptr_eq(&other, &generation(&mut state)));
}

/// What an env trusts in an installed version is its contents, not its
/// existence: a package version deleted whole (a cleared package cache),
/// with no watch event, is seen in the next generation that reads the env.
#[test]
fn a_removed_package_version_is_seen_without_an_event() {
    let fixture = fixture();
    let mut state = state(&fixture);
    outwait_racy_stamps();
    let generation = |state: &mut State| {
        state.begin_generation();
        let app = env_of(state, &fixture.app);
        state.end_generation();
        app
    };
    let app = generation(&mut state);
    assert!(Arc::ptr_eq(&app, &generation(&mut state)));
    std::fs::remove_dir_all(fixture.root.join("nuget/foo/1.0.0")).unwrap();
    assert!(
        !Arc::ptr_eq(&app, &generation(&mut state)),
        "a package version deleted whole left the env standing"
    );
}
