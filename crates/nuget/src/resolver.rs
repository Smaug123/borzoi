//! Offline package-closure resolver over a caller-supplied global packages root.
//!
//! This module is deliberately pure over process state: it never reads
//! `$NUGET_PACKAGES`, `$HOME`, NuGet.config, feeds, or any other environment.
//! The shell layer decides which cache root to use and passes it in.
//!
//! # The algorithm
//!
//! This is NuGet's default dependency resolver, the .NET 10 SDK's
//! `DependencyGraphResolver` (`NuGet.Commands`), which `dotnet restore` runs for
//! a `PackageReference` project unless the `RestoreUseLegacyDependencyResolver`
//! key selects the old walker. It is reimplemented pass for pass, with the
//! packages folder standing in for every source restore would consult (see
//! *What restore reads*):
//!
//! 1. **Choosing** (`ResolveDependencyGraphItemsAsync`, `Engine::choose`).
//!    An *item* is one dependency edge: a package name, a version range, and
//!    the path of edges from the project down to the edge's parent, each edge
//!    identified by its name and range (`RangeIndex`). Items are processed
//!    breadth-first from the project's own references. The first item of a
//!    name is *chosen*; a later item to a chosen name is skipped when the
//!    chosen range is at least as high (by lower bound), *evicts* the chosen
//!    item when it demands more, and at precisely the chosen range replaces
//!    the chosen item with its own path unless the two paths share a prefix.
//!    A direct package reference is never evicted. An eviction is keyed by the
//!    evicted item's range, so every item at that range is skipped from then
//!    on; it starts the whole pass over when a chosen item descends from the
//!    evictee, and an evictee's own evictions go with it. An item skipped as
//!    lower records the parent it came from as *eclipsed*, which from then on
//!    protects the chosen item from demands below that parent.
//!    `PrivateAssets="All"` suppressions aggregate down the subtree, a
//!    transitive edge to a direct package reference is never enqueued, and an
//!    edge to a name already chosen at that very range is not enqueued again.
//! 2. **Flattening** (`CreateRestoreTargetGraphAsync`, `Engine::flatten`).
//!    The chosen items are walked again, breadth-first by name, over every
//!    dependency they declare: a dependency named like its parent or the
//!    project, or whose range lies on the parent's path, is a cycle (NU1108);
//!    a second visit to a name whose edge demands more than the chosen range
//!    is a downgrade (NU1605, an error under the SDK's default
//!    `WarningsAsErrors`) unless the chosen version satisfies it, a
//!    suppression covers it, or an eclipsed parent does; a first visit whose
//!    range the chosen version does not satisfy is a version conflict
//!    (NU1107). Restore reports cycles before conflicts before downgrades, and
//!    the decline here names the one it reports. The closure is the chosen
//!    packages the flattening reaches.
//!
//! # Transitive pinning
//!
//! Under central package management with `CentralPackageTransitivePinningEnabled`
//! ([`ProjectGraph::transitive_pinning`]), the choosing pass raises every
//! transitive package edge whose name has a central version to that version's
//! range, and gives the item the root's path, as though the root referenced
//! it; a direct package reference keeps its own range. Every package item of
//! a pinned name so carries one range, and is never evicted. A pinned item's
//! own dependencies are walked whatever is suppressed above it, and the
//! pinned item records each item found depending on its name (`Parents`).
//! The flattening then changes in three ways: a downgrade of a pinned package
//! is attributed to the first of its parents the demanding item descends
//! from; a downgrade of a direct reference is not recorded at all; and a
//! first visit whose edge demands more than the chosen range is a downgrade
//! below the root. Downgrades of the first and third kind are `NU1109`, an
//! error. A pin above an edge's upper bound is that edge's version conflict,
//! which restore records but, with no other edge at the pinned range,
//! never reports: the package leaves the graph, and restore succeeds without
//! it. `dgspec_resolve_diff.rs` grades each of these against restore, and
//! `resolver_diff.rs` sweeps pinned graphs.
//!
//! # What restore reads
//!
//! Restore starts a lookup of every edge it enqueues, chosen or not
//! (`SourceRepositoryDependencyProvider.FindLibraryCoreAsync`): a range with
//! an inclusive lower bound resolves to that very version when its folder is
//! installed, and otherwise the range resolves to the lowest version
//! satisfying it across the packages folder and the sources, a prerelease
//! only where a bound of the range is one (`VersionRange.IsBetter`). With
//! the packages folder its only source, every edge here resolves exactly so
//! (`PackageCache::resolve`), the chosen ones included: an edge whose lower
//! bound is not installed resolves to the next installed version, and one no
//! installed version satisfies resolves to nothing, restore's *unresolved*
//! item, which has no dependencies and fails the restore (NU1101) if it
//! stays in the graph. Two installed versions that compare equal but are
//! spelled apart (`1.0.0-0` and `1.0.0--0`) are told apart by the lower
//! bound's spelling, and where no folder spells it restore keeps whichever
//! it enumerated first, so the resolver declines. A version folder holding
//! the `.nupkg.sha512` hash file but no `.nupkg.metadata` marker is
//! installed to restore, which repairs it as it reads the package, and is
//! unreadable here, so it declines too. Whether a source could answer
//! differently is the caller's question: `dgspec::resolve::check_sources`
//! declines on a source holding a package restore looks up past the packages
//! folder. Every version an edge resolves to is listed in
//! [`ResolvedPackageClosure::examined`], since restore reads its metadata as
//! the edge is enqueued; a chosen item is read in full, and declines on
//! anything unreadable, because its dependencies drive the walk.
//!
//! # Correctness envelope
//!
//! Floating ranges decline: their answer lives in feed state we cannot see,
//! and NuGet compares them by release prefix. A dependency without a range,
//! and a package depending on a project of the graph, decline as shapes the
//! model does not hold. Everything else is the engine's own verdict: a closure
//! is the one the default engine writes, and a decline that names a restore
//! failure (a cycle, a version conflict, a downgrade) is one the default engine
//! reports. `tests/all/resolver_diff.rs` holds both to a real restore under
//! that engine, and `dgspec_resolve_diff.rs` holds project graphs to it. The
//! legacy engine's verdicts are recorded there, not modelled: it walks a losing
//! version's subtree before rejecting it and can fail on what it finds (a
//! cycle below a rejected version, cousin conflicts its conflict pass cannot
//! settle), and it adjudicates a potential downgrade against its ancestors'
//! edges where the default engine adjudicates against what it chose.
//! Dependency-level `include`/`exclude` asset filters decide which assets flow,
//! not which packages are in the graph, and are ignored.

use crate::{
    InstalledPackage, NuGetFramework, NuGetVersion, PackageCacheError, PackageId, PackageIdentity,
    PackagePaths, PackageReadError, VersionRange, list_installed_package_versions,
    read_installed_package,
};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

