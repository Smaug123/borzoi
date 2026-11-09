//! Systematic guard for the on-demand restore's *invocation* invariants under
//! adversarial project configurations (nuget-restore-plan Slice 8).
//!
//! The `restore_to_scratch_diff` differential guards the *result* (the DLL set
//! equals a normal restore's) for happy-path projects. It cannot catch the class
//! of hazard that repeated review surfaced — a project or import knob that makes
//! restore write into the source tree, reach the network, or decline spuriously.
//! Those are *environmental*, not result, bugs, so this drives the real restore
//! against a matrix of hostile configs and asserts two invariants mechanically:
//!
//!  1. **It never writes the project's source tree.** The whole file set under
//!     the project directory is identical before and after — no `obj/`, no
//!     redirected output dir, no `packages.lock.json`, no `.nuget.g.*`. This is
//!     the invariant behind the `MSBuildProjectExtensionsPath` /
//!     `RestoreOutputPath` / lock-file findings.
//!  2. **It reaches the expected outcome** (resolve vs decline) — catching a
//!     *spurious* decline (e.g. a project override sending output somewhere we
//!     don't read) as distinct from a correct one.
//!
//! Each case would have caught a specific past regression; adding a hazard here
//! is cheaper and more durable than another review round.
//!
//! Not covered here (documented gaps): a genuinely relative `TMPDIR` (process
//! global — can't set per-test without disturbing siblings; guarded by
//! `std::path::absolute` in `restore.rs`), and an unreachable `auditSources`
//! endpoint (would need a real network stall to exercise; guarded by
//! `-p:NuGetAudit=false`).
//!
//! Requires the .NET SDK + the vendored NuGet cache — the Nix devShell provides
//! both.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use borzoi::restore::{RestoreOutcome, restore_to_scratch_assemblies};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::workspace::Workspace;

/// A version of `FSharp.Core` vendored in the devshell's `$NUGET_PACKAGES`.
const FSHARP_CORE_VERSION: &str = "10.1.204";

fn dotnet_root() -> PathBuf {
    std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset — run under `nix develop`")
}

/// Every file path under `root`, recursively — the snapshot compared before and
/// after a restore to prove the source tree is untouched.
fn tree_snapshot(root: &Path) -> BTreeSet<PathBuf> {
    let mut files = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.insert(path);
            }
        }
    }
    files
}

/// Expected restore outcome for a hazard case.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    /// The warm-cache project resolves (its DLL set is non-empty).
    Resolves,
    /// The config makes restore decline (locked graph, cold package).
    Declines,
}

/// A fresh scratch folder for `tag`, by its real path: the on-demand restore
/// declines a project whose directory is reached through a symlink, and the
/// temporary directory is one on macOS.
fn scratch_folder(tag: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-hostile-{tag}-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(&root).unwrap()
}

/// Write a hazard project (`App.fsproj` body + optional sibling files), run the
/// on-demand restore against it, and assert both invariants: the project tree is
/// byte-for-byte unchanged, and the outcome matches `expect`.
fn assert_hazard(tag: &str, fsproj_body: &str, siblings: &[(&str, &str)], expect: Expect) {
    let root = scratch_folder(tag);
    let outcome = restore_hazard(
        tag,
        &root,
        fsproj_body,
        siblings,
        SdkDiscoveryEnv::from_process_env(),
    );
    assert_outcome(tag, expect, &outcome);
    std::fs::remove_dir_all(&root).ok();
}

/// Write the hazard project into `root`, run the on-demand restore under `env`,
/// assert that the project tree is byte-for-byte unchanged, and return the
/// outcome.
fn restore_hazard(
    tag: &str,
    root: &Path,
    fsproj_body: &str,
    siblings: &[(&str, &str)],
    env: SdkDiscoveryEnv,
) -> RestoreOutcome {
    let fsproj = root.join("App.fsproj");
    std::fs::write(&fsproj, fsproj_body).unwrap();
    std::fs::write(root.join("App.fs"), "module App\n\nlet answer = 42\n").unwrap();
    for (name, content) in siblings {
        std::fs::write(root.join(name), content).unwrap();
    }

    let mut workspace = Workspace::with_env(env);
    let served = workspace.served_tfm_for_project(&fsproj);
    let dotnet = dotnet_root();

    let before = tree_snapshot(root);
    let outcome = restore_to_scratch_assemblies(&fsproj, &dotnet, &served, Some(workspace.env()));
    let after = tree_snapshot(root);

    assert_eq!(
        before,
        after,
        "[{tag}] restore must not write the project's source tree\n\
         created: {:?}\n removed: {:?}",
        after.difference(&before).collect::<Vec<_>>(),
        before.difference(&after).collect::<Vec<_>>(),
    );
    outcome
}

