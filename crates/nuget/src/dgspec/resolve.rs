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
//! # Restore's compatibility check
//!
//! After resolving, restore fails (`NU1202`) on a package with `lib/` or `ref/`
//! assets none of which suit the framework. The resolver reads only nuspecs, so
//! [`resolve_dgspec`] reads each resolved package's files and commits only for
//! packages with no assets, or with compile assets for the framework.
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
    AssetSelectionDecline, DirectPackageRequirement, NuGetFramework, NuGetVersion, PackageId,
    PackageIdentity, PackageNuspec, PackageReadError, ResolveDecline, ResolvedPackageClosure,
    list_package_files, read_installed_package, resolve_offline_with_fallback,
    select_compile_assets,
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
    /// `restore.projectName` is absent or empty: restore requires it, and it
    /// names the graph's root, which a package of the same name collides with.
    ProjectName,
    /// `restore.packagesPath` is absent, so NuGet would find the packages
    /// folder from the environment and config files, or holds a control
    /// character.
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
    /// Metadata NuGet requires is absent or inconsistent, so restore throws:
    /// the project's path or output path, or `originalTargetFrameworks`
    /// naming exactly the aliases of the frameworks and of the restore
    /// metadata's frameworks, each alias once ignoring case, in the characters
    /// the SDK writes, and each naming a different framework.
    RequiredMetadata,
    /// A config file NuGet loads its settings from is outside the vocabulary
    /// B₁ admits (see `config_is_admitted`).
    Config,
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
            UnmodelledField::RequiredMetadata => "required-metadata",
            UnmodelledField::Config => "config",
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
    /// A package in a framework's closure carries assets, but none the
    /// framework is shown to compile against, so restore's compatibility
    /// check (`NU1202`) may fail it. `decline` says why compile-asset
    /// selection could not answer, when it could not.
    CompatibilityUnproven {
        target_alias: String,
        package: Box<PackageIdentity>,
        decline: Option<Box<AssetSelectionDecline>>,
    },
    /// A package in a framework's closure carries metadata restore validates a
    /// package by, outside the domain B₁ admits: a package type other than
    /// `Dependency`, or a minimum client version not shown to be below the
    /// restoring NuGet's.
    PackageMetadata {
        target_alias: String,
        package: Box<PackageIdentity>,
    },
    /// A package in a framework's closure could not be read back for the
    /// compatibility check.
    PackageRead {
        target_alias: String,
        package: Box<PackageIdentity>,
        source: Box<PackageReadError>,
    },
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
            } => write!(
                f,
                "framework {target_alias:?}: package {} {} has a package type or minimum client \
                 version restore may refuse",
                package.id, package.version
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
            check_metadata(
                nuget,
                Path::new(packages_path),
                &framework.target_alias,
                &closure,
            )?;
            check_compatibility(
                Path::new(packages_path),
                &framework.target_alias,
                &framework.framework,
                &closure,
            )?;
            Ok(ResolvedFramework {
                target_alias: framework.target_alias.clone(),
                framework: framework.framework.clone(),
                closure,
            })
        })
        .collect()
}

/// Restore's compatibility check, in the direction that can only decline:
/// every package of `closure` either carries no assets at all, or has compile
/// assets for `framework`.
///
/// Restore fails (`NU1202`) on a package with `lib/` or `ref/` assets of which
/// none suits the framework, and that holds even where the asset-target
/// fallback list would admit them. Compile assets for the framework are among
/// the assets that make a package compatible, and a package with no file below
/// its root has nothing to be incompatible with. Anything else declines,
/// including a package whose only assets are in other folders (`build/`,
/// `analyzers/`), which this does not model.
fn check_compatibility(
    packages: &Path,
    target_alias: &str,
    framework: &NuGetFramework,
    closure: &ResolvedPackageClosure,
) -> Result<(), DgSpecResolveDecline> {
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
        // Root-level files are the package's bookkeeping (the nuspec, the
        // commit marker); assets are in folders.
        if !files.iter().any(|file| file.contains('/')) {
            continue;
        }
        let compile = select_compile_assets(&files, &package.nuspec, framework)
            .map_err(|decline| unproven(Some(decline)))?;
        if compile.items.is_empty() {
            return Err(unproven(None));
        }
    }
    Ok(())
}

/// Every version restore may read for `closure` ([`ResolvedPackageClosure::examined`])
/// has metadata and a commit marker in the domain B₁ admits.
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
        if !metadata_is_admitted(&package.nuspec, identity, nuget)
            || !commit_marker_is_admitted(&package.paths.metadata_path)
        {
            return Err(DgSpecResolveDecline::PackageMetadata {
                target_alias: target_alias.to_owned(),
                package: Box::new(identity.clone()),
            });
        }
    }
    Ok(())
}

