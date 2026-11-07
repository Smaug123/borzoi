//! B₁ of `docs/nuget-dgspec-boundary-plan.md`: the offline resolver
//! ([`crate::resolve_offline_with_fallback`]) consuming a parsed
//! dependency-graph spec.
//!
//! [`resolve_dgspec`] commits only where NuGet's restore over the same spec
//! (B₀: `RestoreRunner`, run by the NuGet the caller names) would succeed and
//! write the same package closure for every target framework of the project.
//! `tests/all/dgspec_resolve_diff.rs` holds it to that.
//!
//! # Every field has a verdict
//!
//! The resolver models a project's direct package references and nothing else.
//! Every other field of the spec is in one of three classes, and
//! [`unmodelled_fields`] destructures every struct the spec is made of, so a
//! field added to the model does not compile until it has been put in one:
//!
//! - **Modelled.** Direct dependencies; each framework's asset-target fallback
//!   list; the packages folder the resolver reads.
//! - **Inert.** Fields that cannot move the closure, or turn a success into a
//!   failure, of a restore the resolver commits to. Each one's reason is
//!   stated at the point it is discarded. The reasons the differential cannot
//!   check by construction are named there too.
//! - **Declined when present.** Each is an [`UnmodelledField`], reported all at
//!   once rather than at the first, so the corpus census can count how many
//!   projects each one blocks (stages 5–7 are ordered by those counts).
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

use super::{
    DependencyGraphSpec, DownloadDependency, FrameworkReference, LibraryDependency,
    LibraryDependencyTarget, NuGetLogCode, PackageRangeEntry, PackageSpec, ProjectRestoreMetadata,
    ProjectRestoreReference, RestoreAuditProperties, RestoreFrameworkInfo, RestoreLockProperties,
    TargetFrameworkInformation, WarningProperties,
};
use crate::{
    DirectPackageRequirement, NuGetFramework, PackageId, ResolveDecline, ResolvedPackageClosure,
    resolve_offline_with_fallback,
};

use std::collections::BTreeSet;
use std::error::Error;
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
    /// `restore.projectName` is absent, so the name of the graph's root, which
    /// a package of the same name would collide with, is unknown.
    ProjectName,
    /// `restore.packagesPath` is absent, so NuGet would find the packages
    /// folder from the environment and config files.
    PackagesPath,
    /// A source that is not a local absolute path. A local folder can only
    /// add versions, and every version the resolver relies on is the lowest
    /// a range accepts and is in the packages folder, where NuGet looks first;
    /// a remote source can fail the restore by being unreachable.
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
    /// A warning policy that could turn a warning restore emits on a committed
    /// closure into an error: `allWarningsAsErrors`, or a `warnAsError` code
    /// other than `NU1605` (which the resolver declines on as a failure).
    WarningPolicy,
    /// Central package management (stage 7a).
    CentralPackageManagement,
    /// A framework's project references (stage 6).
    ProjectReferences,
    /// A framework's `packagesToPrune` (stage 5).
    PackagesToPrune,
    /// A framework's `downloadDependencies` (stage 7b): a download missing
    /// from the packages folder fails the restore.
    DownloadDependencies,
    /// An `imports` list that is not an asset-target fallback (the legacy
    /// `PackageTargetFallback`).
    PackageTargetFallback,
    /// A dependency whose `target` is not `Package`, which a project could
    /// satisfy.
    DependencyTarget,
    /// The project has no target framework.
    NoFrameworks,
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
            UnmodelledField::WarningPolicy => "warning-policy",
            UnmodelledField::CentralPackageManagement => "central-package-management",
            UnmodelledField::ProjectReferences => "project-references",
            UnmodelledField::PackagesToPrune => "packages-to-prune",
            UnmodelledField::DownloadDependencies => "download-dependencies",
            UnmodelledField::PackageTargetFallback => "package-target-fallback",
            UnmodelledField::DependencyTarget => "dependency-target",
            UnmodelledField::NoFrameworks => "no-frameworks",
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
    /// A package in a framework's closure has the project's own name, which
    /// NuGet's graph has as its root.
    PackageNamedLikeProject { target_alias: String, id: PackageId },
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
            DgSpecResolveDecline::PackageNamedLikeProject { target_alias, id } => write!(
                f,
                "framework {target_alias:?}: package {id} has the project's own name"
            ),
        }
    }
}

