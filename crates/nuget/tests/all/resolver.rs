use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use borzoi_nuget::{
    DirectPackageRequirement, NuGetFramework, NuGetVersion, PackageId, PackageIdentity,
    PackagePaths, PackageReadError, ResolveDecline, VersionRange, resolve_offline,
};
use proptest::prelude::*;

#[derive(Debug, Clone)]
struct Dep {
    id: String,
    version: String,
    include: Option<String>,
    exclude: Option<String>,
}

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

fn write_package(root: &Path, id: &str, version: &str, deps: &[Dep]) -> PackageIdentity {
    let identity = PackageIdentity::new(self::id(id), self::version(version));
    let paths = PackagePaths::new(root, &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.metadata_path, "{}").expect("commit marker");
    fs::write(&paths.nuspec_path, nuspec(id, version, deps)).expect("nuspec");
    identity
}

fn write_uncommitted_package(root: &Path, id: &str, version: &str, deps: &[Dep]) {
    let identity = PackageIdentity::new(self::id(id), self::version(version));
    let paths = PackagePaths::new(root, &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.nuspec_path, nuspec(id, version, deps)).expect("nuspec");
}

fn nuspec(id: &str, version: &str, deps: &[Dep]) -> String {
    let dependencies = if deps.is_empty() {
        String::new()
    } else {
        let deps = deps
            .iter()
            .map(|dep| {
                let include = dep
                    .include
                    .as_ref()
                    .map(|value| format!(r#" include="{value}""#))
                    .unwrap_or_default();
                let exclude = dep
                    .exclude
                    .as_ref()
                    .map(|value| format!(r#" exclude="{value}""#))
                    .unwrap_or_default();
                format!(
                    r#"        <dependency id="{}" version="{}"{}{} />"#,
                    dep.id, dep.version, include, exclude
                )
            })
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
    <version>{version}</version>
    <authors>a</authors>
    <description>d</description>{dependencies}
  </metadata>
</package>
"#
    )
}

fn dep(id: &str, version: &str) -> Dep {
    Dep {
        id: id.to_owned(),
        version: version.to_owned(),
        include: None,
        exclude: None,
    }
}

fn package_ids(result: &borzoi_nuget::ResolvedPackageClosure) -> Vec<String> {
    result
        .packages
        .iter()
        .map(|package| package.identity.id.as_str().to_owned())
        .collect()
}

#[test]
fn non_envelope_project_frameworks_decline_before_resolution() {
    let root = tempfile::tempdir().expect("root");

    for tfm in ["any", "agnostic", "windowsphone81"] {
        let err = resolve_offline(root.path(), &framework(tfm), &[])
            .expect_err("resolver must decline project TFMs outside its exact envelope");

        assert!(
            matches!(err, ResolveDecline::UnsupportedProjectFramework { .. }),
            "unexpected error for {tfm:?}: {err:?}"
        );
    }
}

#[test]
fn resolves_direct_package_from_supplied_global_packages_root() {
    let unused_root = tempfile::tempdir().expect("unused root");
    let actual_root = tempfile::tempdir().expect("actual root");
    write_package(actual_root.path(), "Alpha", "1.0.0", &[]);

    let err = resolve_offline(
        unused_root.path(),
        &framework("net8.0"),
        &[req("Alpha", "1.0.0")],
    )
    .expect_err("package is absent from the supplied root");

    assert!(
        matches!(
            &err,
            ResolveDecline::PackageRead { source, .. }
                if matches!(source.as_ref(), PackageReadError::NotInstalled { .. })
        ),
        "unexpected error: {err:?}"
    );

    let result = resolve_offline(
        actual_root.path(),
        &framework("net8.0"),
        &[req("Alpha", "1.0.0")],
    )
    .expect("package resolves from explicit root");

    assert_eq!(package_ids(&result), ["Alpha"]);
}

#[test]
fn walks_transitive_dependency_groups_for_target_framework() {
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Beta", "2.0.0")]);
    write_package(root.path(), "Beta", "2.0.0", &[]);

    let result = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.0.0")])
        .expect("closure resolves");

    assert_eq!(package_ids(&result), ["Alpha", "Beta"]);
}

#[test]
fn exact_lower_bound_must_be_committed_even_when_higher_version_exists() {
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "2.0.0", &[]);

    let err = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.0.0")])
        .expect_err("resolver must not substitute a nearby installed version");

    assert!(
        matches!(
            &err,
            ResolveDecline::PackageRead { identity, source }
                if identity.id == id("Alpha")
                    && identity.version.eq_strict(&version("1.0.0"))
                    && matches!(source.as_ref(), PackageReadError::NotInstalled { .. })
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn uncommitted_exact_lower_bound_declines() {
    let root = tempfile::tempdir().expect("root");
    write_uncommitted_package(root.path(), "Alpha", "1.0.0", &[]);

    let err = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.0.0")])
        .expect_err("uncommitted package cannot be trusted");

    assert!(
        matches!(
            &err,
            ResolveDecline::PackageRead { source, .. }
                if matches!(source.as_ref(), PackageReadError::NotInstalled { .. })
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn floating_and_open_ranges_decline() {
    let root = tempfile::tempdir().expect("root");

    let floating = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.*")])
        .expect_err("floating versions need feed state");
    assert!(
        matches!(floating, ResolveDecline::FloatingRange { .. }),
        "unexpected error: {floating:?}"
    );

    let open = resolve_offline(
        root.path(),
        &framework("net8.0"),
        &[req("Alpha", "(, 2.0]")],
    )
    .expect_err("open lower bounds need candidate selection");
    assert!(
        matches!(open, ResolveDecline::OpenLowerBound { .. }),
        "unexpected error: {open:?}"
    );

    let exclusive = resolve_offline(
        root.path(),
        &framework("net8.0"),
        &[req("Alpha", "(1.0, )")],
    )
    .expect_err("exclusive lower bounds need candidate selection");
    assert!(
        matches!(exclusive, ResolveDecline::ExclusiveLowerBound { .. }),
        "unexpected error: {exclusive:?}"
    );
}

/// Cousins merge upward: two edges reaching Gamma from neither-is-an-ancestor
/// positions both survive, and the higher one wins.
///
/// The *losing* version has to be on disk, which is subtler than it looks. We
/// never read it — a version restore rejects is one it never looks inside — but
/// its presence is what tells us restore rejected it at all. Absent, restore may
/// instead have found nothing below 2.0.0 on the feed and bumped the losing edge
/// up to the winner, and a bumped edge brings the winner's dependencies along
/// *its own* path, where the ancestors eclipse differently. See
/// `a_losing_path_does_not_contribute_its_winners_dependencies`.
#[test]
fn cousin_requirements_merge_to_the_higher_version() {
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Gamma", "1.0.0")]);
    write_package(root.path(), "Beta", "1.0.0", &[dep("Gamma", "2.0.0")]);
    write_package(root.path(), "Gamma", "1.0.0", &[]);
    write_package(root.path(), "Gamma", "2.0.0", &[]);

    let closure = resolve_offline(
        root.path(),
        &framework("net8.0"),
        &[req("Alpha", "1.0.0"), req("Beta", "1.0.0")],
    )
    .expect("cousins merge to the greater lower bound");

    assert_eq!(
        closure
            .packages
            .iter()
            .map(|package| (
                package.identity.id.as_str().to_owned(),
                package.identity.version.to_normalized_string()
            ))
            .collect::<Vec<_>>(),
        [
            ("Alpha".to_owned(), "1.0.0".to_owned()),
            ("Beta".to_owned(), "1.0.0".to_owned()),
            ("Gamma".to_owned(), "2.0.0".to_owned()),
        ]
    );
}

/// A cousin merge only works when the loser's range *accepts* the winner. An
/// exact pin that does not is NU1107, and restore fails outright.
#[test]
fn cousins_that_cannot_agree_are_a_version_conflict() {
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Gamma", "[1.0.0]")]);
    write_package(root.path(), "Beta", "1.0.0", &[dep("Gamma", "[2.0.0]")]);
    write_package(root.path(), "Gamma", "1.0.0", &[]);
    write_package(root.path(), "Gamma", "2.0.0", &[]);

    let err = resolve_offline(
        root.path(),
        &framework("net8.0"),
        &[req("Alpha", "1.0.0"), req("Beta", "1.0.0")],
    )
    .expect_err("restore fails this graph with NU1107");

    assert!(
        matches!(
            &err,
            ResolveDecline::VersionConflict { id, .. } if id == &self::id("Gamma")
        ),
        "unexpected error: {err:?}"
    );
}

/// Nearest wins: a direct dependency overrides a transitive one outright, and
/// the transitive edge is dropped rather than merged — so Gamma 3.0.0 is
/// selected even though nothing else asked for it.
#[test]
fn a_direct_dependency_eclipses_a_deeper_one() {
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Gamma", "1.0.0")]);
    write_package(root.path(), "Gamma", "3.0.0", &[]);

    let closure = resolve_offline(
        root.path(),
        &framework("net8.0"),
        &[req("Alpha", "1.0.0"), req("Gamma", "3.0.0")],
    )
    .expect("the direct edge eclipses the transitive one");

    assert_eq!(
        closure
            .packages
            .iter()
            .map(|package| (
                package.identity.id.as_str().to_owned(),
                package.identity.version.to_normalized_string()
            ))
            .collect::<Vec<_>>(),
        [
            ("Alpha".to_owned(), "1.0.0".to_owned()),
            ("Gamma".to_owned(), "3.0.0".to_owned()),
        ]
    );
}

/// The other side of nearest-wins: when the nearer edge is *lower* than the
/// deeper one needs, restore reports NU1605 and fails.
#[test]
fn a_direct_dependency_below_a_transitive_one_is_a_downgrade() {
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Gamma", "2.0.0")]);
    write_package(root.path(), "Gamma", "1.0.0", &[]);
    write_package(root.path(), "Gamma", "2.0.0", &[]);

    let err = resolve_offline(
        root.path(),
        &framework("net8.0"),
        &[req("Alpha", "1.0.0"), req("Gamma", "1.0.0")],
    )
    .expect_err("restore fails this graph with NU1605");

    assert!(
        matches!(
            &err,
            ResolveDecline::Downgrade { id, .. } if id == &self::id("Gamma")
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn dependency_asset_filters_decline_until_asset_semantics_land() {
    let root = tempfile::tempdir().expect("root");
    write_package(
        root.path(),
        "Alpha",
        "1.0.0",
        &[Dep {
            id: "Beta".to_owned(),
            version: "2.0.0".to_owned(),
            include: Some("compile".to_owned()),
            exclude: None,
        }],
    );
    write_package(root.path(), "Beta", "2.0.0", &[]);

    let err = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.0.0")])
        .expect_err("include/exclude asset lists are not interpreted in 6a");

    assert!(
        matches!(
            &err,
            ResolveDecline::DependencyAssetFilterUnsupported {
                package,
                dependency,
            } if package.id == self::id("Alpha") && dependency == &self::id("Beta")
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn dependency_cycles_decline() {
    // `dotnet restore` fails a dependency cycle (NU1108), so resolving one
    // would be an over-resolution; the resolver must decline. (The end-to-end
    // differential against real NuGet lives in `resolver_diff.rs`.)
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Beta", "1.0.0")]);
    write_package(root.path(), "Beta", "1.0.0", &[dep("Alpha", "1.0.0")]);

    let err = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.0.0")])
        .expect_err("a dependency cycle must not resolve");
    assert!(
        matches!(&err, ResolveDecline::DependencyCycle { cycle } if cycle.len() >= 2),
        "unexpected error: {err:?}"
    );

    // A self-cycle is a cycle too.
    let root = tempfile::tempdir().expect("root");
    write_package(root.path(), "Alpha", "1.0.0", &[dep("Alpha", "1.0.0")]);
    let err = resolve_offline(root.path(), &framework("net8.0"), &[req("Alpha", "1.0.0")])
        .expect_err("a self-cycle must not resolve");
    assert!(
        matches!(&err, ResolveDecline::DependencyCycle { .. }),
        "unexpected error: {err:?}"
    );
}

fn acyclic_edges() -> impl Strategy<Value = (usize, Vec<Vec<usize>>)> {
    (1usize..=6, proptest::collection::vec(any::<u16>(), 6)).prop_map(|(count, masks)| {
        let mut edges = vec![Vec::new(); count];
        for i in 0..count {
            for j in (i + 1)..count {
                if (masks[i] & (1 << j)) != 0 {
                    edges[i].push(j);
                }
            }
        }
        (count, edges)
    })
}

fn reachable_from_root(edges: &[Vec<usize>]) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![0usize];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        stack.extend(edges[node].iter().copied());
    }
    seen
}

proptest! {
    #[test]
    fn resolves_generated_acyclic_no_conflict_graphs((count, edges) in acyclic_edges()) {
        let root = tempfile::tempdir().expect("root");
        for (node, _) in edges.iter().enumerate().take(count) {
            let deps = edges[node]
                .iter()
                .map(|target| dep(&format!("P{target}"), &format!("{}.0.0", target + 1)))
                .collect::<Vec<_>>();
            write_package(root.path(), &format!("P{node}"), &format!("{}.0.0", node + 1), &deps);
        }

        let result = resolve_offline(
            root.path(),
            &framework("net8.0"),
            &[req("P0", "1.0.0")],
        )
        .expect("generated graph is acyclic and version-consistent");

        let actual = result
            .packages
            .iter()
            .map(|package| package.identity.id.as_str().to_owned())
            .collect::<Vec<_>>();
        let expected = reachable_from_root(&edges)
            .into_iter()
            .map(|node| format!("P{node}"))
            .collect::<Vec<_>>();
        prop_assert_eq!(actual, expected);
    }
}