/// The engine processes one item per dependency edge, and a package reached by
/// several cousin paths at one range is chosen again for each (its children
/// enqueued again each time), so a diamond-heavy graph grows the queue
/// exponentially; NuGet has the same exposure. The number of items enqueued,
/// across every pass, is bounded, and a graph past the bound declines rather
/// than hangs. Real closures are orders of magnitude below it.
const MAX_ITEMS: usize = 200_000;

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
    /// Whether the edge's assets stop at the project (`PrivateAssets="All"`,
    /// or the project's own prune list covers it): NuGet's
    /// `SuppressParent == All`. A referenced project's such edge is left out
    /// of the walk, and the name stays suppressed throughout its subtree; the
    /// root project's edges are walked whatever this says, and the flag then
    /// only exempts the edge from the downgrade and conflict checks.
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
    /// The root project's name, when it is a package id: a dependency of that
    /// name anywhere in the graph is a cycle to the root (`NU1108`) if the
    /// flattening reaches it. A root named outside the package id vocabulary
    /// is `None`, since no edge can carry such a name.
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
    /// Central package management's transitive pinning, when the root
    /// project enables it (`CentralPackageTransitivePinningEnabled` under
    /// central package management): the root's central version of each
    /// package, for this framework. `None` when pinning is off.
    ///
    /// Restore raises every transitive package edge to a package with a
    /// central version to that version, and walks it as though the root
    /// referenced it: see *Transitive pinning* in the module docs.
    pub transitive_pinning: Option<BTreeMap<PackageId, VersionRange>>,
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
            transitive_pinning: None,
        }
    }

    fn project(&self, id: &PackageId) -> Option<&ProjectNode> {
        self.projects.iter().find(|project| &project.name == id)
    }

    fn is_project(&self, id: &PackageId) -> bool {
        self.project(id).is_some()
    }

    /// Whether `id` names a referenced project restore knows: one of the
    /// graph, or one of the spec's others. No package edge may carry such a
    /// name. (The root's own name is not one: a dependency carrying it is a
    /// cycle, which the engine reports.)
    fn names_a_project(&self, id: &PackageId) -> bool {
        self.is_project(id) || self.other_projects.contains(id)
    }

    /// Every package edge of the graph, the root's and each project's.
    pub(crate) fn package_edges(&self) -> impl Iterator<Item = (&PackageId, &VersionRange)> {
        self.direct
            .iter()
            .chain(self.projects.iter().flat_map(|project| &project.edges))
            .filter(|edge| edge.target == EdgeTarget::Package)
            .map(|edge| (&edge.id, &edge.range))
    }

    /// The shapes the engine cannot model, checked before it: a project named
    /// twice, a project reference to no project of the graph, a package
    /// reference named like a referenced project (NuGet lets a project
    /// override a same-named package in the same list, and otherwise chooses
    /// between them by type-constraint rules this does not model), and a
    /// floating central version, on which restore fails (`NU1011`) whether or
    /// not any edge is pinned to it.
    fn check(&self) -> Result<(), ResolveDecline> {
        if let Some((id, range)) = self
            .transitive_pinning
            .iter()
            .flatten()
            .find(|(_, range)| range.is_floating())
        {
            return Err(ResolveDecline::FloatingRange {
                id: id.clone(),
                range: Box::new(range.clone()),
            });
        }
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
                EdgeTarget::Project
                    if !self.is_project(&edge.id) && self.root.as_ref() != Some(&edge.id) =>
                {
                    return Err(ResolveDecline::UnknownProject {
                        name: edge.id.clone(),
                    });
                }
                EdgeTarget::Package if self.names_a_project(&edge.id) => {
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

/// A package of the final graph as restore records it (`ResolvedDependencyKey`,
/// made when the flattening first meets the package): the range of the item
/// chosen for it, which is the edge that chose it, and the version it
/// resolved to. Restore judges its lower-bound warnings on these (`NU1602`, a
/// range without an inclusive lower bound; `NU1603`, a lower bound that is not
/// installed, so the package resolved above it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDependency {
    pub id: PackageId,
    /// The chosen item's range.
    pub range: VersionRange,
    pub version: NuGetVersion,
}

/// The resolved package closure, sorted by package id for deterministic output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageClosure {
    pub packages: Vec<ResolvedPackage>,
    /// Each package of the closure as restore records it, in the order the
    /// flattening first met them.
    pub resolved_dependencies: Vec<ResolvedDependency>,
    /// Every installed version restore reads while resolving this closure,
    /// sorted: for every edge the engine enqueues, in every pass, the
    /// installed version the edge resolves to (the lowest satisfying it, a
    /// prerelease only where a bound of the range is one), where there is
    /// one. Restore reads a package's metadata as it enqueues the edge,
    /// whether or not the edge's package is already chosen and whether or not
    /// the item is ever dequeued, so a caller that must know restore accepts
    /// every package it reads checks these.
    pub examined: Vec<PackageIdentity>,
}

/// Why the offline resolver declined to produce a closure.
#[derive(Debug)]
pub enum ResolveDecline {
    UnsupportedProjectFramework {
        framework: NuGetFramework,
    },
    /// An edge of the graph floats. A float resolves against feed state we
    /// cannot see, and NuGet compares it by its release prefix.
    FloatingRange {
        id: PackageId,
        range: Box<VersionRange>,
    },
    /// A package the engine chose could not be read, a version folder restore
    /// treats as installed is not committed
    /// ([`PackageReadError::HashOnly`]), or a package no installed version
    /// satisfies stayed in the graph (restore fails on it, `NU1101`).
    PackageRead {
        identity: PackageIdentity,
        source: Box<PackageReadError>,
    },
    /// Two or more installed versions of `id` compare equal, none is the
    /// range's inclusive lower bound by spelling, and the range resolves to
    /// one of them: restore keeps whichever the folder enumerated first.
    EquivalentVersions {
        id: PackageId,
        range: Box<VersionRange>,
        versions: Vec<NuGetVersion>,
    },
    /// Restore fails with NU1107: the first edge its flattening meets for a
    /// package does not accept the version the engine chose.
    VersionConflict {
        id: PackageId,
        selected: Box<NuGetVersion>,
        range: Box<VersionRange>,
    },
    /// Restore fails with NU1605: an edge demands more than the version the
    /// engine chose for the package, which a nearer edge settled. Under
    /// transitive pinning, a downgrade below a pinned package, or one found
    /// at a first visit, is `NU1109` instead (`central`), an error whatever
    /// the warning policy.
    Downgrade {
        id: PackageId,
        selected: Box<NuGetVersion>,
        required: Box<VersionRange>,
        central: bool,
    },
    DependencyWithoutRange {
        package: PackageIdentity,
        dependency: PackageId,
    },
    /// Restore fails with NU1108: a dependency names its own parent, the root
    /// project, or a package on the path that reached its parent.
    DependencyCycle {
        cycle: Vec<PackageId>,
    },
    /// The engine enqueued more items than the resolver's bound. A diamond-heavy
    /// graph can grow the queue exponentially — NuGet has the same exposure —
    /// so it is bounded, and declines rather than hangs.
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
    /// graph), names a referenced project. NuGet chooses a project over a
    /// package by type-constraint rules this does not model.
    DependencyNamesAProject {
        package: Option<PackageIdentity>,
        project: PackageId,
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
                    "package {id} resolved to version {selected}, which does not satisfy {range} \
                     (restore fails with NU1107)"
                )
            }
            ResolveDecline::Downgrade {
                id,
                selected,
                required,
                central,
            } => {
                let code = if *central { "NU1109" } else { "NU1605" };
                write!(
                    f,
                    "package {id} is held at {selected} by a nearer dependency, below the \
                     {required} a deeper one needs (restore fails with {code})"
                )
            }
            ResolveDecline::EquivalentVersions {
                id,
                range,
                versions,
            } => {
                let versions = versions
                    .iter()
                    .map(NuGetVersion::to_normalized_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "the installed versions {versions} of package {id} compare equal, and which \
                     of them restore resolves {range} to depends on the folder's enumeration \
                     order"
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
                write!(
                    f,
                    "dependency cycle detected: {path} (restore fails with NU1108)"
                )
            }
            ResolveDecline::GraphTooLarge => {
                f.write_str("dependency graph grew past the resolver's bounds")
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
    /// engine (`dgspec_resolve_diff.rs`), the one a dependency-graph spec
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
/// The closure holds packages only.
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

    let mut engine = Engine::new(global_packages_root, target_framework, context, graph);
    let chosen = engine.choose()?;
    let Flattened {
        names: flattened,
        resolved_dependencies,
    } = engine.flatten(&chosen)?;

    let mut packages: Vec<ResolvedPackage> = flattened
        .iter()
        .filter_map(|name| match &chosen[name].node.kind {
            NodeKind::Package { package } => Some(ResolvedPackage {
                identity: package.package.identity.clone(),
                paths: package.package.paths.clone(),
            }),
            NodeKind::Root | NodeKind::Project { .. } | NodeKind::Unresolved { .. } => None,
        })
        .collect();
    packages.sort_by(|a, b| a.identity.id.cmp(&b.identity.id));
    Ok(ResolvedPackageClosure {
        packages,
        resolved_dependencies,
        examined: engine.examined.into_values().collect(),
    })
}

/// NuGet's `LibraryDependencyIndex`: a package name, ignoring case. The root
/// project is [`ROOT_NAME`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NameIndex(u32);

/// NuGet's `LibraryRangeIndex`: a name together with a range, so that two
/// edges to one package at one range are one index wherever they sit. Paths
/// and evictions are made of these. The root project is [`ROOT_RANGE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RangeIndex(u32);

const ROOT_NAME: NameIndex = NameIndex(0);
const ROOT_RANGE: RangeIndex = RangeIndex(0);

/// A range under NuGet's `VersionRangeComparer.Default`, which
/// `LibraryRangeIndex` and `VersionRangePreciseEquals` compare by: the bounds,
/// each under NuGet's version *identity* (`VersionComparer.Equals`, release
/// labels as case-insensitive strings, so `1.0.0-0` and `1.0.0--0` are two
/// bounds although they order as one), and their inclusivity. (The float,
/// which the comparer also reads, is absent: a floating edge declines before
/// it is interned.)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RangeKey {
    /// The bound's normalised spelling, lower-cased: equal exactly when the
    /// versions are one under [`NuGetVersion::eq_strict`].
    min: Option<String>,
    max: Option<String>,
    min_inclusive: bool,
    max_inclusive: bool,
}

