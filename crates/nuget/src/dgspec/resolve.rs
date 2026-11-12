//! B₁ of `docs/nuget-dgspec-boundary-plan.md`: the offline resolver
//! ([`crate::resolve_offline_in`]) consuming a parsed
//! dependency-graph spec.
//!
//! [`resolve_dgspec`] commits only where NuGet's restore over the same spec
//! (B₀: `RestoreRunner`, run by the NuGet the caller names) would succeed and
//! write the same package closure for every target framework of the project.
//! `tests/all/dgspec_resolve_diff.rs` holds it to that.
//!
//! # Every field has a verdict
//!
//! The resolver models a project's direct package references, and its
//! project references as nodes of the graph ([`project_graph`]), and nothing
//! else. Every other field of the spec is in one of three classes, and
//! [`unmodelled_fields`] destructures every struct the spec is made of, so a
//! field added to the model does not compile until it has been put in one:
//!
//! - **Modelled.** Direct dependencies; project references, and the
//!   referenced projects' frameworks, dependencies and references; each
//!   framework's asset-target fallback list and packages to prune; the
//!   restored project's transitive pinning and central versions; the
//!   packages folder the resolver reads.
//! - **Inert.** Fields that cannot move the closure, or turn a success into a
//!   failure, of a restore the resolver commits to. Each one's reason is
//!   stated at the point it is discarded. The reasons the differential cannot
//!   check by construction are named there too.
//! - **Declined when present.** Each is an [`UnmodelledField`], reported all at
//!   once rather than at the first, so the corpus census can count how many
//!   projects each one blocks (stages 5–7 are ordered by those counts).
//!
//! # Restore's compatibility check
//!
//! After resolving, restore fails (`NU1202`) on a package with `lib/` or `ref/`
//! assets none of which suit the framework. The resolver reads only nuspecs, so
//! [`resolve_dgspec`] reads each resolved package's files and commits only for
//! packages with no assets, or with compile assets for the framework.
//!
//! # What a package must be
//!
//! Restore reads every version it considers, winners and losers, and throws
//! or fails on a nuspec or commit marker it rejects. Which inputs it rejects
//! is not something a list can bound, so B₁ states what it *admits* instead:
//! [`super::nuspec`] walks every byte, declaration pseudo-attribute, element,
//! attribute and text node of a nuspec and declines anything outside its
//! domain, layer by layer, and [`commit_marker_is_admitted`] does the same for
//! the marker's JSON. [`check_metadata`] holds every version restore may read
//! ([`ResolvedPackageClosure::examined`]) to both.
//!
//! # The whole project, or nothing
//!
//! Restore succeeds or fails per *project*, not per framework: one framework
//! failing (`NU1107`, say) fails the restore, and the LSP reads no assets file
//! from a failed restore. So [`resolve_dgspec`] resolves every framework of the
//! project and commits only when all of them commit.
//!
//! # The NuGet that would restore
//!
//! A NuGet release can change resolution while emitting the same spec, so the
//! caller names the NuGet that would perform the restore ([`NuGetIdentity`])
//! and only an identity on the verified list (`verified_nuget.txt`) commits.
//! The list is exactly the set of identities the differential grades in CI:
//! `dgspec_resolve_diff.rs` derives it from the oracle and fails when the two
//! differ, so nothing reaches it by hand.

use super::environment::{EnvironmentDecline, check_environment};
use super::nuspec::{NuspecDecline, nuspec_is_admitted};
use super::{
    DependencyGraphSpec, DownloadDependency, FrameworkReference, LibraryDependency,
    LibraryDependencyTarget, LibraryIncludeFlags, NuGetLogCode, PackageRangeEntry, PackageSpec,
    ProjectRestoreMetadata, ProjectRestoreReference, RestoreAuditProperties, RestoreFrameworkInfo,
    RestoreLockProperties, TargetFrameworkInformation, WarningProperties,
};
use crate::assets::content_model_framework;
use crate::{
    AssetSelectionDecline, EdgeTarget, NuGetFramework, NuGetVersion, PackageId, PackageIdentity,
    PackagePaths, PackageReadError, ProjectEdge, ProjectGraph, ProjectNode, ResolveDecline,
    ResolvedPackageClosure, TargetContext, VersionRange, list_package_files,
    read_installed_package, resolve_graph, select_compile_assets,
};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

/// The checked-in verified list, in the manifest format of
/// `borzoi_oracle_harness::manifest`: comment lines start with `#`, and every
/// other line is one identity.
const VERIFIED: &str = include_str!("verified_nuget.txt");

/// The identity of the NuGet that would perform a restore: the
/// `AssemblyInformationalVersion` of the `NuGet.Commands.dll` in the SDK that
/// `dotnet` resolves for the project. It names the commit the SDK's NuGet was
/// built from, and is the same on every platform's build of one SDK.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NuGetIdentity(String);

impl NuGetIdentity {
    pub fn new(informational_version: impl Into<String>) -> NuGetIdentity {
        NuGetIdentity(informational_version.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this identity is on the verified list.
    pub fn is_verified(&self) -> bool {
        verified_nuget_identities().any(|verified| verified == self.0)
    }
}

impl fmt::Display for NuGetIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The verified list: every NuGet identity [`resolve_dgspec`] commits for.
pub fn verified_nuget_identities() -> impl Iterator<Item = &'static str> {
    VERIFIED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// A field of the spec, present with a value the resolver does not model, that
/// makes [`resolve_dgspec`] decline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnmodelledField {
    /// A project's `restore.projectName` is absent or empty (restore requires
    /// it), a referenced project's is not a package id (its identity in the
    /// graph), or two projects of the spec share one ignoring case.
    ProjectName,
    /// `restore.packagesPath` is absent, so NuGet would find the packages
    /// folder from the environment and config files, or holds a control
    /// character.
    PackagesPath,
    /// A source that is not a local absolute path. A local folder is read
    /// only for a package the graph names, which [`DgSpecResolveDecline::SourceHoldsPackage`]
    /// declines; a remote source can fail the restore by being unreachable.
    Sources,
    /// Fallback package folders, which NuGet reads packages from as though
    /// they were the packages folder.
    FallbackFolders,
    /// A lock file is in use, or might be: it is requested, locked mode is on,
    /// its path is not stated, or a file exists at the stated path. Restore
    /// then resolves the lock file's graph, not the dependencies'.
    LockFile,
    /// Audit is not explicitly off. It reads vulnerability data from the
    /// sources and can warn.
    Audit,
    /// `usingMicrosoftNETSdk` is false.
    NotMicrosoftNetSdk,
    /// Central package management in a shape B₁ does not admit: under it, a
    /// dependency not centrally managed, an implicit one, or a floating
    /// central version or override (restore fails on each); without it, a
    /// centrally managed dependency, an override or a central entry.
    CentralPackageManagement,
    /// A framework's `packagesToPrune` entry without an upper bound, or a
    /// floating one, whose pruning is not modelled.
    PackagesToPrune,
    /// An `imports` list that is not an asset-target fallback (the legacy
    /// `PackageTargetFallback`).
    PackageTargetFallback,
    /// A dependency whose `target` is not `Package`, which a project could
    /// satisfy.
    DependencyTarget,
    /// The project has no target framework.
    NoFrameworks,
    /// Metadata NuGet requires is absent or inconsistent, so restore throws:
    /// the project's unique name, path or output path, or `originalTargetFrameworks`
    /// naming exactly the aliases of the frameworks and of the restore
    /// metadata's frameworks, each alias once ignoring case, in the characters
    /// the SDK writes, and each naming a different framework.
    RequiredMetadata,
    /// A config file NuGet loads its settings from is outside the vocabulary
    /// B₁ admits (see `config_is_admitted`).
    Config,
    /// The spec restores more than this project, or holds a project no chain
    /// of references from this one reaches. NuGet validates the whole graph
    /// before restoring any of it, so either can fail this project's restore
    /// for a reason outside its own graph.
    OtherProjects,
}

impl UnmodelledField {
    /// The field's name in the census.
    pub fn name(self) -> &'static str {
        match self {
            UnmodelledField::ProjectName => "project-name",
            UnmodelledField::PackagesPath => "packages-path",
            UnmodelledField::Sources => "sources",
            UnmodelledField::FallbackFolders => "fallback-folders",
            UnmodelledField::LockFile => "lock-file",
            UnmodelledField::Audit => "audit",
            UnmodelledField::NotMicrosoftNetSdk => "not-microsoft-net-sdk",
            UnmodelledField::CentralPackageManagement => "central-package-management",
            UnmodelledField::PackagesToPrune => "packages-to-prune",
            UnmodelledField::PackageTargetFallback => "package-target-fallback",
            UnmodelledField::DependencyTarget => "dependency-target",
            UnmodelledField::NoFrameworks => "no-frameworks",
            UnmodelledField::RequiredMetadata => "required-metadata",
            UnmodelledField::Config => "config",
            UnmodelledField::OtherProjects => "other-projects",
        }
    }
}

