//! Offline package-closure resolver over a caller-supplied global packages root.
//!
//! This module is deliberately pure over process state: it never reads
//! `$NUGET_PACKAGES`, `$HOME`, NuGet.config, feeds, or any other environment.
//! The shell layer decides which cache root to use and passes it in.
//!
//! # The algorithm
//!
//! This is NuGet's `PackageReference` resolution — `RemoteDependencyWalker`
//! plus `GraphOperations.Analyze`, which is what `dotnet restore` itself runs —
//! restricted to what a warm cache can answer. Three rules, in NuGet's terms:
//!
//! - **Nearest wins.** A dependency edge is *eclipsed* when an ancestor already
//!   depends on the same package with a range whose lower bound is at least as
//!   high: the deeper edge is dropped outright, not merged. This is what makes a
//!   direct `PackageReference` override a transitive one.
//! - **Cousins merge upward.** When two edges reach the same package from
//!   neither-is-an-ancestor-of-the-other positions, both survive and the
//!   *highest* selected version wins.
//! - **A nearer-but-lower edge is a downgrade.** If the eclipsing ancestor's
//!   range is *lower* than the deeper one, restore reports NU1605 and fails —
//!   unless the version it settled on happens to satisfy the deeper range anyway.
//!
//! # What the losers cost us
//!
//! NuGet resolves each edge against a *feed* — the lowest published version
//! satisfying the range — and then rejects the losers. Offline we have no feed,
//! which is why slice 6a declined on every conflict.
//!
//! Most of that turns out to be unnecessary. Write `lb(R)` for a range's lower
//! bound and `v(R)` for the version restore picks. For any surviving edge
//! `lb(R) ≤ v(R)`; and if the winning version `W` satisfies `R` — which it must,
//! or restore would have failed with a version conflict — then `v(R) ≤ W` too,
//! since `W` is in the feed. So `W = max v(R) = max lb(R)`: **a package's
//! selected version is the greatest lower bound among the edges that survive
//! eclipsing**, and only the *winner's* bound has to be pinned down.
//!
//! What that argument does *not* buy — and an earlier version of this module
//! wrongly assumed it did — is the right to ignore a losing edge entirely. A
//! loser's subtree is rejected wholesale by restore, contributing nothing to the
//! closure, and that much is true. But whether an edge *is* a loser depends on
//! `v(R)`, and `v(R)` is only known when `lb(R)` is on disk: absent, the feed may
//! have held nothing below `W`, in which case restore bumped the edge up to `W`
//! and it is not a loser at all. The two possibilities are not
//! interchangeable, because a bumped edge brings `W`'s dependencies along *its
//! own path*, where the ancestors eclipse differently — so it can turn a
//! downgrade restore fails on into a closure. Hence:
//!
//! - A losing edge's lower bound must be **committed** (its presence is the
//!   proof that restore rejected it), though its *contents* are never read.
//! - Only the winner's occurrence of a package is ever expanded.
//!
//! This is exact, not a heuristic — `tests/resolver_diff.rs` diffs it against the
//! genuine restore engine, and the over-resolution above is pinned there by
//! `a_losing_path_does_not_contribute_its_winners_dependencies`.
//!
//! # Correctness envelope
//!
//! Ranges must be non-floating with an inclusive lower bound (a floating range's
//! answer lives in feed state we cannot see). The selected version must be
//! committed on disk, as must the lower bound of any edge that loses to it.
//! Anything restore would *fail* on — a live cycle (NU1108), a version conflict
//! (NU1107), a relevant downgrade (NU1605) — we decline on, because a failed
//! restore has no closure to reproduce. Dependency-level `include`/`exclude`
//! asset filters are unmodelled and decline — but only when they sit on a
//! version the graph actually settled on, since restore does not look inside a
//! package it rejected either.

use crate::{
    InstalledPackage, NuGetFramework, NuGetVersion, PackageId, PackageIdentity, PackagePaths,
    PackageReadError, VersionRange, read_installed_package,
};

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;

/// The walk is a *tree*, so a pathological graph can blow it up exponentially.
/// NuGet has the same shape and the same exposure; we bound it and decline
/// rather than hang. Real closures are orders of magnitude below this.
const MAX_NODES: usize = 20_000;

/// Selected versions only ever rise as edges are discovered, so the fixpoint
/// converges in a handful of rounds; the bound is a backstop against a cycle of
/// eclipse decisions we have not thought of, not a real limit.
const MAX_ROUNDS: usize = 64;

