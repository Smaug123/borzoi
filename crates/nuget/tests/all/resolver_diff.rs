//! End-to-end differential oracle for [`resolve_offline`].
//!
//! The per-primitive oracle ops (`parseVersion`, `parseRange`,
//! `selectDependencyGroup`, …) pin the pieces; this file pins the *whole*
//! offline resolve against the genuine PackageReference restore engine — the
//! `restore` op runs a real restore through `RestoreRunner`, once with NuGet's
//! legacy dependency resolver (`RestoreUseLegacyDependencyResolver`) and once
//! with the .NET 10 SDK's default `DependencyGraphResolver`. The two engines
//! disagree on roughly one generated graph in ten, so the contract is stated
//! against both: a closure must be the one both engines write.
//!
//! The correctness policy (`docs/nuget-restore-plan.md`) is "resolve
//! identically or degrade": whenever `resolve_offline` returns a closure it
//! must be *exactly* the closure restore would produce, and otherwise it may
//! decline. So the load-bearing invariant here is **soundness**:
//!
//!   `resolve_offline` returns `Ok(S)`  ⟹  both engines succeed, each with
//!                                          the same package set `S`.
//!
//! A decline is always permitted (we under-resolve, never mis-resolve). The
//! only declines checked against the oracle are the three that name a legacy
//! engine error, which must be one the legacy engine reports. A second, narrower sweep
//! (`completeness_on_consistent_acyclic_envelope`) additionally requires that
//! we *do* resolve on the version-consistent, acyclic, fully-committed graphs
//! that sit squarely inside the current envelope — extending the naive
//! reachability proptest in `resolver.rs` with case-insensitive identity and
//! multi-TFM dependency-group selection, now checked against real NuGet.
//!
//! The same nuspec string feeds both sides: the on-disk warm cache that
//! `resolve_offline` reads and the oracle request, so the two can never
//! silently disagree about a package's declared dependencies.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::common::{Oracle, SplitMix64, gen_version_string};
use borzoi_nuget::{
    DirectPackageRequirement, NuGetFramework, NuGetVersion, PackageId, PackageIdentity,
    PackagePaths, ResolveDecline, VersionRange, resolve_offline,
};
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

/// One of the restore errors `resolve_offline`'s outcome classes are drawn
/// from, by its NuGet code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RestoreFailure {
    /// NU1101/NU1102/NU1103: a dependency has no package to resolve to.
    Missing,
    /// NU1108.
    Cycle,
    /// NU1106: conflicting requests the resolver could not settle.
    Undecided,
    /// NU1107.
    Conflict,
    /// NU1605, an error under the SDK's default `WarningsAsErrors`.
    Downgrade,
}

impl RestoreFailure {
    fn from_code(code: &str) -> RestoreFailure {
        match code {
            "NU1101" | "NU1102" | "NU1103" => RestoreFailure::Missing,
            "NU1108" => RestoreFailure::Cycle,
            "NU1106" => RestoreFailure::Undecided,
            "NU1107" => RestoreFailure::Conflict,
            "NU1605" => RestoreFailure::Downgrade,
            other => panic!("restore failed with {other}, which no outcome class covers"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            RestoreFailure::Missing => "missing",
            RestoreFailure::Cycle => "cycle",
            RestoreFailure::Undecided => "undecided",
            RestoreFailure::Conflict => "conflict",
            RestoreFailure::Downgrade => "downgrade",
        }
    }
}

/// What one restore engine did with a graph: the closure it wrote, or every
/// error it failed with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EngineOutcome {
    Resolved(BTreeSet<(String, String)>),
    Failed(BTreeSet<RestoreFailure>),
}

impl EngineOutcome {
    fn parse(response: &serde_json::Value) -> EngineOutcome {
        if response["resolved"]
            .as_bool()
            .expect("oracle resolved flag")
        {
            return EngineOutcome::Resolved(oracle_set(&response["packages"]));
        }
        let errors: BTreeSet<RestoreFailure> = response["errors"]
            .as_array()
            .expect("oracle error codes")
            .iter()
            .map(|code| RestoreFailure::from_code(code.as_str().expect("error code")))
            .collect();
        assert!(!errors.is_empty(), "a failed restore reports an error");
        EngineOutcome::Failed(errors)
    }

    /// One label for the census: `resolved`, or the failures joined.
    fn class(&self) -> String {
        match self {
            EngineOutcome::Resolved(_) => "resolved".to_owned(),
            EngineOutcome::Failed(failures) => failures
                .iter()
                .map(|failure| failure.name())
                .collect::<Vec<_>>()
                .join("+"),
        }
    }

    fn fails_with(&self, failure: RestoreFailure) -> bool {
        matches!(self, EngineOutcome::Failed(failures) if failures.contains(&failure))
    }
}

/// What `dotnet restore` did with a graph, under each of NuGet's two dependency
/// resolvers: the legacy one (`RestoreUseLegacyDependencyResolver`) and the
/// .NET 10 SDK's default.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreOutcome {
    legacy: EngineOutcome,
    default: EngineOutcome,
}