/// Why [`resolve_dgspec`] declined.
#[derive(Debug)]
pub enum DgSpecResolveDecline {
    /// The NuGet that would restore is not on the verified list.
    UnverifiedNuGet { identity: NuGetIdentity },
    /// The spec's `restore` list does not name the project, or no project in
    /// the spec has that unique name, so NuGet would not restore it.
    ProjectNotRestored { project: String },
    /// Fields the resolver does not model; never empty.
    Unmodelled { fields: BTreeSet<UnmodelledField> },
    /// The resolver declined on one of the project's frameworks.
    Resolve {
        target_alias: String,
        decline: Box<ResolveDecline>,
    },
    /// A framework's project reference (the project's own, or a referenced
    /// project's) names a project by a unique name the spec does not hold.
    /// Restore fails on it (`NU1105`).
    ProjectReferenceMissing {
        target_alias: String,
        project: String,
    },
    /// No framework of a referenced project is compatible with this
    /// framework itself. Restore fails (`NU1201`) when none is compatible
    /// through the fallback list either, and otherwise walks the one it
    /// selects through that list, which B₁ does not model.
    ProjectIncompatible {
        target_alias: String,
        project: String,
    },
    /// Restore would log a warning for a framework that the project's warning
    /// policy promotes to an error, so it fails; `code` is `None` when B₁
    /// cannot predict the framework's warnings exactly and the policy promotes
    /// any.
    PromotedWarning {
        target_alias: String,
        code: Option<NuGetLogCode>,
    },
    /// A package in a framework's closure carries assets, but none the
    /// framework is shown to compile against, so restore's compatibility
    /// check (`NU1202`) may fail it. `decline` says why compile-asset
    /// selection could not answer, when it could not.
    CompatibilityUnproven {
        target_alias: String,
        package: Box<PackageIdentity>,
        decline: Option<Box<AssetSelectionDecline>>,
    },
    /// A framework's `downloadDependencies` entry that B₁ cannot show is in
    /// the packages folder: its range is not one exact version, or that
    /// version is not committed there. Restore fails on a missing download
    /// (`NU1101`).
    DownloadUnavailable {
        target_alias: String,
        id: PackageId,
        range: Box<VersionRange>,
    },
    /// A package restore would read (a winner or a losing version) has a
    /// nuspec or a commit marker outside the domain B₁ admits (the nuspec's is
    /// stated in `dgspec::nuspec`; the marker's in `commit_marker_is_admitted`).
    PackageMetadata {
        target_alias: String,
        package: Box<PackageIdentity>,
        decline: PackageMetadataDecline,
    },
    /// A package in a framework's closure could not be read back for the
    /// compatibility check.
    PackageRead {
        target_alias: String,
        package: Box<PackageIdentity>,
        source: Box<PackageReadError>,
    },
    /// A local folder source holds `entry`, named like a package restore looks
    /// up past the packages folder (an edge whose inclusive lower bound is not
    /// installed), or does not exist or could not be listed (`entry` is
    /// `None`). Restore resolves such an edge to the lowest version satisfying
    /// it across the packages folder and every source, and fails (`NU1301`)
    /// on a source it cannot read, so the resolver's account of the graph and
    /// of what restore reads ([`ResolvedPackageClosure::examined`]) holds only
    /// while every source is a folder holding no version of the package. See
    /// `check_sources`.
    SourceHoldsPackage {
        target_alias: String,
        source: String,
        entry: Option<String>,
    },
    /// The environment restore would run under is outside what B₁ admits.
    Environment { decline: EnvironmentDecline },
}

impl fmt::Display for DgSpecResolveDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DgSpecResolveDecline::UnverifiedNuGet { identity } => {
                write!(f, "NuGet {identity} is not on the verified list")
            }
            DgSpecResolveDecline::ProjectNotRestored { project } => {
                write!(f, "the spec does not restore {project:?}")
            }
            DgSpecResolveDecline::Unmodelled { fields } => {
                let names: Vec<&str> = fields.iter().map(|field| field.name()).collect();
                write!(f, "the spec uses unmodelled fields: {}", names.join(", "))
            }
            DgSpecResolveDecline::Resolve {
                target_alias,
                decline,
            } => write!(f, "framework {target_alias:?}: {decline}"),
            DgSpecResolveDecline::ProjectReferenceMissing {
                target_alias,
                project,
            } => write!(
                f,
                "framework {target_alias:?}: a project reference names {project:?}, which the \
                 spec does not hold (restore fails with NU1105)"
            ),
            DgSpecResolveDecline::ProjectIncompatible {
                target_alias,
                project,
            } => write!(
                f,
                "framework {target_alias:?}: no framework of the referenced project {project:?} \
                 is compatible with it"
            ),
            DgSpecResolveDecline::PromotedWarning { target_alias, code } => match code {
                Some(code) => write!(
                    f,
                    "framework {target_alias:?}: restore warns {code}, which the warning policy \
                     makes an error"
                ),
                None => write!(
                    f,
                    "framework {target_alias:?}: the warning policy promotes warnings this \
                     framework's may include"
                ),
            },
            DgSpecResolveDecline::CompatibilityUnproven {
                target_alias,
                package,
                decline,
            } => {
                write!(
                    f,
                    "framework {target_alias:?}: package {} {} has assets, but none it is shown \
                     to compile against",
                    package.id, package.version
                )?;
                match decline {
                    Some(decline) => write!(f, " ({decline})"),
                    None => Ok(()),
                }
            }
            DgSpecResolveDecline::PackageMetadata {
                target_alias,
                package,
                decline,
            } => write!(
                f,
                "framework {target_alias:?}: package {} {}: {decline}",
                package.id, package.version
            ),
            DgSpecResolveDecline::DownloadUnavailable {
                target_alias,
                id,
                range,
            } => write!(
                f,
                "framework {target_alias:?}: download {id} {range} is not one exact version in \
                 the packages folder"
            ),
            DgSpecResolveDecline::PackageRead {
                target_alias,
                package,
                source,
            } => write!(
                f,
                "framework {target_alias:?}: package {} {} could not be read: {source}",
                package.id, package.version
            ),
            DgSpecResolveDecline::SourceHoldsPackage {
                target_alias,
                source,
                entry: Some(entry),
            } => write!(
                f,
                "framework {target_alias:?}: source {source} holds {entry}, named like a package \
                 restore looks up past the packages folder, so it may read from it"
            ),
            DgSpecResolveDecline::SourceHoldsPackage {
                target_alias,
                source,
                entry: None,
            } => write!(
                f,
                "framework {target_alias:?}: source {source} does not exist or could not be \
                 listed, and restore consults it for a package it looks up past the packages \
                 folder (NU1301)"
            ),
            DgSpecResolveDecline::Environment { decline } => write!(f, "{decline}"),
        }
    }
}

impl Error for DgSpecResolveDecline {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            DgSpecResolveDecline::Resolve { decline, .. } => Some(decline.as_ref()),
            DgSpecResolveDecline::CompatibilityUnproven {
                decline: Some(decline),
                ..
            } => Some(decline.as_ref()),
            DgSpecResolveDecline::PackageRead { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// Which of a package's files is outside the domain B₁ admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageMetadataDecline {
    Nuspec(NuspecDecline),
    /// The `.nupkg.metadata` commit marker is outside the vocabulary of
    /// `commit_marker_is_admitted`.
    CommitMarker,
}

impl fmt::Display for PackageMetadataDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageMetadataDecline::Nuspec(decline) => decline.fmt(f),
            PackageMetadataDecline::CommitMarker => {
                f.write_str("the commit marker is outside the admitted vocabulary")
            }
        }
    }
}

/// One framework's resolved closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFramework {
    /// The framework's alias, which keys its target in the assets file.
    pub target_alias: String,
    pub framework: NuGetFramework,
    pub closure: ResolvedPackageClosure,
    /// The warning codes restore logs for this framework's graph, after the
    /// project's `noWarn`; `None` when B₁ cannot say exactly, which is when a
    /// dependency carries its own `noWarn`.
    pub warnings: Option<BTreeSet<NuGetLogCode>>,
}