impl RangeKey {
    fn of(range: &VersionRange) -> RangeKey {
        let identity = |version: &NuGetVersion| version.to_normalized_string().to_ascii_lowercase();
        RangeKey {
            min: range.min_version().map(identity),
            max: range.max_version().map(identity),
            min_inclusive: range.is_min_inclusive(),
            max_inclusive: range.is_max_inclusive(),
        }
    }
}

/// The edge's `LibraryDependencyTarget`, as far as the engine reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Constraint {
    /// A project's `PackageReference`.
    Package,
    /// A nuspec dependency.
    PackageProjectExternal,
    /// A `ProjectReference`.
    ExternalProject,
}

impl Constraint {
    /// `TypeConstraintAllows(LibraryDependencyTarget.Package)`.
    fn allows_package(self) -> bool {
        self != Constraint::ExternalProject
    }
}

/// One dependency edge, as the engine reads it: NuGet's `LibraryDependency`
/// with its two indices.
#[derive(Debug, Clone)]
struct Edge {
    id: PackageId,
    range: VersionRange,
    name: NameIndex,
    range_index: RangeIndex,
    constraint: Constraint,
    /// `SuppressParent == All`; see [`ProjectEdge::suppressed`].
    suppress_parent: bool,
}

/// NuGet's `DependencyGraphItem`: an edge queued for processing, with where it
/// sits.
#[derive(Clone)]
struct Item {
    name: NameIndex,
    range_index: RangeIndex,
    /// `None` for the root project.
    edge: Option<Edge>,
    /// The range indices of the edges from the root down to this edge's
    /// parent: `[]` for the root, `[ROOT_RANGE]` for its children.
    path: Rc<Vec<RangeIndex>>,
    /// The names suppressed above this item, aggregated down from the root.
    suppressions: Rc<BTreeSet<NameIndex>>,
    /// A package reference of the root project, which is never evicted and
    /// eclipses every transitive edge to its name.
    is_root_package_reference: bool,
    /// The range index of the item whose dependency this is (`Parent`);
    /// `None` for the root.
    parent: Option<RangeIndex>,
    /// `IsCentrallyPinnedTransitivePackage`: transitive pinning has a central
    /// version for the name. A transitive such item carries that version's
    /// range and the root's path ([`Engine::choose`]). A root package
    /// reference is flagged too, and keeps its own range; nothing the engine
    /// does with the flag reaches one.
    pinned: bool,
}

impl Item {
    fn root() -> Item {
        Item {
            name: ROOT_NAME,
            range_index: ROOT_RANGE,
            edge: None,
            path: Rc::new(Vec::new()),
            suppressions: Rc::new(BTreeSet::new()),
            is_root_package_reference: false,
            parent: None,
            pinned: false,
        }
    }

    /// A transitive item transitive pinning raised (`IsCentrallyPinnedTransitivePackage
    /// && !IsRootPackageReference`).
    fn is_pinned_transitive(&self) -> bool {
        self.pinned && !self.is_root_package_reference
    }

    fn range(&self) -> Option<&VersionRange> {
        self.edge.as_ref().map(|edge| &edge.range)
    }

    fn constraint(&self) -> Option<Constraint> {
        self.edge.as_ref().map(|edge| edge.constraint)
    }

    fn allows_package(&self) -> bool {
        self.constraint().is_some_and(Constraint::allows_package)
    }
}

/// What an item resolved to: NuGet's `GraphItem<RemoteResolveResult>`.
struct Node {
    kind: NodeKind,
    dependencies: Rc<Vec<Edge>>,
}

enum NodeKind {
    Root,
    Project {
        version: NuGetVersion,
    },
    Package {
        package: Rc<CachedPackage>,
    },
    /// No installed version satisfies the range: restore's `Unresolved`
    /// library, whose version is the range's lower bound and which has no
    /// dependencies.
    Unresolved {
        version: Option<NuGetVersion>,
    },
}

impl Node {
    /// The resolved version (`Match.Library.Version`).
    fn version(&self) -> Option<&NuGetVersion> {
        match &self.kind {
            NodeKind::Root => None,
            NodeKind::Project { version } => Some(version),
            NodeKind::Package { package } => Some(&package.package.identity.version),
            NodeKind::Unresolved { version } => version.as_ref(),
        }
    }
}

/// NuGet's `ResolvedDependencyGraphItem`: the item chosen for a name.
struct Chosen {
    item: Item,
    node: Node,
    /// The suppression sets of the occurrences coalesced here; the first is
    /// the chosen occurrence's own.
    suppressions: Vec<Rc<BTreeSet<NameIndex>>>,
    /// `ParentPathsThatHaveBeenEclipsed`: the parents of the lower items this
    /// one eclipsed from a path sharing no prefix with its own.
    eclipsed_parents: BTreeSet<RangeIndex>,
    /// `Parents`, kept for an item transitive pinning raised: the range
    /// index of every item found depending on the name, in the order found
    /// (NuGet's `HashSet`, which enumerates in insertion order since nothing
    /// is removed). `None` for any other item.
    parents: Option<Vec<RangeIndex>>,
}

