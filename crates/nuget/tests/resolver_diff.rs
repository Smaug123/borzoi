//! End-to-end differential oracle for [`resolve_offline`].
//!
//! The per-primitive oracle ops (`parseVersion`, `parseRange`,
//! `selectDependencyGroup`, …) pin the pieces; this file pins the *whole*
//! offline resolve against the genuine PackageReference restore engine — the
//! `resolve` op drives `RemoteDependencyWalker` + `GraphOperations.Analyze`,
//! exactly what `dotnet restore` runs for an SDK-style project.
//!
//! The correctness policy (`docs/nuget-restore-plan.md`) is "resolve
//! identically or degrade": whenever `resolve_offline` returns a closure it
//! must be *exactly* the closure restore would produce, and otherwise it may
//! decline. So the load-bearing invariant here is **soundness**:
//!
//!   `resolve_offline` returns `Ok(S)`  ⟹  restore also succeeds, with the
//!                                          same package set `S`.
//!
//! A decline is always permitted (we under-resolve, never mis-resolve), so the
//! `Err` branch asserts nothing against the oracle. A second, narrower sweep
//! (`completeness_on_consistent_acyclic_envelope`) additionally requires that
//! we *do* resolve on the version-consistent, acyclic, fully-committed graphs
//! that sit squarely inside the current envelope — extending the naive
//! reachability proptest in `resolver.rs` with case-insensitive identity and
//! multi-TFM dependency-group selection, now checked against real NuGet.
//!
//! The same nuspec string feeds both sides: the on-disk warm cache that
//! `resolve_offline` reads and the oracle request, so the two can never
//! silently disagree about a package's declared dependencies.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use borzoi_nuget::{
    DirectPackageRequirement, NuGetFramework, NuGetVersion, PackageId, PackageIdentity,
    PackagePaths, VersionRange, resolve_offline,
};
use common::{Oracle, SplitMix64};
use serde_json::json;

// ============================================================================
// Abstract graph model — rendered once to nuspec, then to both the on-disk
// cache and the oracle request.
// ============================================================================

#[derive(Debug, Clone)]
struct Dep {
    id: String,
    range: String,
    include: Option<String>,
    exclude: Option<String>,
}

impl Dep {
    fn new(id: &str, range: &str) -> Dep {
        Dep {
            id: id.to_owned(),
            range: range.to_owned(),
            include: None,
            exclude: None,
        }
    }
}

/// One dependency group. `tfm: None` renders a `<group>` with no
/// `targetFramework` (the "Any" group); `Some(t)` renders `targetFramework="t"`.
#[derive(Debug, Clone)]
struct Group {
    tfm: Option<String>,
    deps: Vec<Dep>,
}

#[derive(Debug, Clone)]
struct Pkg {
    id: String,
    version: String,
    groups: Vec<Group>,
    /// Whether to write the `.nupkg.metadata` commit marker. An uncommitted
    /// package is invisible to a correct reader, so it is excluded from the
    /// oracle universe as well.
    committed: bool,
}

impl Pkg {
    fn simple(id: &str, version: &str, deps: Vec<Dep>) -> Pkg {
        Pkg {
            id: id.to_owned(),
            version: version.to_owned(),
            groups: vec![Group {
                tfm: Some("net8.0".to_owned()),
                deps,
            }],
            committed: true,
        }
    }