/// Resolve every framework of the project whose unique name is `project`, as
/// NuGet `nuget` would restore `spec`, or decline. The frameworks are in the
/// spec's order.
///
/// `environment` is the one the restore would run under, every variable in
/// it (see [`super::environment_variable`]).
///
/// The packages folder is the spec's own `packagesPath`; the file system is
/// read there, at the config files the spec names, at the user config folder
/// the environment locates, and at the spec's `nuGetLockFilePath`, which must
/// hold no lock file.
pub fn resolve_dgspec(
    nuget: &NuGetIdentity,
    spec: &DependencyGraphSpec,
    project: &str,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<Vec<ResolvedFramework>, DgSpecResolveDecline> {
    if !nuget.is_verified() {
        return Err(DgSpecResolveDecline::UnverifiedNuGet {
            identity: nuget.clone(),
        });
    }
    check_environment(environment)
        .map_err(|decline| DgSpecResolveDecline::Environment { decline })?;
    let package_spec = spec
        .projects
        .iter()
        .find(|candidate| candidate.restore.project_unique_name == project)
        .filter(|_| spec.restore.iter().any(|restored| restored == project))
        .ok_or_else(|| DgSpecResolveDecline::ProjectNotRestored {
            project: project.to_owned(),
        })?;

    // Every project of the spec is admitted on the same terms: the referenced
    // ones are restored with this one (their restore metadata is validated
    // with its), and their frameworks are walked as nodes of its graph.
    let mut fields = unmodelled_fields(package_spec);
    for other in spec
        .projects
        .iter()
        .filter(|other| !std::ptr::eq(*other, package_spec))
    {
        fields.extend(unmodelled_fields(other));
    }
    // NuGet validates the whole graph before restoring any of it, so another
    // entry in the restore list, or a project no reference reaches, can fail
    // this one's restore for a reason outside its graph. The spec `restore.rs`
    // generates restores the entry alone and holds exactly the projects its
    // references reach.
    if spec.restore.len() != 1 || !every_project_is_reached(spec, package_spec) {
        fields.insert(UnmodelledField::OtherProjects);
    }
    // A project's name is its identity in NuGet's graph: each referenced
    // project's must be a package id (which is what the walk compares), and no
    // two projects, the restored one included, may share one.
    if !project_names_are_distinct_ids(spec, package_spec) {
        fields.insert(UnmodelledField::ProjectName);
    }
    if !fields.is_empty() {
        return Err(DgSpecResolveDecline::Unmodelled { fields });
    }
    let Some(packages_path) = package_spec.restore.packages_path.as_deref() else {
        unreachable!("an absent packages path is an unmodelled field")
    };

    let not_pruned = not_pruned_warning(&package_spec.frameworks);
    package_spec
        .frameworks
        .iter()
        .map(|framework| {
            let context = target_context(framework);
            let FrameworkGraph {
                graph,
                warnings_predictable,
            } = project_graph(spec, package_spec, framework, &context)?;
            let closure = resolve_graph(
                Path::new(packages_path),
                &framework.framework,
                &context,
                &graph,
            )
            .map_err(|decline| DgSpecResolveDecline::Resolve {
                target_alias: framework.target_alias.clone(),
                decline: Box::new(decline),
            })?;
            check_downloads(Path::new(packages_path), framework)?;
            check_metadata(
                nuget,
                Path::new(packages_path),
                &framework.target_alias,
                &closure,
            )?;
            check_sources(
                Path::new(packages_path),
                &package_spec.restore.sources,
                &framework.target_alias,
                &framework.framework,
                &context,
                &graph,
                &closure,
            )?;
            check_compatibility(
                Path::new(packages_path),
                &framework.target_alias,
                &framework.framework,
                &context.asset_target_fallback,
                &closure,
            )?;
            let warnings = if warnings_predictable {
                predicted_warnings(
                    Path::new(packages_path),
                    &package_spec.frameworks,
                    framework,
                    &context,
                    not_pruned,
                    &package_spec.restore.warning_properties,
                    &graph,
                    &closure,
                )?
            } else {
                None
            };
            let policy = &package_spec.restore.warning_properties;
            match &warnings {
                Some(codes) => {
                    if let Some(code) = codes.iter().find(|code| fails_restore(policy, code)) {
                        return Err(DgSpecResolveDecline::PromotedWarning {
                            target_alias: framework.target_alias.clone(),
                            code: Some(*code),
                        });
                    }
                }
                None if promotes_anything(policy) => {
                    return Err(DgSpecResolveDecline::PromotedWarning {
                        target_alias: framework.target_alias.clone(),
                        code: None,
                    });
                }
                None => {}
            }
            Ok(ResolvedFramework {
                target_alias: framework.target_alias.clone(),
                framework: framework.framework.clone(),
                closure,
                warnings,
            })
        })
        .collect()
}

/// Restore's compatibility check, in the direction that can only decline:
/// every package of `closure` either has compile assets for `framework`, or
/// has no file restore could select for a framework at all.
///
/// Restore fails (`NU1202`) on a package with `lib/` or `ref/` files none of
/// which is an asset for the framework, and it takes a package's assets
/// through the asset-target fallback list (warning `NU1701`) when none of its
/// framework-specific assets, `build/` and `buildTransitive/` ones included,
/// suits the framework itself. A package with compile assets for the framework
/// needs neither. One without them is admitted only when it has no file in
/// `lib/` or `ref/`, and none in a `build/` or `buildTransitive/` folder whose
/// name restore's content model reads ([`content_model_framework`]: the short
/// form, or the long `FrameworkName` form it falls back to) as a framework
/// compatible with the project's or with one of its fallbacks. Its other
/// files (`analyzers/`, `tools/`, `contentFiles/`, and `build/` folders named
/// for nothing the project can take, such as Nerdbank.GitVersioning's
/// `build/MSBuildCore/`, which the content model reads as a framework named
/// `MSBuildCore`) neither fail the check nor warn.
/// `asset_folder_shapes_are_graded` holds each shape to B₀, and
/// `framework_folder_names_are_graded` sweeps the folder names.
fn check_compatibility(
    packages: &Path,
    target_alias: &str,
    framework: &NuGetFramework,
    fallback: &[NuGetFramework],
    closure: &ResolvedPackageClosure,
) -> Result<(), DgSpecResolveDecline> {
    let takes = |candidate: &NuGetFramework| {
        NuGetFramework::is_compatible(framework, candidate)
            || fallback
                .iter()
                .any(|fallback| NuGetFramework::is_compatible(fallback, candidate))
    };
    for resolved in &closure.packages {
        let unproven =
            |decline: Option<AssetSelectionDecline>| DgSpecResolveDecline::CompatibilityUnproven {
                target_alias: target_alias.to_owned(),
                package: Box::new(resolved.identity.clone()),
                decline: decline.map(Box::new),
            };
        let package =
            read_installed_package(packages, resolved.identity.clone()).map_err(|source| {
                DgSpecResolveDecline::PackageRead {
                    target_alias: target_alias.to_owned(),
                    package: Box::new(resolved.identity.clone()),
                    source: Box::new(source),
                }
            })?;
        let files = list_package_files(&package.paths.package_dir)
            .map_err(|decline| unproven(Some(decline)))?;
        // Restore parses every folder in a framework position of its content
        // model and throws on one it cannot parse, whatever else the package
        // holds.
        if files.iter().any(|file| {
            framework_folder(file).is_some_and(|folder| content_model_framework(folder).is_err())
        }) {
            return Err(unproven(None));
        }
        // Root-level files are the package's bookkeeping (the nuspec, the
        // commit marker); assets are in folders.
        if !files.iter().any(|file| file.contains('/')) {
            continue;
        }
        let compile = select_compile_assets(&files, &package.nuspec, framework)
            .map_err(|decline| unproven(Some(decline)))?;
        if !compile.items.is_empty() {
            continue;
        }
        let framework_specific = files.iter().any(|file| {
            let mut segments = file.split('/');
            let (Some(folder), Some(next), rest) =
                (segments.next(), segments.next(), segments.next())
            else {
                return false;
            };
            let named = |name: &str| folder.eq_ignore_ascii_case(name);
            if named("lib") || named("ref") {
                return true;
            }
            (named("build") || named("buildTransitive"))
                && rest.is_some()
                && matches!(content_model_framework(next), Ok(Some(candidate)) if takes(&candidate))
        });
        if framework_specific {
            return Err(unproven(None));
        }
    }
    Ok(())
}

/// The segment of a package file's path that restore's content model reads as
/// a framework, if the file is in a folder that has one: `lib`, `ref`,
/// `build`, `buildTransitive`, `tools`, `embed` and `analyzers` then
/// `{framework}`; `contentFiles/{language}/{framework}`; and
/// `runtimes/{rid}/lib` or `runtimes/{rid}/nativeassets` then `{framework}`;
/// each followed by at least one more segment. Folder names compare ignoring
/// case.
fn framework_folder(file: &str) -> Option<&str> {
    let segments: Vec<&str> = file.split('/').collect();
    let named = |index: usize, names: &[&str]| {
        segments
            .get(index)
            .is_some_and(|segment| names.iter().any(|name| segment.eq_ignore_ascii_case(name)))
    };
    let at = |index: usize| (segments.len() > index + 1).then(|| segments[index]);
    if named(
        0,
        &[
            "lib",
            "ref",
            "build",
            "buildTransitive",
            "tools",
            "embed",
            "analyzers",
        ],
    ) {
        at(1)
    } else if named(0, &["contentFiles"]) {
        at(2)
    } else if named(0, &["runtimes"]) && named(2, &["lib", "nativeassets"]) {
        at(3)
    } else {
        None
    }
}

/// Every `downloadDependencies` entry of `framework` names one exact version
/// that is committed in the packages folder, with its nuspec.
///
/// Restore only checks that a download is present: it fails on a missing one
/// (`NU1101`) and validates nothing else about it (not its package type, its
/// minimum client version, nor its commit marker's contents, unlike a
/// dependency's). The SDK writes each download as an exact range; restore's
/// handling of any other range is not modelled, so one declines.
fn check_downloads(
    packages: &Path,
    framework: &TargetFrameworkInformation,
) -> Result<(), DgSpecResolveDecline> {
    for DownloadDependency { id, range } in &framework.download_dependencies {
        let unavailable = || DgSpecResolveDecline::DownloadUnavailable {
            target_alias: framework.target_alias.clone(),
            id: id.clone(),
            range: Box::new(range.clone()),
        };
        // A floating range consults the sources whatever its bounds.
        let exact = match (range.min_version(), range.max_version()) {
            (Some(min), Some(max))
                if !range.is_floating()
                    && range.is_min_inclusive()
                    && range.is_max_inclusive()
                    && min.eq_strict(max) =>
            {
                min.clone()
            }
            _ => return Err(unavailable()),
        };
        let paths = PackagePaths::new(packages, &PackageIdentity::new(id.clone(), exact));
        let present = matches!(paths.is_committed(), Ok(true))
            && matches!(paths.nuspec_path.try_exists(), Ok(true));
        if !present {
            return Err(unavailable());
        }
    }
    Ok(())
}

/// Every version restore may read for `closure` ([`ResolvedPackageClosure::examined`])
/// has a nuspec in the domain of [`super::nuspec`] and a commit marker in the
/// vocabulary of [`commit_marker_is_admitted`].
///
/// Restore validates each version it reads, not only the ones it settles on: a
/// losing version's minimum client version fails the restore as surely as the
/// winner's.
fn check_metadata(
    nuget: &NuGetIdentity,
    packages: &Path,
    target_alias: &str,
    closure: &ResolvedPackageClosure,
) -> Result<(), DgSpecResolveDecline> {
    for identity in &closure.examined {
        let package = read_installed_package(packages, identity.clone()).map_err(|source| {
            DgSpecResolveDecline::PackageRead {
                target_alias: target_alias.to_owned(),
                package: Box::new(identity.clone()),
                source: Box::new(source),
            }
        })?;
        let declined = |decline: PackageMetadataDecline| DgSpecResolveDecline::PackageMetadata {
            target_alias: target_alias.to_owned(),
            package: Box::new(identity.clone()),
            decline,
        };
        let bytes = std::fs::read(&package.paths.nuspec_path).map_err(|source| {
            DgSpecResolveDecline::PackageRead {
                target_alias: target_alias.to_owned(),
                package: Box::new(identity.clone()),
                source: Box::new(PackageReadError::Io {
                    path: package.paths.nuspec_path.clone(),
                    source,
                }),
            }
        })?;
        nuspec_is_admitted(&bytes, identity, nuget)
            .map_err(|decline| declined(PackageMetadataDecline::Nuspec(decline)))?;
        if !commit_marker_is_admitted(&package.paths.metadata_path) {
            return Err(declined(PackageMetadataDecline::CommitMarker));
        }
    }
    Ok(())
}

/// No local folder source holds anything named like a package that restore
/// looks up past the packages folder.
///
/// Restore looks an edge up in the packages folder first and takes what it
/// finds there when it is the edge's lower bound; otherwise it asks every
/// source as well and reads the lowest version satisfying the edge across
/// them. [`ResolvedPackageClosure::examined`] is that version among the
/// installed ones, which is restore's only while no source holds a version
/// of the package. So the edges that matter are those whose inclusive lower
/// bound is not installed, among the graph's package edges and the
/// dependencies of every version restore may read; an SDK's `library-packs`
/// folder holds `FSharp.Core`, which every F# graph names through an edge
/// whose lower bound is installed, and passes.
///
/// A folder feed's entries are `<id>.<version>.nupkg` files or `<id>`
/// folders, so an entry is taken to name `id` when, ignoring case, it is `id`
/// or starts with `id` followed by a dot; a package id containing another as
/// a prefix declines more than restore would read.
fn check_sources(
    packages: &Path,
    sources: &[String],
    target_alias: &str,
    framework: &NuGetFramework,
    context: &TargetContext,
    graph: &ProjectGraph,
    closure: &ResolvedPackageClosure,
) -> Result<(), DgSpecResolveDecline> {
    // Transitive pinning looks a pinned edge up at its central version; every
    // central version is taken, which may decline more than restore reads.
    let mut edges: Vec<(PackageId, VersionRange)> = graph
        .package_edges()
        .chain(graph.transitive_pinning.iter().flatten())
        .map(|(id, range)| (id.clone(), range.clone()))
        .collect();
    for identity in &closure.examined {
        let package = read_installed_package(packages, identity.clone()).map_err(|source| {
            DgSpecResolveDecline::PackageRead {
                target_alias: target_alias.to_owned(),
                package: Box::new(identity.clone()),
                source: Box::new(source),
            }
        })?;
        for dependency in context.dependencies(&package.nuspec, framework) {
            if let Some(range) = dependency.version_range {
                edges.push((dependency.id, range));
            }
        }
    }
    // The packages restore looks up past the packages folder.
    let mut consulted: BTreeSet<String> = BTreeSet::new();
    for (id, range) in &edges {
        let installed_bound = match range.min_version() {
            Some(bound) if range.is_min_inclusive() => {
                let paths =
                    PackagePaths::new(packages, &PackageIdentity::new(id.clone(), bound.clone()));
                paths
                    .is_committed()
                    .map_err(|source| DgSpecResolveDecline::PackageRead {
                        target_alias: target_alias.to_owned(),
                        package: Box::new(PackageIdentity::new(id.clone(), bound.clone())),
                        source: Box::new(PackageReadError::Io {
                            path: paths.metadata_path.clone(),
                            source,
                        }),
                    })?
            }
            _ => false,
        };
        if !installed_bound {
            consulted.insert(id.as_str().to_ascii_lowercase());
        }
    }
    if consulted.is_empty() {
        return Ok(());
    }
    for source in sources {
        let declined = |entry: Option<String>| DgSpecResolveDecline::SourceHoldsPackage {
            target_alias: target_alias.to_owned(),
            source: source.clone(),
            entry,
        };
        // Restore consults every source for such a package and fails
        // (`NU1301`) on one it cannot read, a missing folder included.
        let Ok(entries) = std::fs::read_dir(source) else {
            return Err(declined(None));
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return Err(declined(None));
            };
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if consulted
                .iter()
                .any(|id| name == *id || name.starts_with(&format!("{id}.")))
            {
                return Err(declined(Some(
                    entry.file_name().to_string_lossy().into_owned(),
                )));
            }
        }
    }
    Ok(())
}