impl RestoreOutcome {
    fn engines(&self) -> [(&'static str, &EngineOutcome); 2] {
        [("legacy", &self.legacy), ("default", &self.default)]
    }

    /// The closure both engines write, if they write the same one.
    fn agreed_closure(&self) -> Option<&BTreeSet<(String, String)>> {
        match (&self.legacy, &self.default) {
            (EngineOutcome::Resolved(a), EngineOutcome::Resolved(b)) if a == b => Some(a),
            _ => None,
        }
    }
}

/// What `resolve_offline` did with a graph, split by what it *claims*.
///
/// A closure claims to be the closure both restore engines write. Three declines
/// claim more than "we cannot tell": that the *legacy* engine fails, and with
/// which error. They claim nothing about the default engine, which may resolve
/// the same graph or fail it differently. Every other decline claims nothing at
/// all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OurOutcome {
    Resolved(BTreeSet<(String, String)>),
    ClaimsLegacyFails(RestoreFailure),
    Declined(&'static str),
}

impl OurOutcome {
    fn of(result: &Result<borzoi_nuget::ResolvedPackageClosure, ResolveDecline>) -> OurOutcome {
        let decline = match result {
            Ok(closure) => return OurOutcome::Resolved(closure_set(closure)),
            Err(decline) => decline,
        };
        match decline {
            ResolveDecline::DependencyCycle { .. } => {
                OurOutcome::ClaimsLegacyFails(RestoreFailure::Cycle)
            }
            ResolveDecline::VersionConflict { .. } => {
                OurOutcome::ClaimsLegacyFails(RestoreFailure::Conflict)
            }
            ResolveDecline::Downgrade { .. } => {
                OurOutcome::ClaimsLegacyFails(RestoreFailure::Downgrade)
            }
            ResolveDecline::UnsupportedProjectFramework { .. } => {
                OurOutcome::Declined("unsupported-framework")
            }
            ResolveDecline::FloatingRange { .. } => OurOutcome::Declined("floating"),
            ResolveDecline::OpenLowerBound { .. } => OurOutcome::Declined("open-lower"),
            ResolveDecline::ExclusiveLowerBound { .. } => OurOutcome::Declined("exclusive-lower"),
            ResolveDecline::UnsatisfiedLowerBound { .. } => {
                OurOutcome::Declined("unsatisfied-lower")
            }
            ResolveDecline::PackageRead { .. } => OurOutcome::Declined("package-read"),
            ResolveDecline::DependencyAssetFilterUnsupported { .. } => {
                OurOutcome::Declined("asset-filter")
            }
            ResolveDecline::DependencyWithoutRange { .. } => OurOutcome::Declined("no-range"),
            ResolveDecline::UnresolvableLosingEdge { .. } => OurOutcome::Declined("losing-edge"),
            ResolveDecline::LosingVersionNotALeaf { .. } => {
                OurOutcome::Declined("loser-not-a-leaf")
            }
            ResolveDecline::TransitivePotentialDowngrade { .. } => {
                OurOutcome::Declined("transitive-potential-downgrade")
            }
            ResolveDecline::GraphTooLarge => OurOutcome::Declined("too-large"),
        }
    }