/// One direct package input after the caller has evaluated project files,
/// Central Package Management, and package-reference metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectPackageRequirement {
    pub id: PackageId,
    pub range: VersionRange,
}

impl DirectPackageRequirement {
    pub fn new(id: PackageId, range: VersionRange) -> DirectPackageRequirement {
        DirectPackageRequirement { id, range }
    }
}

/// A package selected into the resolved closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    pub identity: PackageIdentity,
    pub paths: PackagePaths,
}

/// The resolved package closure, sorted by package id for deterministic output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageClosure {
    pub packages: Vec<ResolvedPackage>,
}

/// Why the offline resolver declined to produce a closure.
#[derive(Debug)]
pub enum ResolveDecline {
    UnsupportedProjectFramework {
        framework: NuGetFramework,
    },
    FloatingRange {
        id: PackageId,
        range: Box<VersionRange>,
    },
    OpenLowerBound {
        id: PackageId,
        range: Box<VersionRange>,
    },
    ExclusiveLowerBound {
        id: PackageId,
        range: Box<VersionRange>,
    },
    UnsatisfiedLowerBound {
        id: PackageId,
        range: Box<VersionRange>,
        version: Box<NuGetVersion>,
    },
    PackageRead {
        identity: PackageIdentity,
        source: Box<PackageReadError>,
    },
    /// Restore would fail with NU1107: a surviving edge's range does not accept
    /// the version the graph settled on.
    VersionConflict {
        id: PackageId,
        selected: Box<NuGetVersion>,
        range: Box<VersionRange>,
    },
    /// Restore would fail with NU1605: a nearer edge pinned the package below
    /// what a deeper one needs.
    Downgrade {
        id: PackageId,
        selected: Box<NuGetVersion>,
        required: Box<VersionRange>,
    },
    DependencyAssetFilterUnsupported {
        package: PackageIdentity,
        dependency: PackageId,
    },
    DependencyWithoutRange {
        package: PackageIdentity,
        dependency: PackageId,
    },
    /// Restore would fail with NU1108.
    DependencyCycle {
        cycle: Vec<PackageId>,
    },
    /// An edge asks for a *lower* version than the graph settled on, and that
    /// lower version is not on disk — so we cannot tell whether restore rejected
    /// the edge (resolving it against a feed version below the winner) or bumped
    /// it up to the winner itself. The two differ: a rejected edge contributes
    /// nothing, while a bumped one contributes the winner's dependencies *along
    /// its own path*, where the ancestors eclipse differently.
    UnresolvableLosingEdge {
        id: PackageId,
        range: Box<VersionRange>,
        selected: Box<NuGetVersion>,
    },
    /// The dependency tree grew past the resolver's node bound, or the selected
    /// versions did not settle within its round bound. A pathological graph can
    /// blow the walk up exponentially — NuGet has the same exposure — so it is
    /// bounded, and declines rather than hangs.
    GraphTooLarge,
}

impl fmt::Display for ResolveDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveDecline::UnsupportedProjectFramework { framework } => {
                write!(
                    f,
                    "target framework {:?} is outside the offline resolver project framework envelope",
                    framework.short_folder_name()
                )
            }
            ResolveDecline::FloatingRange { id, range } => {
                write!(f, "package {id} has floating range {range}")
            }
            ResolveDecline::OpenLowerBound { id, range } => {
                write!(f, "package {id} has no lower bound in range {range}")
            }
            ResolveDecline::ExclusiveLowerBound { id, range } => {
                write!(f, "package {id} has exclusive lower bound in range {range}")
            }
            ResolveDecline::UnsatisfiedLowerBound { id, range, version } => {
                write!(
                    f,
                    "lower-bound version {version} for package {id} does not satisfy {range}"
                )
            }
            ResolveDecline::PackageRead { identity, source } => {
                write!(
                    f,
                    "failed to read package {} {}: {source}",
                    identity.id, identity.version
                )
            }
            ResolveDecline::VersionConflict {
                id,
                selected,
                range,
            } => {
                write!(
                    f,
                    "package {id} settled on version {selected}, which does not satisfy {range} \
                     (restore would fail with NU1107)"
                )
            }
            ResolveDecline::Downgrade {
                id,
                selected,
                required,
            } => {
                write!(
                    f,
                    "package {id} is pinned to {selected} by a nearer dependency, below the \
                     {required} a deeper one needs (restore would fail with NU1605)"
                )
            }
            ResolveDecline::DependencyAssetFilterUnsupported {
                package,
                dependency,
            } => {
                write!(
                    f,
                    "package {} {} uses dependency asset filters on dependency {dependency}",
                    package.id, package.version
                )
            }
            ResolveDecline::DependencyWithoutRange {
                package,
                dependency,
            } => {
                write!(
                    f,
                    "package {} {} dependency {dependency} has no version range",
                    package.id, package.version
                )
            }
            ResolveDecline::DependencyCycle { cycle } => {
                let path = cycle
                    .iter()
                    .map(PackageId::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                write!(f, "dependency cycle detected: {path}")
            }
            ResolveDecline::UnresolvableLosingEdge {
                id,
                range,
                selected,
            } => {
                write!(
                    f,
                    "package {id} settled on {selected}, but the lower bound of the losing edge                      {range} is not on disk, so we cannot tell what restore resolved it to"
                )
            }
            ResolveDecline::GraphTooLarge => {
                f.write_str("dependency graph did not settle within the resolver's bounds")
            }
        }
    }
}