/// Whether a package's `.nupkg.metadata` commit marker is one restore reads
/// without complaint: a JSON object whose `contentHash` is a string (empty is
/// accepted: Nix-built caches write `{ "contentHash": "" }`), and whose only
/// other keys are `version`, 1 or 2, and `source`, a string or null, none
/// repeated. Restore
/// throws on a marker without a content hash, or with a null one, which the
/// package readers, checking only that the marker exists, cannot see.
fn commit_marker_is_admitted(marker: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(marker) else {
        return false;
    };
    // The dgspec's reader, which keeps a repeated key: restore reads every
    // occurrence and throws on a bad one that a last-wins reader would drop.
    let Ok(super::json::Json::Object(members)) = super::json::parse(&text) else {
        return false;
    };
    let keys: BTreeSet<&str> = members.iter().map(|(key, _)| key.as_str()).collect();
    keys.len() == members.len()
        && keys.contains("contentHash")
        && members.iter().all(|(key, value)| {
            use super::json::Json;
            match (key.as_str(), value) {
                ("version", Json::Number(number)) => matches!(number.as_u64(), Some(1 | 2)),
                ("contentHash", Json::String(_)) => true,
                ("source", Json::String(_) | Json::Null) => true,
                _ => false,
            }
        })
}

/// Every field of `spec` present with a value the resolver does not model.
///
/// Each struct is destructured in full, so a field added to the model is a
/// compile error here until it is classified.
fn unmodelled_fields(spec: &PackageSpec) -> BTreeSet<UnmodelledField> {
    let mut fields = BTreeSet::new();
    let mut decline = |field: UnmodelledField| {
        fields.insert(field);
    };

    let PackageSpec {
        // The project's own package version: the root of the graph, which no
        // package resolves to unless it shares the project's name, which the
        // resolver declines (`ResolveDecline::DependencyNamesAProject`); a
        // referenced project's is what a reference to it settles on.
        version: _,
        is_default_version: _,
        restore,
        frameworks,
    } = spec;

    let ProjectRestoreMetadata {
        // How the spec names the project; `resolve_dgspec` looks it up by it,
        // and restore requires it.
        project_unique_name,
        project_name,
        // Where the project and its outputs are, not what it resolves; but
        // restore requires them.
        project_path,
        output_path,
        packages_path,
        sources,
        fallback_folders,
        // NuGet loads its settings from these files, and a file it cannot load
        // fails the restore. See `config_is_admitted`.
        config_file_paths,
        frameworks: restore_frameworks,
        // Names, and whether there is more than one framework; restore
        // requires them to match the frameworks.
        original_target_frameworks,
        cross_targeting: _,
        // Whether content files are written into the project's output.
        skip_content_file_write: _,
        // Applied to the warnings B₁ predicts, after resolving.
        warning_properties: _,
        restore_lock_properties,
        central_package_versions_enabled,
        // Modelled on the restored project (`project_graph`); a referenced
        // project's is never read.
        central_package_transitive_pinning_enabled: _,
        restore_audit_properties,
        // Graded by the differential, whose generator varies it.
        sdk_analysis_level: _,
        using_microsoft_net_sdk,
    } = restore;

    let present = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
    // A path restore can use: present, and without control characters (NuGet
    // throws on a NUL).
    let path_admitted = |value: &Option<String>| {
        present(value)
            && value
                .as_deref()
                .is_some_and(|v| !v.chars().any(char::is_control))
    };
    if !present(project_name) {
        decline(UnmodelledField::ProjectName);
    }
    // An alias in the vocabulary the SDK writes (NuGet refuses path separators
    // and non-ASCII characters, `NU1019`), once per list ignoring case, as
    // NuGet compares them.
    let aliases_admitted = |aliases: &[&str]| {
        let distinct: BTreeSet<String> = aliases
            .iter()
            .map(|alias| alias.to_ascii_lowercase())
            .collect();
        distinct.len() == aliases.len()
            && aliases.iter().all(|alias| {
                !alias.is_empty()
                    && alias
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            })
    };
    let original: Vec<&str> = original_target_frameworks
        .iter()
        .map(String::as_str)
        .collect();
    let aliases: Vec<&str> = frameworks.iter().map(|f| f.target_alias.as_str()).collect();
    let restore_aliases: Vec<&str> = restore_frameworks
        .iter()
        .map(|f| f.target_alias.as_str())
        .collect();
    fn as_set<'a>(aliases: &[&'a str]) -> BTreeSet<&'a str> {
        aliases.iter().copied().collect()
    }
    // The SDK writes a project's unique name as its path, an absolute one.
    // NuGet looks projects up by unique name and by name in one table, so a
    // unique name that is not a path could be another project's name.
    if project_unique_name.is_empty()
        || !path_admitted(project_path)
        || project_path.as_deref() != Some(project_unique_name.as_str())
        || !Path::new(project_unique_name).is_absolute()
        || !path_admitted(output_path)
        || !aliases_admitted(&original)
        || !aliases_admitted(&aliases)
        || !aliases_admitted(&restore_aliases)
        || as_set(&original) != as_set(&aliases)
        || as_set(&restore_aliases) != as_set(&aliases)
        || frameworks
            .iter()
            .map(|f| f.framework.short_folder_name())
            .collect::<BTreeSet<_>>()
            .len()
            != frameworks.len()
        // Restore validates every framework of every project in the spec
        // as specific (`SpecValidationUtility.ValidateFrameworks`), the
        // ones it never walks included.
        || frameworks
            .iter()
            .any(|f| !f.framework.is_specific_framework())
        || restore_frameworks
            .iter()
            .any(|f| !f.framework.is_specific_framework())
        // The SDK writes each restore-metadata entry as its alias's
        // framework. NuGet's releases select a framework's project
        // references by alias or by framework name, which agree only then.
        || restore_frameworks.iter().any(|restore_framework| {
            !frameworks.iter().any(|f| {
                f.target_alias
                    .eq_ignore_ascii_case(&restore_framework.target_alias)
                    && f.framework == restore_framework.framework
            })
        })
    {
        decline(UnmodelledField::RequiredMetadata);
    }
    if !path_admitted(packages_path) {
        decline(UnmodelledField::PackagesPath);
    }
    if !sources.iter().all(|source| is_local_absolute_path(source)) {
        decline(UnmodelledField::Sources);
    }
    if !fallback_folders.is_empty() {
        decline(UnmodelledField::FallbackFolders);
    }
    if !config_file_paths
        .iter()
        .all(|path| config_is_admitted(Path::new(path)))
    {
        decline(UnmodelledField::Config);
    }

    if !lock_file_is_off(restore_lock_properties) {
        decline(UnmodelledField::LockFile);
    }
    // Central package management moves no direct reference's closure: the
    // SDK writes each centrally managed dependency's range into the spec, and
    // restore resolves that range, whatever the central entry or a
    // `versionOverride` says. It does fail the restore on a dependency not
    // centrally managed (`NU1008`), on an implicit reference, managed or not
    // (`NU1009`), and on a floating central version (`NU1011`). Transitive
    // pinning, which restore reads only under central package management and
    // only on the restored project, raises transitive packages to their
    // central versions; `project_graph` hands those to the resolver.
    if !audit_is_off(restore_audit_properties.as_ref()) {
        decline(UnmodelledField::Audit);
    }
    if !*using_microsoft_net_sdk {
        decline(UnmodelledField::NotMicrosoftNetSdk);
    }

    for restore_framework in restore_frameworks {
        let RestoreFrameworkInfo {
            target_alias: _,
            framework: _,
            project_references,
        } = restore_framework;
        for reference in project_references {
            let ProjectRestoreReference {
                // The reference, which `project_graph` resolves to a project
                // of the spec, or declines.
                project_unique_name: _,
                // The name restore gives a reference to a project the spec
                // does not hold, which declines before it is needed.
                project_path: _,
                // Which of the referenced project's assets flow; not which
                // packages are in the graph. Graded by the differential,
                // whose generator varies them.
                include_assets: _,
                exclude_assets: _,
                // `All` leaves the reference out of the graph when this
                // project is itself reached as a reference (`project_graph`);
                // anything else is inert like the two above.
                private_assets: _,
            } = reference;
        }
    }

    if frameworks.is_empty() {
        decline(UnmodelledField::NoFrameworks);
    }
    for framework in frameworks {
        let TargetFrameworkInformation {
            target_alias: _,
            // Checked by the resolver, which declines one it does not model.
            framework: _,
            dependencies,
            imports,
            asset_target_fallback,
            // Whether restore warns (NU1701) when it takes a package's assets
            // through the fallback list, which B₁ declines before then (its
            // compatibility check admits only assets for the framework itself);
            // taking a dependency group through it logs nothing.
            warn: _,
            // Checked for presence after resolving (`check_downloads`).
            download_dependencies: _,
            central_package_versions,
            // A framework reference adds no package to the closure; what it
            // takes out of it is `packagesToPrune`, which declines.
            framework_references,
            packages_to_prune,
            // Read only for runtime-specific targets, which the spec does not
            // request (its `runtimes` key declines in the parser).
            runtime_identifier_graph_path: _,
        } = framework;

        for dependency in dependencies {
            let LibraryDependency {
                // The requirement itself.
                id: _,
                range: _,
                target,
                // Which of the package's assets the project consumes, and which
                // flow on to projects that reference this one. Neither decides
                // which packages are in this project's graph; graded by the
                // differential, whose generator varies them.
                include: _,
                suppress_parent: _,
                // Silences warnings, so can only turn a failure into success.
                no_warn: _,
                // Inert outside central package management (graded by the
                // differential, whose generator varies it); under it, restore
                // fails on an implicit reference (`NU1009`).
                auto_referenced,
                version_centrally_managed,
                // Outputs of restore for the build, not inputs to resolution.
                generate_path_property: _,
                aliases: _,
                version_override,
            } = dependency;
            if *target != LibraryDependencyTarget::PACKAGE {
                decline(UnmodelledField::DependencyTarget);
            }
            let managed_as_restore_requires = if *central_package_versions_enabled {
                *version_centrally_managed
                    && !*auto_referenced
                    && version_override
                        .as_ref()
                        .is_none_or(|range| !range.is_floating())
            } else {
                !*version_centrally_managed && version_override.is_none()
            };
            if !managed_as_restore_requires {
                decline(UnmodelledField::CentralPackageManagement);
            }
        }
        if !imports.is_empty() && !*asset_target_fallback {
            decline(UnmodelledField::PackageTargetFallback);
        }
        for entry in central_package_versions {
            let PackageRangeEntry { id: _, range } = entry;
            if !*central_package_versions_enabled || range.is_floating() {
                decline(UnmodelledField::CentralPackageManagement);
            }
        }
        for reference in framework_references {
            let FrameworkReference {
                name: _,
                private_assets: _,
            } = reference;
        }
        // Restore prunes by each entry's upper bound (see `TargetContext`); an
        // entry without one, or a floating one, is not modelled.
        for entry in packages_to_prune {
            let PackageRangeEntry { id: _, range } = entry;
            if range.max_version().is_none() || range.is_floating() {
                decline(UnmodelledField::PackagesToPrune);
            }
        }
    }

    fields
}