    fn nuspec(&self) -> String {
        let groups = self
            .groups
            .iter()
            .map(|group| {
                let deps = group
                    .deps
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
                            r#"      <dependency id="{}" version="{}"{include}{exclude} />"#,
                            dep.id, dep.range
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let tfm = group
                    .tfm
                    .as_ref()
                    .map(|value| format!(r#" targetFramework="{value}""#))
                    .unwrap_or_default();
                format!("    <group{tfm}>\n{deps}\n    </group>")
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{}</id>
    <version>{}</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
{groups}
    </dependencies>
  </metadata>
</package>
"#,
            self.id, self.version
        )
    }
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

fn req(id_: &str, range_: &str) -> DirectPackageRequirement {
    DirectPackageRequirement::new(id(id_), range(range_))
}

/// Materialise the committed packages into a warm-cache layout and return the
/// oracle request's `packages` array (the committed subset the oracle should
/// treat as available).
fn materialize(root: &Path, packages: &[Pkg]) -> Vec<serde_json::Value> {
    let mut universe = Vec::new();
    for pkg in packages {
        let identity = PackageIdentity::new(id(&pkg.id), version(&pkg.version));
        let paths = PackagePaths::new(root, &identity);
        fs::create_dir_all(&paths.package_dir).expect("package dir");
        let nuspec = pkg.nuspec();
        fs::write(&paths.nuspec_path, &nuspec).expect("nuspec");
        if pkg.committed {
            fs::write(&paths.metadata_path, "{}").expect("commit marker");
            universe.push(json!({
                "id": pkg.id,
                "version": pkg.version,
                "nuspec": nuspec,
            }));
        }
    }
    universe
}

/// The resolved closure as a comparable set: `(lowercased id, normalised
/// version)`, the same shape the oracle reports.
fn closure_set(closure: &borzoi_nuget::ResolvedPackageClosure) -> BTreeSet<(String, String)> {
    closure
        .packages
        .iter()
        .map(|package| {
            (
                package.identity.id.as_str().to_ascii_lowercase(),
                package.identity.version.to_normalized_string(),
            )
        })
        .collect()
}

fn oracle_set(oracle_packages: &serde_json::Value) -> BTreeSet<(String, String)> {
    oracle_packages
        .as_array()
        .expect("oracle packages array")
        .iter()
        .map(|package| {
            (
                package["id"].as_str().expect("id").to_owned(),
                package["version"].as_str().expect("version").to_owned(),
            )
        })
        .collect()
}

/// The soundness check: run both sides over the same graph and require that a
/// resolved closure exactly matches restore's. A decline asserts nothing.
///
/// Returns the pair `(resolved_here, oracle_resolved)` so callers writing
/// hand scenarios can additionally assert the *expected* branch was taken.
fn assert_sound(
    oracle: &mut Oracle,
    tfm: &str,
    packages: &[Pkg],
    direct: &[(&str, &str)],
) -> (bool, bool) {
    let root = tempfile::tempdir().expect("root");
    let universe = materialize(root.path(), packages);

    let direct_reqs = direct
        .iter()
        .map(|(id_, range_)| req(id_, range_))
        .collect::<Vec<_>>();
    let direct_json = direct
        .iter()
        .map(|(id_, range_)| json!({ "id": id_, "range": range_ }))
        .collect::<Vec<_>>();

    let response = oracle.request(&json!({
        "op": "resolve",
        "framework": tfm,
        "packages": universe,
        "direct": direct_json,
    }));
    let oracle_resolved = response["resolved"]
        .as_bool()
        .expect("oracle resolved flag");

    let rust = resolve_offline(root.path(), &framework(tfm), &direct_reqs);

    match &rust {
        Ok(closure) => {
            assert!(
                oracle_resolved,
                "resolve_offline produced a closure but `dotnet restore` would fail \
                 (reason {:?}); over-resolution violates the correctness policy.\n\
                 direct={direct:?}\nclosure={:?}",
                response["reason"],
                closure_set(closure),
            );
            assert_eq!(
                closure_set(closure),
                oracle_set(&response["packages"]),
                "resolved closure differs from `dotnet restore`.\ndirect={direct:?}",
            );
        }
        Err(_) => {
            // Declining is always sound: we under-resolve, never mis-resolve.
        }
    }

    (rust.is_ok(), oracle_resolved)
}

// ============================================================================
// Hand-written scenario anchors (one per named gap).
// ============================================================================

#[test]
fn linear_chain_resolves_identically() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("Alpha", "1.0.0", vec![Dep::new("Beta", "[2.0.0, )")]),
            Pkg::simple("Beta", "2.0.0", vec![]),
        ],
        &[("Alpha", "[1.0.0, )")],
    );
    assert!(
        rust && oracle_ok,
        "linear chain should resolve on both sides"
    );
}

/// A→G[1.0,) and B→G[2.0,): the cousin edges merge upward and G resolves to
/// 2.0 — note that G 1.0 is *not even on disk*, because restore resolved the
/// losing edge against a feed and then discarded it. The whole reason the
/// selected version is the greatest *lower bound* rather than the lowest
/// satisfying feed version is that the latter is unknowable here, and unneeded.
#[test]
fn cousin_open_ranges_merge_upward() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert!(rust, "cousins merge to G 2.0.0");
    assert!(
        oracle_ok,
        "restore merges the open cousin ranges to G 2.0.0"
    );
}