impl Chosen {
    /// The item chosen afresh, its parents starting with its own if
    /// transitive pinning raised it.
    fn new(item: Item, node: Node, suppressions: Vec<Rc<BTreeSet<NameIndex>>>) -> Chosen {
        let parents = item
            .is_pinned_transitive()
            .then(|| vec![item.parent.expect("a transitive item has a parent")]);
        Chosen {
            item,
            node,
            suppressions,
            eclipsed_parents: BTreeSet::new(),
            parents,
        }
    }

    /// `item` replacing `existing` at the very same range: it takes the
    /// replaced item's pinning and parents, and nothing else of it.
    fn replacing(
        existing: &Chosen,
        item: Item,
        node: Node,
        suppressions: Vec<Rc<BTreeSet<NameIndex>>>,
    ) -> Chosen {
        debug_assert_eq!(
            existing.item.is_root_package_reference, item.is_root_package_reference,
            "a root package reference is never replaced"
        );
        Chosen {
            item: Item {
                pinned: existing.item.pinned,
                ..item
            },
            node,
            suppressions,
            eclipsed_parents: BTreeSet::new(),
            parents: existing.parents.clone(),
        }
    }

    /// Record `parent` among a pinned item's parents (`Parents?.Add`).
    fn add_parent(&mut self, parent: RangeIndex) {
        if let Some(parents) = &mut self.parents
            && !parents.contains(&parent)
        {
            parents.push(parent);
        }
    }

    /// Whether this item carries one empty suppression set: the shape under
    /// which a same-range occurrence coalesces onto it.
    fn unsuppressed(&self) -> bool {
        self.suppressions.len() == 1 && self.suppressions[0].is_empty()
    }
}

/// One entry of the engine's eviction table, keyed by the evicted range index.
struct Eviction {
    /// The evicter's path, the evicter included.
    path: Vec<RangeIndex>,
    /// The evicted (and evicting) name.
    name: NameIndex,
    /// The evicted item's constraint.
    constraint: Constraint,
}

/// What the engine does with an item whose name is already chosen.
enum Decision {
    /// Drop the item.
    Skip,
    /// Drop the item, recording its parent as eclipsed by the chosen one.
    Eclipse(RangeIndex),
    /// Evict the chosen item in favour of this one.
    Evict,
    /// Choose this item, the first of its name.
    Choose,
    /// Replace the chosen item, at the very same range, with this one, with
    /// these suppressions.
    Replace(Vec<Rc<BTreeSet<NameIndex>>>),
}

/// What the flattening reached.
struct Flattened {
    /// The names it visited, the root first.
    names: Vec<NameIndex>,
    /// See [`ResolvedPackageClosure::resolved_dependencies`].
    resolved_dependencies: Vec<ResolvedDependency>,
}

/// A record of a downgrade the flattening found, keyed by the chosen range
/// index of the downgraded package.
struct DowngradeRecord {
    /// The range index of the item whose edge demanded more.
    from: RangeIndex,
    /// The parent of the chosen item (the last entry of its path).
    to_parent: RangeIndex,
    /// The downgraded package.
    to: NameIndex,
    id: PackageId,
    selected: Option<NuGetVersion>,
    required: VersionRange,
    /// `IsCentralTransitive`: restore logs it as `NU1109`.
    central: bool,
}

/// `DependencyGraphResolver`, over one project graph and one packages folder.
struct Engine<'a> {
    graph: &'a ProjectGraph,
    cache: PackageCache<'a>,
    /// Name index → id; `names[0]` is the root's, if it is a package id.
    names: Vec<Option<PackageId>>,
    name_indices: BTreeMap<PackageId, NameIndex>,
    /// Range index → its name and range; `ranges[0]` is the root's.
    ranges: Vec<Option<(NameIndex, RangeKey)>>,
    range_indices: BTreeMap<(NameIndex, RangeKey), RangeIndex>,
    root_edges: Option<Rc<Vec<Edge>>>,
    project_edges: BTreeMap<PackageId, Rc<Vec<Edge>>>,
    package_edges: BTreeMap<(PackageId, String), Rc<Vec<Edge>>>,
    /// See [`ResolvedPackageClosure::examined`].
    examined: BTreeMap<(PackageId, String), PackageIdentity>,
    /// Items enqueued so far, across every pass.
    enqueued: usize,
}