    fn class(&self) -> String {
        match self {
            OurOutcome::Resolved(_) => "resolved".to_owned(),
            OurOutcome::ClaimsLegacyFails(failure) => {
                format!("claims-legacy-{}", failure.name())
            }
            OurOutcome::Declined(why) => format!("declined-{why}"),
        }
    }
}

/// Both sides' outcome on one graph.
struct Comparison {
    ours: OurOutcome,
    restore: RestoreOutcome,
}

/// Render a graph in full, so a generated failure can be lifted straight into a
/// hand-written scenario.
fn describe(tfm: &str, packages: &[Pkg], direct: &[(&str, &str)]) -> String {
    let mut out = format!("tfm={tfm}\ndirect={direct:?}\n");
    for pkg in packages {
        let marker = if pkg.committed { "" } else { " (uncommitted)" };
        out.push_str(&format!("  {} {}{marker}\n", pkg.id, pkg.version));
        for group in &pkg.groups {
            let deps = group
                .deps
                .iter()
                .map(|dep| {
                    let filter = if dep.include.is_some() || dep.exclude.is_some() {
                        " +filter"
                    } else {
                        ""
                    };
                    format!("{} {}{filter}", dep.id, dep.range)
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("    [{:?}] {deps}\n", group.tfm));
        }
    }
    out
}

/// The differential: run `resolve_offline` and both restore engines over the
/// same graph and compare their outcome *classes*, not only their package sets.
///
/// - A closure must be the closure *both* engines write. Where either engine
///   fails, or the two write different closures, we must decline.
/// - A decline that claims the legacy engine fails (a cycle, conflict or
///   downgrade diagnosis) must be right: the legacy engine must fail, with that
///   error among the ones it reports. The claim says nothing about the default
///   engine, which the resolver does not model beyond knowing where it agrees.
/// - Any other decline makes no claim, and is always permitted.
fn compare(
    oracle: &mut Oracle,
    tfm: &str,
    packages: &[Pkg],
    direct: &[(&str, &str)],
) -> Comparison {
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

    let mut restore_with = |engine: &str| {
        EngineOutcome::parse(&oracle.request(&json!({
            "op": "restore",
            "engine": engine,
            "framework": tfm,
            "packages": universe,
            "direct": direct_json,
        })))
    };
    let restore = RestoreOutcome {
        legacy: restore_with("legacy"),
        default: restore_with("default"),
    };
    let result = resolve_offline(root.path(), &framework(tfm), &direct_reqs);
    let ours = OurOutcome::of(&result);

    for (engine, outcome) in restore.engines() {
        match (&ours, outcome) {
            (OurOutcome::Resolved(mine), EngineOutcome::Resolved(theirs)) => {
                assert_eq!(
                    mine,
                    theirs,
                    "resolved closure differs from `dotnet restore` ({engine} engine).\n{}",
                    describe(tfm, packages, direct),
                );
            }
            (OurOutcome::Resolved(mine), EngineOutcome::Failed(failures)) => {
                panic!(
                    "resolve_offline produced a closure but `dotnet restore` fails ({engine} \
                     engine: {failures:?}); over-resolution violates the correctness policy.\n\
                     closure={mine:?}\n{}",
                    describe(tfm, packages, direct),
                );
            }
            (OurOutcome::ClaimsLegacyFails(claim), theirs) if engine == "legacy" => {
                assert!(
                    theirs.fails_with(*claim),
                    "resolve_offline declined claiming the legacy engine fails with {claim:?}, \
                     but it {}.\ndecline={}\n{}",
                    match theirs {
                        EngineOutcome::Resolved(set) => format!("resolves to {set:?}"),
                        EngineOutcome::Failed(failures) => format!("fails with {failures:?}"),
                    },
                    result.as_ref().expect_err("a claim is a decline"),
                    describe(tfm, packages, direct),
                );
            }
            (OurOutcome::ClaimsLegacyFails(_), _) | (OurOutcome::Declined(_), _) => {}
        }
    }

    Comparison { ours, restore }
}

/// [`compare`], returning the pair `(resolved_here, oracle_resolved)` so callers
/// writing hand scenarios can additionally assert the *expected* branch was
/// taken.
fn assert_sound(
    oracle: &mut Oracle,
    tfm: &str,
    packages: &[Pkg],
    direct: &[(&str, &str)],
) -> (bool, bool) {
    let comparison = compare(oracle, tfm, packages, direct);
    (
        matches!(comparison.ours, OurOutcome::Resolved(_)),
        comparison.restore.agreed_closure().is_some(),
    )
}

/// [`compare`] on a `net8.0` hand scenario, for asserting each side's outcome.
fn scenario(oracle: &mut Oracle, packages: &[Pkg], direct: &[(&str, &str)]) -> Comparison {
    compare(oracle, "net8.0", packages, direct)
}

/// An engine outcome that resolved to `packages`, given as `(id, version)`.
fn resolved_to(packages: &[(&str, &str)]) -> EngineOutcome {
    EngineOutcome::Resolved(
        packages
            .iter()
            .map(|(id_, version_)| (id_.to_ascii_lowercase(), (*version_).to_owned()))
            .collect(),
    )
}

/// An engine outcome that failed with exactly `failures`.
fn failed_with(failures: &[RestoreFailure]) -> EngineOutcome {
    EngineOutcome::Failed(failures.iter().copied().collect())
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

/// A cycle below a version that loses its conflict still fails a legacy restore.
///
/// `A → G[1.0] → H → G[1.0]` is a cycle, and `B → G[2.0]` makes G 1.0 lose, so
/// the cycle sits on a rejected branch. The legacy engine reports NU1108 all the
/// same; the default engine writes `{A, B, G 2.0}`. Both checked against a real
/// `dotnet restore`: .NET 8, and .NET 10 with and without
/// `RestoreUseLegacyDependencyResolver`. The engines disagree, so we decline.
#[test]
fn a_cycle_below_a_rejected_version_fails_legacy_restore() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![Dep::new("H", "[1.0.0, )")]),
            Pkg::simple("H", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert_eq!(
        comparison.restore.legacy,
        failed_with(&[RestoreFailure::Cycle])
    );
    assert_eq!(
        comparison.restore.default,
        resolved_to(&[("a", "1.0.0"), ("b", "1.0.0"), ("g", "2.0.0")])
    );
    assert_eq!(comparison.ours, OurOutcome::Declined("loser-not-a-leaf"));
}

/// What the leaf-loser rule costs: a graph both engines resolve, declined.
///
/// `B → G[2.0]` makes G 1.0 lose, so the K conflict inside G 1.0's subtree never
/// bites, and both engines write `{A, B, G 2.0}`. But G 1.0 has dependencies
/// that would become nodes of the legacy tree, and from G 1.0's dependency list
/// alone this graph cannot be told from `mutually_dependent_cousin_conflicts_fail_restore`,
/// where the loser's subtree is exactly what the engines disagree over.
#[test]
fn a_losing_version_with_dependencies_declines() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
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
    let both = resolved_to(&[("a", "1.0.0"), ("b", "1.0.0"), ("g", "2.0.0")]);
    assert_eq!(comparison.restore.legacy, both);
    assert_eq!(comparison.restore.default, both);
    assert_eq!(comparison.ours, OurOutcome::Declined("loser-not-a-leaf"));
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

/// A missing package below a rejected version does not fail restore.
///
/// A→G[1.0,) and B→G[2.0,): restore merges G to 2.0 and *rejects* G 1.0, so
/// G 1.0's missing dependency dangles off a rejected branch and both engines
/// still write `{A, B, G 2.0}`. G 1.0 is not a leaf, so we decline.
#[test]
fn rejected_branch_missing_dependency_does_not_fail_restore() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("G", "[2.0.0, )")]),
            Pkg::simple("G", "1.0.0", vec![Dep::new("Missing", "[1.0.0, )")]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    let both = resolved_to(&[("a", "1.0.0"), ("b", "1.0.0"), ("g", "2.0.0")]);
    assert_eq!(comparison.restore.legacy, both);
    assert_eq!(comparison.restore.default, both);
    assert_eq!(comparison.ours, OurOutcome::Declined("loser-not-a-leaf"));
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
    let mut total = 0usize;
    // Graphs both engines resolve identically that we decline, by why.
    let mut restore_only: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();

    for seed in [0x6b_u64, 0xBEEF, 0x0DDBA11, 0xFACE, 0xC0DE, 0x5EED] {
        let mut rng = SplitMix64(seed);
        for _ in 0..150 {
            // `compare` is the soundness check: if we produce a closure it must
            // be the one both engines write.
            let comparison = generate_multi_version(&mut rng).compare(&mut oracle);
            total += 1;
            match comparison.ours {
                OurOutcome::Resolved(_) => resolved += 1,
                OurOutcome::Declined(why) if comparison.restore.agreed_closure().is_some() => {
                    *restore_only.entry(why).or_default() += 1;
                }
                _ => {}
            }
        }
    }

    eprintln!("multi-version: {resolved}/{total} resolved, restore-only declines {restore_only:?}");

    // Completeness, stated as strongly as it goes: on graphs whose every version
    // is on disk, we resolve everything both engines resolve to the same
    // closure, except where the engine-agreement envelope declines (see
    // `engines_agree` in `resolver.rs`). That envelope is the one deliberate
    // price of committing only to what both engines write; any other decline
    // here is a completeness bug. The rest of the corpus is graphs restore
    // itself fails, and there we must fail too, which `compare` has checked.
    let envelope = ["loser-not-a-leaf", "transitive-potential-downgrade"];
    let other: Vec<_> = restore_only
        .iter()
        .filter(|(why, _)| !envelope.contains(why))
        .collect();
    assert!(
        other.is_empty(),
        "declined graph(s) both engines resolve, outside the engine-agreement envelope: {other:?}"
    );
    // And a sweep that resolved nothing would satisfy that vacuously.
    assert!(
        resolved > 50,
        "generator degenerated: only {resolved}/{total} graphs resolved"
    );
}