#[test]
fn cousin_exact_conflict_fails_on_both_sides() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0]")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0]")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert!(!rust, "we decline the version conflict");
    assert!(
        !oracle_ok,
        "restore fails the exact-version conflict (NU1107)"
    );
}

#[test]
fn dependency_cycle_must_not_resolve() {
    // `dotnet restore` fails a dependency cycle with NU1108, so producing any
    // closure over one is an over-resolution. `assert_sound` enforces it.
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("B", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("A", "[1.0.0, )")]),
        ],
        &[("A", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "restore rejects the cycle");
    assert!(
        !rust,
        "resolve_offline must decline the cycle, not resolve it"
    );
}

#[test]
fn cycle_confined_to_rejected_branch_still_resolves() {
    // A→G[1.0,)→H→G[1.0,) is a cycle, but B→G[2.0,) makes restore pick G 2.0
    // and reject the whole G 1.0 branch (cycle included), so restore succeeds
    // with {A,B,G 2.0}. A graph-wide cycle count would wrongly report `cycle`.
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![Dep::new("H", "[1.0.0, )")]),
            Pkg::simple("H", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert!(
        rust,
        "G resolves to 2.0, so G 1.0's branch — cycle and all — is never walked"
    );
    assert!(
        oracle_ok,
        "restore resolves G to 2.0; the rejected G 1.0's cycle is irrelevant"
    );
}

#[test]
fn conflict_confined_to_rejected_branch_still_resolves() {
    // B→G[2.0,) makes restore reject the whole G 1.0 branch, so the
    // unresolvable K conflict inside G 1.0's subtree never bites — restore
    // succeeds with {A,B,G 2.0}. Verified against a real `dotnet restore`.
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple(
                "G",
                "1.0.0",
                vec![Dep::new("K", "[1.0.0]"), Dep::new("L", "[1.0.0, )")],
            ),
            Pkg::simple("L", "1.0.0", vec![Dep::new("K", "[2.0.0]")]),
            Pkg::simple("K", "1.0.0", vec![]),
            Pkg::simple("K", "2.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert!(
        rust,
        "G resolves to 2.0, so the K conflict inside G 1.0's branch is never seen"
    );
    assert!(
        oracle_ok,
        "restore resolves G to 2.0; the rejected branch's K conflict is irrelevant"
    );
}

#[test]
fn self_cycle_must_not_resolve() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[Pkg::simple("A", "1.0.0", vec![Dep::new("A", "[1.0.0, )")])],
        &[("A", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "restore rejects the self-cycle");
    assert!(!rust, "resolve_offline must decline the self-cycle");
}

#[test]
fn direct_downgrade_fails_on_both_sides() {
    // root→A[1.0,)→G[2.0,) and root→G[1.0,): the nearer direct G 1.0 downgrades
    // the transitive G 2.0 → restore fails NU1605.
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("G", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "restore fails the downgrade (NU1605)");
    assert!(!rust, "we decline the version conflict");
}

#[test]
fn case_insensitive_identity_resolves_identically() {
    // Direct `alpha`, package id `Alpha`, transitive `BETA`/`Beta`: NuGet ids
    // are case-insensitive, so this is one linear chain, not four packages.
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("Alpha", "1.0.0", vec![Dep::new("BETA", "[1.0.0, )")]),
            Pkg::simple("Beta", "1.0.0", vec![]),
        ],
        &[("alpha", "[1.0.0, )")],
    );
    assert!(
        rust && oracle_ok,
        "case-insensitive chain resolves on both sides"
    );
}

