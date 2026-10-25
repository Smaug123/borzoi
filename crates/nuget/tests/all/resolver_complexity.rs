//! Complexity / DoS-resistance of the offline resolver's graph walk.
//!
//! `resolve_offline`'s walk is a *tree*: nearest-wins is path-dependent (a
//! package's eclipse outcome depends on which ancestors pin a competing version
//! of it), so a package reached via N cousin paths is walked N times and cannot
//! be deduplicated by `(id, version)` without changing the answer. NuGet's own
//! `RemoteDependencyWalker` has the same shape. That means a pathological
//! diamond-heavy graph is genuinely exponential — the resolver bounds it
//! (`MAX_NODES`) and **declines** (`GraphTooLarge`) rather than hang.
//!
//! These tests pin both ends of that behaviour:
//!
//! - A diamond *chain* (each level's package reached via twice as many paths as
//!   the last) exceeds the node bound and declines — *quickly*, verified with a
//!   wall-clock deadline so a regression that dropped the bound would fail here
//!   (as a timeout) instead of hanging the suite.
//! - A realistic-shaped closure (a wide-but-shallow diamond, dozens of packages)
//!   resolves cleanly, well under the bound — the bound never bites real work.
//!
//! The expensive per-`(id, version)` work (reading and parsing each nuspec off
//! disk) is memoised in the resolver's package cache, so even the bounded
//! pathological walk does no repeated I/O; only cheap in-memory node bookkeeping
//! grows, and it stops at the bound.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use borzoi_nuget::{
    DirectPackageRequirement, NuGetFramework, NuGetVersion, PackageId, PackageIdentity,
    PackagePaths, ResolveDecline, ResolvedPackageClosure, VersionRange, resolve_offline,
};

fn id(s: &str) -> PackageId {
    PackageId::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse as package id: {e}"))
}

fn version(s: &str) -> NuGetVersion {
    NuGetVersion::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse as version: {e}"))
}

fn range(s: &str) -> VersionRange {
    VersionRange::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse as range: {e}"))
}

fn framework(s: &str) -> NuGetFramework {
    NuGetFramework::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse as framework: {e}"))
}

fn req(id: &str, range: &str) -> DirectPackageRequirement {
    DirectPackageRequirement::new(self::id(id), self::range(range))
}

/// Commit a single-version package with the given dependency ids (each depended
/// on at `1.0.0`, an inclusive-minimum bare range) into the local-folder feed.
fn write_package(root: &Path, id: &str, deps: &[&str]) {
    let identity = PackageIdentity::new(self::id(id), version("1.0.0"));
    let paths = PackagePaths::new(root, &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    // NuGet writes the `.nupkg.metadata` marker last; its presence is what makes
    // the package "committed" (readable by the resolver).
    fs::write(&paths.metadata_path, "{}").expect("commit marker");
    fs::write(&paths.nuspec_path, nuspec(id, deps)).expect("nuspec");
}

fn nuspec(id: &str, deps: &[&str]) -> String {
    let dependencies = if deps.is_empty() {
        String::new()
    } else {
        let deps = deps
            .iter()
            .map(|dep| format!(r#"        <dependency id="{dep}" version="1.0.0" />"#))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            r#"
    <dependencies>
      <group targetFramework="net8.0">
{deps}
      </group>
    </dependencies>"#
        )
    };
    format!(
        r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>{dependencies}
  </metadata>
</package>
"#
    )
}

fn package_ids(closure: &ResolvedPackageClosure) -> Vec<String> {
    closure
        .packages
        .iter()
        .map(|package| package.identity.id.as_str().to_owned())
        .collect()
}

/// A chain of diamonds: `a{i}` fans out to `l{i}` and `r{i}`, and both rejoin at
/// `a{i+1}`. The number of distinct root→`a{i}` paths doubles every level, so at
/// `depth` levels the walk faces `2^depth` paths through only `3*depth+1`
/// packages — the exact DAG-vs-paths blow-up. With `depth` well past
/// `log2(MAX_NODES)` the walk must hit the node bound.
fn write_diamond_chain(root: &Path, depth: usize) {
    for level in 0..depth {
        write_package(
            root,
            &format!("a{level}"),
            &[&format!("l{level}"), &format!("r{level}")],
        );
        let next = format!("a{}", level + 1);
        write_package(root, &format!("l{level}"), &[&next]);
        write_package(root, &format!("r{level}"), &[&next]);
    }
    // The chain's tail: a leaf so the deepest edges resolve to a real package.
    write_package(root, &format!("a{depth}"), &[]);
}

#[test]
fn a_pathological_diamond_chain_declines_quickly_instead_of_hanging() {
    let cache = tempfile::TempDir::new().expect("tempdir");
    // 2^40 paths through ~121 packages. Chosen so the walk is intractable
    // *without* the bound (~10^12 nodes ⇒ a genuine hang), while *with* it the
    // walk stops after MAX_NODES (20_000) cheap expansions in well under a
    // second. So both failure modes are covered: dropping the bound trips the
    // deadline below (a hang), and mis-sizing it so the graph resolves trips the
    // GraphTooLarge assertion.
    write_diamond_chain(cache.path(), 40);

    // Run the resolve on a worker thread with a wall-clock deadline. Completion
    // proves it declined rather than hung; if a change dropped the node bound
    // this would time out here (and fail) rather than wedge the whole suite.
    let root = cache.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = resolve_offline(&root, &framework("net8.0"), &[req("a0", "1.0.0")]);
        // The channel may already be gone if the deadline elapsed; ignore that.
        let _ = tx.send(matches!(result, Err(ResolveDecline::GraphTooLarge)));
    });

    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(declined_as_too_large) => {
            worker.join().expect("worker thread panicked");
            assert!(
                declined_as_too_large,
                "a diamond chain past the node bound must decline with GraphTooLarge"
            );
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!(
                "resolve_offline did not finish within the deadline on a pathological \
                 graph — the node bound is not stopping the exponential walk"
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("worker thread dropped the channel without sending a result");
        }
    }
}

#[test]
fn a_realistic_shallow_closure_resolves_well_under_the_bound() {
    let cache = tempfile::TempDir::new().expect("tempdir");

    // A wide-but-shallow closure with genuine diamonds: an app with ten direct
    // dependencies, each pulling two shared libraries, which share a common
    // runtime leaf. Every shared node is reached via many cousin paths — the
    // same shape as the pathological case, but bounded in depth, so the node
    // count stays in the low hundreds rather than exploding.
    let direct: Vec<String> = (0..10).map(|i| format!("app.dep{i}")).collect();
    for dep in &direct {
        write_package(cache.path(), dep, &["lib.core", "lib.json"]);
    }
    write_package(cache.path(), "lib.core", &["lib.runtime"]);
    write_package(cache.path(), "lib.json", &["lib.core", "lib.runtime"]);
    write_package(cache.path(), "lib.runtime", &[]);

    let requirements: Vec<DirectPackageRequirement> =
        direct.iter().map(|dep| req(dep, "1.0.0")).collect();
    let closure = resolve_offline(cache.path(), &framework("net8.0"), &requirements)
        .expect("a realistic shallow closure must resolve well under the node bound");

    let mut got = package_ids(&closure);
    got.sort();
    let mut expected: Vec<String> = direct.clone();
    expected.extend(["lib.core", "lib.json", "lib.runtime"].map(str::to_owned));
    expected.sort();
    assert_eq!(
        got, expected,
        "the closure is exactly the app's dependencies and their shared transitive leaves"
    );
}