/// What a resolved closure needs from the versions it *rejected*, stated exactly.
///
/// A losing version's *presence* is what tells us restore rejected the edge
/// rather than bumping it up to the winner (see
/// `a_losing_path_does_not_contribute_its_winners_dependencies`). Of its
/// contents, only its dependency list is read, to check it is a leaf of the
/// legacy tree (`engines_agree`); nothing below a loser is read at all, which
/// is as much of a rejected branch as the default engine's restore installs.
///
/// So, for every version the closure does not name, delete its nuspec (leaving
/// the commit marker) and resolve again. The answer must be the same closure,
/// or a decline that names that very package as unreadable: never a different
/// closure, and never another decline. Then delete together every nuspec whose
/// absence changed nothing: those are versions the resolver never read, and
/// still the closure must be identical.
#[test]
fn a_resolved_closure_reads_only_the_dependency_lists_of_the_versions_it_rejected() {
    let mut closures = 0usize;
    let mut never_read = 0usize;
    let mut losers_read = 0usize;

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
            let resolve = || resolve_offline(root.path(), &framework(&g.tfm), &direct);

            let Ok(closure) = resolve() else {
                continue;
            };
            let before = closure_set(&closure);
            closures += 1;

            let mut unread = Vec::new();
            for pkg in &g.packages {
                let key = (
                    pkg.id.to_ascii_lowercase(),
                    version(&pkg.version).to_normalized_string(),
                );
                if before.contains(&key) {
                    continue;
                }
                let identity = PackageIdentity::new(id(&pkg.id), version(&pkg.version));
                let nuspec = PackagePaths::new(root.path(), &identity).nuspec_path;
                let contents = fs::read(&nuspec).expect("nuspec");
                fs::remove_file(&nuspec).expect("remove nuspec");

                match resolve() {
                    Ok(after) => {
                        assert_eq!(
                            before,
                            closure_set(&after),
                            "closure changed once {identity:?}'s nuspec left the cache"
                        );
                        unread.push(nuspec.clone());
                        never_read += 1;
                    }
                    Err(ResolveDecline::PackageRead { identity: read, .. }) if read == identity => {
                        losers_read += 1;
                    }
                    Err(other) => panic!(
                        "deleting {identity:?}'s nuspec turned a closure into another decline: \
                         {other}"
                    ),
                }
                fs::write(&nuspec, contents).expect("restore nuspec");
            }

            for nuspec in &unread {
                fs::remove_file(nuspec).expect("remove nuspec");
            }
            let after = resolve().expect("versions never read, deleted together");
            assert_eq!(
                before,
                closure_set(&after),
                "closure changed once every never-read nuspec left the cache"
            );
        }
    }

    eprintln!(
        "{closures} closures: {never_read} rejected version(s) never read, {losers_read} \
         losers' dependency lists read"
    );
    // Both halves must actually happen: a sweep with no losers would say nothing
    // about what a loser costs, and one where every rejected version is read
    // would say nothing about what is spared.
    assert!(
        closures > 40,
        "generator degenerated: only {closures} graphs resolved to prune"
    );
    assert!(
        never_read > 40 && losers_read > 10,
        "generator degenerated: {never_read} never read, {losers_read} losers read"
    );
}

// ============================================================================
// Wide graphs: the version and range vocabulary real nuspecs use
// ============================================================================

/// One graph's versions: distinct under NuGet equality, sorted ascending, each
/// with the spelling it was generated as.
///
/// Drawn from the parser soak's [`gen_version_string`] rather than from a
/// hand-picked list, so graphs meet multi-digit and four-part versions, numeric
/// and alphanumeric prerelease labels, build metadata, and non-normalised
/// spellings (`01.0`, `1.0.0.0`): the space the version differential already
/// pins one string at a time. Spellings are restricted to the characters a
/// nuspec attribute and a nupkg file name carry verbatim. The whitespace- and
/// noise-bearing ones exist to exercise the version parser, which
/// `version_diff` does.
fn gen_version_pool(rng: &mut SplitMix64, want: usize) -> Vec<(String, NuGetVersion)> {
    let mut pool: Vec<(String, NuGetVersion)> = Vec::new();
    for _ in 0..want * 500 {
        if pool.len() == want {
            break;
        }
        let spelling = gen_version_string(rng);
        let plain = !spelling.is_empty()
            && spelling
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
        if !plain {
            continue;
        }
        let Ok(parsed) = NuGetVersion::parse(&spelling) else {
            continue;
        };
        if pool.iter().any(|(_, existing)| existing == &parsed) {
            continue;
        }
        pool.push((spelling, parsed));
    }
    assert_eq!(pool.len(), want, "version pool generator degenerated");
    pool.sort_by(|a, b| a.1.cmp(&b.1));
    pool
}

/// One edge's range over `pool`, aimed at a target whose committed versions are
/// the pool indices `committed` (sorted, non-empty).
///
/// The bounds are drawn from the pool, so they land on, between and around the
/// versions that exist: that is what makes a bounded range exclude the version
/// the graph settles on (restore's NU1107), and a nearer edge pin a package
/// below what a deeper one needs (NU1605). Bounds are spelled either as
/// generated or normalised, so a non-normalised spelling meets its normal form.
///
/// A `clean` range is one the resolver's envelope admits: an inclusive lower
/// bound on a committed version. Otherwise the vocabulary adds the shapes it
/// declines (exclusive and open lower bounds, floats, a lower bound that is not
/// on disk), which matter wherever they *survive*, and also wherever they sit
/// on an eclipsed or rejected edge and must not.
fn gen_wide_range(
    rng: &mut SplitMix64,
    pool: &[(String, NuGetVersion)],
    committed: &[usize],
    clean: bool,
) -> String {
    let spell = |rng: &mut SplitMix64, index: usize| -> String {
        if rng.below(2) == 0 {
            pool[index].0.clone()
        } else {
            pool[index].1.to_normalized_string()
        }
    };
    let close = |rng: &mut SplitMix64| if rng.below(2) == 0 { ')' } else { ']' };

    let low = *rng.pick(committed);
    let above: Vec<usize> = ((low + 1)..pool.len()).collect();
    let absent: Vec<usize> = (0..pool.len()).filter(|i| !committed.contains(i)).collect();

    match rng.below(if clean { 65 } else { 100 }) {
        // Inclusive lower bound, unbounded above.
        0..=14 => format!("[{}, )", spell(rng, low)),
        // The bare-version spelling: NuGet's "this version or higher".
        15..=24 => spell(rng, low),
        // Exact pin.
        25..=37 => format!("[{}]", spell(rng, low)),
        // Bounded, inclusive lower: `[1.0, 2.0)` and `[1.0, 2.0]`.
        38..=64 => match above.is_empty() {
            true => format!("[{}, )", spell(rng, low)),
            false => {
                let high = *rng.pick(&above);
                format!("[{}, {}{}", spell(rng, low), spell(rng, high), close(rng))
            }
        },
        // Exclusive lower bound, with or without an upper one.
        65..=70 => match above.is_empty() || rng.below(2) == 0 {
            true => format!("({}, )", spell(rng, low)),
            false => {
                let high = *rng.pick(&above);
                format!("({}, {}{}", spell(rng, low), spell(rng, high), close(rng))
            }
        },
        // No lower bound at all.
        71..=74 => format!("(, {}{}", spell(rng, low), close(rng)),
        // A lower bound that is not on disk: restore resolves the edge to the
        // lowest committed version above it, if any.
        75..=84 => match absent.is_empty() {
            true => format!("[{}, )", spell(rng, low)),
            false => {
                let missing = *rng.pick(&absent);
                let above_missing: Vec<usize> = ((missing + 1)..pool.len()).collect();
                if above_missing.is_empty() || rng.below(2) == 0 {
                    format!("[{}, )", spell(rng, missing))
                } else {
                    let high = *rng.pick(&above_missing);
                    format!(
                        "[{}, {}{}",
                        spell(rng, missing),
                        spell(rng, high),
                        close(rng)
                    )
                }
            }
        },
        // Floating, in the shapes nuspecs and project files use.
        85..=92 => {
            let v = &pool[low].1;
            match rng.below(5) {
                0 => "*".to_owned(),
                1 => format!("{}.*", v.major()),
                2 => format!("{}.{}.*", v.major(), v.minor()),
                3 => format!("{}.{}.{}-*", v.major(), v.minor(), v.patch()),
                _ => format!("[{}.*, )", v.major()),
            }
        }
        _ => format!("[{}, )", spell(rng, low)),
    }
}