/// Whether a config file NuGet loads its settings from is in the closed
/// vocabulary B₁ admits.
///
/// Restore loads every config file the spec names, and fails on one it cannot
/// load (malformed XML, an empty package source mapping pattern) even when
/// every package is in the packages folder. What it loads that bears on
/// resolution is already in the spec: `RestoreSources` overrides the
/// configured sources, and the packages folder and fallback folders are
/// resolved into it. So B₁ admits a file holding only source lists, which it
/// can show load, and declines anything else rather than model NuGet's
/// settings validation: a well-formed XML document whose root is
/// `configuration`, with only `packageSources` and `disabledPackageSources`
/// sections, each holding only `clear` elements and `add` elements with a
/// `key` and `value` that are non-empty and without surrounding whitespace
/// (and, in `packageSources`, a `protocolVersion`), without text.
pub(super) fn config_is_admitted(path: &Path) -> bool {
    fn trimmed_and_present(value: &str) -> bool {
        !value.is_empty() && value.trim() == value
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let Ok(document) = roxmltree::Document::parse(text) else {
        return false;
    };
    let root = document.root_element();
    /// The element children of `node`, or `None` if it holds text.
    fn elements<'a, 'input>(
        node: roxmltree::Node<'a, 'input>,
    ) -> Option<Vec<roxmltree::Node<'a, 'input>>> {
        let mut elements = Vec::new();
        for child in node.children().filter(|child| !child.is_comment()) {
            if child.is_element() {
                elements.push(child);
            } else if child.text().is_some_and(|text| !text.trim().is_empty()) {
                return None;
            }
        }
        Some(elements)
    }
    let plain = |node: roxmltree::Node<'_, '_>| node.tag_name().namespace().is_none();
    let attributes_are = |node: roxmltree::Node<'_, '_>, allowed: &[&str]| {
        node.attributes()
            .all(|attribute| attribute.namespace().is_none() && allowed.contains(&attribute.name()))
    };
    let entry_admitted = |entry: roxmltree::Node<'_, '_>, extra: &[&str]| {
        if !plain(entry) || !elements(entry).is_some_and(|children| children.is_empty()) {
            return false;
        }
        match entry.tag_name().name() {
            "clear" => entry.attributes().next().is_none(),
            "add" => {
                let mut allowed = vec!["key", "value"];
                allowed.extend_from_slice(extra);
                attributes_are(entry, &allowed)
                    && entry.attribute("key").is_some_and(trimmed_and_present)
                    && entry.attribute("value").is_some_and(trimmed_and_present)
            }
            _ => false,
        }
    };
    plain(root)
        && root.tag_name().name() == "configuration"
        && root.attributes().next().is_none()
        && elements(root).is_some_and(|sections| {
            sections.into_iter().all(|section| {
                let extra: &[&str] = match section.tag_name().name() {
                    "packageSources" => &["protocolVersion"],
                    "disabledPackageSources" => &[],
                    _ => return false,
                };
                plain(section)
                    && section.attributes().next().is_none()
                    && elements(section).is_some_and(|entries| {
                        entries.into_iter().all(|e| entry_admitted(e, extra))
                    })
            })
        })
}

/// A source the resolver admits: a local folder named by an absolute path.
fn is_local_absolute_path(source: &str) -> bool {
    !source.contains("://") && Path::new(source).is_absolute()
}

/// The warning codes restore logs for one framework's graph, after the
/// project's `noWarn`, or `None` when B₁ cannot say exactly.
///
/// Within the envelope the resolver commits in, restore fails rather than warns
/// on everything else the resolver models (a downgrade, a conflict, a missing
/// package), and the inputs that would make it warn otherwise (a missing lower
/// bound, a fallback-selected asset, pruning, audit) decline before here. What
/// is left is `NU1608`: a package in the closure depends on a range whose
/// upper bound the version the graph settled on for that dependency exceeds,
/// which happens when a nearer edge (a direct reference, typically) eclipses
/// the dependency with a higher version. A settled version below a range's
/// lower bound logs nothing. And on `net10.0` there is `NU1510`: a direct
/// reference the prune list covers, which restore keeps but warns about.
///
/// A dependency's own `noWarn` suppresses warnings attributed to it, and B₁
/// does not model which warnings those are, so a framework with one predicts
/// `None`. The differential holds every `Some` prediction to equal the codes
/// B₀ logs.
#[allow(clippy::too_many_arguments)]
fn predicted_warnings(
    packages: &Path,
    project_frameworks: &[TargetFrameworkInformation],
    framework: &TargetFrameworkInformation,
    context: &TargetContext,
    not_pruned: NotPrunedWarning,
    policy: &WarningProperties,
    graph: &ProjectGraph,
    closure: &ResolvedPackageClosure,
) -> Result<Option<BTreeSet<NuGetLogCode>>, DgSpecResolveDecline> {
    if framework
        .dependencies
        .iter()
        .any(|dependency| !dependency.no_warn.is_empty())
    {
        return Ok(None);
    }
    let outside_constraint = NuGetLogCode::parse("NU1608").expect("NU1608 is a NuGet log code");
    let selected = |id: &PackageId| {
        closure
            .packages
            .iter()
            .find(|package| package.identity.id == *id)
            .map(|package| &package.identity.version)
    };
    // Above the upper bound only: a version below the lower bound, which a
    // nearer edge can also settle on, logs nothing.
    let above = |range: &VersionRange, version: &NuGetVersion| {
        range.max_version().is_some_and(|max| {
            if range.is_max_inclusive() {
                version > max
            } else {
                version >= max
            }
        })
    };
    let mut codes = BTreeSet::new();
    for resolved in &closure.packages {
        let package =
            read_installed_package(packages, resolved.identity.clone()).map_err(|source| {
                DgSpecResolveDecline::PackageRead {
                    target_alias: framework.target_alias.clone(),
                    package: Box::new(resolved.identity.clone()),
                    source: Box::new(source),
                }
            })?;
        for dependency in &context.dependencies(&package.nuspec, &framework.framework) {
            let (Some(range), Some(version)) =
                (dependency.version_range.as_ref(), selected(&dependency.id))
            else {
                continue;
            };
            if above(range, version) {
                codes.insert(outside_constraint);
            }
        }
    }
    // A referenced project's package edges are edges of the graph too, bounded
    // above like a package's. (The root's own are direct references, which
    // settle on their own lower bound or fail the restore.)
    for project in &graph.projects {
        for edge in &project.edges {
            if edge.target != EdgeTarget::Package || context.covers(&edge.id, &edge.range) {
                continue;
            }
            if let Some(version) = selected(&edge.id)
                && above(&edge.range, version)
            {
                codes.insert(outside_constraint);
            }
        }
    }
    // Restore's lower-bound warnings (`UnexpectedDependencyMessages`), in the
    // order it checks them, each package warned about once:
    // - `NU1604`: a direct package dependency of the project, in any of its
    //   frameworks, without an inclusive lower bound;
    // - `NU1602`/`NU1603`: a package of the final graph that did not resolve
    //   to an inclusive lower bound of the range chosen for it, because the
    //   range has none (`NU1602`) or it is not installed (`NU1603`).
    // Its `NU1601` (a direct dependency resolved above its inclusive lower
    // bound) cannot follow: a direct reference's own edge is the first the
    // flattening meets for its package, so `NU1603` has covered it.
    let code = |code: &str| NuGetLogCode::parse(code).expect("a NuGet log code");
    let missing_lower_bound = |range: &VersionRange| {
        !range.is_floating() && (!range.is_min_inclusive() || !range.has_lower_bound())
    };
    let mut warned: BTreeSet<PackageId> = BTreeSet::new();
    for project_framework in project_frameworks {
        for dependency in &project_framework.dependencies {
            if missing_lower_bound(&dependency.range) {
                warned.insert(dependency.id.clone());
                if project_framework.target_alias == framework.target_alias {
                    codes.insert(code("NU1604"));
                }
            }
        }
    }
    for resolved in &closure.resolved_dependencies {
        if resolved.range.is_floating() || warned.contains(&resolved.id) {
            continue;
        }
        // NuGet's version identity compares release labels as strings
        // (`VersionComparer.Equals`), where its ordering compares them as
        // numbers: `1.0.0--0` orders with `1.0.0-0` and is not it.
        if resolved.range.is_min_inclusive()
            && resolved
                .range
                .min_version()
                .is_some_and(|bound| bound.eq_strict(&resolved.version))
        {
            continue;
        }
        codes.insert(code(if missing_lower_bound(&resolved.range) {
            "NU1602"
        } else {
            "NU1603"
        }));
        warned.insert(resolved.id.clone());
    }
    match not_pruned {
        NotPrunedWarning::Never => {}
        NotPrunedWarning::Logged => {
            codes.insert(code("NU1510"));
        }
        NotPrunedWarning::Unknown => return Ok(None),
    }
    codes.retain(|code| !policy.no_warn.contains(code));
    Ok(Some(codes))
}

/// Whether restore warns `NU1510` (a direct reference the prune list covers,
/// which it keeps) for a project's frameworks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotPrunedWarning {
    /// For none of them.
    Never,
    /// For the project's one framework.
    Logged,
    /// Not predicted exactly.
    Unknown,
}

