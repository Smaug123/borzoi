//! Offline package-closure resolver over a caller-supplied global packages root.
//!
//! This module is deliberately pure over process state: it never reads
//! `$NUGET_PACKAGES`, `$HOME`, NuGet.config, feeds, or any other environment.
//! The shell layer decides which cache root to use and passes it in.
//!
//! # The algorithm
//!
//! This is NuGet's `PackageReference` resolution as the legacy dependency
//! resolver does it — `RemoteDependencyWalker` plus `GraphOperations.Analyze`,
//! which `dotnet restore` runs under `RestoreUseLegacyDependencyResolver` —
//! restricted to what a warm cache can answer, and further restricted to the
//! graphs on which the .NET 10 SDK's default resolver (`DependencyGraphResolver`)
//! writes the same closure (see *Correctness envelope*). Three rules, in NuGet's
//! terms:
//!
//! - **Nearest wins.** A dependency edge is *eclipsed* when an ancestor already
//!   depends on the same package with a range whose lower bound is at least as
//!   high: the deeper edge is dropped outright, not merged. This is what makes a
//!   direct `PackageReference` override a transitive one.
//! - **Cousins merge upward.** When two edges reach the same package from
//!   neither-is-an-ancestor-of-the-other positions, both survive and the
//!   *highest* selected version wins.
//! - **A nearer-but-lower edge is a downgrade.** If the eclipsing ancestor's
//!   range is *lower* than the deeper one, the deeper edge is dropped as
//!   *potentially* downgraded, and a second pass decides: it is held to the
//!   ancestor edge on that package that *survived* the walk (not the declared
//!   one that marked it), and restore reports NU1605 and fails only if that
//!   edge is lower, resolves outside the deeper range, and is the accepted
//!   version (`downgrade_is_relevant`).
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
//!   proof that restore rejected it). Of its contents, its dependency list is
//!   read (to know whether the legacy engine's verdict can be claimed), and
//!   its subtree only as deep as the default engine walks it before evicting
//!   it (see *Correctness envelope*).
//! - Only the winner's occurrence of a package is ever expanded.
//!
//! This is exact, not a heuristic — `tests/all/resolver_diff.rs` diffs it against the
//! genuine restore engine, and the over-resolution above is pinned there by
//! `a_losing_path_does_not_contribute_its_winners_dependencies`.
//!
//! # Correctness envelope
//!
//! Ranges must be non-floating with an inclusive lower bound (a floating range's
//! answer lives in feed state we cannot see). The selected version must be
//! committed on disk, as must the lower bound of any edge that loses to it.
//! Anything the legacy engine would *fail* on — a cycle (NU1108), a conflict
//! its conflict pass cannot settle (NU1106), a version conflict (NU1107), a
//! relevant downgrade (NU1605) — we decline on, because a failed restore has no
//! closure to reproduce. Settling is not a corollary of the walk: the pass
//! marks ambiguity by package name, so it can stall on disputes whose losers
//! are all leaves (`settle_conflicts` runs it). Those declines name the legacy
//! engine's error; the default engine may resolve the same graph or fail it
//! differently, so they claim nothing about it. Dependency-level
//! `include`/`exclude` asset filters are ignored: they decide which of a
//! package's assets flow to the project, not which packages are in the graph
//! (`resolver_diff`'s generators attach them, and both engines agree).
//!
//! A closure is committed only where the **default** engine would write it,
//! which is the engine a dependency-graph spec restores under (the spec key
//! that selects the legacy one declines in `dgspec`). The walk is the legacy
//! tree's winners, which the two engines agree on except in how they
//! adjudicate potential downgrades of transitive packages, so `engines_agree`
//! declines unless every potential downgrade is of a direct reference. A
//! losing version's subtree is where the engines part: the legacy engine
//! walks it before rejecting it and can fail on what it finds (cousin
//! conflicts that each decide the other, NU1106; a cycle below the rejected
//! version, NU1108); the default engine restarts its breadth-first walk at
//! every eviction and walks the loser's subtree in every pass before the one
//! that evicts it, so the subtree is held inert as a whole: readable
//! throughout, and naming no package the closure holds
//! (`losing_subtree_is_inert`). The closure is the
//! default engine's, and the legacy engine's verdict is claimed only where no
//! loser has a subtree. `tests/all/resolver_diff.rs` holds every closure to
//! the default engine's, and every decline that names a legacy failure to the
//! legacy engine's.

use crate::{
    FloatBehavior, InstalledPackage, NuGetFramework, NuGetVersion, PackageCacheError, PackageId,
    PackageIdentity, PackagePaths, PackageReadError, VersionRange, list_committed_package_versions,
    read_installed_package,
};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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

/// What an edge a project declares names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTarget {
    /// A package (`PackageReference`).
    Package,
    /// A project of the graph (`ProjectReference`), by its name.
    Project,
}

/// One edge a project declares, in the spec's order: a package reference, or
/// a project reference to a [`ProjectNode`] of the graph.
#[derive(Debug, Clone)]
pub struct ProjectEdge {
    pub id: PackageId,
    /// For a project reference, `[version, )` of the referenced project.
    pub range: VersionRange,
    pub target: EdgeTarget,
    /// Whether restore leaves the edge out when it reaches the project as a
    /// reference: `PrivateAssets="All"`, or the project's own prune list
    /// covers it. Both engines skip it there; the legacy one still counts it
    /// as a sibling when asking whether an ancestor eclipses a deeper edge.
    /// The root project's edges are walked whatever this says.
    pub suppressed: bool,
}

impl ProjectEdge {
    /// A package reference restore walks.
    pub fn package(id: PackageId, range: VersionRange) -> ProjectEdge {
        ProjectEdge {
            id,
            range,
            target: EdgeTarget::Package,
            suppressed: false,
        }
    }
}

/// A referenced project as a node of the graph, for one target framework of
/// the root: NuGet walks the framework of the project nearest the root's,
/// whose edges these are.
#[derive(Debug, Clone)]
pub struct ProjectNode {
    /// The project's name (its `projectName`), which is its id in the graph.
    pub name: PackageId,
    /// The project's package version, which a reference's range `[version, )`
    /// settles on.
    pub version: NuGetVersion,
    pub edges: Vec<ProjectEdge>,
}

/// The graph a project's target framework resolves: the root's own edges and
/// every project a chain of references from it reaches.
#[derive(Debug, Clone, Default)]
pub struct ProjectGraph {
    /// The root project's name, when it is a package id: NuGet fails a
    /// restore whose graph has an edge of that name (`NU1108`, a cycle to
    /// the root), so no edge may carry it. A root named outside the package
    /// id vocabulary is `None`, since no edge can carry such a name.
    pub root: Option<PackageId>,
    /// The root project's edges.
    pub direct: Vec<ProjectEdge>,
    /// The referenced projects, each named once.
    pub projects: Vec<ProjectNode>,
    /// The names of the projects restore knows besides these (the spec's
    /// other projects, which another framework of the root references).
    /// NuGet binds a package edge of such a name to the project, whether or
    /// not this framework's graph reaches it, so no package edge may carry
    /// one here either.
    pub other_projects: Vec<PackageId>,
}

impl ProjectGraph {
    /// The root's package references alone.
    pub fn of_packages(direct: &[DirectPackageRequirement]) -> ProjectGraph {
        ProjectGraph {
            root: None,
            direct: direct
                .iter()
                .map(|requirement| {
                    ProjectEdge::package(requirement.id.clone(), requirement.range.clone())
                })
                .collect(),
            projects: Vec::new(),
            other_projects: Vec::new(),
        }
    }