/// Graphs over a [`gen_version_pool`] and [`gen_wide_range`]s: several versions
/// per id, dense acyclic edges (so diamonds and cousins are the norm), and a
/// sprinkling of back-edges, asset filters and multi-TFM groups.
///
/// Half the graphs are *clean*: every range is inside the resolver's envelope
/// and every version is committed, so the sweep's budget goes on the outcomes
/// restore itself decides between (a closure, a conflict, a downgrade, a
/// cycle). The other half add the declined range shapes and uncommitted
/// versions, whose every placement must still decline or be ignored exactly as
/// restore ignores it.
fn generate_wide(rng: &mut SplitMix64) -> Generated {
    let clean = rng.below(2) == 0;
    let pool_size = 5 + rng.below(3);
    let pool = gen_version_pool(rng, pool_size);
    let ids = 3 + rng.below(4);

    // Each id's committed versions, as sorted pool indices.
    let versions: Vec<Vec<usize>> = (0..ids)
        .map(|_| {
            let want = 1 + rng.below(4);
            let mut chosen: Vec<usize> = Vec::new();
            while chosen.len() < want {
                let index = rng.below(pool.len());
                if !chosen.contains(&index) {
                    chosen.push(index);
                }
            }
            chosen.sort_unstable();
            chosen
        })
        .collect();

    let mut packages = Vec::new();
    for (node, node_versions) in versions.iter().enumerate() {
        for &index in node_versions {
            let mut deps = Vec::new();
            for (target, target_versions) in versions.iter().enumerate() {
                let forward = target > node && rng.below(2) == 0;
                let back = target <= node && rng.below(60) == 0;
                if !(forward || back) {
                    continue;
                }
                let mut dep = Dep::new(
                    &maybe_recase(rng, &format!("P{target}")),
                    &gen_wide_range(rng, &pool, target_versions, clean),
                );
                if rng.below(80) == 0 {
                    dep.include = Some("compile".to_owned());
                }
                deps.push(dep);
            }
            let groups = if rng.below(10) == 0 {
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
                version: pool[index].0.clone(),
                groups,
                committed: clean || rng.below(15) != 0,
            });
        }
    }

    // One to three distinct direct requirements, biased towards the roots of the
    // DAG so that most of the graph is reachable.
    let mut direct = Vec::new();
    let mut chosen: Vec<usize> = Vec::new();
    for _ in 0..(1 + rng.below(3)) {
        let target = rng.below(ids.min(3));
        if chosen.contains(&target) {
            continue;
        }
        chosen.push(target);
        direct.push((
            maybe_recase(rng, &format!("P{target}")),
            gen_wide_range(rng, &pool, &versions[target], clean),
        ));
    }

    Generated {
        tfm: "net8.0".to_owned(),
        packages,
        direct,
    }
}

impl Generated {
    fn compare(&self, oracle: &mut Oracle) -> Comparison {
        let direct = self
            .direct
            .iter()
            .map(|(id_, range_)| (id_.as_str(), range_.as_str()))
            .collect::<Vec<_>>();
        compare(oracle, &self.tfm, &self.packages, &direct)
    }

    /// Every range in the graph, direct and transitive, with the id it targets.
    fn ranges(&self) -> impl Iterator<Item = (&str, &str)> {
        let transitive = self.packages.iter().flat_map(|pkg| {
            pkg.groups
                .iter()
                .flat_map(|group| group.deps.iter())
                .map(|dep| (dep.id.as_str(), dep.range.as_str()))
        });
        self.direct
            .iter()
            .map(|(id_, range_)| (id_.as_str(), range_.as_str()))
            .chain(transitive)
    }