/// When restore warns `NU1510`, as far as it is graded. Restore enables the
/// warning when any target framework is `net10.0` or later, and decides it
/// across the project's frameworks together. Only a single-framework project
/// is graded: `net10.0` with a covered direct reference warns, no earlier
/// framework does whatever `SdkAnalysisLevel` says, and anything else that
/// could warn (several frameworks, one of them `net10.0`, with a covered
/// reference; a `.NETCoreApp` after 10) is not predicted exactly.
fn not_pruned_warning(frameworks: &[TargetFrameworkInformation]) -> NotPrunedWarning {
    let core_major = |framework: &NuGetFramework| {
        (framework.framework() == ".NETCoreApp")
            .then(|| {
                framework
                    .version_string()
                    .split('.')
                    .next()
                    .and_then(|major| major.parse::<u32>().ok())
            })
            .flatten()
    };
    let majors: Vec<Option<u32>> = frameworks
        .iter()
        .map(|framework| core_major(&framework.framework))
        .collect();
    if majors
        .iter()
        .any(|major| major.is_some_and(|major| major > 10))
    {
        return NotPrunedWarning::Unknown;
    }
    let covered = frameworks.iter().any(|framework| {
        let context = target_context(framework);
        framework
            .dependencies
            .iter()
            .any(|dependency| context.covers(&dependency.id, &dependency.range))
    });
    match (majors.contains(&Some(10)), covered, frameworks.len()) {
        (true, true, 1) => NotPrunedWarning::Logged,
        (true, true, _) => NotPrunedWarning::Unknown,
        _ => NotPrunedWarning::Never,
    }
}