#[test]
fn multi_tfm_group_selection_resolves_identically() {
    // A ships a net6.0 group (→X) and a netstandard2.0 group (→Y); a net8.0
    // project selects the net6.0 group, so the closure is {A, X}, not {A, Y}.
    let a = Pkg {
        id: "A".to_owned(),
        version: "1.0.0".to_owned(),
        groups: vec![
            Group {
                tfm: Some("net6.0".to_owned()),
                deps: vec![Dep::new("X", "[1.0.0, )")],
            },
            Group {
                tfm: Some("netstandard2.0".to_owned()),
                deps: vec![Dep::new("Y", "[1.0.0, )")],
            },
        ],
        committed: true,
    };
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            a,
            Pkg::simple("X", "1.0.0", vec![]),
            Pkg::simple("Y", "1.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )")],
    );
    assert!(
        rust && oracle_ok,
        "multi-TFM selection resolves on both sides"
    );
}

#[test]
fn dependency_asset_filter_we_decline_restore_resolves() {
    let a = Pkg {
        id: "A".to_owned(),
        version: "1.0.0".to_owned(),
        groups: vec![Group {
            tfm: Some("net8.0".to_owned()),
            deps: vec![Dep {
                id: "Beta".to_owned(),
                range: "[2.0.0, )".to_owned(),
                include: Some("compile".to_owned()),
                exclude: None,
            }],
        }],
        committed: true,
    };
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[a, Pkg::simple("Beta", "2.0.0", vec![])],
        &[("A", "[1.0.0, )")],
    );
    assert!(!rust, "asset filters are unsupported in 6a");
    assert!(
        oracle_ok,
        "restore ignores asset filters for the version set"
    );
}

#[test]
fn rejected_branch_missing_dependency_does_not_fail_restore() {
    // A→G[1.0,) and B→G[2.0,): restore merges G to 2.0 and *rejects* G 1.0, so
    // G 1.0's missing dependency dangles off a rejected branch and never
    // reaches the output — restore still succeeds. The oracle must not report
    // `missing` here (a graph-wide "any unresolved node" scan would).
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![Dep::new("Missing", "[1.0.0, )")]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert!(
        rust,
        "G resolves to 2.0, so G 1.0's missing dependency is never looked for"
    );
    assert!(
        oracle_ok,
        "restore resolves G to 2.0; the rejected G 1.0's missing dep is irrelevant"
    );
}

#[test]
fn synthetic_root_id_collision_is_handled() {
    // A universe package named exactly like the oracle's synthetic-root
    // sentinel must not overwrite the root nupkg — the oracle picks a
    // collision-free root id, so this resolves normally on both sides.
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple(
                "__oracle_root__",
                "1.0.0",
                vec![Dep::new("Beta", "[1.0.0, )")],
            ),
            Pkg::simple("Beta", "1.0.0", vec![]),
        ],
        &[("__oracle_root__", "[1.0.0, )")],
    );
    assert!(
        rust && oracle_ok,
        "the sentinel-named package resolves normally"
    );
}

#[test]
fn synthetic_root_id_collision_with_absent_direct_is_missing() {
    // A direct requirement names the sentinel but no such package is committed:
    // both sides must report "not found", not a self-dependency cycle (the root
    // id must be chosen clear of direct ids, not just universe ids).
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[Pkg::simple("Beta", "1.0.0", vec![])],
        &[("__oracle_root__", "[1.0.0, )")],
    );
    assert!(
        !oracle_ok,
        "the sentinel package is not installed → missing"
    );
    assert!(!rust, "we decline the missing package read");
}

#[test]
fn missing_transitive_dependency_fails_on_both_sides() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[Pkg::simple(
            "A",
            "1.0.0",
            vec![Dep::new("Gone", "[1.0.0, )")],
        )],
        &[("A", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "restore cannot find `Gone` (NU1101)");
    assert!(!rust, "we decline the missing package read");
}

#[test]
fn uncommitted_package_is_invisible_to_both() {
    // A depends on Beta, but Beta lacks the commit marker: our reader treats it
    // as not installed, and the oracle universe excludes it → both fail.
    let mut oracle = Oracle::spawn();
    let beta = Pkg {
        committed: false,
        ..Pkg::simple("Beta", "2.0.0", vec![])
    };
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("Beta", "[2.0.0, )")]),
            beta,
        ],
        &[("A", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "uncommitted Beta is not an available package");
    assert!(!rust, "we decline the uncommitted package read");
}

// ============================================================================
// Randomised soundness sweep — the workhorse. Generates graphs biased towards
// the hard features (cycles, cousins, case variation, multi-TFM, asset
// filters, uncommitted entries) and asserts soundness on every one.
// ============================================================================