impl Error for ResolveDecline {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ResolveDecline::PackageRead { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// Resolve an offline package closure from direct requirements.
///
/// `global_packages_root` is an explicit caller-supplied root. The resolver
/// never discovers it from environment variables or config files.
pub fn resolve_offline(
    global_packages_root: &Path,
    target_framework: &NuGetFramework,
    direct: &[DirectPackageRequirement],
) -> Result<ResolvedPackageClosure, ResolveDecline> {
    if !target_framework.is_resolver_project_framework() {
        return Err(ResolveDecline::UnsupportedProjectFramework {
            framework: target_framework.clone(),
        });
    }

    let mut cache = PackageCache::new(global_packages_root, target_framework);
    let mut selected: BTreeMap<PackageId, NuGetVersion> = BTreeMap::new();

    for _ in 0..MAX_ROUNDS {
        let walk = walk(direct, &selected, &mut cache)?;

        if walk.selected == selected {
            return finish(walk, &mut cache);
        }
        selected = walk.selected;
    }

    Err(ResolveDecline::GraphTooLarge)
}

/// Turn a settled walk into a closure, having first ruled out everything restore
/// would have failed on.
fn finish(walk: Walk, cache: &mut PackageCache) -> Result<ResolvedPackageClosure, ResolveDecline> {
    // Reads come first, and not merely for a tidier error. A selected version
    // that is not committed means restore resolved that edge against a feed
    // version we cannot see — so the version is wrong, the walk expanded that
    // node as a *leaf*, and every conclusion drawn from the resulting tree (its
    // cycles, its conflicts, its downgrades) is untrustworthy. Adjudicating
    // those first would report a confident diagnosis of a graph we never saw.
    let mut packages = Vec::with_capacity(walk.selected.len());
    for id in walk.edges.keys() {
        let version = walk
            .selected
            .get(id)
            .expect("a demanded package is selected");
        let identity = PackageIdentity::new(id.clone(), version.clone());

        let Some(cached) = cache.get(&identity) else {
            let source = cache
                .take_error(&identity)
                .expect("a package that did not read has an error");
            return Err(ResolveDecline::PackageRead {
                identity,
                source: Box::new(source),
            });
        };
        packages.push(ResolvedPackage {
            identity: cached.package.identity.clone(),
            paths: cached.package.paths.clone(),
        });
    }

    // A dependency shape we cannot model, on a package the graph actually
    // settled on. (On one it rejected, restore would not have looked either.)
    for id in walk.edges.keys() {
        let version = walk
            .selected
            .get(id)
            .expect("a demanded package is selected");
        let identity = PackageIdentity::new(id.clone(), version.clone());
        let key = (identity.id.clone(), identity.version_folder_name());
        match walk.shapes.get(&key) {
            Some(ShapeError::AssetFilter { dependency }) => {
                return Err(ResolveDecline::DependencyAssetFilterUnsupported {
                    package: identity,
                    dependency: dependency.clone(),
                });
            }
            Some(ShapeError::NoRange { dependency }) => {
                return Err(ResolveDecline::DependencyWithoutRange {
                    package: identity,
                    dependency: dependency.clone(),
                });
            }
            None => {}
        }
    }

    // An edge below the settled version is one restore *rejects*, having
    // resolved it against a feed version lower than the winner — which we can
    // only know if that lower bound is on disk. If it is not, restore may
    // instead have bumped the edge up to the winner, and a bumped edge
    // contributes the winner's dependencies along its own path. The two answers
    // differ, so we decline rather than pick one.
    for (id, ranges) in &walk.edges {
        let selected = walk
            .selected
            .get(id)
            .expect("a demanded package is selected");
        for range in ranges {
            let lower_bound =
                exact_lower_bound(id, range).expect("every surviving edge has a lower bound");
            if &lower_bound == selected {
                continue;
            }
            let losing = PackageIdentity::new(id.clone(), lower_bound);
            if !cache.is_committed(&losing)? {
                return Err(ResolveDecline::UnresolvableLosingEdge {
                    id: id.clone(),
                    range: Box::new(range.clone()),
                    selected: Box::new(selected.clone()),
                });
            }
        }
    }

    if let Some(cycle) = walk.cycle {
        return Err(ResolveDecline::DependencyCycle { cycle });
    }

    // NU1107: every surviving edge must accept the version the graph settled on.
    for (id, ranges) in &walk.edges {
        let version = walk
            .selected
            .get(id)
            .expect("a demanded package is selected");
        for range in ranges {
            if !range.satisfies(version) {
                return Err(ResolveDecline::VersionConflict {
                    id: id.clone(),
                    selected: Box::new(version.clone()),
                    range: Box::new(range.clone()),
                });
            }
        }
    }

    // NU1605: a nearer edge that pins the package below what a deeper one needs.
    // Restore only counts it when the *accepted* version fails to satisfy the
    // deeper range — the package may have been bumped into range anyway, by a
    // cousin or by a feed that had nothing lower.
    for (id, range) in &walk.downgrades {
        match walk.selected.get(id) {
            Some(version) if range.satisfies(version) => {}
            Some(version) => {
                return Err(ResolveDecline::Downgrade {
                    id: id.clone(),
                    selected: Box::new(version.clone()),
                    required: Box::new(range.clone()),
                });
            }
            // The eclipsing edge was itself dropped, so nothing pins the package.
            None => {}
        }
    }

    Ok(ResolvedPackageClosure { packages })
}

/// One node of the walked dependency tree.
struct Node {
    /// `None` for the synthetic root: the project itself is not a package.
    id: Option<PackageId>,
    parent: Option<usize>,
    /// This node's position in its parent's dependency list. NuGet excludes
    /// *that* dependency when asking whether the parent eclipses a candidate —
    /// it is the edge we came down, not a sibling.
    dep_index: usize,
}

/// A dependency a package declares in a shape we do not model.
#[derive(Debug, Clone)]
enum ShapeError {
    AssetFilter { dependency: PackageId },
    NoRange { dependency: PackageId },
}

/// What one round of the walk found.
struct Walk {
    /// Package id → the ranges of every edge that survived eclipsing. A package
    /// with no surviving edge is not in the closure.
    edges: BTreeMap<PackageId, Vec<VersionRange>>,
    /// Package id → the greatest lower bound among those edges: the version
    /// restore settles on.
    selected: BTreeMap<PackageId, NuGetVersion>,
    /// Edges dropped as potential downgrades, to be adjudicated once the
    /// selected versions are known.
    downgrades: Vec<(PackageId, VersionRange)>,
    cycle: Option<Vec<PackageId>>,
    /// Packages whose dependency list we cannot model, held rather than raised:
    /// a version that is still moving may be one restore rejects, and restore
    /// never looks at a rejected package's dependencies either.
    shapes: BTreeMap<(PackageId, String), ShapeError>,
}

/// What NuGet's `CalculateDependencyResult` says about one candidate edge.
enum Verdict {
    Acceptable,
    /// An ancestor already requires this package, at least as high: nearest wins,
    /// and the deeper edge is dropped entirely.
    Eclipsed,
    /// An ancestor requires this package *lower* than we do.
    PotentiallyDowngraded,
    Cycle,
}

fn walk(
    direct: &[DirectPackageRequirement],
    selected: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
) -> Result<Walk, ResolveDecline> {
    let mut nodes: Vec<Node> = vec![Node {
        id: None,
        parent: None,
        dep_index: 0,
    }];
    let mut walk = Walk {
        edges: BTreeMap::new(),
        selected: BTreeMap::new(),
        downgrades: Vec::new(),
        cycle: None,
        shapes: BTreeMap::new(),
    };

    // Versions rise as edges are discovered, so a node expanded early in a round
    // may be expanded at a stale version; the fixpoint in `resolve_offline`
    // re-walks until nothing moves, and only a *settled* walk is trusted.
    let mut current = selected.clone();
    let mut stack = vec![0usize];

    while let Some(node) = stack.pop() {
        let dependencies = node_dependencies(&nodes, node, direct, &current, cache, &mut walk)?;

        for (dep_index, (id, range)) in dependencies.iter().enumerate() {
            match classify(&nodes, node, id, range, direct, &current, cache) {
                Verdict::Eclipsed => continue,
                Verdict::Cycle => {
                    if walk.cycle.is_none() {
                        walk.cycle = Some(cycle_path(&nodes, node, id));
                    }
                    continue;
                }
                Verdict::PotentiallyDowngraded => {
                    walk.downgrades.push((id.clone(), range.clone()));
                    continue;
                }
                Verdict::Acceptable => {}
            }

            let lower_bound = exact_lower_bound(id, range)?;
            if current
                .get(id)
                .is_none_or(|existing| &lower_bound > existing)
            {
                current.insert(id.clone(), lower_bound.clone());
            }

            walk.edges
                .entry(id.clone())
                .or_default()
                .push(range.clone());

            // Expand *only the winner's occurrence*. A node whose lower bound is
            // below the settled version is one restore resolves to a lower
            // version and then rejects — subtree and all — so it contributes
            // nothing. Expanding it at the winner's version instead (as this
            // once did) invents dependency edges along a path restore never
            // accepts, where the ancestors eclipse differently: an
            // over-resolution, found by review and pinned by
            // `a_losing_path_does_not_contribute_its_winners_dependencies`.
            let winner = current
                .get(id)
                .is_some_and(|version| version == &lower_bound);
            if !winner {
                continue;
            }

            if nodes.len() >= MAX_NODES {
                return Err(ResolveDecline::GraphTooLarge);
            }
            nodes.push(Node {
                id: Some(id.clone()),
                parent: Some(node),
                dep_index,
            });
            stack.push(nodes.len() - 1);
        }
    }

    // The settled version is the greatest lower bound among the *surviving
    // edges of this round*, recomputed from scratch. Carrying the previous
    // round's map forward would let a version that should fall — because the
    // edge that raised it has since been eclipsed away — stay high for ever.
    walk.selected = walk
        .edges
        .iter()
        .map(|(id, ranges)| {
            let version = ranges
                .iter()
                .map(|range| {
                    exact_lower_bound(id, range).expect("every surviving edge has a lower bound")
                })
                .max()
                .expect("a package in the edge map has at least one edge");
            (id.clone(), version)
        })
        .collect();

    Ok(walk)
}

/// The dependencies a node contributes, in nuspec order: the direct requirements
/// for the synthetic root, and the selected version's dependency group for a
/// package.
///
/// A package that cannot be read yields *no* dependencies rather than an error:
/// its version may still be moving, and only [`finish`] decides that a failed
/// read was fatal.
/// The dependencies a node contributes, in nuspec order.
///
/// Nothing here is fatal. A package that will not read, or whose dependency list
/// is in a shape we do not model, becomes a *leaf* and the problem is recorded:
/// the version may still be moving, and restore does not look at a rejected
/// package's dependencies either. Only [`finish`], which knows what the graph
/// settled on, decides that a recorded problem was on the answer's path.
fn node_dependencies(
    nodes: &[Node],
    node: usize,
    direct: &[DirectPackageRequirement],
    current: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
    walk: &mut Walk,
) -> Result<Vec<(PackageId, VersionRange)>, ResolveDecline> {
    let Some(id) = &nodes[node].id else {
        return Ok(direct
            .iter()
            .map(|requirement| (requirement.id.clone(), requirement.range.clone()))
            .collect());
    };

    let version = current
        .get(id)
        .expect("an expanded node's package has a version")
        .clone();
    let identity = PackageIdentity::new(id.clone(), version);
    let key = (identity.id.clone(), identity.version_folder_name());

    let Some(cached) = cache.get(&identity) else {
        return Ok(Vec::new());
    };

    let mut dependencies = Vec::with_capacity(cached.dependencies.len());
    for dependency in &cached.dependencies {
        if dependency.include.is_some() || dependency.exclude.is_some() {
            walk.shapes.insert(
                key,
                ShapeError::AssetFilter {
                    dependency: dependency.id.clone(),
                },
            );
            return Ok(Vec::new());
        }
        let Some(range) = &dependency.version_range else {
            walk.shapes.insert(
                key,
                ShapeError::NoRange {
                    dependency: dependency.id.clone(),
                },
            );
            return Ok(Vec::new());
        };
        dependencies.push((dependency.id.clone(), range.clone()));
    }
    Ok(dependencies)
}

/// `RemoteDependencyWalker.WalkParentsAndCalculateDependencyResult`: what an
/// ancestor already says about this candidate.
///
/// The chain runs from the candidate's *parent* upward to the root. At each
/// ancestor we ask two things: is the candidate that ancestor itself (a cycle),
/// and does that ancestor have a *sibling* dependency on the same package — one
/// other than the edge we came down through it?
fn classify(
    nodes: &[Node],
    parent: usize,
    id: &PackageId,
    range: &VersionRange,
    direct: &[DirectPackageRequirement],
    current: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
) -> Verdict {
    // A package depending on itself. NuGet checks this on the node being
    // expanded, separately from the ancestor chain.
    if nodes[parent].id.as_ref() == Some(id) {
        return Verdict::Cycle;
    }

    // `child` is the node we reached the current ancestor through, so its
    // `dep_index` names the edge to exclude from that ancestor's sibling list.
    let mut child = parent;
    let mut ancestor = nodes[parent].parent;

    while let Some(index) = ancestor {
        if nodes[index].id.as_ref() == Some(id) {
            return Verdict::Cycle;
        }

        let siblings = ancestor_dependencies(nodes, index, direct, current, cache);
        for (dep_index, (sibling_id, sibling_range)) in siblings.iter().enumerate() {
            if dep_index == nodes[child].dep_index {
                continue;
            }
            if sibling_id != id {
                continue;
            }
            return if is_at_least(sibling_range, range) {
                Verdict::Eclipsed
            } else {
                Verdict::PotentiallyDowngraded
            };
        }

        child = index;
        ancestor = nodes[index].parent;
    }

    Verdict::Acceptable
}

/// An ancestor's dependency list, as the eclipse check sees it. Unlike
/// [`node_dependencies`] this cannot fail: an unreadable ancestor simply has no
/// siblings, and the round it belongs to is not the settled one.
fn ancestor_dependencies(
    nodes: &[Node],
    node: usize,
    direct: &[DirectPackageRequirement],
    current: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
) -> Vec<(PackageId, VersionRange)> {
    let Some(id) = &nodes[node].id else {
        return direct
            .iter()
            .map(|requirement| (requirement.id.clone(), requirement.range.clone()))
            .collect();
    };
    let Some(version) = current.get(id) else {
        return Vec::new();
    };
    let identity = PackageIdentity::new(id.clone(), version.clone());
    let Some(cached) = cache.get(&identity) else {
        return Vec::new();
    };
    cached
        .dependencies
        .iter()
        .map(|dependency| {
            (
                dependency.id.clone(),
                dependency
                    .version_range
                    .clone()
                    .unwrap_or_else(all_versions_range),
            )
        })
        .collect()
}

fn all_versions_range() -> VersionRange {
    VersionRange::parse("(, )").expect("literal all-versions range parses")
}

/// `RemoteDependencyWalker.IsGreaterThanOrEqualTo`: is the *nearer* edge's lower
/// bound at least the deeper one's? Floating ranges are out of the envelope and
/// decline before this is reached.
fn is_at_least(nearer: &VersionRange, deeper: &VersionRange) -> bool {
    let Some(near_min) = nearer.min_version() else {
        // No lower bound at all accepts everything the deeper edge could want.
        return true;
    };
    let Some(deep_min) = deeper.min_version() else {
        return false;
    };
    near_min >= deep_min
}

fn cycle_path(nodes: &[Node], node: usize, id: &PackageId) -> Vec<PackageId> {
    let mut path = Vec::new();
    let mut current = Some(node);
    while let Some(index) = current {
        if let Some(id) = &nodes[index].id {
            path.push(id.clone());
        }
        current = nodes[index].parent;
    }
    path.reverse();

    let start = path
        .iter()
        .position(|candidate| candidate == id)
        .unwrap_or(0);
    let mut cycle = path[start..].to_vec();
    cycle.push(id.clone());
    cycle
}

fn exact_lower_bound(id: &PackageId, range: &VersionRange) -> Result<NuGetVersion, ResolveDecline> {
    if range.is_floating() {
        return Err(ResolveDecline::FloatingRange {
            id: id.clone(),
            range: Box::new(range.clone()),
        });
    }
    let Some(version) = range.min_version() else {
        return Err(ResolveDecline::OpenLowerBound {
            id: id.clone(),
            range: Box::new(range.clone()),
        });
    };
    if !range.is_min_inclusive() {
        return Err(ResolveDecline::ExclusiveLowerBound {
            id: id.clone(),
            range: Box::new(range.clone()),
        });
    }
    if !range.satisfies(version) {
        return Err(ResolveDecline::UnsatisfiedLowerBound {
            id: id.clone(),
            range: Box::new(range.clone()),
            version: Box::new(version.clone()),
        });
    }
    Ok(version.clone())
}

/// An installed package as the walk needs it: where it lives, and the
/// dependencies its nuspec declares for the project's target framework.
struct CachedPackage {
    package: InstalledPackage,
    dependencies: Vec<crate::PackageDependency>,
}

/// Reads each installed package once, keeping the error for the ones that would
/// not read.
///
/// A read failure is never raised from the walk. A version that is still moving
/// may fail to read and then never be asked for again — that is the ordinary
/// cousin case, where restore resolved the losing edge against a feed version it
/// never installed. Only [`finish`], looking at the versions the graph actually
/// settled on, decides that a failure was fatal.
struct PackageCache<'a> {
    root: &'a Path,
    framework: &'a NuGetFramework,
    packages: BTreeMap<(PackageId, String), Result<CachedPackage, PackageReadError>>,
}

impl<'a> PackageCache<'a> {
    fn new(root: &'a Path, framework: &'a NuGetFramework) -> PackageCache<'a> {
        PackageCache {
            root,
            framework,
            packages: BTreeMap::new(),
        }
    }