/// One framework's graph, and whether B₁ can predict its warnings.
struct FrameworkGraph {
    graph: ProjectGraph,
    /// `false` where a referenced project carries something whose warnings
    /// B₁ does not model: a dependency with its own `noWarn`, a suppressed
    /// edge, or a project reference the root's prune list covers (`NU1511`
    /// on `net10.0` and later).
    warnings_predictable: bool,
}

/// The graph `framework` of `root` resolves: the root's edges, and every
/// project a chain of references from it reaches, each as the node NuGet
/// walks for it.
///
/// NuGet walks a referenced project's framework nearest the root's
/// (`PackageSpec.GetTargetFramework`); one it reaches only through the
/// root's fallback list, or not at all (`NU1201`), declines. A project's edges
/// are its dependencies for that framework, with `PrivateAssets="All"` and the
/// ones its own prune list covers suppressed
/// (`PackageSpecReferenceDependencyProvider.GetSpecDependencies`), then its
/// project references for that framework, each as `[version, )` of the
/// referenced project and overriding a same-named package dependency
/// (`GetDependenciesFromSpecRestoreMetadata`). A reference to a project the
/// spec does not hold declines (`NU1105`).
fn project_graph(
    spec: &DependencyGraphSpec,
    root: &PackageSpec,
    framework: &TargetFrameworkInformation,
    context: &TargetContext,
) -> Result<FrameworkGraph, DgSpecResolveDecline> {
    let target_alias = &framework.target_alias;
    let mut warnings_predictable = true;
    // The edges of `project`'s framework `selected`, as a node of this graph.
    let mut edges_of = |project: &PackageSpec,
                        selected: &TargetFrameworkInformation,
                        is_root: bool|
     -> Result<Vec<ProjectEdge>, DgSpecResolveDecline> {
        let own_context = target_context(selected);
        // A referenced project's own `noWarn` suppresses warnings along the
        // paths it brings (`TransitiveNoWarnUtils`), which B₁ does not model.
        if !is_root && !project.restore.warning_properties.no_warn.is_empty() {
            warnings_predictable = false;
        }
        let mut edges: Vec<ProjectEdge> = selected
            .dependencies
            .iter()
            .map(|dependency| {
                let suppressed = dependency.suppress_parent == LibraryIncludeFlags::ALL
                    || own_context.covers(&dependency.id, &dependency.range);
                if !is_root && (suppressed || !dependency.no_warn.is_empty()) {
                    warnings_predictable = false;
                }
                ProjectEdge {
                    id: dependency.id.clone(),
                    range: dependency.range.clone(),
                    target: EdgeTarget::Package,
                    suppressed,
                }
            })
            .collect();
        // The references are those of the restore metadata's framework
        // nearest `selected`'s (`PackageSpec.GetRestoreMetadataFramework`);
        // with none compatible, NuGet adds no reference.
        let restore_frameworks: Vec<NuGetFramework> = project
            .restore
            .frameworks
            .iter()
            .map(|candidate| candidate.framework.clone())
            .collect();
        let references = NuGetFramework::get_nearest(&selected.framework, &restore_frameworks)
            .map(|nearest| &project.restore.frameworks[nearest].project_references[..])
            .unwrap_or_default();
        for reference in references {
            let referenced =
                find_project(spec, &reference.project_unique_name).ok_or_else(|| {
                    DgSpecResolveDecline::ProjectReferenceMissing {
                        target_alias: target_alias.clone(),
                        project: reference.project_unique_name.clone(),
                    }
                })?;
            let name = project_id(referenced).expect("admitted: a project's name is a package id");
            let range = VersionRange::parse(&format!(
                "[{}, )",
                referenced.version.to_normalized_string()
            ))
            .expect("a normalised version opens a range");
            let suppressed = reference.private_assets == LibraryIncludeFlags::ALL;
            if !is_root && suppressed {
                warnings_predictable = false;
            }
            if is_root && context.covers(&name, &range) {
                warnings_predictable = false;
            }
            edges.retain(|edge| !(edge.target == EdgeTarget::Package && edge.id == name));
            edges.push(ProjectEdge {
                id: name,
                range,
                target: EdgeTarget::Project,
                suppressed,
            });
        }
        Ok(edges)
    };

    let direct = edges_of(root, framework, true)?;
    let mut projects: Vec<ProjectNode> = Vec::new();
    // A reference back to the root is an edge of the graph (the resolver
    // declines it as a cycle), not a node to walk.
    let referenced = |edges: &[ProjectEdge]| -> Vec<&PackageSpec> {
        edges
            .iter()
            .filter(|edge| edge.target == EdgeTarget::Project)
            .map(|edge| project_named(spec, &edge.id).expect("the edge was made from the project"))
            .filter(|project| !std::ptr::eq(*project, root))
            .collect()
    };
    let mut queue: VecDeque<&PackageSpec> = referenced(&direct).into();
    while let Some(project) = queue.pop_front() {
        let name = project_id(project).expect("admitted: a project's name is a package id");
        if projects.iter().any(|node| node.name == name) {
            continue;
        }
        let candidates: Vec<NuGetFramework> = project
            .frameworks
            .iter()
            .map(|candidate| candidate.framework.clone())
            .collect();
        let nearest =
            NuGetFramework::get_nearest(&framework.framework, &candidates).ok_or_else(|| {
                DgSpecResolveDecline::ProjectIncompatible {
                    target_alias: target_alias.clone(),
                    project: project.restore.project_unique_name.clone(),
                }
            })?;
        let edges = edges_of(project, &project.frameworks[nearest], false)?;
        queue.extend(referenced(&edges));
        projects.push(ProjectNode {
            name,
            version: project.version.clone(),
            edges,
        });
    }
    // The spec's other projects, which another framework reaches: restore
    // knows them too, so a package edge may not carry their names.
    let other_projects: Vec<PackageId> = spec
        .projects
        .iter()
        .filter(|project| !std::ptr::eq(*project, root))
        .filter_map(project_id)
        .filter(|name| !projects.iter().any(|node| &node.name == name))
        .collect();
    // Restore pins only under central package management, by the restored
    // project's central versions for this framework.
    let transitive_pinning = (root.restore.central_package_versions_enabled
        && root.restore.central_package_transitive_pinning_enabled)
        .then(|| {
            framework
                .central_package_versions
                .iter()
                .map(|entry| (entry.id.clone(), entry.range.clone()))
                .collect()
        });
    Ok(FrameworkGraph {
        graph: ProjectGraph {
            root: project_id(root),
            direct,
            projects,
            other_projects,
            transitive_pinning,
        },
        warnings_predictable,
    })
}