/// A generated graph: the package set, the direct roots, and the project TFM.
struct Generated {
    tfm: String,
    packages: Vec<Pkg>,
    direct: Vec<(String, String)>,
}

fn gen_range(rng: &mut SplitMix64, target: usize) -> String {
    // Bias hard towards inclusive-lower `[x, )` (inside the envelope, so the
    // Ok branch actually fires), with a minority of shapes that force declines
    // or drive cousin/conflict behaviour on the oracle side.
    let v = format!("{}.0.0", target + 1);
    match rng.below(10) {
        0 => format!("[{v}]"),       // exact pin
        1 => "(1.0.0, )".to_owned(), // exclusive lower — we decline
        2 => "*".to_owned(),         // floating — we decline
        _ => format!("[{v}, )"),     // inclusive lower — envelope
    }
}

fn maybe_recase(rng: &mut SplitMix64, s: &str) -> String {
    if rng.below(4) == 0 {
        s.chars()
            .map(|c| {
                if rng.below(2) == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    } else {
        s.to_owned()
    }
}

fn generate(rng: &mut SplitMix64) -> Generated {
    let tfm = if rng.below(4) == 0 {
        "netstandard2.0"
    } else {
        "net8.0"
    };
    let count = 1 + rng.below(6);

    let mut packages = Vec::new();
    for node in 0..count {
        let name = format!("P{node}");
        let version = format!("{}.0.0", node + 1);

        // Random edges to any node (self and back-edges allowed → cycles).
        let mut deps = Vec::new();
        for target in 0..count {
            if rng.below(3) == 0 {
                let mut dep = Dep::new(
                    &maybe_recase(rng, &format!("P{target}")),
                    &gen_range(rng, target),
                );
                // Occasionally attach an asset filter (we must then decline).
                if rng.below(12) == 0 {
                    dep.include = Some("compile".to_owned());
                }
                deps.push(dep);
            }
        }

        // Occasionally a second dependency group on a different TFM. The nearest
        // group for the project TFM is what both sides must agree on.
        let groups = if rng.below(5) == 0 {
            vec![
                Group {
                    tfm: Some("net6.0".to_owned()),
                    deps: deps.clone(),
                },
                Group {
                    tfm: Some("netstandard2.0".to_owned()),
                    deps: vec![],
                },
            ]
        } else {
            vec![Group {
                tfm: Some(tfm.to_owned()),
                deps,
            }]
        };

        packages.push(Pkg {
            id: name,
            version,
            groups,
            // A small fraction of packages are left uncommitted.
            committed: rng.below(8) != 0,
        });
    }

    // One or two direct roots, referenced by inclusive lower bound.
    let root_count = 1 + rng.below(2);
    let mut direct = Vec::new();
    for _ in 0..root_count {
        let target = rng.below(count);
        direct.push((
            maybe_recase(rng, &format!("P{target}")),
            format!("[{}.0.0, )", target + 1),
        ));
    }

    Generated {
        tfm: tfm.to_owned(),
        packages,
        direct,
    }
}

#[test]
fn randomised_soundness_sweep() {
    let mut oracle = Oracle::spawn();
    // A handful of fixed seeds; each generates many graphs. Fixed so a failure
    // reproduces exactly (the ignored soak below re-rolls for fresh coverage).
    for seed in [0x5eed_u64, 0xC0FFEE, 0x1234_5678, 0xABCD] {
        let mut rng = SplitMix64(seed);
        for _ in 0..60 {
            let g = generate(&mut rng);
            let direct = g
                .direct
                .iter()
                .map(|(id_, range_)| (id_.as_str(), range_.as_str()))
                .collect::<Vec<_>>();
            assert_sound(&mut oracle, &g.tfm, &g.packages, &direct);
        }
    }
}

/// Deeper fresh-seed exploration; `#[ignore]`d because each graph runs a real
/// restore walk. Run with `--ignored --nocapture` when hunting for divergences
/// the fixed-seed sweep didn't reach.
#[test]
#[ignore]
fn randomised_soundness_soak() {
    let mut oracle = Oracle::spawn();
    for seed in 0..40u64 {
        let mut rng = SplitMix64(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1));
        for _ in 0..80 {
            // Both shapes: the single-version graphs the earlier slices swept,
            // and the multi-version ones whose conflicts slice 6b resolves.
            let g = if rng.below(2) == 0 {
                generate(&mut rng)
            } else {
                generate_multi_version(&mut rng)
            };
            let direct = g
                .direct
                .iter()
                .map(|(id_, range_)| (id_.as_str(), range_.as_str()))
                .collect::<Vec<_>>();
            assert_sound(&mut oracle, &g.tfm, &g.packages, &direct);
        }
    }
}

// ============================================================================
// Completeness on the version-consistent, acyclic, committed sub-envelope.
// Here we must *not* decline: both sides resolve, to the identical closure.
// ============================================================================

/// Acyclic (edges only point to higher indices), one committed version per id,
/// every reference an inclusive lower bound at the committed version. Adds
/// case-variation and optional multi-TFM groups on top of the naive
/// reachability proptest in `resolver.rs`.
fn generate_consistent(rng: &mut SplitMix64) -> Generated {
    let count = 1 + rng.below(6);
    let mut packages = Vec::new();
    for node in 0..count {
        let mut deps = Vec::new();
        for target in (node + 1)..count {
            if rng.below(2) == 0 {
                deps.push(Dep::new(
                    &maybe_recase(rng, &format!("P{target}")),
                    &format!("[{}.0.0, )", target + 1),
                ));
            }
        }
        let groups = if rng.below(4) == 0 {
            vec![
                Group {
                    tfm: Some("net6.0".to_owned()),
                    deps,
                },
                Group {
                    tfm: Some("netstandard2.0".to_owned()),
                    deps: vec![],
                },
            ]
        } else {
            vec![Group {
                tfm: Some("net8.0".to_owned()),
                deps,
            }]
        };
        packages.push(Pkg {
            id: format!("P{node}"),
            version: format!("{}.0.0", node + 1),
            groups,
            committed: true,
        });
    }
    Generated {
        tfm: "net8.0".to_owned(),
        packages,
        direct: vec![(maybe_recase(rng, "P0"), "[1.0.0, )".to_owned())],
    }
}

#[test]
fn completeness_on_consistent_acyclic_envelope() {
    let mut oracle = Oracle::spawn();
    for seed in [0x11_u64, 0x22, 0x33, 0x44] {
        let mut rng = SplitMix64(seed);
        for _ in 0..60 {
            let g = generate_consistent(&mut rng);
            let direct = g
                .direct
                .iter()
                .map(|(id_, range_)| (id_.as_str(), range_.as_str()))
                .collect::<Vec<_>>();
            let (rust, oracle_ok) = assert_sound(&mut oracle, &g.tfm, &g.packages, &direct);
            assert!(
                rust,
                "consistent acyclic committed graph must resolve, not decline"
            );
            assert!(oracle_ok, "restore must resolve the consistent graph too");
        }
    }
}

// ============================================================================
// Multi-version graphs: the shapes slice 6b exists for
// ============================================================================

/// Several versions of one id, so that cousin edges disagree, direct edges
/// eclipse transitive ones, and nearer-but-lower edges are downgrades. Both of
/// the generators above put *one* version on each id, which means neither ever
/// produced a conflict to resolve — the whole of nearest-wins and cousin merging
/// went unexercised by the sweeps.
///
/// Every version is committed, so the oracle's synthetic feed and our cache agree
/// on what exists. That keeps the sweep pointed at the *resolution* semantics
/// rather than at the (already well covered) decline for a lower bound that is
/// not on disk — and the warm-cache reality, where the cache holds only the
/// winners, is what `a_resolved_closure_never_needs_the_versions_it_rejected`
/// exists to check.
fn generate_multi_version(rng: &mut SplitMix64) -> Generated {
    const VERSIONS: &[&str] = &["1.0.0", "2.0.0", "3.0.0"];

    let ids = 3 + rng.below(3);
    let mut packages = Vec::new();
    for node in 0..ids {
        // At least two versions most of the time: a package with a single
        // version can never *lose* a conflict, and the losing occurrence is
        // where the hard cases live.
        let version_count = if rng.below(4) == 0 {
            1
        } else {
            2 + rng.below(2)
        };
        for version in VERSIONS.iter().take(version_count) {
            let mut deps = Vec::new();
            for target in (node + 1)..ids {
                // Dense: an edge two thirds of the time, so that a package and
                // its parent often depend on the *same* deeper package — which
                // is what makes eclipsing, and its path-dependence, bite.
                if rng.below(3) != 0 {
                    deps.push(Dep::new(
                        &maybe_recase(rng, &format!("P{target}")),
                        &format!("[{}, )", rng.pick(VERSIONS)),
                    ));
                }
            }
            packages.push(Pkg {
                id: format!("P{node}"),
                version: (*version).to_owned(),
                groups: vec![Group {
                    tfm: Some("net8.0".to_owned()),
                    deps,
                }],
                committed: true,
            });
        }
    }

    // Two direct requirements at least: one parent cannot be another's cousin.
    let mut direct = Vec::new();
    for node in 0..(2 + rng.below(2)).min(ids) {
        direct.push((
            maybe_recase(rng, &format!("P{node}")),
            format!("[{}, )", rng.pick(VERSIONS)),
        ));
    }

    Generated {
        tfm: "net8.0".to_owned(),
        packages,
        direct,
    }
}

#[test]
fn multi_version_graphs_resolve_identically() {
    let mut oracle = Oracle::spawn();
    let mut resolved = 0usize;
    let mut restore_only = 0usize;
    let mut total = 0usize;

    for seed in [0x6b_u64, 0xBEEF, 0x0DDBA11, 0xFACE, 0xC0DE, 0x5EED] {
        let mut rng = SplitMix64(seed);
        for _ in 0..150 {
            let g = generate_multi_version(&mut rng);
            let direct = g
                .direct
                .iter()
                .map(|(id_, range_)| (id_.as_str(), range_.as_str()))
                .collect::<Vec<_>>();
            // `assert_sound` is the whole check: if we produce a closure it must
            // be restore's, exactly, and restore must have produced one at all.
            let (rust, oracle_ok) = assert_sound(&mut oracle, &g.tfm, &g.packages, &direct);
            total += 1;
            if rust {
                resolved += 1;
            } else if oracle_ok {
                restore_only += 1;
            }
        }
    }

    eprintln!("multi-version: {resolved}/{total} resolved, {restore_only} restore-only declines");

    // Completeness, stated as strongly as it goes: on graphs whose every version
    // is on disk, we resolve *everything restore resolves*. The rest of the
    // corpus is graphs restore itself fails — random ranges over several
    // versions conflict often — and there we must fail too, which `assert_sound`
    // has already checked.
    assert_eq!(
        restore_only, 0,
        "declined {restore_only} graph(s) that `dotnet restore` resolves"
    );
    // And a sweep that resolved nothing would satisfy that vacuously.
    assert!(
        resolved > 50,
        "generator degenerated: only {resolved}/{total} graphs resolved"
    );
}

/// What a resolved closure needs from the versions it *rejected*, stated exactly.
///
/// The design first claimed it needed nothing at all — that a loser's subtree is
/// rejected wholesale, so its version could be absent. That is half right, and
/// the half that is wrong is an over-resolution (see
/// `a_losing_path_does_not_contribute_its_winners_dependencies`): a losing
/// version's *presence* is what tells us restore rejected the edge rather than
/// bumping it up to the winner.
///
/// Its *contents*, though, are genuinely never read — restore does not look
/// inside a package it rejected either. So: resolve, delete the nuspec of every
/// version the closure does not name (leaving the commit marker), and resolve
/// again. The closure must be identical, and it must still *be* a closure.
#[test]
fn a_resolved_closure_never_reads_the_versions_it_rejected() {
    let mut checked = 0usize;

    for seed in [0x6b_c0_u64, 0xD00D, 0x5AFE, 0xFEED, 0xB0B5] {
        let mut rng = SplitMix64(seed);
        for _ in 0..120 {
            let g = generate_multi_version(&mut rng);
            let root = tempfile::tempdir().expect("root");
            materialize(root.path(), &g.packages);

            let direct = g
                .direct
                .iter()
                .map(|(id_, range_)| req(id_, range_))
                .collect::<Vec<_>>();

            let Ok(closure) = resolve_offline(root.path(), &framework(&g.tfm), &direct) else {
                continue;
            };
            let before = closure_set(&closure);

            for pkg in &g.packages {
                let key = (
                    pkg.id.to_ascii_lowercase(),
                    version(&pkg.version).to_normalized_string(),
                );
                if before.contains(&key) {
                    continue;
                }
                let identity = PackageIdentity::new(id(&pkg.id), version(&pkg.version));
                let paths = PackagePaths::new(root.path(), &identity);
                let _ = fs::remove_file(&paths.nuspec_path);
            }

            let after = resolve_offline(root.path(), &framework(&g.tfm), &direct)
                .expect("a rejected version's contents are never read");
            assert_eq!(
                before,
                closure_set(&after),
                "closure changed once the rejected versions' nuspecs left the cache"
            );
            checked += 1;
        }
    }

    assert!(
        checked > 40,
        "generator degenerated: only {checked} graphs resolved to prune"
    );
    eprintln!("stripped and re-resolved {checked} closures");
}

// ============================================================================
// The two over-resolutions review found, pinned
// ============================================================================

/// A losing occurrence must not contribute the *winner's* dependencies.
///
/// The resolver once expanded every occurrence of a package at that package's
/// settled version — reasoning that a loser's subtree is rejected wholesale, so
/// substituting the winner's could not matter. It can: the two occurrences sit
/// on different paths, and eclipsing is a property of the path. Here
/// `A → P[1]` loses to `B → P[2]`, and expanding it at P 2.0 gives it a `G[2]`
/// edge that is *acceptable under A* while the identical edge under B is a
/// downgrade (B pins `G[1]`). Restore fails this graph; we produced a closure.
#[test]
fn a_losing_path_does_not_contribute_its_winners_dependencies() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("P", "[1.0.0, )")]),
            Pkg::simple(
                "B",
                "1.0.0",
                vec![Dep::new("P", "[2.0.0, )"), Dep::new("G", "[1.0.0, )")],
            ),
            Pkg::simple("P", "1.0.0", vec![]),
            Pkg::simple("P", "2.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("B", "[1.0.0, )"), ("A", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "restore fails this graph with a downgrade");
    assert!(
        !rust,
        "and so must we: producing a closure here over-resolves"
    );
}