impl<'a> Engine<'a> {
    fn new(
        root: &'a Path,
        framework: &'a NuGetFramework,
        context: &'a TargetContext,
        graph: &'a ProjectGraph,
    ) -> Engine<'a> {
        let mut name_indices = BTreeMap::new();
        if let Some(root) = &graph.root {
            name_indices.insert(root.clone(), ROOT_NAME);
        }
        Engine {
            graph,
            cache: PackageCache::new(root, framework, context),
            names: vec![graph.root.clone()],
            name_indices,
            ranges: vec![None],
            range_indices: BTreeMap::new(),
            root_edges: None,
            project_edges: BTreeMap::new(),
            package_edges: BTreeMap::new(),
            examined: BTreeMap::new(),
            enqueued: 0,
        }
    }

    fn name(&mut self, id: &PackageId) -> NameIndex {
        if let Some(index) = self.name_indices.get(id) {
            return *index;
        }
        let index = NameIndex(u32::try_from(self.names.len()).expect("fewer names than u32::MAX"));
        self.names.push(Some(id.clone()));
        self.name_indices.insert(id.clone(), index);
        index
    }

    /// The edge for a dependency on `id` at `range`, declining on the shapes
    /// the engine does not model. `package` is the package declaring it, if a
    /// package does.
    fn intern_edge(
        &mut self,
        id: PackageId,
        range: VersionRange,
        constraint: Constraint,
        suppress_parent: bool,
        package: Option<&PackageIdentity>,
    ) -> Result<Edge, ResolveDecline> {
        if range.is_floating() {
            return Err(ResolveDecline::FloatingRange {
                id,
                range: Box::new(range),
            });
        }
        if constraint.allows_package() && self.graph.names_a_project(&id) {
            return Err(ResolveDecline::DependencyNamesAProject {
                package: package.cloned(),
                project: id,
            });
        }
        let name = self.name(&id);
        let key = RangeKey::of(&range);
        let range_index = match self.range_indices.get(&(name, key.clone())) {
            Some(index) => *index,
            None => {
                let index = RangeIndex(
                    u32::try_from(self.ranges.len()).expect("fewer ranges than u32::MAX"),
                );
                self.ranges.push(Some((name, key.clone())));
                self.range_indices.insert((name, key), index);
                index
            }
        };
        Ok(Edge {
            id,
            range,
            name,
            range_index,
            constraint,
            suppress_parent,
        })
    }

    fn project_edge(&mut self, edge: &ProjectEdge) -> Result<Edge, ResolveDecline> {
        let constraint = match edge.target {
            EdgeTarget::Package => Constraint::Package,
            EdgeTarget::Project => Constraint::ExternalProject,
        };
        self.intern_edge(
            edge.id.clone(),
            edge.range.clone(),
            constraint,
            edge.suppressed,
            None,
        )
    }

    /// The root's edges, in the spec's order.
    fn root_edges(&mut self) -> Result<Rc<Vec<Edge>>, ResolveDecline> {
        if let Some(edges) = &self.root_edges {
            return Ok(edges.clone());
        }
        let edges: Vec<Edge> = self
            .graph
            .direct
            .iter()
            .map(|edge| self.project_edge(edge))
            .collect::<Result<_, _>>()?;
        let edges = Rc::new(edges);
        self.root_edges = Some(edges.clone());
        Ok(edges)
    }

    /// A referenced project's edges, in the spec's order, less the package
    /// edges the root's prune list covers (the engine prunes a referenced
    /// project's dependencies as it prunes a package's). Suppressed edges stay
    /// in the list, flagged.
    fn project_edges(&mut self, project: &PackageId) -> Result<Rc<Vec<Edge>>, ResolveDecline> {
        if let Some(edges) = self.project_edges.get(project) {
            return Ok(edges.clone());
        }
        let node = self
            .graph
            .project(project)
            .expect("a project item names a project of the graph");
        let context = self.cache.context;
        let edges: Vec<Edge> = node
            .edges
            .iter()
            .filter(|edge| {
                edge.target == EdgeTarget::Project || !context.covers(&edge.id, &edge.range)
            })
            .map(|edge| self.project_edge(edge))
            .collect::<Result<_, _>>()?;
        let edges = Rc::new(edges);
        self.project_edges.insert(project.clone(), edges.clone());
        Ok(edges)
    }

    /// A package version's edges: its selected dependency group, in nuspec
    /// order, less the pruned ones.
    fn package_edges(&mut self, package: &CachedPackage) -> Result<Rc<Vec<Edge>>, ResolveDecline> {
        let identity = &package.package.identity;
        let key = (identity.id.clone(), identity.version_folder_name());
        if let Some(edges) = self.package_edges.get(&key) {
            return Ok(edges.clone());
        }
        let mut edges = Vec::with_capacity(package.dependencies.len());
        for dependency in &package.dependencies {
            let Some(range) = &dependency.version_range else {
                return Err(ResolveDecline::DependencyWithoutRange {
                    package: identity.clone(),
                    dependency: dependency.id.clone(),
                });
            };
            edges.push(self.intern_edge(
                dependency.id.clone(),
                range.clone(),
                Constraint::PackageProjectExternal,
                false,
                Some(identity),
            )?);
        }
        let edges = Rc::new(edges);
        self.package_edges.insert(key, edges.clone());
        Ok(edges)
    }

    /// What `item` resolves to: `FindLibraryCachedAsync` with the packages
    /// folder as the only source, then the item's dependency list.
    fn node_of(&mut self, item: &Item) -> Result<Node, ResolveDecline> {
        let Some(edge) = &item.edge else {
            return Ok(Node {
                kind: NodeKind::Root,
                dependencies: self.root_edges()?,
            });
        };
        if edge.constraint == Constraint::ExternalProject {
            let version = self
                .graph
                .project(&edge.id)
                .expect("a project item names a project of the graph")
                .version
                .clone();
            return Ok(Node {
                kind: NodeKind::Project { version },
                dependencies: self.project_edges(&edge.id)?,
            });
        }
        let Some(version) = self.cache.resolve(&edge.id, &edge.range)? else {
            return Ok(Node {
                kind: NodeKind::Unresolved {
                    version: edge.range.min_version().cloned(),
                },
                dependencies: Rc::new(Vec::new()),
            });
        };
        let package = self
            .cache
            .read(&PackageIdentity::new(edge.id.clone(), version))?;
        let dependencies = self.package_edges(&package)?;
        Ok(Node {
            kind: NodeKind::Package { package },
            dependencies,
        })
    }

    /// Queue `item`, starting its read: the version its edge resolves to
    /// among the installed ones joins the examined set.
    fn enqueue(&mut self, queue: &mut VecDeque<Item>, item: Item) -> Result<(), ResolveDecline> {
        self.enqueued += 1;
        if self.enqueued > MAX_ITEMS {
            return Err(ResolveDecline::GraphTooLarge);
        }
        if let Some(edge) = &item.edge
            && edge.constraint.allows_package()
            && let Some(version) = self.cache.resolve(&edge.id, &edge.range)?
        {
            let identity = PackageIdentity::new(edge.id.clone(), version);
            self.examined.insert(
                (identity.id.clone(), identity.version_folder_name()),
                identity,
            );
        }
        queue.push_back(item);
        Ok(())
    }

    /// `ResolveDependencyGraphItemsAsync`: the chosen item of every name.
    fn choose(&mut self) -> Result<BTreeMap<NameIndex, Chosen>, ResolveDecline> {
        let mut evictions: BTreeMap<RangeIndex, Eviction> = BTreeMap::new();
        'pass: loop {
            let mut queue: VecDeque<Item> = VecDeque::new();
            let mut chosen: BTreeMap<NameIndex, Chosen> = BTreeMap::new();
            let mut direct_references: BTreeSet<NameIndex> = BTreeSet::new();
            // The path of the root's own references, which a pinned item
            // takes too (`_rootedDependencyPath`).
            let rooted = Rc::new(vec![ROOT_RANGE]);
            self.enqueue(&mut queue, Item::root())?;

            while let Some(current) = queue.pop_front() {
                let is_root = current.name == ROOT_NAME;

                if let Some(eviction) = evictions.get(&current.range_index) {
                    // An evicted nuspec dependency or project reference does
                    // not evict a project's package reference at the same
                    // range: that is the item the engine wants.
                    let exempt = matches!(
                        eviction.constraint,
                        Constraint::PackageProjectExternal | Constraint::ExternalProject
                    ) && current.constraint() == Some(Constraint::Package);
                    if !exempt {
                        continue;
                    }
                }

                let decision = match chosen.get_mut(&current.name) {
                    None => Decision::Choose,
                    Some(existing) => decide(existing, &current),
                };
                match decision {
                    Decision::Skip => continue,
                    Decision::Eclipse(parent) => {
                        chosen
                            .get_mut(&current.name)
                            .expect("decided against the chosen item")
                            .eclipsed_parents
                            .insert(parent);
                        continue;
                    }
                    Decision::Evict => {
                        let existing = chosen
                            .remove(&current.name)
                            .expect("decided against the chosen item");
                        let evicted = existing.item.range_index;
                        let mut start_over = false;
                        // The evictee's own evictions go with it, except one
                        // of a project-shaped item of this very name, which
                        // stays evicted in favour of the package.
                        let stale: Vec<RangeIndex> = evictions
                            .iter()
                            .filter(|(_, eviction)| {
                                eviction.path.contains(&evicted)
                                    && !(eviction.name == current.name
                                        && matches!(
                                            eviction.constraint,
                                            Constraint::ExternalProject
                                                | Constraint::PackageProjectExternal
                                        ))
                            })
                            .map(|(key, _)| *key)
                            .collect();
                        if !stale.is_empty() {
                            start_over = true;
                            for key in stale {
                                evictions.remove(&key);
                            }
                        }
                        if chosen
                            .values()
                            .any(|other| other.item.path.contains(&evicted))
                        {
                            start_over = true;
                        }
                        evictions.insert(
                            evicted,
                            Eviction {
                                path: extended(&current.path, current.range_index),
                                name: current.name,
                                constraint: existing
                                    .item
                                    .constraint()
                                    .expect("the root is never evicted"),
                            },
                        );
                        if start_over {
                            continue 'pass;
                        }
                        let node = self.node_of(&current)?;
                        chosen.insert(
                            current.name,
                            Chosen::new(current.clone(), node, vec![current.suppressions.clone()]),
                        );
                        queue.retain(|item| !item.path.contains(&evicted));
                    }
                    Decision::Choose => {
                        let node = self.node_of(&current)?;
                        chosen.insert(
                            current.name,
                            Chosen::new(current.clone(), node, vec![current.suppressions.clone()]),
                        );
                    }
                    Decision::Replace(suppressions) => {
                        let node = self.node_of(&current)?;
                        let existing = &chosen[&current.name];
                        let replacement =
                            Chosen::replacing(existing, current.clone(), node, suppressions);
                        chosen.insert(current.name, replacement);
                    }
                }

                let dependencies = chosen[&current.name].node.dependencies.clone();
                if is_root {
                    direct_references = dependencies
                        .iter()
                        .filter(|dependency| dependency.constraint.allows_package())
                        .map(|dependency| dependency.name)
                        .collect();
                }
                // The names suppressed below this item: its own suppressed
                // dependencies (not for the root, whose every reference is
                // walked) and everything suppressed above it.
                let suppressions: Rc<BTreeSet<NameIndex>> = if is_root {
                    current.suppressions.clone()
                } else {
                    let own: BTreeSet<NameIndex> = dependencies
                        .iter()
                        .filter(|dependency| dependency.suppress_parent)
                        .map(|dependency| dependency.name)
                        .collect();
                    if own.is_empty() {
                        current.suppressions.clone()
                    } else {
                        Rc::new(own.union(&current.suppressions).copied().collect())
                    }
                };
                let path = Rc::new(extended(&current.path, current.range_index));
                for dependency in dependencies.iter() {
                    // A dependency named like the root project is left to the
                    // flattening, which reports it as a cycle.
                    if dependency.name == ROOT_NAME {
                        continue;
                    }
                    let is_root_package_reference =
                        is_root && dependency.constraint.allows_package();
                    // A pinned item's own dependencies are walked whatever is
                    // suppressed above it.
                    if !current.pinned && suppressions.contains(&dependency.name) {
                        continue;
                    }
                    if !is_root_package_reference && direct_references.contains(&dependency.name) {
                        continue;
                    }
                    // Chosen already at this very range (the dependency's own,
                    // before pinning), without suppressions: its children are
                    // in the queue.
                    if let Some(other) = chosen.get_mut(&dependency.name)
                        && other.item.range_index == dependency.range_index
                        && other.unsuppressed()
                    {
                        if !other.item.is_root_package_reference {
                            other.add_parent(current.range_index);
                        }
                        continue;
                    }
                    let pin = self
                        .graph
                        .transitive_pinning
                        .as_ref()
                        .filter(|_| dependency.constraint.allows_package())
                        .and_then(|pins| pins.get(&dependency.id));
                    let pinned = pin.is_some();
                    // A transitive edge with a central version is raised to
                    // it and walked as though the root referenced it.
                    let (edge, item_path) = match pin {
                        Some(range) if !is_root_package_reference => (
                            self.intern_edge(
                                dependency.id.clone(),
                                range.clone(),
                                dependency.constraint,
                                dependency.suppress_parent,
                                None,
                            )?,
                            rooted.clone(),
                        ),
                        _ => (dependency.clone(), path.clone()),
                    };
                    self.enqueue(
                        &mut queue,
                        Item {
                            name: edge.name,
                            range_index: edge.range_index,
                            edge: Some(edge),
                            path: item_path,
                            suppressions: suppressions.clone(),
                            is_root_package_reference,
                            parent: Some(current.range_index),
                            pinned,
                        },
                    )?;
                }
            }
            return Ok(chosen);
        }
    }

    /// `CreateRestoreTargetGraphAsync`: walk the chosen items' dependencies
    /// for cycles, conflicts and downgrades, and return what the walk reaches.
    fn flatten(&self, chosen: &BTreeMap<NameIndex, Chosen>) -> Result<Flattened, ResolveDecline> {
        let pinning = self.graph.transitive_pinning.is_some();
        let mut visited: BTreeSet<NameIndex> = BTreeSet::new();
        // `nodesById`: the range indices a node was created for.
        let mut nodes: BTreeSet<RangeIndex> = BTreeSet::from([ROOT_RANGE]);
        let mut queue: VecDeque<NameIndex> = VecDeque::from([ROOT_NAME]);
        let mut flattened: Vec<NameIndex> = Vec::new();
        let mut resolved_dependencies: Vec<ResolvedDependency> = Vec::new();
        let mut cycle: Option<Vec<PackageId>> = None;
        let mut conflicts: BTreeMap<RangeIndex, (PackageId, NuGetVersion, VersionRange)> =
            BTreeMap::new();
        let mut downgrades: BTreeMap<RangeIndex, DowngradeRecord> = BTreeMap::new();
        let mut unresolved: Option<(PackageId, VersionRange)> = None;

        while let Some(name) = queue.pop_front() {
            let Some(item) = chosen.get(&name) else {
                continue;
            };
            flattened.push(name);
            for edge in item.node.dependencies.iter() {
                if edge.name == name || edge.name == ROOT_NAME {
                    cycle.get_or_insert_with(|| self.cycle_through(item, edge, None));
                    continue;
                }
                let Some(child) = chosen.get(&edge.name) else {
                    continue;
                };
                if !visited.insert(edge.name) {
                    if item.item.path.contains(&edge.range_index) {
                        cycle.get_or_insert_with(|| {
                            self.cycle_through(item, edge, Some(edge.range_index))
                        });
                        continue;
                    }
                    let child_range = child
                        .item
                        .range()
                        .expect("a chosen item that is not the root has a range");
                    if !is_greater_or_equal(child_range, &edge.range) {
                        if edge.suppress_parent || item.suppressions[0].contains(&edge.name) {
                            continue;
                        }
                        if child
                            .node
                            .version()
                            .is_some_and(|version| edge.range.satisfies(version))
                        {
                            continue;
                        }
                        if let std::collections::btree_map::Entry::Vacant(record) =
                            downgrades.entry(child.item.range_index)
                        {
                            if child
                                .eclipsed_parents
                                .iter()
                                .any(|parent| item.item.path.contains(parent))
                            {
                                continue;
                            }
                            // A pinned package is downgraded below the first
                            // of its parents this item descends from; failing
                            // that, below its own parent, unless it is a
                            // direct reference under transitive pinning,
                            // whose downgrade restore does not record.
                            let to_parent = child
                                .parents
                                .iter()
                                .flatten()
                                .find(|parent| {
                                    item.item.path.contains(parent)
                                        && !item.item.is_root_package_reference
                                })
                                .copied()
                                .or_else(|| {
                                    (!pinning || !child.item.is_root_package_reference).then(|| {
                                        *child.item.path.last().expect(
                                            "a chosen item that is not the root has a parent",
                                        )
                                    })
                                });
                            if let Some(to_parent) = to_parent {
                                record.insert(DowngradeRecord {
                                    from: item.item.range_index,
                                    to_parent,
                                    to: edge.name,
                                    id: edge.id.clone(),
                                    selected: child.node.version().cloned(),
                                    required: edge.range.clone(),
                                    central: pinning && child.item.pinned,
                                });
                            }
                        }
                        continue;
                    }
                    if conflicts.contains_key(&child.item.range_index) {
                        nodes.insert(edge.range_index);
                    }
                    continue;
                }
                // Under transitive pinning, a first visit that demands more
                // than the chosen range is a downgrade below the root
                // (`NU1109`), the node made all the same. (Restore exempts an
                // edge whose range is the `VersionRange.All` instance, which
                // only a dependency without a range carries; that declines
                // before here.)
                if pinning
                    && !child.item.is_root_package_reference
                    && !edge.suppress_parent
                    && !downgrades.contains_key(&child.item.range_index)
                    && !is_greater_or_equal(
                        child
                            .item
                            .range()
                            .expect("a chosen item that is not the root has a range"),
                        &edge.range,
                    )
                {
                    downgrades.insert(
                        child.item.range_index,
                        DowngradeRecord {
                            from: item.item.range_index,
                            to_parent: ROOT_RANGE,
                            to: edge.name,
                            id: edge.id.clone(),
                            selected: child.node.version().cloned(),
                            required: edge.range.clone(),
                            central: true,
                        },
                    );
                }
                if let NodeKind::Package { package } = &child.node.kind
                    && !conflicts.contains_key(&child.item.range_index)
                    && !edge.suppress_parent
                    && !edge.range.satisfies(&package.package.identity.version)
                    && !downgrades.contains_key(&child.item.range_index)
                {
                    conflicts.insert(
                        child.item.range_index,
                        (
                            edge.id.clone(),
                            package.package.identity.version.clone(),
                            edge.range.clone(),
                        ),
                    );
                    continue;
                }
                nodes.insert(child.item.range_index);
                queue.push_back(edge.name);
                if let NodeKind::Package { package } = &child.node.kind {
                    resolved_dependencies.push(ResolvedDependency {
                        id: package.package.identity.id.clone(),
                        range: child
                            .item
                            .range()
                            .expect("a chosen item that is not the root has a range")
                            .clone(),
                        version: package.package.identity.version.clone(),
                    });
                }
                if let NodeKind::Unresolved { .. } = child.node.kind
                    && unresolved.is_none()
                {
                    let range = child
                        .item
                        .range()
                        .expect("a chosen item that is not the root has a range");
                    unresolved = Some((edge.id.clone(), range.clone()));
                }
            }
        }

        // A package no installed version satisfies, in the graph: restore
        // fails (NU1101), whatever else it reports.
        if let Some((id, range)) = unresolved {
            let identity = PackageIdentity::new(id, lower_bound_or_zero(&range));
            let metadata_path = PackagePaths::new(self.cache.root, &identity).metadata_path;
            return Err(ResolveDecline::PackageRead {
                identity,
                source: Box::new(PackageReadError::NotInstalled { metadata_path }),
            });
        }
        if let Some(cycle) = cycle {
            return Err(ResolveDecline::DependencyCycle { cycle });
        }
        for (index, (id, selected, range)) in &conflicts {
            if nodes.contains(index) {
                return Err(ResolveDecline::VersionConflict {
                    id: id.clone(),
                    selected: Box::new(selected.clone()),
                    range: Box::new(range.clone()),
                });
            }
        }
        for downgrade in downgrades.values() {
            if nodes.contains(&downgrade.from)
                && nodes.contains(&downgrade.to_parent)
                && chosen.contains_key(&downgrade.to)
            {
                return Err(ResolveDecline::Downgrade {
                    id: downgrade.id.clone(),
                    selected: Box::new(
                        downgrade
                            .selected
                            .clone()
                            .expect("a downgraded item that is in the graph resolved to a version"),
                    ),
                    required: Box::new(downgrade.required.clone()),
                    central: downgrade.central,
                });
            }
        }
        Ok(Flattened {
            names: flattened,
            resolved_dependencies,
        })
    }

    /// The cycle an edge of `item` closes, as the ids along `item`'s path
    /// (from `from`, when the cycle starts there), then `item`'s, then the
    /// edge's.
    fn cycle_through(
        &self,
        item: &Chosen,
        edge: &Edge,
        from: Option<RangeIndex>,
    ) -> Vec<PackageId> {
        let start = from
            .and_then(|index| item.item.path.iter().position(|entry| *entry == index))
            .unwrap_or(0);
        let mut cycle: Vec<PackageId> = item.item.path[start..]
            .iter()
            .filter_map(|index| self.range_id(*index))
            .collect();
        if let Some(own) = &item.item.edge {
            cycle.push(own.id.clone());
        }
        cycle.push(edge.id.clone());
        cycle
    }

    /// The id a range index is of, if its name is a package id.
    fn range_id(&self, index: RangeIndex) -> Option<PackageId> {
        let (name, _) = self.ranges[index.0 as usize].as_ref()?;
        self.names[name.0 as usize].clone()
    }
}