    fn project(&self, id: &PackageId) -> Option<&ProjectNode> {
        self.projects.iter().find(|project| &project.name == id)
    }

    fn is_project(&self, id: &PackageId) -> bool {
        self.project(id).is_some()
    }

    /// Whether `id` names a project restore knows: one of the graph, the root,
    /// or one of the spec's others. No package edge may carry such a name.
    fn is_project_name(&self, id: &PackageId) -> bool {
        self.is_project(id) || self.root.as_ref() == Some(id) || self.other_projects.contains(id)
    }

    /// Every package edge of the graph, the root's and each project's.
    pub(crate) fn package_edges(&self) -> impl Iterator<Item = (&PackageId, &VersionRange)> {
        self.direct
            .iter()
            .chain(self.projects.iter().flat_map(|project| &project.edges))
            .filter(|edge| edge.target == EdgeTarget::Package)
            .map(|edge| (&edge.id, &edge.range))
    }

    /// The shapes the walk cannot model, checked before it: a project named
    /// twice, a project reference to no project of the graph, and a package
    /// reference named like a project (NuGet lets a project override a
    /// same-named package in the same list, and otherwise chooses between
    /// them by rules the two engines state differently).
    fn check(&self) -> Result<(), ResolveDecline> {
        let mut names = BTreeSet::new();
        for project in &self.projects {
            if !names.insert(&project.name) {
                return Err(ResolveDecline::DuplicateProjectName {
                    name: project.name.clone(),
                });
            }
        }
        let edges = self
            .direct
            .iter()
            .chain(self.projects.iter().flat_map(|project| &project.edges));
        for edge in edges {
            match edge.target {
                // A reference back to the root: restore fails (`NU1108`).
                EdgeTarget::Project if self.root.as_ref() == Some(&edge.id) => {
                    return Err(ResolveDecline::DependencyCycle {
                        cycle: vec![edge.id.clone()],
                    });
                }
                EdgeTarget::Project if !self.is_project(&edge.id) => {
                    return Err(ResolveDecline::UnknownProject {
                        name: edge.id.clone(),
                    });
                }
                EdgeTarget::Package if self.is_project_name(&edge.id) => {
                    return Err(ResolveDecline::DependencyNamesAProject {
                        package: None,
                        project: edge.id.clone(),
                    });
                }
                EdgeTarget::Project | EdgeTarget::Package => {}
            }
        }
        Ok(())
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
    /// Every installed version restore may read while resolving this closure,
    /// sorted: each version the walk read, and the version each dependency
    /// they declare, and each direct requirement, resolves to among the
    /// installed versions (the lowest that satisfies it), where there is one.
    /// Restore starts a read of every edge it enqueues, whether or not its
    /// package is already chosen, and the edges of a loser's subtree among
    /// them, so a caller that must know restore accepts every package it
    /// reads checks these.
    pub examined: Vec<PackageIdentity>,
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
    /// The legacy restore engine fails with NU1107: a surviving edge's range
    /// does not accept the version the graph settled on. The default engine
    /// may resolve the graph, or fail it differently.
    VersionConflict {
        id: PackageId,
        selected: Box<NuGetVersion>,
        range: Box<VersionRange>,
    },
    /// The legacy restore engine fails with NU1106: its conflict pass cannot
    /// settle these packages. It marks ambiguity by package *name*, so cousin
    /// disputes that each have a node of the other's name beneath them block
    /// one another for ever, even when every losing version is a leaf. The
    /// default engine may resolve the graph, or fail it differently.
    UndecidedConflict {
        ids: Vec<PackageId>,
    },
    /// The legacy restore engine fails with NU1605: a nearer edge pinned the
    /// package below what a deeper one needs. The default engine may resolve the
    /// graph, or fail it differently.
    Downgrade {
        id: PackageId,
        selected: Box<NuGetVersion>,
        required: Box<VersionRange>,
    },
    DependencyWithoutRange {
        package: PackageIdentity,
        dependency: PackageId,
    },
    /// A package's selected dependency group names one package more than once
    /// (ignoring case). NuGet keeps every such edge, so the walk's one edge per
    /// package does not model it; with different lower bounds, restore reads the
    /// lower one as pinning the package below the higher one's demand (NU1605).
    DuplicateDependency {
        package: PackageIdentity,
        dependency: PackageId,
    },
    /// The legacy restore engine fails with NU1108. The default engine may resolve
    /// the graph, or fail it differently.
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
    /// Whether an edge is eclipsed turned on a floating range, which NuGet
    /// compares by its release prefix and we do not model. A float that
    /// survives the walk declines as [`ResolveDecline::FloatingRange`]; this
    /// is one that only took part in an ancestor comparison.
    FloatingRangeComparison {
        id: PackageId,
        range: Box<VersionRange>,
    },
    /// A potential downgrade of a package that is not a direct reference, which
    /// the two restore engines adjudicate differently. See `engines_agree`.
    TransitivePotentialDowngrade {
        id: PackageId,
        range: Box<VersionRange>,
    },
    /// The dependency tree grew past the resolver's node bound, or the selected
    /// versions did not settle within its round bound. A pathological graph can
    /// blow the walk up exponentially — NuGet has the same exposure — so it is
    /// bounded, and declines rather than hangs.
    GraphTooLarge,
    /// Two projects of the graph share a name (ignoring case), which is a
    /// project's identity in NuGet's graph.
    DuplicateProjectName {
        name: PackageId,
    },
    /// A project reference names a project the graph does not hold. Restore
    /// fails on it (`NU1105`).
    UnknownProject {
        name: PackageId,
    },
    /// A package reference, or a dependency of `package` (a package of the
    /// graph), names a project of the graph, or the root. NuGet chooses a
    /// project over a package by rules the two engines state differently,
    /// and fails on an edge named like the root (`NU1108`); neither is
    /// modelled.
    DependencyNamesAProject {
        package: Option<PackageIdentity>,
        project: PackageId,
    },
    /// The walk's winners show a failure the legacy engine reports (a cycle,
    /// an unsettled conflict pass, a version conflict, a downgrade), but
    /// `loser` loses its conflict with a subtree the legacy engine walks
    /// before rejecting it and may fail on first, so which failure it reports
    /// is not known. Nothing is claimed; the default engine may fail the graph
    /// too, or resolve it.
    LegacyVerdictUnknown {
        loser: Box<PackageIdentity>,
    },
    /// The default engine walks `loser`'s subtree before evicting it, and
    /// there `dependency` is demanded: a package the closure holds (at
    /// `selected`), whose state in the engine is then not the winners' alone,
    /// or one in a shape not modelled, a version restore resolves the edge to
    /// that the walk cannot name among them. See `losing_subtree_is_inert`.
    LosingSubtreeContends {
        loser: Box<PackageIdentity>,
        dependency: PackageId,
        selected: Option<Box<NuGetVersion>>,
    },
    /// A referenced project leaves `id` out (`PrivateAssets="All"`, or its
    /// prune list), and `id` names the project itself, one of its ancestors
    /// or the root. The default engine checks every dependency for a cycle
    /// before it leaves any out, and fails (`NU1108`); the legacy engine never
    /// walks the edge, and may succeed.
    SuppressedCycle {
        project: PackageId,
        id: PackageId,
    },
    /// A referenced project leaves `id` out (`PrivateAssets="All"`, or its
    /// prune list), and a node below that project depends on `id`. The
    /// default engine leaves every such edge out too, down the whole subtree;
    /// the legacy engine walks it, eclipsed or not by the suppressed sibling.
    /// Where they agree is not modelled.
    SuppressedEdgeReachable {
        project: PackageId,
        id: PackageId,
    },
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
                     (the legacy restore engine fails with NU1107)"
                )
            }
            ResolveDecline::UndecidedConflict { ids } => {
                let ids = ids
                    .iter()
                    .map(PackageId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "the conflicts over {ids} each leave the other ambiguous (the legacy restore \
                     engine fails with NU1106)"
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
                     {required} a deeper one needs (the legacy restore engine fails with NU1605)"
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
            ResolveDecline::DuplicateDependency {
                package,
                dependency,
            } => {
                write!(
                    f,
                    "package {} {} names dependency {dependency} more than once",
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
            ResolveDecline::FloatingRangeComparison { id, range } => {
                write!(
                    f,
                    "whether the edge {id} {range} is eclipsed turns on a floating range, which \
                     NuGet compares by its release prefix"
                )
            }
            ResolveDecline::TransitivePotentialDowngrade { id, range } => {
                write!(
                    f,
                    "a nearer dependency may downgrade package {id} below {range}, which the \
                     restore engines adjudicate differently"
                )
            }
            ResolveDecline::GraphTooLarge => {
                f.write_str("dependency graph did not settle within the resolver's bounds")
            }
            ResolveDecline::DuplicateProjectName { name } => {
                write!(f, "two projects of the graph are named {name}")
            }
            ResolveDecline::UnknownProject { name } => {
                write!(
                    f,
                    "a project reference names {name}, which the graph does not hold (restore \
                     fails with NU1105)"
                )
            }
            ResolveDecline::DependencyNamesAProject { package, project } => match package {
                Some(package) => write!(
                    f,
                    "package {} {} depends on {project}, the name of a project of the graph",
                    package.id, package.version
                ),
                None => write!(
                    f,
                    "a package reference names {project}, the name of a project of the graph"
                ),
            },
            ResolveDecline::SuppressedEdgeReachable { project, id } => {
                write!(
                    f,
                    "project {project} leaves its dependency {id} out, and a node below it \
                     depends on {id}, which the restore engines treat differently"
                )
            }
            ResolveDecline::LegacyVerdictUnknown { loser } => {
                write!(
                    f,
                    "the graph's winners fail a restore, but package {} {} loses its conflict \
                     with a subtree the legacy restore engine walks first, so which failure it \
                     reports is not known",
                    loser.id, loser.version
                )
            }
            ResolveDecline::LosingSubtreeContends {
                loser,
                dependency,
                selected,
            } => {
                write!(
                    f,
                    "package {} {} loses its conflict, but before the default restore engine \
                     evicts it its subtree demands {dependency}",
                    loser.id, loser.version
                )?;
                match selected {
                    Some(selected) => write!(f, ", which the graph settled on at {selected}"),
                    None => write!(f, " in a shape the resolver does not model"),
                }
            }
            ResolveDecline::SuppressedCycle { project, id } => {
                write!(
                    f,
                    "project {project} leaves its dependency {id} out, which closes a cycle \
                     (the default restore engine fails with NU1108, the legacy one does not \
                     walk the edge)"
                )
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
    resolve_offline_in(
        global_packages_root,
        target_framework,
        &TargetContext::default(),
        direct,
    )
}

/// What a project's target framework carries, beyond its direct requirements,
/// that decides which dependencies each package contributes.
#[derive(Debug, Clone, Default)]
pub struct TargetContext {
    /// The asset-target fallback list (the SDK's `AssetTargetFallback`,
    /// `net461` to `net481` on every modern project). A package with no
    /// dependency group the project's own framework can consume contributes the
    /// nearest group for the first fallback framework that has one: NuGet's
    /// `NuGetFrameworkUtility.GetNearest` over an `AssetTargetFallbackFramework`.
    /// A package with a group for the framework itself never looks at the
    /// fallbacks, even when that group is empty.
    pub asset_target_fallback: Vec<NuGetFramework>,
    /// The packages to prune (the spec's `packagesToPrune`), each as its id and
    /// the upper bound of its range. Restore drops a dependency a package
    /// declares on a pruned id when the dependency's range accepts that upper
    /// bound, before it walks the graph; the prune range's lower bound and the
    /// inclusivity of its upper bound play no part. A project's direct
    /// references are never pruned. This is graded against the default restore
    /// engine only (`dgspec_resolve_diff.rs`), the one a dependency-graph spec
    /// can select.
    pub prune: Vec<(PackageId, NuGetVersion)>,
}

impl TargetContext {
    /// The dependencies a package contributes for `framework`: its selected
    /// dependency group, through the fallback list, less the pruned ones.
    pub fn dependencies(
        &self,
        nuspec: &crate::PackageNuspec,
        framework: &NuGetFramework,
    ) -> Vec<crate::PackageDependency> {
        nuspec
            .select_dependency_group_with_fallback(framework, &self.asset_target_fallback)
            .map(|group| {
                group
                    .dependencies
                    .iter()
                    .filter(|dependency| !self.prunes(dependency))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether restore prunes `dependency`.
    fn prunes(&self, dependency: &crate::PackageDependency) -> bool {
        dependency
            .version_range
            .as_ref()
            .is_some_and(|range| self.covers(&dependency.id, range))
    }

    /// Whether the prune list covers a dependency on `id` with `range`: an
    /// entry for `id` whose upper bound the range accepts. Restore prunes a
    /// transitive dependency it covers, and may warn (`NU1510`) about a direct
    /// one, which it keeps. A floating range is never covered here, so the
    /// resolver's own decline on it stands.
    pub fn covers(&self, id: &PackageId, range: &VersionRange) -> bool {
        !range.is_floating()
            && self
                .prune
                .iter()
                .any(|(pruned, maximum)| pruned == id && range.satisfies(maximum))
    }
}

/// [`resolve_offline`] for a project whose target framework carries `context`.
pub fn resolve_offline_in(
    global_packages_root: &Path,
    target_framework: &NuGetFramework,
    context: &TargetContext,
    direct: &[DirectPackageRequirement],
) -> Result<ResolvedPackageClosure, ResolveDecline> {
    resolve_graph(
        global_packages_root,
        target_framework,
        context,
        &ProjectGraph::of_packages(direct),
    )
}

/// [`resolve_offline_in`] for a project with project references: the closure
/// of `graph`, whose referenced projects are nodes of the walk.
///
/// A referenced project is walked as NuGet walks it: its edges for the
/// framework the caller selected, less the suppressed ones
/// ([`ProjectEdge::suppressed`]) and the package edges the root's prune list
/// covers; a project reference settles on the referenced project's version.
/// The closure holds packages only. Outside the envelope where the two
/// restore engines agree on project nodes, this declines
/// ([`ResolveDecline::SuppressedEdgeReachable`],
/// [`ResolveDecline::DependencyNamesAProject`]).
pub fn resolve_graph(
    global_packages_root: &Path,
    target_framework: &NuGetFramework,
    context: &TargetContext,
    graph: &ProjectGraph,
) -> Result<ResolvedPackageClosure, ResolveDecline> {
    for framework in std::iter::once(target_framework).chain(&context.asset_target_fallback) {
        if !framework.is_resolver_project_framework() {
            return Err(ResolveDecline::UnsupportedProjectFramework {
                framework: framework.clone(),
            });
        }
    }
    graph.check()?;

    let mut cache = PackageCache::new(global_packages_root, target_framework, context);
    let mut selected: BTreeMap<PackageId, NuGetVersion> = BTreeMap::new();

    for _ in 0..MAX_ROUNDS {
        let walk = walk(graph, &selected, &mut cache)?;

        if walk.selected == selected {
            return finish(walk, graph, &mut cache);
        }
        selected = walk.selected;
    }

    Err(ResolveDecline::GraphTooLarge)
}

/// Turn a settled walk into a closure, having first ruled out everything restore
/// would have failed on.
fn finish(
    walk: Walk,
    graph: &ProjectGraph,
    cache: &mut PackageCache,
) -> Result<ResolvedPackageClosure, ResolveDecline> {
    // Reads come first, and not merely for a tidier error. A selected version
    // that is not committed means restore resolved that edge against a feed
    // version we cannot see — so the version is wrong, the walk expanded that
    // node as a *leaf*, and every conclusion drawn from the resulting tree (its
    // cycles, its conflicts, its downgrades) is untrustworthy. Adjudicating
    // those first would report a confident diagnosis of a graph we never saw.
    //
    // A project node is not a package: nothing is read for it, and it is not
    // in the closure.
    let package_ids = || walk.edges.keys().filter(|id| !graph.is_project(id));
    let mut packages = Vec::with_capacity(walk.selected.len());
    for id in package_ids() {
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

    // A dependency group naming one package twice, on any package the walk
    // read. The walk keeps one edge per package, so it cannot say what the
    // second edge did, wherever it sat.
    if let Some(decline) = cache.duplicate_dependency() {
        return Err(decline);
    }

    // A dependency shape we cannot model, on a package the graph actually
    // settled on. (On one it rejected, restore would not have looked either.)
    for id in package_ids() {
        let version = walk
            .selected
            .get(id)
            .expect("a demanded package is selected");
        let identity = PackageIdentity::new(id.clone(), version.clone());
        let key = (identity.id.clone(), identity.version_folder_name());
        match walk.shapes.get(&key) {
            Some(ShapeError::NoRange { dependency }) => {
                return Err(ResolveDecline::DependencyWithoutRange {
                    package: identity,
                    dependency: dependency.clone(),
                });
            }
            Some(ShapeError::NamesAProject { dependency }) => {
                return Err(ResolveDecline::DependencyNamesAProject {
                    package: Some(identity),
                    project: dependency.clone(),
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
    // differ, so we decline rather than pick one. (Every edge to a project
    // settles on its one version, so none loses.)
    for (id, ranges) in &walk.edges {
        if graph.is_project(id) {
            continue;
        }
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

    // Outside this envelope the default restore engine can write another
    // closure, so neither a closure nor a diagnosis would be trustworthy.
    engines_agree(&walk, graph, cache)?;

    // The failures below are read from the winners' tree, and name the legacy
    // engine's error. The legacy engine walks every loser's subtree before it
    // rejects it and can fail there first (a cycle, an unsettled conflict), so
    // where a loser has one the failure is reported without a claim.
    let legacy_unknown: Option<PackageIdentity> = walk.losers.iter().find_map(|loser| match cache
        .get(&loser.identity)
    {
        Some(cached) if cached.dependencies.is_empty() => None,
        _ => Some(loser.identity.clone()),
    });
    let legacy = |decline: ResolveDecline| match &legacy_unknown {
        Some(loser) => ResolveDecline::LegacyVerdictUnknown {
            loser: Box::new(loser.clone()),
        },
        None => decline,
    };

    if let Some(cycle) = walk.cycle {
        return Err(legacy(ResolveDecline::DependencyCycle { cycle }));
    }

    // NU1106: the legacy conflict pass must settle every node. This comes before
    // the conflict and downgrade checks, which both assume the winners were
    // accepted: restore reports neither against a node it left undecided.
    settle_conflicts(&walk).map_err(legacy)?;

    // NU1107: every surviving edge must accept the version the graph settled on.
    for (id, ranges) in &walk.edges {
        let version = walk
            .selected
            .get(id)
            .expect("a demanded package is selected");
        for range in ranges {
            if !range.satisfies(version) {
                return Err(legacy(ResolveDecline::VersionConflict {
                    id: id.clone(),
                    selected: Box::new(version.clone()),
                    range: Box::new(range.clone()),
                }));
            }
        }
    }

    // NU1605: a nearer edge that pins the package below what a deeper one needs.
    for downgrade in &walk.downgrades {
        // The package left the closure altogether, so nothing pins it.
        let Some(selected) = walk.selected.get(&downgrade.id) else {
            continue;
        };
        if downgrade_is_relevant(&walk, downgrade, selected) {
            return Err(legacy(ResolveDecline::Downgrade {
                id: downgrade.id.clone(),
                selected: Box::new(selected.clone()),
                required: Box::new(downgrade.range.clone()),
            }));
        }
    }

    let examined = cache.examined(graph.package_edges())?;
    Ok(ResolvedPackageClosure { packages, examined })
}

/// The envelope in which the walk's closure is the one NuGet's default
/// dependency resolver (the .NET 10 SDK's `DependencyGraphResolver`) writes,
/// and in which the legacy failures the walk reports (`settle_conflicts`, the
/// cycle and downgrade checks) are read from what the walk expanded.
///
/// - **No eclipse verdict turns on a floating range.** NuGet compares floats by
///   their release prefix (`RemoteDependencyWalker.IsGreaterThanOrEqualTo`),
///   which [`is_at_least`] does not model.
/// - **Potential downgrades are of direct references only.** The legacy engine
///   adjudicates a potential downgrade against its ancestors' edges; the
///   default one, against whichever version its breadth-first walk chose first
///   and may since have evicted. They agree when the package is a direct
///   reference, which the default engine never evicts and whose root edge
///   settles the legacy verdict.
/// - **Project nodes.** A dependency below a referenced project on a name the
///   project leaves out, and a suppressed edge that closes a cycle, are where
///   the engines walk project nodes differently ([`ResolveDecline::SuppressedEdgeReachable`],
///   [`ResolveDecline::SuppressedCycle`]).
///
/// - **Every losing version's subtree is inert** ([`losing_subtree_is_inert`]):
///   the default engine walks a loser's subtree in every pass before the one
///   that evicts it, so every version reachable from the loser is read, and
///   none of them names a package the closure holds.
///
/// The legacy engine walks every loser's subtree before rejecting it, and can
/// fail on what it finds (NU1106, NU1108) where the default engine writes the
/// winners' closure; the closure is the default engine's, and `finish` claims
/// no legacy failure where a loser has a subtree.
///
/// This envelope is empirical: `tests/all/resolver_diff.rs` sweeps generated
/// graphs against both engines, holds every closure to the default engine's,
/// and holds the declines that name a legacy failure to the legacy engine's.
fn engines_agree(
    walk: &Walk,
    graph: &ProjectGraph,
    cache: &mut PackageCache,
) -> Result<(), ResolveDecline> {
    let is_direct = |id: &PackageId| graph.direct.iter().any(|edge| &edge.id == id);

    if let Some((project, id)) = &walk.suppressed_cycle {
        return Err(ResolveDecline::SuppressedCycle {
            project: project.clone(),
            id: id.clone(),
        });
    }
    if let Some((project, id)) = &walk.suppressed_reachable {
        return Err(ResolveDecline::SuppressedEdgeReachable {
            project: project.clone(),
            id: id.clone(),
        });
    }

    if let Some((id, range)) = &walk.float_compared {
        return Err(ResolveDecline::FloatingRangeComparison {
            id: id.clone(),
            range: Box::new(range.clone()),
        });
    }

    if let Some(downgrade) = walk.downgrades.iter().find(|d| !is_direct(&d.id)) {
        return Err(ResolveDecline::TransitivePotentialDowngrade {
            id: downgrade.id.clone(),
            range: Box::new(downgrade.range.clone()),
        });
    }

    // Every loser occurrence of one version has the same verdict.
    let mut checked: BTreeSet<(PackageId, String)> = BTreeSet::new();
    for loser in &walk.losers {
        assert!(
            !graph.is_project(&loser.identity.id),
            "every edge to a project settles on its one version"
        );
        if !checked.insert((
            loser.identity.id.clone(),
            loser.identity.version_folder_name(),
        )) {
            continue;
        }
        losing_subtree_is_inert(walk, graph, cache, loser)?;
    }

    Ok(())
}

/// Whether the default engine can write the winners' closure with `loser` in
/// the graph (`DependencyGraphResolver.ResolveDependencyGraphItemsAsync`).
///
/// The engine walks breadth-first and starts over whenever an edge it
/// dequeues demands more than a version it chose, recording the choice as
/// evicted. A loser is chosen in every pass before the one in which its
/// evicter (a surviving edge demanding more) is dequeued ahead of it, and in
/// each of those passes some of its subtree is dequeued; how much depends on
/// where every other eviction falls, and the walk's tree does not say, since
/// the engine chooses a package again, deeper, when the edge that first chose
/// it is evicted. So the subtree is held inert as a whole: every version
/// reachable from the loser along lower bounds is read (the engine reads a
/// package's metadata as it enqueues it, so they are versions restore may
/// read, `PackageCache::examined`), and none of them names a package the
/// closure holds. Then no pass through the subtree touches the engine's state
/// about the closure's packages, and the loser goes when its evicter is
/// dequeued. A demand there above a settled version could evict a winner
/// first and leave the loser in the graph (the mutually dependent cousin
/// conflicts of `resolver_diff`), and one at a settled version is coalesced
/// onto the loser's path, with the engine's downgrade bookkeeping following
/// (`a_losers_subtree_reaching_a_winner_at_its_version_declines`).
///
/// A shape the walk does not model there (no lower bound, a float, a
/// project's name) declines, and so does a version that is not installed:
/// restore resolves that edge against its sources, and what it reads there,
/// if anything, the walk cannot name. Absence from the packages folder is not
/// absence from every source, so it licenses nothing.
fn losing_subtree_is_inert(
    walk: &Walk,
    graph: &ProjectGraph,
    cache: &mut PackageCache,
    loser: &Loser,
) -> Result<(), ResolveDecline> {
    let contends = |dependency: &PackageId, selected: Option<&NuGetVersion>| {
        ResolveDecline::LosingSubtreeContends {
            loser: Box::new(loser.identity.clone()),
            dependency: dependency.clone(),
            selected: selected.cloned().map(Box::new),
        }
    };
    let key = |identity: &PackageIdentity| (identity.id.clone(), identity.version_folder_name());
    let mut visited: BTreeSet<(PackageId, String)> = BTreeSet::from([key(&loser.identity)]);
    let mut frontier: VecDeque<PackageIdentity> = VecDeque::from([loser.identity.clone()]);
    while let Some(identity) = frontier.pop_front() {
        // The walk checked the loser's commit marker, not its contents; every
        // descendant is read below before it is enqueued.
        if cache.get(&identity).is_none() {
            return Err(match cache.take_error(&identity) {
                Some(source) => ResolveDecline::PackageRead {
                    identity,
                    source: Box::new(source),
                },
                None => contends(&identity.id, None),
            });
        }
        let cached = cache.get(&identity).expect("read just now");
        let dependencies = cached.dependencies.clone();
        for dependency in &dependencies {
            if graph.is_project_name(&dependency.id) {
                return Err(ResolveDecline::DependencyNamesAProject {
                    package: Some(identity.clone()),
                    project: dependency.id.clone(),
                });
            }
            let range = dependency
                .version_range
                .as_ref()
                .ok_or_else(|| contends(&dependency.id, None))?;
            // An edge back to the loser's own package that its version
            // satisfies: the loser is the chosen version while its subtree
            // is walked, so the engine records the edge and never expands
            // it (a cycle the legacy engine fails,
            // `a_cycle_below_a_rejected_version_fails_legacy_restore`). It
            // is read like every edge, at its lower bound, below.
            let back_edge =
                dependency.id == loser.identity.id && range.satisfies(&loser.identity.version);
            if !back_edge && let Some(selected) = walk.selected.get(&dependency.id) {
                return Err(contends(&dependency.id, Some(selected)));
            }
            let bound = exact_lower_bound(&dependency.id, range)
                .map_err(|_| contends(&dependency.id, None))?;
            let child = PackageIdentity::new(dependency.id.clone(), bound);
            if !visited.insert(key(&child)) {
                continue;
            }
            if visited.len() > MAX_NODES {
                return Err(ResolveDecline::GraphTooLarge);
            }
            if cache.get(&child).is_none() {
                return Err(match cache.take_error(&child) {
                    Some(PackageReadError::NotInstalled { .. }) | None => contends(&child.id, None),
                    Some(source) => ResolveDecline::PackageRead {
                        identity: child,
                        source: Box::new(source),
                    },
                });
            }
            if !back_edge {
                frontier.push_back(child);
            }
        }
    }
    Ok(())
}

/// NuGet's `TryResolveConflicts` gives up after this many rounds
/// (`patience = 1000`, decremented before each round).
const CONFLICT_PASS_ROUNDS: usize = 999;

/// Where the legacy conflict pass has got to with one node.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Acceptable,
    Accepted,
    Rejected,
}

/// The state `WalkTreeMarkAmbiguousNodes` threads from a node to its children.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AmbiguityWalk {
    Walking,
    Rejected,
    Ambiguous,
}

/// One node of the tree the legacy conflict pass sees. `id` and `version` are
/// `None` for the synthetic root.
struct ConflictNode<'a> {
    id: Option<&'a PackageId>,
    version: Option<&'a NuGetVersion>,
    children: Vec<usize>,
}

/// `GraphOperations.TryResolveConflicts`: does the legacy engine's conflict pass
/// settle every node of the tree, or leave some `Acceptable` and fail with
/// NU1106?
///
/// "Highest version wins" is the pass's *answer* when it finishes, but it does
/// not always finish. Each round it rejects the subtrees of rejected nodes,
/// tracks the versions of every remaining node by name, and marks a package
/// name *ambiguous* if any node of that name sits strictly below a node whose
/// name is disputed (tracked at more than one version). It then accepts or
/// rejects, top down, only nodes whose name is not ambiguous. Ambiguity is by
/// name, not by node, so a leaf loser of Y beneath the disputed winner of X,
/// and a leaf loser of X beneath the disputed winner of Y, block both winners
/// for ever.
///
/// The tree is the walk's: every expanded node at its settled version, plus
/// each losing edge as a leaf. That is exactly the legacy tree once its cycles
/// and potential downgrades are removed (as `CheckCycleAndNearestWins` does
/// before this pass), given [`engines_agree`]'s guarantee that no loser has a
/// subtree. Every round is a function of the dispositions alone, so a round
/// that changes none is one the pass repeats until its patience runs out.
fn settle_conflicts(walk: &Walk) -> Result<(), ResolveDecline> {
    let mut nodes: Vec<ConflictNode<'_>> = walk
        .nodes
        .iter()
        .map(|node| ConflictNode {
            id: node.id.as_ref(),
            version: node.id.as_ref().map(|id| {
                walk.selected
                    .get(id)
                    .expect("an expanded node's package is selected")
            }),
            children: Vec::new(),
        })
        .collect();
    for (index, node) in walk.nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            nodes[parent].children.push(index);
        }
    }
    for loser in &walk.losers {
        nodes.push(ConflictNode {
            id: Some(&loser.identity.id),
            version: Some(&loser.identity.version),
            children: Vec::new(),
        });
        let index = nodes.len() - 1;
        nodes[loser.parent].children.push(index);
    }

    let mut dispositions = vec![Disposition::Acceptable; nodes.len()];
    for _ in 0..CONFLICT_PASS_ROUNDS {
        if !dispositions.contains(&Disposition::Acceptable) {
            break;
        }
        let before = dispositions.clone();
        conflict_round(&nodes, &mut dispositions);
        if dispositions == before {
            break;
        }
    }

    let undecided: BTreeSet<&PackageId> = nodes
        .iter()
        .zip(&dispositions)
        .filter(|(_, disposition)| **disposition == Disposition::Acceptable)
        .filter_map(|(node, _)| node.id)
        .collect();
    if !undecided.is_empty() {
        return Err(ResolveDecline::UndecidedConflict {
            ids: undecided.into_iter().cloned().collect(),
        });
    }

    // Settled, the pass accepts exactly the walk's winners and rejects its
    // losers: every expanded node is at its package's highest tracked version,
    // as is every ancestor of one, and every loser is below its winner.
    for (index, disposition) in dispositions.iter().enumerate() {
        let expected = if index < walk.nodes.len() {
            Disposition::Accepted
        } else {
            Disposition::Rejected
        };
        assert!(
            *disposition == expected,
            "a settled conflict pass accepts the winners and rejects the losers"
        );
    }
    Ok(())
}

/// One round of `TryResolveConflicts`. NuGet walks each pass breadth-first, but
/// every pass reads only what the previous one wrote, so any parent-first
/// order gives the same result.
fn conflict_round(nodes: &[ConflictNode<'_>], dispositions: &mut [Disposition]) {
    // `WalkTreeRejectNodesOfRejectedNodes`: reject the subtree of every
    // rejected node, and track the versions of everything else by name.
    let mut tracked: BTreeMap<Option<&PackageId>, Vec<Option<&NuGetVersion>>> = BTreeMap::new();
    let mut stack = vec![(0usize, true)];
    while let Some((index, live)) = stack.pop() {
        let live = live && dispositions[index] != Disposition::Rejected;
        if live {
            let versions = tracked.entry(nodes[index].id).or_default();
            if !versions.contains(&nodes[index].version) {
                versions.push(nodes[index].version);
            }
        } else {
            dispositions[index] = Disposition::Rejected;
        }
        stack.extend(nodes[index].children.iter().map(|&child| (child, live)));
    }
    let disputed = |id: Option<&PackageId>| tracked.get(&id).is_some_and(|v| v.len() > 1);

    // `WalkTreeMarkAmbiguousNodes`: every name with a node strictly below a
    // disputed one is ambiguous.
    let mut ambiguous: BTreeSet<Option<&PackageId>> = BTreeSet::new();
    let mut stack = vec![(0usize, AmbiguityWalk::Walking)];
    while let Some((index, state)) = stack.pop() {
        let next = if dispositions[index] == Disposition::Rejected {
            AmbiguityWalk::Rejected
        } else if state == AmbiguityWalk::Walking && disputed(nodes[index].id) {
            AmbiguityWalk::Ambiguous
        } else {
            if state == AmbiguityWalk::Ambiguous {
                ambiguous.insert(nodes[index].id);
            }
            state
        };
        stack.extend(nodes[index].children.iter().map(|&child| (child, next)));
    }

    // `WalkTreeAcceptOrRejectNodes`: top down through accepted nodes, decide
    // every unambiguous one by whether it is its name's highest version.
    let mut stack = vec![0usize];
    while let Some(index) = stack.pop() {
        let node = &nodes[index];
        if dispositions[index] == Disposition::Rejected || ambiguous.contains(&node.id) {
            continue;
        }
        if dispositions[index] == Disposition::Acceptable {
            let best = tracked[&node.id].iter().all(|known| node.version >= *known);
            dispositions[index] = if best {
                Disposition::Accepted
            } else {
                Disposition::Rejected
            };
        }
        if dispositions[index] == Disposition::Accepted {
            stack.extend(&node.children);
        }
    }
}

/// `GraphOperations.CheckCycleAndNearestWins` plus `IsRelevantDowngrade`: does
/// restore fail this potentially-downgraded edge with NU1605?
///
/// The walk marks an edge potentially downgraded on the *nearest* ancestor that
/// declares the same package lower. Restore's verdict is a second, separate
/// pass, and it looks at different edges: every ancestor from the parent up to
/// the root, at the edges that *survived* the walk there (an eclipsed edge
/// never becomes a node, so it is not a candidate), in that order, each
/// overriding the last. An ancestor edge at least as high as this one clears
/// it; a lower one whose own resolved version still satisfies this range leaves
/// the verdict as it was; any other lower one makes it a downgrade *to that
/// edge*. And the downgrade only fails restore if that edge is accepted, which
/// is to say it settled on the winning version.
///
/// The order rarely matters: an edge survives only when no ancestor above it
/// declares the same package, so an ancestor chain holds at most one surviving
/// edge per package unless one dependency list names a package twice. The loop
/// follows NuGet's order regardless.
///
/// Every surviving edge's resolved version is its lower bound: [`finish`] has
/// already declined if that bound is not on disk, which is the only way restore
/// could have resolved the edge higher.
///
/// Neither range here floats: an eclipse comparison involving a float is
/// [`Verdict::FloatCompared`], never a potential downgrade, and a surviving edge
/// that floats has already declined.
fn downgrade_is_relevant(
    walk: &Walk,
    downgrade: &PotentialDowngrade,
    selected: &NuGetVersion,
) -> bool {
    assert!(
        !downgrade.range.is_floating(),
        "a floating edge is never classified as a potential downgrade"
    );

    let mut held_to: Option<NuGetVersion> = None;
    let mut ancestor = Some(downgrade.parent);
    while let Some(index) = ancestor {
        for (id, range) in &walk.surviving[index] {
            if id != &downgrade.id {
                continue;
            }
            if is_at_least(range, &downgrade.range) {
                held_to = None;
                continue;
            }
            let resolved =
                exact_lower_bound(id, range).expect("every surviving edge has a lower bound");
            if !downgrade.range.satisfies(&resolved) {
                held_to = Some(resolved);
            }
        }
        ancestor = walk.parents[index];
    }

    held_to.is_some_and(|version| &version == selected)
}

/// One node of the walked dependency tree.
#[derive(Clone)]
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
    NoRange {
        dependency: PackageId,
    },
    /// The dependency names a project of the graph.
    NamesAProject {
        dependency: PackageId,
    },
}

/// One edge of a node's dependency list, as the walk sees it.
#[derive(Debug, Clone)]
struct Edge {
    id: PackageId,
    range: VersionRange,
    /// See [`ProjectEdge::suppressed`]; always `false` for a package's.
    suppressed: bool,
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
    downgrades: Vec<PotentialDowngrade>,
    /// Node index → its parent's index, `None` for the synthetic root.
    parents: Vec<Option<usize>>,
    /// Node index → the edges that survived the walk there (those classified
    /// [`Verdict::Acceptable`]), whether or not the walk then expanded them.
    /// These are the nodes restore's tree holds below that node, and so the
    /// candidates its downgrade check compares against.
    surviving: Vec<Vec<(PackageId, VersionRange)>>,
    cycle: Option<Vec<PackageId>>,
    /// The first edge whose eclipse verdict turned on a floating range.
    float_compared: Option<(PackageId, VersionRange)>,
    /// Every edge that survived the walk but lost to a higher version of its
    /// package, so was never expanded.
    losers: Vec<Loser>,
    /// The walked tree, for classifying a loser's dependencies in place.
    nodes: Vec<Node>,
    /// Packages whose dependency list we cannot model, held rather than raised:
    /// a version that is still moving may be one restore rejects, and restore
    /// never looks at a rejected package's dependencies either.
    shapes: BTreeMap<(PackageId, String), ShapeError>,
    /// The first edge found below a project node that depends on a package
    /// the project leaves out: `(project, package)`. See
    /// [`ResolveDecline::SuppressedEdgeReachable`].
    suppressed_reachable: Option<(PackageId, PackageId)>,
    /// The first suppressed edge of a project node that names the node, an
    /// ancestor or the root: `(project, name)`. See
    /// [`ResolveDecline::SuppressedCycle`].
    suppressed_cycle: Option<(PackageId, PackageId)>,
}

/// A surviving edge whose lower bound lost to a higher version of its package.
struct Loser {
    identity: PackageIdentity,
    /// The node whose dependency this edge is.
    parent: usize,
}

/// An edge the walk dropped because an ancestor declares the same package lower.
struct PotentialDowngrade {
    id: PackageId,
    range: VersionRange,
    /// The node whose dependency this edge is.
    parent: usize,
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
    /// The ancestor's edge or this one floats. NuGet then decides eclipse by the
    /// float's release prefix, which [`is_at_least`] does not model, so the
    /// verdict is unknown.
    FloatCompared,
}

fn walk(
    graph: &ProjectGraph,
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
        parents: Vec::new(),
        surviving: vec![Vec::new()],
        cycle: None,
        float_compared: None,
        losers: Vec::new(),
        nodes: Vec::new(),
        shapes: BTreeMap::new(),
        suppressed_reachable: None,
        suppressed_cycle: None,
    };

    // Versions rise as edges are discovered, so a node expanded early in a round
    // may be expanded at a stale version; the fixpoint in `resolve_offline`
    // re-walks until nothing moves, and only a *settled* walk is trusted.
    let mut current = selected.clone();
    let mut stack = vec![0usize];

    while let Some(node) = stack.pop() {
        let dependencies = node_dependencies(&nodes, node, graph, &current, cache, &mut walk)?;

        for (
            dep_index,
            Edge {
                id,
                range,
                suppressed,
            },
        ) in dependencies.iter().enumerate()
        {
            // A referenced project's suppressed edge is not walked; the
            // root's are walked whatever they say. The edge stays in the list
            // (and so in every ancestor's sibling list), as it does in NuGet.
            if *suppressed && node != 0 {
                if walk.suppressed_cycle.is_none() && closes_a_cycle(&nodes, node, id, graph) {
                    let project = nodes[node].id.clone().expect("a non-root node has an id");
                    walk.suppressed_cycle = Some((project, id.clone()));
                }
                continue;
            }
            if let Some(project) = suppressing_ancestor(&nodes, node, id, graph, &current, cache)
                && walk.suppressed_reachable.is_none()
            {
                walk.suppressed_reachable = Some((project, id.clone()));
            }
            match classify(&nodes, node, id, range, graph, &current, cache) {
                Verdict::Eclipsed => continue,
                Verdict::FloatCompared => {
                    if walk.float_compared.is_none() {
                        walk.float_compared = Some((id.clone(), range.clone()));
                    }
                    continue;
                }
                Verdict::Cycle => {
                    if walk.cycle.is_none() {
                        walk.cycle = Some(cycle_path(&nodes, node, id));
                    }
                    continue;
                }
                Verdict::PotentiallyDowngraded => {
                    walk.downgrades.push(PotentialDowngrade {
                        id: id.clone(),
                        range: range.clone(),
                        parent: node,
                    });
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
            walk.surviving[node].push((id.clone(), range.clone()));

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
                walk.losers.push(Loser {
                    identity: PackageIdentity::new(id.clone(), lower_bound),
                    parent: node,
                });
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
            walk.surviving.push(Vec::new());
            stack.push(nodes.len() - 1);
        }
    }
    walk.parents = nodes.iter().map(|node| node.parent).collect();
    walk.nodes = nodes;

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

/// The edges a project of the graph contributes, in the spec's order: the
/// root's own, or a referenced project's less the package edges the root's
/// prune list covers (the default engine prunes a referenced project's
/// dependencies as it prunes a package's; stage 5 graded pruning against that
/// engine alone). Suppressed edges stay in the list, flagged.
fn project_edges(graph: &ProjectGraph, context: &TargetContext, project: &PackageId) -> Vec<Edge> {
    let project = graph
        .project(project)
        .expect("a project node is a project of the graph");
    project
        .edges
        .iter()
        .filter(|edge| edge.target == EdgeTarget::Project || !context.covers(&edge.id, &edge.range))
        .map(|edge| Edge {
            id: edge.id.clone(),
            range: edge.range.clone(),
            suppressed: edge.suppressed,
        })
        .collect()
}

/// The root's edges, in the spec's order.
fn root_edges(graph: &ProjectGraph) -> Vec<Edge> {
    graph
        .direct
        .iter()
        .map(|edge| Edge {
            id: edge.id.clone(),
            range: edge.range.clone(),
            suppressed: edge.suppressed,
        })
        .collect()
}

/// The dependencies a node contributes, in nuspec order: the root's edges for
/// the synthetic root, a referenced project's edges for a project node, and
/// the selected version's dependency group for a package.
///
/// Nothing here is fatal. A package that will not read, or whose dependency list
/// is in a shape we do not model, becomes a *leaf* and the problem is recorded:
/// the version may still be moving, and restore does not look at a rejected
/// package's dependencies either. Only [`finish`], which knows what the graph
/// settled on, decides that a recorded problem was on the answer's path.
fn node_dependencies(
    nodes: &[Node],
    node: usize,
    graph: &ProjectGraph,
    current: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
    walk: &mut Walk,
) -> Result<Vec<Edge>, ResolveDecline> {
    let Some(id) = &nodes[node].id else {
        return Ok(root_edges(graph));
    };
    if graph.is_project(id) {
        return Ok(project_edges(graph, cache.context, id));
    }

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
        let Some(range) = &dependency.version_range else {
            walk.shapes.insert(
                key,
                ShapeError::NoRange {
                    dependency: dependency.id.clone(),
                },
            );
            return Ok(Vec::new());
        };
        if graph.is_project_name(&dependency.id) {
            walk.shapes.insert(
                key,
                ShapeError::NamesAProject {
                    dependency: dependency.id.clone(),
                },
            );
            return Ok(Vec::new());
        }
        dependencies.push(Edge {
            id: dependency.id.clone(),
            range: range.clone(),
            suppressed: false,
        });
    }
    Ok(dependencies)
}

/// Whether an edge from `node` to `id` closes a cycle: `id` names `node`
/// itself, one of its ancestors, or the root. The default engine's cycle
/// check (`DependencyGraphResolver.CreateRestoreTargetGraphAsync`) asks this
/// of every dependency, suppressed ones included.
fn closes_a_cycle(nodes: &[Node], node: usize, id: &PackageId, graph: &ProjectGraph) -> bool {
    if graph.root.as_ref() == Some(id) {
        return true;
    }
    let mut current = Some(node);
    while let Some(index) = current {
        if nodes[index].id.as_ref() == Some(id) {
            return true;
        }
        current = nodes[index].parent;
    }
    false
}

/// The nearest strict ancestor of `node` that is a referenced project leaving
/// `id` out, if any: the shape the two restore engines walk differently.
fn suppressing_ancestor(
    nodes: &[Node],
    node: usize,
    id: &PackageId,
    graph: &ProjectGraph,
    current: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
) -> Option<PackageId> {
    let mut ancestor = nodes[node].parent;
    while let Some(index) = ancestor {
        if let Some(project) = &nodes[index].id
            && graph.is_project(project)
            && ancestor_dependencies(nodes, index, graph, current, cache)
                .iter()
                .any(|edge| edge.suppressed && &edge.id == id)
        {
            return Some(project.clone());
        }
        ancestor = nodes[index].parent;
    }
    None
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
    graph: &ProjectGraph,
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

        let siblings = ancestor_dependencies(nodes, index, graph, current, cache);
        for (dep_index, sibling) in siblings.iter().enumerate() {
            if dep_index == nodes[child].dep_index {
                continue;
            }
            if &sibling.id != id {
                continue;
            }
            let sibling_range = &sibling.range;
            if sibling_range.is_floating() || range.is_floating() {
                return Verdict::FloatCompared;
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
    graph: &ProjectGraph,
    current: &BTreeMap<PackageId, NuGetVersion>,
    cache: &mut PackageCache,
) -> Vec<Edge> {
    let Some(id) = &nodes[node].id else {
        return root_edges(graph);
    };
    if graph.is_project(id) {
        return project_edges(graph, cache.context, id);
    }
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
        .map(|dependency| Edge {
            id: dependency.id.clone(),
            range: dependency
                .version_range
                .clone()
                .unwrap_or_else(all_versions_range),
            suppressed: false,
        })
        .collect()
}

fn all_versions_range() -> VersionRange {
    VersionRange::parse("(, )").expect("literal all-versions range parses")
}

/// `RemoteDependencyWalker.IsGreaterThanOrEqualTo` for non-floating ranges: is
/// the *nearer* edge's lower bound at least the deeper one's? NuGet compares a
/// floating range by its release prefix instead; [`classify`] reports that case
/// as [`Verdict::FloatCompared`] rather than calling this.
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
    /// Which dependencies each package contributes; see [`TargetContext`].
    context: &'a TargetContext,
    packages: BTreeMap<(PackageId, String), Result<CachedPackage, PackageReadError>>,
}

impl<'a> PackageCache<'a> {
    fn new(
        root: &'a Path,
        framework: &'a NuGetFramework,
        context: &'a TargetContext,
    ) -> PackageCache<'a> {
        PackageCache {
            root,
            framework,
            context,
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
                let dependencies = self.context.dependencies(&package.nuspec, self.framework);
                CachedPackage {
                    package,
                    dependencies,
                }
            });
            self.packages.insert(key.clone(), entry);
        }
        self.packages.get_mut(&key).expect("just inserted")
    }

    /// See [`ResolvedPackageClosure::examined`]. `edges` are the graph's
    /// package edges, the root's and every referenced project's.
    fn examined<'e>(
        &mut self,
        edges: impl Iterator<Item = (&'e PackageId, &'e VersionRange)>,
    ) -> Result<Vec<PackageIdentity>, ResolveDecline> {
        let key =
            |identity: &PackageIdentity| (identity.id.clone(), identity.version_folder_name());
        let mut examined: BTreeMap<(PackageId, String), PackageIdentity> = self
            .packages
            .values()
            .flatten()
            .map(|cached| {
                (
                    key(&cached.package.identity),
                    cached.package.identity.clone(),
                )
            })
            .collect();
        let mut named: Vec<(PackageId, VersionRange)> = self
            .packages
            .values()
            .flatten()
            .flat_map(|cached| &cached.dependencies)
            .filter_map(|dependency| {
                Some((dependency.id.clone(), dependency.version_range.clone()?))
            })
            .collect();
        named.extend(edges.map(|(id, range)| (id.clone(), range.clone())));
        // The versions installed of each package named, listed once.
        let mut installed: BTreeMap<PackageId, Vec<NuGetVersion>> = BTreeMap::new();
        for (id, range) in &named {
            if !installed.contains_key(id) {
                let versions = list_committed_package_versions(self.root, id)
                    .map_err(|error| {
                        // The folder of every version of `id` failed to list;
                        // the decline names the edge's lower bound, or 0.0.0
                        // for an edge without one.
                        let PackageCacheError::Io { path, source } = error;
                        let version = range.min_version().cloned().unwrap_or_else(|| {
                            NuGetVersion::parse("0.0.0").expect("a literal version")
                        });
                        ResolveDecline::PackageRead {
                            identity: PackageIdentity::new(id.clone(), version),
                            source: Box::new(PackageReadError::Io { path, source }),
                        }
                    })?
                    .into_iter()
                    .map(|entry| entry.identity.version)
                    .collect();
                installed.insert(id.clone(), versions);
            }
            // The lowest installed version satisfying the edge, a prerelease
            // only where a bound of the edge is one (`VersionRange.IsBetter`):
            // what restore's lookup resolves the edge to with the packages
            // folder its only source.
            let prerelease_bounds = range.min_version().is_some_and(NuGetVersion::is_prerelease)
                || range.max_version().is_some_and(NuGetVersion::is_prerelease)
                || matches!(
                    range.float_behavior(),
                    Some(
                        FloatBehavior::Prerelease
                            | FloatBehavior::PrereleaseRevision
                            | FloatBehavior::PrereleasePatch
                            | FloatBehavior::PrereleaseMinor
                            | FloatBehavior::PrereleaseMajor
                            | FloatBehavior::AbsoluteLatest
                    )
                );
            if let Some(version) = installed[id]
                .iter()
                .filter(|version| prerelease_bounds || !version.is_prerelease())
                .filter(|version| range.satisfies(version))
                .min()
            {
                let identity = PackageIdentity::new(id.clone(), version.clone());
                examined.insert(key(&identity), identity);
            }
        }
        Ok(examined.into_values().collect())
    }

    /// The first package read whose selected dependency group names one
    /// package more than once.
    fn duplicate_dependency(&self) -> Option<ResolveDecline> {
        self.packages.values().flatten().find_map(|cached| {
            let mut seen = BTreeSet::new();
            cached
                .dependencies
                .iter()
                .find(|dependency| !seen.insert(&dependency.id))
                .map(|dependency| ResolveDecline::DuplicateDependency {
                    package: cached.package.identity.clone(),
                    dependency: dependency.id.clone(),
                })
        })
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
    /// can own it; a later read of the version repeats the attempt.
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