    fn entry(
        &mut self,
        identity: &PackageIdentity,
    ) -> &mut Result<CachedPackage, PackageReadError> {
        let key = (identity.id.clone(), identity.version_folder_name());
        if !self.packages.contains_key(&key) {
            let entry = read_installed_package(self.root, identity.clone()).map(|package| {
                let dependencies = package
                    .nuspec
                    .select_dependency_group(self.framework)
                    .map(|group| group.dependencies.clone())
                    .unwrap_or_default();
                CachedPackage {
                    package,
                    dependencies,
                }
            });
            self.packages.insert(key.clone(), entry);
        }
        self.packages.get_mut(&key).expect("just inserted")
    }

    /// The package, or `None` if it could not be read.
    fn get(&mut self, identity: &PackageIdentity) -> Option<&CachedPackage> {
        self.entry(identity).as_ref().ok()
    }

    /// Is this exact version committed on disk?
    ///
    /// Only the marker is consulted, never the package's contents: a version the
    /// graph rejected is one restore never looks inside either. What we need
    /// from it is only that it *exists*, which is what tells us restore resolved
    /// the losing edge to it rather than bumping the edge up to the winner.
    fn is_committed(&mut self, identity: &PackageIdentity) -> Result<bool, ResolveDecline> {
        if self.get(identity).is_some() {
            return Ok(true);
        }
        PackagePaths::new(self.root, identity)
            .is_committed()
            .map_err(|source| ResolveDecline::PackageRead {
                identity: identity.clone(),
                source: Box::new(PackageReadError::Io {
                    path: PackagePaths::new(self.root, identity).metadata_path,
                    source,
                }),
            })
    }

    /// Why the package could not be read, taken out of the cache so the caller
    /// can own it. Only ever called on the decline path.
    fn take_error(&mut self, identity: &PackageIdentity) -> Option<PackageReadError> {
        let key = (identity.id.clone(), identity.version_folder_name());
        match self.packages.remove(&key) {
            Some(Err(error)) => Some(error),
            Some(entry) => {
                self.packages.insert(key, entry);
                None
            }
            None => None,
        }
    }
}