/// What the engine does with `current`, whose name `existing` is chosen for.
/// An item at the very same range is recorded among a pinned item's parents.
fn decide(existing: &mut Chosen, current: &Item) -> Decision {
    // A direct package reference is chosen whatever a transitive edge says.
    if existing.item.is_root_package_reference {
        return Decision::Skip;
    }
    let existing_range = existing
        .item
        .range()
        .expect("a chosen item that is not the root has a range");
    let current_range = current
        .range()
        .expect("an item of a chosen name is not the root");

    if !is_greater_or_equal(existing_range, current_range) {
        // NuGet first skips an item whose grandparent is among a pinned
        // chosen item's parents. Every package item of a pinned name carries
        // the one pinned range, which is never greater, so none reaches here.
        debug_assert!(existing.parents.is_none(), "a pinned item is never evicted");
        // The current item demands more. Between packages, it does not evict
        // from within the chosen item's own branch, nor from below a parent
        // the chosen item has already eclipsed.
        if existing.item.allows_package() && current.allows_package() {
            if has_common_ancestor(&existing.item.path, &current.path) {
                return Decision::Skip;
            }
            if existing
                .eclipsed_parents
                .iter()
                .any(|parent| current.path.contains(parent))
            {
                return Decision::Skip;
            }
        }
        return Decision::Evict;
    }

    if !precisely_equal(existing_range, current_range) {
        // The current item is lower (or merely bounded differently above).
        return if has_common_ancestor(&existing.item.path, &current.path) {
            Decision::Skip
        } else {
            Decision::Eclipse(
                *current
                    .path
                    .last()
                    .expect("an item of a chosen name has a parent"),
            )
        };
    }

    // The very same range. A second occurrence from another branch replaces
    // the chosen one, path and all, and its children are enqueued again
    // (those already chosen at their range are not).
    existing.add_parent(
        current
            .parent
            .expect("an item of a chosen name has a parent"),
    );
    if existing.unsuppressed() && has_common_ancestor(&existing.item.path, &current.path) {
        return Decision::Skip;
    }
    if current.suppressions.is_empty() {
        return Decision::Replace(vec![current.suppressions.clone()]);
    }
    if existing
        .suppressions
        .iter()
        .any(|suppressions| current.suppressions.is_superset(suppressions))
    {
        return Decision::Skip;
    }
    let mut suppressions = vec![current.suppressions.clone()];
    suppressions.extend(existing.suppressions.iter().cloned());
    Decision::Replace(suppressions)
}