    /// The shapes a generated graph is meant to exercise, for the census.
    fn features(&self) -> Features {
        let parsed: Vec<VersionRange> = self.ranges().map(|(_, range_)| range(range_)).collect();
        let package_versions: Vec<NuGetVersion> = self
            .packages
            .iter()
            .map(|pkg| version(&pkg.version))
            .collect();

        // A diamond: some id reached from two distinct parents (the project
        // itself counting as one).
        let mut parents: std::collections::BTreeMap<String, BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for (id_, _) in &self.direct {
            parents
                .entry(id_.to_ascii_lowercase())
                .or_default()
                .insert(String::new());
        }
        for pkg in &self.packages {
            for dep in pkg.groups.iter().flat_map(|group| group.deps.iter()) {
                parents
                    .entry(dep.id.to_ascii_lowercase())
                    .or_default()
                    .insert(pkg.id.to_ascii_lowercase());
            }
        }

        Features {
            bounded: parsed.iter().any(|r| {
                !r.is_floating()
                    && r.has_lower_bound()
                    && r.has_upper_bound()
                    && r.min_version() != r.max_version()
            }),
            exact: parsed.iter().any(|r| {
                !r.is_floating() && r.has_upper_bound() && r.min_version() == r.max_version()
            }),
            prerelease: package_versions.iter().any(NuGetVersion::is_prerelease),
            four_part: package_versions.iter().any(|v| v.revision() != 0),
            multi_digit: package_versions.iter().any(|v| {
                [v.major(), v.minor(), v.patch(), v.revision()]
                    .iter()
                    .any(|&part| part >= 10)
            }),
            diamond: parents.values().any(|from| from.len() >= 2),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Features {
    /// An edge with distinct lower and upper bounds: `[1.0, 2.0)`.
    bounded: bool,
    /// An exact pin: `[1.2.3]`.
    exact: bool,
    prerelease: bool,
    four_part: bool,
    multi_digit: bool,
    diamond: bool,
}

/// What a sweep over generated graphs actually reached. Agreement on a shape the
/// generator never builds is vacuous, so each sweep asserts floors on this.
#[derive(Debug, Default)]
struct Census {
    graphs: usize,
    /// Restore's outcome class under each engine, as `legacy/default`.
    restore: std::collections::BTreeMap<String, usize>,
    /// Graphs on which each engine fails with each error.
    failures: std::collections::BTreeMap<(&'static str, RestoreFailure), usize>,
    /// Graphs both engines resolve to the same closure.
    engines_agree_resolved: usize,
    /// Graphs on which the engines' outcomes differ.
    engines_disagree: usize,
    /// Our outcome class, against restore's.
    pairs: std::collections::BTreeMap<(String, String), usize>,
    /// Graphs both sides resolved, by the shapes they contain.
    resolved_with: FeatureCounts,
    /// Graphs restore fails with a version conflict or a downgrade, by the
    /// shapes they contain.
    conflict_or_downgrade_with: FeatureCounts,
}

#[derive(Debug, Default)]
struct FeatureCounts {
    bounded: usize,
    exact: usize,
    prerelease: usize,
    four_part: usize,
    multi_digit: usize,
    diamond: usize,
}

impl FeatureCounts {
    fn add(&mut self, features: Features) {
        self.bounded += usize::from(features.bounded);
        self.exact += usize::from(features.exact);
        self.prerelease += usize::from(features.prerelease);
        self.four_part += usize::from(features.four_part);
        self.multi_digit += usize::from(features.multi_digit);
        self.diamond += usize::from(features.diamond);
    }

    fn min(&self) -> usize {
        [
            self.bounded,
            self.exact,
            self.prerelease,
            self.four_part,
            self.multi_digit,
            self.diamond,
        ]
        .into_iter()
        .min()
        .expect("non-empty")
    }
}

impl Census {
    fn record(&mut self, generated: &Generated, comparison: &Comparison) {
        self.graphs += 1;
        let restore = format!(
            "{}/{}",
            comparison.restore.legacy.class(),
            comparison.restore.default.class()
        );
        *self.restore.entry(restore.clone()).or_default() += 1;
        *self
            .pairs
            .entry((comparison.ours.class(), restore))
            .or_default() += 1;

        let features = generated.features();
        if matches!(comparison.ours, OurOutcome::Resolved(_)) {
            self.resolved_with.add(features);
        }
        let rejected = comparison.restore.engines().iter().any(|(_, outcome)| {
            outcome.fails_with(RestoreFailure::Conflict)
                || outcome.fails_with(RestoreFailure::Downgrade)
        });
        if rejected {
            self.conflict_or_downgrade_with.add(features);
        }

        for (engine, outcome) in comparison.restore.engines() {
            if let EngineOutcome::Failed(failures) = outcome {
                for failure in failures {
                    *self.failures.entry((engine, *failure)).or_default() += 1;
                }
            }
        }
        if comparison.restore.agreed_closure().is_some() {
            self.engines_agree_resolved += 1;
        }
        if comparison.restore.legacy != comparison.restore.default {
            self.engines_disagree += 1;
        }
    }

    fn ours_resolved(&self) -> usize {
        self.pairs
            .iter()
            .filter(|((ours, _), _)| ours == "resolved")
            .map(|(_, count)| count)
            .sum()
    }

    /// Fail unless every outcome class and every shape appears at least `floor`
    /// times where it matters: in graphs both sides resolve (so a closure
    /// containing the shape was compared), and in graphs restore fails on a
    /// conflict or downgrade (so a decline was required in its presence).
    fn assert_floors(&self, floor: usize) {
        eprintln!(
            "census over {} graphs ({} the engines disagree on)",
            self.graphs, self.engines_disagree
        );
        for ((ours, restore), count) in &self.pairs {
            eprintln!("  ours {ours:<28} restore (legacy/default) {restore:<24} {count}");
        }
        eprintln!("  both resolved, containing: {:?}", self.resolved_with);
        eprintln!(
            "  restore conflict/downgrade, containing: {:?}",
            self.conflict_or_downgrade_with
        );
        assert!(
            self.engines_agree_resolved >= floor,
            "generator degenerated: both engines resolved only {} of {} graphs (floor {floor})",
            self.engines_agree_resolved,
            self.graphs,
        );
        for engine in ["legacy", "default"] {
            for failure in [
                RestoreFailure::Missing,
                RestoreFailure::Cycle,
                RestoreFailure::Conflict,
                RestoreFailure::Downgrade,
            ] {
                let count = self.failures.get(&(engine, failure)).copied().unwrap_or(0);
                assert!(
                    count >= floor,
                    "generator degenerated: the {engine} engine fails with {failure:?} only \
                     {count} time(s) in {} graphs (floor {floor})",
                    self.graphs,
                );
            }
        }
        assert!(
            self.ours_resolved() >= floor,
            "generator degenerated: only {} closure(s) to compare (floor {floor})",
            self.ours_resolved(),
        );
        assert!(
            self.resolved_with.min() >= floor,
            "generator degenerated: some shape is never in a compared closure \
             (floor {floor}): {:?}",
            self.resolved_with,
        );
        assert!(
            self.conflict_or_downgrade_with.min() >= floor,
            "generator degenerated: some shape is never in a graph restore rejects \
             (floor {floor}): {:?}",
            self.conflict_or_downgrade_with,
        );
    }
}

#[test]
fn wide_graphs_resolve_identically() {
    let mut oracle = Oracle::spawn();
    let mut census = Census::default();
    for seed in [0x31de_u64, 0xB0DE, 0x7A11, 0x5CA1E] {
        let mut rng = SplitMix64(seed);
        for _ in 0..150 {
            let g = generate_wide(&mut rng);
            let comparison = g.compare(&mut oracle);
            census.record(&g, &comparison);
        }
    }
    census.assert_floors(10);
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().map(|value| {
        value
            .parse()
            .unwrap_or_else(|e| panic!("{name}={value:?} is not a u64: {e}"))
    })
}

/// Fresh-seed exploration over every generator; `#[ignore]`d because each graph
/// runs a real restore walk. CI runs it on every relevant change.
///
/// The seed is the wall clock unless `BORZOI_NUGET_RESOLVER_SOAK_SEED` fixes
/// it, and is printed first so a failure reproduces;
/// `BORZOI_NUGET_RESOLVER_SOAK_GRAPHS` sets the volume.
#[test]
#[ignore = "fresh-seed soak; CI runs it, run it locally when touching the resolver"]
fn randomised_soundness_soak() {
    let seed = env_u64("BORZOI_NUGET_RESOLVER_SOAK_SEED").unwrap_or_else(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos() as u64
    });
    let graphs = env_u64("BORZOI_NUGET_RESOLVER_SOAK_GRAPHS").unwrap_or(3000) as usize;
    println!("resolver soak seed: {seed} (graphs={graphs})");

    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(seed);
    let mut census = Census::default();
    for _ in 0..graphs {
        // The single-version graphs, the multi-version ones whose conflicts
        // slice 6b resolves, and the wide vocabulary. Only the last is counted:
        // the floors below are about the shapes it exists to reach.
        match rng.below(4) {
            0 => {
                generate(&mut rng).compare(&mut oracle);
            }
            1 => {
                generate_multi_version(&mut rng).compare(&mut oracle);
            }
            _ => {
                let g = generate_wide(&mut rng);
                let comparison = g.compare(&mut oracle);
                census.record(&g, &comparison);
            }
        }
    }
    census.assert_floors(graphs / 300);
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
/// `G[3]` edge is gone and only P 2's `G[1]` remains — so G must *fall* to 1.0,
/// and both engines write G 1.0. Carrying the previous round's map forward
/// (which only ever rose) left G at 3.0, a closure restore never produces. P 1
/// loses with a dependency that would be a node, so today the graph declines;
/// the falling G is still what `walk` must compute before that check runs.
#[test]
fn a_settled_version_falls_when_the_edge_that_raised_it_disappears() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
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
    let both = resolved_to(&[
        ("a", "1.0.0"),
        ("b", "1.0.0"),
        ("g", "1.0.0"),
        ("p", "2.0.0"),
    ]);
    assert_eq!(comparison.restore.legacy, both);
    assert_eq!(comparison.restore.default, both);
    assert_eq!(comparison.ours, OurOutcome::Declined("loser-not-a-leaf"));
}

/// A dependency shape we cannot model, on a version restore *rejects*, is never
/// raised as that shape's decline: restore does not adjudicate a rejected
/// package's dependencies, and before the walk settles the answer would depend
/// on which direct requirement it reached first. The loser's dependency on G
/// would still be a node of the legacy tree, so the graph declines as a loser
/// that is not a leaf, whatever order the walk took.
#[test]
fn an_asset_filter_on_a_rejected_version_is_judged_as_a_loser() {
    let mut oracle = Oracle::spawn();
    let mut filtered = Dep::new("G", "[1.0.0, )");
    filtered.include = Some("compile".to_owned());

    let comparison = scenario(
        &mut oracle,
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("P", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("P", "[2.0.0, )")]),
            Pkg::simple("P", "1.0.0", vec![filtered]),
            Pkg::simple("P", "2.0.0", vec![]),
            Pkg::simple("G", "1.0.0", vec![]),
        ],
        &[("B", "[1.0.0, )"), ("A", "[1.0.0, )")],
    );
    let both = resolved_to(&[("a", "1.0.0"), ("b", "1.0.0"), ("p", "2.0.0")]);
    assert_eq!(comparison.restore.legacy, both);
    assert_eq!(comparison.restore.default, both);
    assert_eq!(comparison.ours, OurOutcome::Declined("loser-not-a-leaf"));
}

// ============================================================================
// Downgrade adjudication: which nearer edge a potential downgrade is held to
// ============================================================================

/// The nearer edge that makes a deeper one *potentially* downgraded need not be
/// the one restore finally holds it to.
///
/// `P1 → G[2.0]` is potentially downgraded by its grandparent's `P0 → G[1.0]`.
/// But that edge is itself eclipsed by the direct `G[3.0]`, so restore never
/// creates its node. Restore's downgrade check looks at the ancestors' edges
/// that *survived*, which here is only the root's `G[3.0]`. That is at least
/// `[2.0]`, so there is no downgrade and restore resolves
/// `{P0, P1, G 3.0}`. Comparing `[2.0]` against the settled `G 3.0` instead
/// reports a downgrade restore never reports. Found by
/// `wide_graphs_resolve_identically`.
#[test]
fn a_downgrade_is_judged_against_surviving_edges_not_declared_ones() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple(
                "P0",
                "1.0.0",
                vec![Dep::new("P1", "[1.0.0, )"), Dep::new("G", "[1.0.0]")],
            ),
            Pkg::simple("P1", "1.0.0", vec![Dep::new("G", "[2.0.0]")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
            Pkg::simple("G", "3.0.0", vec![]),
        ],
        &[("P0", "[1.0.0, )"), ("G", "[3.0.0]")],
    );
    assert!(oracle_ok, "restore holds P1's G edge to the root's G[3.0]");
    assert!(rust, "and so must we: there is no downgrade to decline on");
}