/// Whether a package's nuspec metadata is in the closed vocabulary B₁ admits.
///
/// Restore validates parts of a package's nuspec even from a warm cache, and
/// throws or fails on what it rejects; which parts, and how, is not something
/// a list of known refusals can bound (review found a new one in each of three
/// rounds). So every element and attribute of `<metadata>` has a verdict here,
/// and anything without one declines:
///
/// - **Descriptive**, never read by restore: [`DESCRIPTIVE_ELEMENTS`].
/// - **Modelled**: `dependencies` and `references`, which the nuspec reader
///   parses (a nuspec it cannot parse never reaches here); `id` and `version`,
///   each present exactly once and naming the package's own folder; `packageTypes`, which must
///   declare nothing but `Dependency`, without a version (restore refuses
///   other types in a `PackageReference` project, `DotnetTool` with `NU1212`,
///   and throws on a malformed version); and the `minClientVersion`
///   attribute, which must parse without a release label and be below the
///   restoring NuGet's own version. The comparison is strict because the
///   verified NuGets are release candidates, which sort below the release of
///   the same number.
/// - **Declined** otherwise, `contentFiles`, `frameworkAssemblies`,
///   `frameworkReferences` and `developmentDependency` among them: restore
///   reads each, and B₁ does not model how.
fn metadata_is_admitted(
    nuspec: &PackageNuspec,
    identity: &PackageIdentity,
    nuget: &NuGetIdentity,
) -> bool {
    let attributes_admitted = nuspec
        .metadata_attributes
        .iter()
        .all(|attribute| attribute == "minClientVersion");
    let count = |name: &str| {
        nuspec
            .metadata_elements
            .iter()
            .filter(|element| element.name == name)
            .count()
    };
    let identity_stated = count("id") == 1 && count("version") == 1;
    let elements_admitted = nuspec.metadata_elements.iter().all(|element| {
        let text = element.text.as_deref();
        match element.name.as_str() {
            "id" => text.is_some_and(|id| id.eq_ignore_ascii_case(identity.id.as_str())),
            "version" => text
                .and_then(|version| NuGetVersion::parse(version).ok())
                .is_some_and(|version| version.eq_strict(&identity.version)),
            "dependencies" | "references" | "packageTypes" => true,
            name => DESCRIPTIVE_ELEMENTS.contains(&name),
        }
    });
    let types_admitted = nuspec
        .package_types
        .iter()
        .all(|package_type| package_type.name == "Dependency" && package_type.version.is_none());
    let client_admitted = match &nuspec.min_client_version {
        None => true,
        Some(minimum) => {
            let release = nuget
                .as_str()
                .split(['-', '+'])
                .next()
                .and_then(|release| NuGetVersion::parse(release).ok());
            match (NuGetVersion::parse(minimum), release) {
                (Ok(minimum), Some(release)) => !minimum.is_prerelease() && minimum < release,
                _ => false,
            }
        }
    };
    attributes_admitted && identity_stated && elements_admitted && types_admitted && client_admitted
}

/// The `<metadata>` elements restore never reads: what a package says about
/// itself for people and galleries. The differential's generator writes them,
/// malformed ones included, so the claim is graded.
const DESCRIPTIVE_ELEMENTS: &[&str] = &[
    "authors",
    "owners",
    "title",
    "description",
    "summary",
    "releaseNotes",
    "copyright",
    "language",
    "tags",
    "projectUrl",
    "iconUrl",
    "icon",
    "licenseUrl",
    "license",
    "readme",
    "repository",
    "requireLicenseAcceptance",
    "serviceable",
];

/// Whether a package's `.nupkg.metadata` commit marker is one restore reads
/// without complaint: a JSON object whose `contentHash` is a string (empty is
/// accepted: Nix-built caches write `{ "contentHash": "" }`), and whose only
/// other keys are `version`, 1 or 2, and `source`, a string or null. Restore
/// throws on a marker without a content hash, or with a null one, which the
/// package readers, checking only that the marker exists, cannot see.
fn commit_marker_is_admitted(marker: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(marker) else {
        return false;
    };
    let Ok(serde_json::Value::Object(members)) = serde_json::from_str(&text) else {
        return false;
    };
    members.iter().all(|(key, value)| match key.as_str() {
        "version" => matches!(value.as_u64(), Some(1 | 2)),
        "contentHash" => value.is_string(),
        "source" => value.is_string() || value.is_null(),
        _ => false,
    }) && members.contains_key("contentHash")
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
        warning_properties,
        restore_lock_properties,
        central_package_versions_enabled,
        central_package_transitive_pinning_enabled,
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
    if !path_admitted(project_path)
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
fn config_is_admitted(path: &Path) -> bool {
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