/// A settled version must be recomputed from the surviving edges, not carried
/// forward. `A → P[1]` and `B → P[2]` make P settle at 2.0, at which point P 1's
/// `G[3]` edge is gone and only P 2's `G[1]` remains — so G must *fall* to 1.0.
/// Carrying the previous round's map forward (which only ever rose) left G at
/// 3.0, a closure restore never produces.
#[test]
fn a_settled_version_falls_when_the_edge_that_raised_it_disappears() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("P", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("P", "[2.0.0, )")]),
            Pkg::simple("P", "1.0.0", vec![Dep::new("G", "[3.0.0, )")]),
            Pkg::simple("P", "2.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "3.0.0", vec![]),
        ],
        &[("B", "[1.0.0, )"), ("A", "[1.0.0, )")],
    );
    // `assert_sound` has already required our closure to equal restore's; this
    // additionally requires that we produced one at all.
    assert!(oracle_ok, "restore resolves this graph");
    assert!(rust, "and so must we, with G at 1.0.0 rather than 3.0.0");
}

/// A dependency shape we cannot model, on a version restore *rejects*, must not
/// decline: restore never looks inside a rejected package either. Before, the
/// walk raised immediately, so the answer depended on which direct requirement
/// the traversal happened to reach first.
#[test]
fn an_asset_filter_on_a_rejected_version_does_not_decline() {
    let mut oracle = Oracle::spawn();
    let mut filtered = Dep::new("G", "[1.0.0, )");
    filtered.include = Some("compile".to_owned());

    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("P", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("P", "[2.0.0, )")]),
            Pkg::simple("P", "1.0.0", vec![filtered]),
            Pkg::simple("P", "2.0.0", vec![]),
            Pkg::simple("G", "1.0.0", vec![]),
        ],
        &[("B", "[1.0.0, )"), ("A", "[1.0.0, )")],
    );
    assert!(oracle_ok, "restore rejects P 1.0 and resolves");
    assert!(
        rust,
        "P 1.0 is rejected, so its unmodelled asset filter is never read"
    );
}
