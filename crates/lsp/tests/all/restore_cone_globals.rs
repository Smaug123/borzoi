//! The restore-cone census of `docs/nuget-dgspec-boundary-plan.md` (stage 8)
//! runs `GenerateRestoreGraphFile` in-process, so it cannot call `restore.rs`'s
//! command builder; it takes its global properties from
//! [`generate_restore_graph_globals`]. This holds that list to the command line
//! [`restore_graph`] actually runs, read back from a stand-in `dotnet` that
//! records its arguments: the same target, and the same global properties with
//! the same values, none missing and none extra.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borzoi::restore::{RestoreGraph, RestoreGraphSource, restore_graph};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi_oracle_harness::restore_globals::{
    GENERATE_RESTORE_GRAPH_TARGET, generate_restore_graph_globals,
};

/// A `dotnet` that writes its arguments to `record`, one per line, and fails,
/// so the oracle declines without running anything.
#[cfg(unix)]
fn recording_dotnet(dir: &Path, record: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let exe = dir.join("dotnet");
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nexit 1\n",
        record.display()
    );
    std::fs::write(&exe, script).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

#[cfg(unix)]
#[test]
fn the_census_globals_are_the_ones_restore_rs_generates_the_graph_with() {
    let tmp = tempfile::tempdir().unwrap();
    // By its real path: a project named through a symlink declines before any
    // command is built, and the temporary directory is one on macOS.
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let project_dir = root.join("app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let project = project_dir.join("App.fsproj");
    std::fs::write(&project, "<Project Sdk=\"Microsoft.NET.Sdk\" />\n").unwrap();
    let dotnet_root = root.join("dotnet-root");
    std::fs::create_dir_all(&dotnet_root).unwrap();
    let record = root.join("arguments.txt");
    recording_dotnet(&dotnet_root, &record);
    let cache = root.join("packages");
    std::fs::create_dir_all(&cache).unwrap();
    // A home, so the on-demand restore knows which user config NuGet loads
    // (.NET hands NuGet `HOME` only if it can read it).
    std::fs::create_dir_all(root.join("home")).unwrap();
    let env = SdkDiscoveryEnv {
        nuget_packages_dir: Some(cache.clone()),
        home_dir: Some(root.join("home")),
        ..SdkDiscoveryEnv::default()
    };

    let outcome = restore_graph(&project, &dotnet_root, &env, RestoreGraphSource::Generated);
    assert!(
        matches!(outcome, RestoreGraph::Declined),
        "the recording dotnet fails, so the oracle declines"
    );

    let arguments = std::fs::read_to_string(&record).expect("restore_graph ran the dotnet");
    let (properties, positional): (Vec<&str>, Vec<&str>) =
        arguments.lines().partition(|a| a.starts_with("-p:"));
    assert_eq!(
        positional,
        [
            "msbuild",
            project.to_str().unwrap(),
            "-nologo",
            &format!("-t:{GENERATE_RESTORE_GRAPH_TARGET}"),
        ],
        "everything restore.rs passes besides global properties"
    );
    let mut ran = BTreeMap::new();
    for property in &properties {
        let (name, value) = property["-p:".len()..]
            .split_once('=')
            .unwrap_or_else(|| panic!("{property} is not -p:<name>=<value>"));
        assert!(
            ran.insert(name.to_owned(), value.to_owned()).is_none(),
            "restore.rs passes {name} twice"
        );
    }

    let output = PathBuf::from(&ran["RestoreGraphOutputPath"]);
    let scratch = output.parent().expect("the spec is written into scratch");
    let census: BTreeMap<String, String> = generate_restore_graph_globals(scratch, &cache, &output)
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.into_string().unwrap()))
        .collect();
    assert_eq!(
        census.len(),
        generate_restore_graph_globals(scratch, &cache, &output).len(),
        "the census list names each global once"
    );
    assert_eq!(census, ran);
}