impl Error for DgSpecResolveDecline {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            DgSpecResolveDecline::Resolve { decline, .. } => Some(decline.as_ref()),
            _ => None,
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
}

/// Resolve every framework of the project whose unique name is `project`, as
/// NuGet `nuget` would restore `spec`, or decline. The frameworks are in the
/// spec's order.
///
/// The packages folder is the spec's own `packagesPath`; it is the only part
/// of the file system read, besides a check that no lock file exists at the
/// spec's `nuGetLockFilePath`.
pub fn resolve_dgspec(
    nuget: &NuGetIdentity,
    spec: &DependencyGraphSpec,
    project: &str,
) -> Result<Vec<ResolvedFramework>, DgSpecResolveDecline> {
    if !nuget.is_verified() {
        return Err(DgSpecResolveDecline::UnverifiedNuGet {
            identity: nuget.clone(),
        });
    }
    let package_spec = spec
        .projects
        .iter()
        .find(|candidate| candidate.restore.project_unique_name == project)
        .filter(|_| spec.restore.iter().any(|restored| restored == project))
        .ok_or_else(|| DgSpecResolveDecline::ProjectNotRestored {
            project: project.to_owned(),
        })?;

    let fields = unmodelled_fields(package_spec);
    if !fields.is_empty() {
        return Err(DgSpecResolveDecline::Unmodelled { fields });
    }
    let (Some(packages_path), Some(project_name)) = (
        package_spec.restore.packages_path.as_deref(),
        package_spec.restore.project_name.as_deref(),
    ) else {
        unreachable!("an absent packages path or project name is an unmodelled field")
    };

    package_spec
        .frameworks
        .iter()
        .map(|framework| {
            let direct: Vec<DirectPackageRequirement> = framework
                .dependencies
                .iter()
                .map(|dependency| {
                    DirectPackageRequirement::new(dependency.id.clone(), dependency.range.clone())
                })
                .collect();
            let fallback: &[NuGetFramework] = if framework.asset_target_fallback {
                &framework.imports
            } else {
                &[]
            };
            let closure = resolve_offline_with_fallback(
                Path::new(packages_path),
                &framework.framework,
                fallback,
                &direct,
            )
            .map_err(|decline| DgSpecResolveDecline::Resolve {
                target_alias: framework.target_alias.clone(),
                decline: Box::new(decline),
            })?;
            if let Some(clash) = closure.packages.iter().find(|package| {
                package
                    .identity
                    .id
                    .as_str()
                    .eq_ignore_ascii_case(project_name)
            }) {
                return Err(DgSpecResolveDecline::PackageNamedLikeProject {
                    target_alias: framework.target_alias.clone(),
                    id: clash.identity.id.clone(),
                });
            }
            Ok(ResolvedFramework {
                target_alias: framework.target_alias.clone(),
                framework: framework.framework.clone(),
                closure,
            })
        })
        .collect()
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
        // package resolves to unless it shares the project's name, and that
        // declines on its own (`PackageNamedLikeProject`).
        version: _,
        is_default_version: _,
        restore,
        frameworks,
    } = spec;

    let ProjectRestoreMetadata {
        // How the spec names the project; `resolve_dgspec` looks it up by it.
        project_unique_name: _,
        project_name,
        // Where the project and its outputs are, not what it resolves.
        project_path: _,
        output_path: _,
        packages_path,
        sources,
        fallback_folders,
        // NuGet reads its settings from these files. The settings that bear on
        // resolution are already in the spec (sources, fallback folders, the
        // packages folder), except package source mapping, which filters the
        // sources and never the packages folder. The rest govern how packages
        // are fetched and extracted (credentials, proxies, signature
        // validation), and a committed closure fetches nothing. A hand case in
        // the differential pins the source mapping half.
        config_file_paths: _,
        frameworks: restore_frameworks,
        // Names, and whether there is more than one framework.
        original_target_frameworks: _,
        cross_targeting: _,
        // Whether content files are written into the project's output.
        skip_content_file_write: _,
        warning_properties,
        restore_lock_properties,
        central_package_versions_enabled,
        central_package_transitive_pinning_enabled,
        restore_audit_properties,
        // Graded by the differential, whose generator varies it.
        sdk_analysis_level: _,
        using_microsoft_net_sdk,
    } = restore;

    if project_name.is_none() {
        decline(UnmodelledField::ProjectName);
    }
    if packages_path.is_none() {
        decline(UnmodelledField::PackagesPath);
    }
    if !sources.iter().all(|source| is_local_absolute_path(source)) {
        decline(UnmodelledField::Sources);
    }
    if !fallback_folders.is_empty() {
        decline(UnmodelledField::FallbackFolders);
    }
    if !warning_policy_is_inert(warning_properties) {
        decline(UnmodelledField::WarningPolicy);
    }
    if !lock_file_is_off(restore_lock_properties) {
        decline(UnmodelledField::LockFile);
    }
    if *central_package_versions_enabled || *central_package_transitive_pinning_enabled {
        decline(UnmodelledField::CentralPackageManagement);
    }
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
                project_unique_name: _,
                project_path: _,
                include_assets: _,
                exclude_assets: _,
                private_assets: _,
            } = reference;
            decline(UnmodelledField::ProjectReferences);
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
            // Whether restore warns (NU1701) when it uses the fallback list: a
            // warning, and the warning policy admits no code but NU1605.
            warn: _,
            download_dependencies,
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
                // Graded by the differential, whose generator varies it.
                auto_referenced: _,
                version_centrally_managed,
                // Outputs of restore for the build, not inputs to resolution.
                generate_path_property: _,
                aliases: _,
                version_override,
            } = dependency;
            if *target != LibraryDependencyTarget::PACKAGE {
                decline(UnmodelledField::DependencyTarget);
            }
            if *version_centrally_managed || version_override.is_some() {
                decline(UnmodelledField::CentralPackageManagement);
            }
        }
        if !imports.is_empty() && !*asset_target_fallback {
            decline(UnmodelledField::PackageTargetFallback);
        }
        for download in download_dependencies {
            let DownloadDependency { id: _, range: _ } = download;
            decline(UnmodelledField::DownloadDependencies);
        }
        for entry in central_package_versions {
            let PackageRangeEntry { id: _, range: _ } = entry;
            decline(UnmodelledField::CentralPackageManagement);
        }
        for reference in framework_references {
            let FrameworkReference {
                name: _,
                private_assets: _,
            } = reference;
        }
        for entry in packages_to_prune {
            let PackageRangeEntry { id: _, range: _ } = entry;
            decline(UnmodelledField::PackagesToPrune);
        }
    }

    fields
}

