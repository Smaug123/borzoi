//! NuGet primitives for the warm-cache in-house restore
//! (`docs/nuget-restore-plan.md` at the workspace root).
//!
//! The goal is to compute the same assets graph `dotnet restore` would
//! compute, **offline**, against an already-populated global packages
//! folder — or decline. Nothing here talks to a feed. Every primitive is
//! differentially tested against the real NuGet client libraries via
//! `tools/nuget-oracle` (test-only; the oracle never ships in the LSP).
//!
//! Current surface:
//!
//! - [`version::NuGetVersion`] — NuGet's version model: SemVer 2.0.0 *plus*
//!   NuGet's deviations (an optional 4th "revision" part, case-insensitive
//!   release-label comparison, metadata-insensitive equality, partial
//!   versions like `1` / `1.2`).
//! - [`range::VersionRange`] — NuGet's range model: bare minimums,
//!   interval notation, floating patterns (parsed so the resolver can
//!   recognise and decline them), and bounds-only `satisfies`.
//! - [`framework::NuGetFramework`] — NuGet's target-framework model: TFM
//!   parsing, the compatibility relation, nearest-candidate selection.
//! - [`package`] — committed package-version enumeration and exact
//!   installed-package lookup in a global packages folder, nuspec
//!   dependency-group projection, and dependency-group selection for a target
//!   framework.
//! - [`resolver`] — conservative offline package-closure resolution over an
//!   explicit caller-supplied global packages root.
//! - [`assets`] — NuGet's content model, restricted to compile assets: which
//!   assemblies of a resolved package a project actually compiles against.
//!
//! This crate never reads `$NUGET_PACKAGES`, `$HOME`, or NuGet.config by
//! itself. Callers own environment/config discovery and pass concrete roots
//! and typed inputs here.
//!
//! Self-contained by design: no dependencies on the other workspace crates,
//! reusable outside this repo.

pub mod assets;
pub mod framework;
pub mod package;
pub mod range;
pub mod resolver;
pub mod version;

pub use assets::{
    AssetSelectionDecline, CompileAssets, list_package_files, select_compile_assets,
    select_installed_compile_assets,
};
pub use framework::{FrameworkParseError, NuGetFramework};
pub use package::{
    InstalledPackage, PackageCacheEntry, PackageCacheError, PackageDependency,
    PackageDependencyGroup, PackageId, PackageIdParseError, PackageIdentity, PackageNuspec,
    PackageNuspecParseError, PackagePaths, PackageReadError, PackageReferenceGroup,
    list_committed_package_versions, parse_nuspec, read_installed_package,
};
pub use range::{FloatBehavior, RangeParseError, VersionRange};
pub use resolver::{
    DirectPackageRequirement, ResolveDecline, ResolvedPackage, ResolvedPackageClosure,
    resolve_offline,
};
pub use version::{NuGetVersion, VersionParseError};