/// `RemoteDependencyWalker.IsGreaterThanOrEqualTo` for non-floating ranges: is
/// the *nearer* range's lower bound at least the deeper one's? Inclusivity
/// plays no part.
fn is_greater_or_equal(near: &VersionRange, far: &VersionRange) -> bool {
    debug_assert!(
        !near.is_floating() && !far.is_floating(),
        "a floating edge declines before it is compared"
    );
    match (near.min_version(), far.min_version()) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(near), Some(far)) => near >= far,
    }
}

/// `DependencyGraphResolver.VersionRangePreciseEquals`: the bounds (by
/// NuGet's version identity) and their inclusivity (and the float, absent
/// here) agree.
fn precisely_equal(a: &VersionRange, b: &VersionRange) -> bool {
    RangeKey::of(a) == RangeKey::of(b)
}

/// `DependencyGraphResolver.HasCommonAncestor`: one path is a prefix of the
/// other.
fn has_common_ancestor(left: &[RangeIndex], right: &[RangeIndex]) -> bool {
    left.iter().zip(right).all(|(left, right)| left == right)
}

/// `DependencyGraphItemIndexer.CreatePathToRef`.
fn extended(path: &[RangeIndex], index: RangeIndex) -> Vec<RangeIndex> {
    let mut extended = Vec::with_capacity(path.len() + 1);
    extended.extend_from_slice(path);
    extended.push(index);
    extended
}