/// Assert that `outcome` is what `expect` says.
fn assert_outcome(tag: &str, expect: Expect, outcome: &RestoreOutcome) {
    match (expect, outcome) {
        (Expect::Resolves, RestoreOutcome::Resolved(resolved)) => {
            let n = resolved.package_dlls.len() + resolved.framework_dlls.len();
            assert!(n > 0, "[{tag}] expected a non-empty resolve, got nothing");
        }
        (Expect::Declines, RestoreOutcome::Declined) => {}
        (expect, outcome) => panic!(
            "[{tag}] expected {expect:?}, got {}",
            match outcome {
                RestoreOutcome::Resolved(_) => "Resolved",
                RestoreOutcome::Declined => "Declined",
                RestoreOutcome::TransientFailure => "TransientFailure",
            }
        ),
    }
}

/// The process environment with neither `NUGET_PACKAGES` nor
/// `DOTNET_CLI_HOME`, and `home` as the home: the package folder then comes
/// from the config files, and the user's own config is not read.
fn env_choosing_the_folder_by_config(home: &Path) -> SdkDiscoveryEnv {
    // .NET hands NuGet `HOME` only if it can read it.
    std::fs::create_dir_all(home).unwrap();
    let mut env = SdkDiscoveryEnv::from_process_env();
    for name in ["NUGET_PACKAGES", "DOTNET_CLI_HOME", "HOME"] {
        env.build_environment.remove(name);
    }
    env.nuget_packages_dir = None;
    env.dotnet_cli_home = None;
    env.home_dir = Some(home.to_owned());
    env
}

/// Copy the folder `from` into `to`, recursively.
fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `net10.0` project body with the given extra `<PropertyGroup>` lines and item
/// group, always pinning the vendored `FSharp.Core` (implicit reference off, so
/// the package set is deterministic) unless `item_group` overrides it.
fn project(extra_properties: &str, item_group: &str) -> String {
    format!(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
{extra_properties}
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
{item_group}
  </ItemGroup>
</Project>
"#
    )
}

/// The pinned `FSharp.Core` item — the warm-cache package the resolving cases use.
fn fsharp_core_ref() -> String {
    format!("    <PackageReference Include=\"FSharp.Core\" Version=\"{FSHARP_CORE_VERSION}\" />")
}

#[test]
fn baseline_resolves_without_touching_the_tree() {
    assert_hazard(
        "baseline",
        &project("", &fsharp_core_ref()),
        &[],
        Expect::Resolves,
    );
}

#[test]
fn project_restore_sources_is_overridden_offline() {
    // A project pointing `RestoreSources` at nuget.org must still resolve from
    // the warm cache (our empty-source override forces offline) — and not reach
    // out or write the tree.
    let body = project(
        "    <RestoreSources>https://api.nuget.org/v3/index.json</RestoreSources>",
        &fsharp_core_ref(),
    );
    assert_hazard("restore-sources", &body, &[], Expect::Resolves);
}

#[test]
fn custom_restore_output_path_stays_in_scratch() {
    // `RestoreOutputPath=custom/` would send the assets into the source tree
    // unless we pin it to scratch; assert it resolves and writes nothing.
    let body = project(
        "    <RestoreOutputPath>custom/</RestoreOutputPath>",
        &fsharp_core_ref(),
    );
    assert_hazard("restore-output-path", &body, &[], Expect::Resolves);
}

#[test]
fn sibling_lock_file_declines_without_touching_the_tree() {
    // A `packages.lock.json` pins the graph — decline, and never rewrite it.
    let body = project(
        "    <RestorePackagesWithLockFile>true</RestorePackagesWithLockFile>",
        &fsharp_core_ref(),
    );
    assert_hazard(
        "sibling-lock",
        &body,
        &[(
            "packages.lock.json",
            "{\n  \"version\": 1,\n  \"dependencies\": {}\n}\n",
        )],
        Expect::Declines,
    );
}

#[test]
fn lock_file_enabled_without_a_lock_writes_no_lock() {
    // Lock files enabled but none present yet: restore resolves and must not
    // drop a `packages.lock.json` into the tree (we disable + redirect it).
    let body = project(
        "    <RestorePackagesWithLockFile>true</RestorePackagesWithLockFile>",
        &fsharp_core_ref(),
    );
    assert_hazard("lock-enabled-no-lock", &body, &[], Expect::Resolves);
}

#[test]
fn uncached_package_declines_offline_without_touching_the_tree() {
    // A package absent from the warm cache must fail fast offline (NU1101) and
    // decline — not stall on the network, not write the tree.
    let body = project(
        "",
        "    <PackageReference Include=\"Nonexistent.Package.Xyz\" Version=\"1.2.3\" />",
    );
    assert_hazard("cold-package", &body, &[], Expect::Declines);
}