/// A downgrade restore records against a nearer edge that then *loses* its
/// conflict is not one restore fails on: only a downgrade to the accepted
/// version counts.
///
/// `B → G[1.0]` sits beside `B → C`, so `C → G[2.0, 3.0)` is potentially
/// downgraded by it; and `A → G[3.0]` is a cousin that raises G to 3.0, so
/// B's edge loses. Both engines write `{A, B, C, G 3.0}`. G is not a direct
/// reference, and a potential downgrade of a transitive package is where the
/// engines' adjudications part ways, so we decline.
#[test]
fn a_downgrade_to_a_rejected_version_does_not_fail_restore() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("G", "[3.0.0, )")]),
            Pkg::simple(
                "B",
                "1.0.0",
                vec![Dep::new("C", "[1.0.0, )"), Dep::new("G", "[1.0.0, )")],
            ),
            Pkg::simple("C", "1.0.0", vec![Dep::new("G", "[2.0.0, 3.0.0)")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
            Pkg::simple("G", "3.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    let both = resolved_to(&[
        ("a", "1.0.0"),
        ("b", "1.0.0"),
        ("c", "1.0.0"),
        ("g", "3.0.0"),
    ]);
    assert_eq!(comparison.restore.legacy, both);
    assert_eq!(comparison.restore.default, both);
    assert_eq!(
        comparison.ours,
        OurOutcome::Declined("transitive-potential-downgrade")
    );
}