/// A range's lower bound, or `0.0.0` for one without: the version a decline
/// names for an edge that resolved to nothing.
fn lower_bound_or_zero(range: &VersionRange) -> NuGetVersion {
    range
        .min_version()
        .cloned()
        .unwrap_or_else(|| NuGetVersion::parse("0.0.0").expect("a literal version"))
}

/// An installed package as the engine needs it: where it lives, and the
/// dependencies its nuspec declares for the project's target framework.
struct CachedPackage {
    package: InstalledPackage,
    dependencies: Vec<crate::PackageDependency>,
}

/// The packages folder, as restore's only source: which versions of each
/// package are installed, and each package read once.
struct PackageCache<'a> {
    root: &'a Path,
    framework: &'a NuGetFramework,
    /// Which dependencies each package contributes; see [`TargetContext`].
    context: &'a TargetContext,
    packages: BTreeMap<(PackageId, String), Rc<CachedPackage>>,
    installed: BTreeMap<PackageId, Vec<NuGetVersion>>,
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
            installed: BTreeMap::new(),
        }
    }

    /// The versions of `id` restore treats as installed, every one committed.
    /// A folder that will not list declines, naming `range`'s lower bound, and
    /// so does a version folder holding the hash file but no commit marker:
    /// restore counts it and would repair it, and nothing here can read it.
    fn installed(
        &mut self,
        id: &PackageId,
        range: &VersionRange,
    ) -> Result<&[NuGetVersion], ResolveDecline> {
        if !self.installed.contains_key(id) {
            let mut versions = Vec::new();
            let listed = list_installed_package_versions(self.root, id).map_err(|error| {
                let PackageCacheError::Io { path, source } = error;
                ResolveDecline::PackageRead {
                    identity: PackageIdentity::new(id.clone(), lower_bound_or_zero(range)),
                    source: Box::new(PackageReadError::Io { path, source }),
                }
            })?;
            for installed in listed {
                if !installed.committed {
                    return Err(ResolveDecline::PackageRead {
                        identity: installed.identity,
                        source: Box::new(PackageReadError::HashOnly {
                            hash_path: installed.paths.hash_path,
                            metadata_path: installed.paths.metadata_path,
                        }),
                    });
                }
                versions.push(installed.identity.version);
            }
            self.installed.insert(id.clone(), versions);
        }
        Ok(&self.installed[id])
    }

    /// The version restore's lookup resolves an edge to with the packages
    /// folder its only source (`SourceRepositoryDependencyProvider.
    /// FindLibraryCoreAsync`): the range's inclusive lower bound itself when
    /// its canonical folder is installed (`DoesPackageExistAsync`, whether or
    /// not the bound satisfies the range; on a case-insensitive file system
    /// that path reaches a folder spelled in another case), and otherwise the
    /// lowest installed version satisfying the range, a prerelease only where
    /// a bound of the range is one (`VersionRange.IsBetter`); `None` where
    /// there is none. Installed versions that compare equal are one folder
    /// each, spelled apart; restore's best match keeps whichever it enumerated
    /// first, so a tie declines.
    fn resolve(
        &mut self,
        id: &PackageId,
        range: &VersionRange,
    ) -> Result<Option<NuGetVersion>, ResolveDecline> {
        debug_assert!(
            !range.is_floating(),
            "a floating edge declines before it resolves"
        );
        let root = self.root;
        let installed = self.installed(id, range)?;
        if let Some(bound) = range.min_version()
            && range.is_min_inclusive()
        {
            let identity = PackageIdentity::new(id.clone(), bound.clone());
            let paths = PackagePaths::new(root, &identity);
            let io = |path: &Path| {
                let path = path.to_path_buf();
                move |source| ResolveDecline::PackageRead {
                    identity: PackageIdentity::new(id.clone(), bound.clone()),
                    source: Box::new(PackageReadError::Io { path, source }),
                }
            };
            let committed = paths.is_committed().map_err(io(&paths.metadata_path))?;
            if committed {
                return Ok(Some(bound.clone()));
            }
            if paths.has_hash_file().map_err(io(&paths.hash_path))? {
                return Err(ResolveDecline::PackageRead {
                    identity,
                    source: Box::new(PackageReadError::HashOnly {
                        hash_path: paths.hash_path,
                        metadata_path: paths.metadata_path,
                    }),
                });
            }
        }
        let prerelease_bounds = range.min_version().is_some_and(NuGetVersion::is_prerelease)
            || range.max_version().is_some_and(NuGetVersion::is_prerelease);
        let candidates: Vec<&NuGetVersion> = installed
            .iter()
            .filter(|version| prerelease_bounds || !version.is_prerelease())
            .filter(|version| range.satisfies(version))
            .collect();
        let Some(best) = candidates.iter().copied().min() else {
            return Ok(None);
        };
        let equivalent: Vec<NuGetVersion> = candidates
            .iter()
            .filter(|version| **version == best)
            .map(|version| (*version).clone())
            .collect();
        if equivalent.len() > 1 {
            return Err(ResolveDecline::EquivalentVersions {
                id: id.clone(),
                range: Box::new(range.clone()),
                versions: equivalent,
            });
        }
        Ok(Some(best.clone()))
    }

    /// The package, read once; a package that will not read declines.
    fn read(&mut self, identity: &PackageIdentity) -> Result<Rc<CachedPackage>, ResolveDecline> {
        let key = (identity.id.clone(), identity.version_folder_name());
        if let Some(cached) = self.packages.get(&key) {
            return Ok(cached.clone());
        }
        let package = read_installed_package(self.root, identity.clone()).map_err(|source| {
            ResolveDecline::PackageRead {
                identity: identity.clone(),
                source: Box::new(source),
            }
        })?;
        let dependencies = self.context.dependencies(&package.nuspec, self.framework);
        let cached = Rc::new(CachedPackage {
            package,
            dependencies,
        });
        self.packages.insert(key, cached.clone());
        Ok(cached)
    }
}