/// The project of `spec` with unique name `unique_name`, compared as NuGet
/// compares them (ignoring ASCII case).
fn find_project<'a>(spec: &'a DependencyGraphSpec, unique_name: &str) -> Option<&'a PackageSpec> {
    spec.projects.iter().find(|project| {
        project
            .restore
            .project_unique_name
            .eq_ignore_ascii_case(unique_name)
    })
}

/// The project of `spec` whose name is `name`.
fn project_named<'a>(spec: &'a DependencyGraphSpec, name: &PackageId) -> Option<&'a PackageSpec> {
    spec.projects
        .iter()
        .find(|project| project_id(project).as_ref() == Some(name))
}

/// A project's name as a package id, or `None` when it is absent or not one.
fn project_id(project: &PackageSpec) -> Option<PackageId> {
    PackageId::parse(project.restore.project_name.as_deref()?).ok()
}

/// Whether every project of `spec` besides `root` is reached from `root` by a
/// chain of project references, through any framework.
fn every_project_is_reached(spec: &DependencyGraphSpec, root: &PackageSpec) -> bool {
    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<&PackageSpec> = VecDeque::from([root]);
    while let Some(project) = queue.pop_front() {
        if !reached.insert(project.restore.project_unique_name.to_ascii_lowercase()) {
            continue;
        }
        for restore_framework in &project.restore.frameworks {
            for reference in &restore_framework.project_references {
                if let Some(referenced) = find_project(spec, &reference.project_unique_name) {
                    queue.push_back(referenced);
                }
            }
        }
    }
    spec.projects
        .iter()
        .all(|project| reached.contains(&project.restore.project_unique_name.to_ascii_lowercase()))
}

/// Whether every project of `spec` is named by a package id (the root too,
/// once the spec holds another project or any project reference, since a
/// reference may name it), and no two share a name ignoring case.
fn project_names_are_distinct_ids(spec: &DependencyGraphSpec, root: &PackageSpec) -> bool {
    let references_exist = spec.projects.iter().any(|project| {
        project
            .restore
            .frameworks
            .iter()
            .any(|framework| !framework.project_references.is_empty())
    });
    let root_is_a_node = spec.projects.len() > 1 || references_exist;
    let mut names: BTreeSet<String> = BTreeSet::new();
    spec.projects.iter().all(|project| {
        let Some(name) = project.restore.project_name.as_deref() else {
            return false;
        };
        ((std::ptr::eq(project, root) && !root_is_a_node) || PackageId::parse(name).is_ok())
            && names.insert(name.to_ascii_lowercase())
    })
}

/// What a framework of the spec says about which dependencies each package
/// contributes: its asset-target fallback list, and its prune list as each
/// entry's upper bound (an entry without one declines before here).
fn target_context(framework: &TargetFrameworkInformation) -> TargetContext {
    TargetContext {
        asset_target_fallback: if framework.asset_target_fallback {
            framework.imports.clone()
        } else {
            Vec::new()
        },
        prune: framework
            .packages_to_prune
            .iter()
            .filter_map(|entry| Some((entry.id.clone(), entry.range.max_version()?.clone())))
            .collect(),
    }
}

/// Whether restore, logging `code` as a warning, fails under `policy`: the
/// code is promoted by `warnAsError`, or by `allWarningsAsErrors` unless
/// `warnNotAsError` demotes it. (`noWarn` has already removed the codes it
/// suppresses.) A code both promoted and demoted by name counts as failing,
/// since which wins is not modelled.
fn fails_restore(policy: &WarningProperties, code: &NuGetLogCode) -> bool {
    let WarningProperties {
        all_warnings_as_errors,
        warnings_as_errors,
        no_warn,
        warnings_not_as_errors,
    } = policy;
    !no_warn.contains(code)
        && (warnings_as_errors.contains(code)
            || (*all_warnings_as_errors && !warnings_not_as_errors.contains(code)))
}

/// Whether `policy` promotes any warning at all, so that a framework whose
/// warnings B₁ cannot predict exactly could fail.
fn promotes_anything(policy: &WarningProperties) -> bool {
    policy.all_warnings_as_errors || !policy.warnings_as_errors.is_empty()
}

/// Whether restore certainly ignores lock files: none is requested, locked mode
/// is off, and nothing exists at the lock file's stated path, which must be
/// absolute. (Without a stated path, NuGet looks beside the project, under a
/// name that depends on it.)
fn lock_file_is_off(lock: &RestoreLockProperties) -> bool {
    let RestoreLockProperties {
        restore_packages_with_lock_file,
        nuget_lock_file_path,
        restore_locked_mode,
    } = lock;
    let not_requested = restore_packages_with_lock_file
        .as_deref()
        .is_none_or(|value| value.eq_ignore_ascii_case("false"));
    // A relative path is NuGet's to resolve against the project, so only an
    // absolute one is checked.
    let absent = nuget_lock_file_path.as_deref().is_some_and(|path| {
        let path = Path::new(path);
        path.is_absolute() && matches!(path.try_exists(), Ok(false))
    });
    not_requested && !*restore_locked_mode && absent
}

/// Whether audit is explicitly off, which makes its level, mode and
/// suppressions inert.
fn audit_is_off(audit: Option<&RestoreAuditProperties>) -> bool {
    let Some(RestoreAuditProperties {
        enable_audit,
        audit_level: _,
        audit_mode: _,
        suppressed_advisories: _,
    }) = audit
    else {
        return false;
    };
    enable_audit
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("false"))
}