/// The control for the two above: the same shape with the nearer edge
/// *accepted* is a real NU1605, and both sides must fail it.
#[test]
fn a_downgrade_to_the_accepted_version_fails_on_both_sides() {
    let mut oracle = Oracle::spawn();
    let (rust, oracle_ok) = assert_sound(
        &mut oracle,
        "net8.0",
        &[
            Pkg::simple(
                "B",
                "1.0.0",
                vec![Dep::new("C", "[1.0.0, )"), Dep::new("G", "[1.0.0, )")],
            ),
            Pkg::simple("C", "1.0.0", vec![Dep::new("G", "[2.0.0, 3.0.0)")]),
            Pkg::simple("G", "1.0.0", vec![]),
            Pkg::simple("G", "2.0.0", vec![]),
        ],
        &[("B", "[1.0.0, )")],
    );
    assert!(!oracle_ok, "restore fails the downgrade (NU1605)");
    assert!(!rust, "and so must we");
}

// ============================================================================
// Ambiguous cousins: conflicts restore cannot settle
// ============================================================================

/// Two cousin conflicts that each decide the other.
///
/// `A → X[1.0] → Y[3.0]` and `B → Y[1.0] → X[2.0]`: X 2.0 wins only if Y 1.0
/// (its parent) does, and Y 3.0 wins only if X 1.0 (its parent) does. The
/// legacy engine's conflict pass accepts neither and fails with NU1106; the
/// default engine writes `{A, B, X 1.0, Y 3.0}`. Both checked against a real
/// `dotnet restore` on .NET 8 and on .NET 10 with each resolver. The losers X
/// 1.0 and Y 1.0 each have a dependency that would be a node, so we decline.
#[test]
fn mutually_dependent_cousin_conflicts_fail_restore() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
        &[
            Pkg::simple("A", "1.0.0", vec![Dep::new("X", "[1.0.0, )")]),
            Pkg::simple("B", "1.0.0", vec![Dep::new("Y", "[1.0.0, )")]),
            Pkg::simple("X", "1.0.0", vec![Dep::new("Y", "[3.0.0, )")]),
            Pkg::simple("X", "2.0.0", vec![]),
            Pkg::simple("Y", "1.0.0", vec![Dep::new("X", "[2.0.0, )")]),
            Pkg::simple("Y", "3.0.0", vec![]),
        ],
        &[("A", "[1.0.0, )"), ("B", "[1.0.0, )")],
    );
    assert_eq!(
        comparison.restore.legacy,
        failed_with(&[RestoreFailure::Undecided])
    );
    assert_eq!(
        comparison.restore.default,
        resolved_to(&[
            ("a", "1.0.0"),
            ("b", "1.0.0"),
            ("x", "1.0.0"),
            ("y", "3.0.0")
        ])
    );
    assert_eq!(comparison.ours, OurOutcome::Declined("loser-not-a-leaf"));
}

/// A potential downgrade of a transitive package, which the two engines
/// adjudicate differently.
///
/// `P2 → P4[1.0]` sits beside `P2 → P3`, so P3's `P4[2.0]` is potentially
/// downgraded under P2; P1 reaches the same P3 through a different range, and
/// under P1 the `P4[2.0]` edge survives and raises P4 to 2.0. The legacy engine
/// writes `{P1, P2, P3, P4 2.0}`. The default engine walks P3 once, under P2,
/// keeps P4 1.0 and fails with NU1605. Both checked against a real
/// `dotnet restore` on .NET 10. Without the transitive-downgrade rule in
/// `engines_agree` we commit the legacy closure.
#[test]
fn a_potential_downgrade_the_engines_adjudicate_differently() {
    let mut oracle = Oracle::spawn();
    let comparison = scenario(
        &mut oracle,
        &[
            Pkg::simple(
                "P2",
                "1.0.0",
                vec![Dep::new("P3", "[1.0.0, )"), Dep::new("P4", "[1.0.0, )")],
            ),
            Pkg::simple("P1", "1.0.0", vec![Dep::new("P3", "[1.0.0, 2.0.0)")]),
            Pkg::simple("P3", "1.0.0", vec![Dep::new("P4", "[2.0.0]")]),
            Pkg::simple("P4", "1.0.0", vec![]),
            Pkg::simple("P4", "2.0.0", vec![]),
        ],
        &[("P2", "[1.0.0, )"), ("P1", "[1.0.0, )")],
    );
    assert_eq!(
        comparison.restore.legacy,
        resolved_to(&[
            ("p1", "1.0.0"),
            ("p2", "1.0.0"),
            ("p3", "1.0.0"),
            ("p4", "2.0.0")
        ])
    );
    assert_eq!(
        comparison.restore.default,
        failed_with(&[RestoreFailure::Downgrade])
    );
    assert_eq!(
        comparison.ours,
        OurOutcome::Declined("transitive-potential-downgrade")
    );
}