/// A source the resolver admits: a local folder named by an absolute path.
fn is_local_absolute_path(source: &str) -> bool {
    !source.contains("://") && Path::new(source).is_absolute()
}

/// Whether the policy could not turn a warning restore emits on a committed
/// closure into an error. `noWarn` and `warningsNotAsErrors` only ever demote,
/// so they are inert. `NU1605` is the one code admitted to `warnAsError`: it is
/// the downgrade the resolver declines on as a failure whatever the policy (and
/// the code the SDK promotes for C# projects).
fn warning_policy_is_inert(policy: &WarningProperties) -> bool {
    let WarningProperties {
        all_warnings_as_errors,
        warnings_as_errors,
        no_warn: _,
        warnings_not_as_errors: _,
    } = policy;
    let downgrade = NuGetLogCode::parse("NU1605").expect("NU1605 is a NuGet log code");
    !*all_warnings_as_errors && warnings_as_errors.iter().all(|code| *code == downgrade)
}

/// Whether restore certainly ignores lock files: none is requested, locked mode
/// is off, and nothing exists at the lock file's stated path. (Without a stated
/// path, NuGet looks beside the project, under a name that depends on it.)
fn lock_file_is_off(lock: &RestoreLockProperties) -> bool {
    let RestoreLockProperties {
        restore_packages_with_lock_file,
        nuget_lock_file_path,
        restore_locked_mode,
    } = lock;
    let not_requested = restore_packages_with_lock_file
        .as_deref()
        .is_none_or(|value| value.eq_ignore_ascii_case("false"));
    let absent = nuget_lock_file_path
        .as_deref()
        .is_some_and(|path| matches!(Path::new(path).try_exists(), Ok(false)));
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