#[test]
fn repo_local_restore_packages_path_stays_in_cache() {
    // `RestorePackagesPath=local-packages/` would extract package files into the
    // source tree unless we pin the package folder to the warm cache; assert it
    // resolves and writes nothing.
    let body = project(
        "    <RestorePackagesPath>local-packages/</RestorePackagesPath>",
        &fsharp_core_ref(),
    );
    assert_hazard("restore-packages-path", &body, &[], Expect::Resolves);
}

#[test]
fn config_global_packages_folder_inside_the_project_declines() {
    // With no `NUGET_PACKAGES`, a repo-local `globalPackagesFolder` is the
    // folder NuGet would use, and restore could extract into it: decline,
    // writing nothing.
    let scratch = scratch_folder("config-gpf-inside");
    let root = scratch.join("app");
    std::fs::create_dir_all(&root).unwrap();
    let config = "<?xml version=\"1.0\"?>\n\
                  <configuration><config>\
                  <add key=\"globalPackagesFolder\" value=\"local-packages\" />\
                  </config></configuration>\n";
    let outcome = restore_hazard(
        "config-gpf-inside",
        &root,
        &project("", &fsharp_core_ref()),
        &[("nuget.config", config)],
        env_choosing_the_folder_by_config(&scratch.join("home")),
    );
    assert_outcome("config-gpf-inside", Expect::Declines, &outcome);
    std::fs::remove_dir_all(&scratch).ok();
}

#[test]
fn config_global_packages_folder_inside_the_repository_declines() {
    // A repository-root config choosing a folder beside it, for a project in
    // a subfolder: the folder is outside the project's directory but inside
    // the tree the config governs. Decline, writing nothing there.
    let repository = scratch_folder("config-gpf-repository");
    let root = repository.join("src");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        repository.join("nuget.config"),
        "<configuration><config>\
         <add key=\"globalPackagesFolder\" value=\".packages\" />\
         </config></configuration>\n",
    )
    .unwrap();
    let home = scratch_folder("config-gpf-repository-home");
    let outcome = restore_hazard(
        "config-gpf-repository",
        &root,
        &project("", &fsharp_core_ref()),
        &[],
        env_choosing_the_folder_by_config(&home),
    );
    assert_outcome("config-gpf-repository", Expect::Declines, &outcome);
    assert!(
        !repository.join(".packages").exists(),
        "the restore must not create the repository's package folder"
    );
    std::fs::remove_dir_all(&repository).ok();
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn config_global_packages_folder_is_the_folder_restore_reads() {
    // With no `NUGET_PACKAGES`, the restore reads the folder the config names,
    // and only that one: every package assembly resolves from it.
    let scratch = scratch_folder("config-gpf-respected");
    let root = scratch.join("app");
    std::fs::create_dir_all(&root).unwrap();
    let vendored = PathBuf::from(
        std::env::var_os("NUGET_PACKAGES").expect("NUGET_PACKAGES unset — run under `nix develop`"),
    );
    let packages = scratch.join("packages");
    copy_folder(
        &vendored.join("fsharp.core").join(FSHARP_CORE_VERSION),
        &packages.join("fsharp.core").join(FSHARP_CORE_VERSION),
    );
    let config = format!(
        "<configuration><config>\
         <add key=\"globalPackagesFolder\" value=\"{}\" />\
         </config></configuration>\n",
        packages.display()
    );
    let outcome = restore_hazard(
        "config-gpf-respected",
        &root,
        &project("", &fsharp_core_ref()),
        &[("nuget.config", &config)],
        env_choosing_the_folder_by_config(&scratch.join("home")),
    );
    assert_outcome("config-gpf-respected", Expect::Resolves, &outcome);
    let RestoreOutcome::Resolved(resolved) = outcome else {
        unreachable!("asserted above")
    };
    assert!(
        !resolved.package_dlls.is_empty()
            && resolved
                .package_dlls
                .iter()
                .all(|dll| dll.starts_with(&packages)),
        "every package assembly must come from the configured folder {}: {:?}",
        packages.display(),
        resolved.package_dlls
    );
    std::fs::remove_dir_all(&scratch).ok();
}

#[test]
fn config_global_packages_folder_stays_in_cache() {
    // `NUGET_PACKAGES` (the devshell sets it) outranks a repo-local
    // `globalPackagesFolder`, so the restore reads the cache it names and
    // writes nothing in the tree.
    let config = "<?xml version=\"1.0\"?>\n\
                  <configuration><config>\
                  <add key=\"globalPackagesFolder\" value=\"local-packages\" />\
                  </config></configuration>\n";
    assert_hazard(
        "config-gpf",
        &project("", &fsharp_core_ref()),
        &[("nuget.config", config)],
        Expect::Resolves,
    );
}
