//! The restore cone's item seam (stage A₁.1 of
//! `docs/nuget-dgspec-boundary-plan.md`): the dependency-graph spec
//! `WriteRestoreGraphTask` writes for the `_RestoreGraphEntry` items the
//! restore targets produce.
//!
//! The task wraps each item, hands the list to `NuGet.Commands`'
//! `MSBuildRestoreUtility.GetDependencySpec`, adds every restorable project to
//! `restore` when `RestoreRecursive` is set, and saves the spec. That is
//! NuGet library code with no MSBuild in it, so it is a function of the
//! items alone. [`write_restore_graph`] is that function: it returns the
//! file's text, byte for byte, and the spec [`parse_dgspec`] reads from it,
//! or declines. `tests/all/restore_items_diff.rs` holds it to
//! certain-implies-exact against the task's own code, run by the oracle: a
//! commit's text equals the file NuGet saves. Like [`resolve_dgspec`], it
//! models NuGet's code, so it commits only for a NuGet on the verified list,
//! which both differentials grade.
//!
//! [`resolve_dgspec`]: super::resolve_dgspec
//!
//! Producing the text rather than the spec keeps the model to what the task
//! does. The writer sorts, drops and groups as it saves (a warning policy
//! with only `WarningsNotAsErrors` is not written at all; two downloads of
//! one package become one entry), and what NuGet's reader makes of the
//! result is already modelled, and graded, by [`parse_dgspec`].
//!
//! The model declines rather than reproduce NuGet where NuGet's answer is
//! not a function of the items:
//!
//! - **The host.** NuGet compares project paths case-insensitively on a
//!   case-insensitive file system and ordinally elsewhere, and sorts them the
//!   same way. Paths that compare equal one way and not the other, or that
//!   sort differently, decline. So does a `file:/` source, which NuGet
//!   completes with two slashes on Windows and three elsewhere, and a list
//!   of downloads whose order depends on the culture.
//! - **The environment.** `NUGET_ENABLE_EXPERIMENTAL_MACROS` makes the writer
//!   abbreviate paths under the user's settings folder.
//!
//! And, as [`parse_dgspec`] does, it accepts only the vocabulary it models:
//! - Every item type and metadata name it does not know declines, and so does
//!   every value it does not model. That includes:
//!   - a project style other than `PackageReference`;
//!   - runtime identifiers;
//!   - `PackageTargetFallback`;
//!   - a C++/CLI project;
//!   - a platform-qualified `net5.0`-era framework.
//! - A value NuGet rejects by throwing fails the task, so it declines too.
//! - A NUL declines anywhere, a control character wherever it would be
//!   written, and non-ASCII wherever NuGet compares or sorts ignoring case.
//!   .NET's case folding beyond ASCII is not worth reproducing for names that
//!   are ASCII in practice.

use super::{DependencyGraphSpec, DgSpecDecline, NuGetIdentity, NuGetLogCode, parse_dgspec};
use crate::{NuGetFramework, NuGetVersion, PackageId, VersionRange};

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;

/// One `_RestoreGraphEntry` as `WriteRestoreGraphTask` reads it: the item's
/// spec and its custom metadata, each value as `ITaskItem.GetMetadata`
/// returns it (unescaped, untrimmed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreGraphItem {
    pub identity: String,
    pub metadata: Vec<(String, String)>,
}

/// The file `WriteRestoreGraphTask` would write, and the spec read from it.
#[derive(Debug, Clone)]
pub struct RestoreGraphFile {
    pub text: String,
    pub spec: DependencyGraphSpec,
}

/// Why [`write_restore_graph`] makes no claim.
#[derive(Debug, Clone)]
pub enum RestoreItemsDecline {
    /// The task's NuGet is not one the differential has graded.
    UnverifiedNuGet { identity: NuGetIdentity },
    /// An environment variable that moves what the writer writes.
    Environment { variable: String },
    /// An item outside the modelled vocabulary: a type or metadata name the
    /// model does not know, or a name given twice.
    Vocabulary { item: usize, detail: String },
    /// A value NuGet reads that the model does not cover, or one NuGet
    /// rejects by throwing.
    Value {
        item: usize,
        name: &'static str,
        value: String,
        detail: &'static str,
    },
    /// NuGet's answer would depend on the host: its file system's case
    /// sensitivity, its operating system, or its culture.
    HostDependent { detail: String },
    /// The file the task would write is one [`parse_dgspec`] declines.
    Unreadable { decline: DgSpecDecline },
}

impl fmt::Display for RestoreItemsDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreItemsDecline::UnverifiedNuGet { identity } => {
                write!(f, "NuGet {identity} is not on the verified list")
            }
            RestoreItemsDecline::Environment { variable } => {
                write!(f, "the environment sets {variable}")
            }
            RestoreItemsDecline::Vocabulary { item, detail } => {
                write!(f, "item {item}: {detail}")
            }
            RestoreItemsDecline::Value {
                item,
                name,
                value,
                detail,
            } => write!(f, "item {item}: {name} = {value:?}: {detail}"),
            RestoreItemsDecline::HostDependent { detail } => {
                write!(f, "host-dependent: {detail}")
            }
            RestoreItemsDecline::Unreadable { decline } => {
                write!(f, "the written spec is not readable: {decline}")
            }
        }
    }
}

impl std::error::Error for RestoreItemsDecline {}

type Result<T> = std::result::Result<T, RestoreItemsDecline>;

/// The variable that makes `PackageSpecWriter` abbreviate paths with
/// `$(User)`.
const MACROS_VARIABLE: &str = "NUGET_ENABLE_EXPERIMENTAL_MACROS";

/// What `WriteRestoreGraphTask` writes for `items`, under `nuget` (the
/// `NuGet.Commands` the task loads) and the process `environment`, with the
/// task's `RestoreRecursive` set to `restore_recursive`.
pub fn write_restore_graph(
    items: &[RestoreGraphItem],
    restore_recursive: bool,
    nuget: &NuGetIdentity,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<RestoreGraphFile> {
    if !nuget.is_verified() {
        return Err(RestoreItemsDecline::UnverifiedNuGet {
            identity: nuget.clone(),
        });
    }
    if environment.contains_key(OsStr::new(MACROS_VARIABLE)) {
        return Err(RestoreItemsDecline::Environment {
            variable: MACROS_VARIABLE.to_owned(),
        });
    }
    let items = items
        .iter()
        .enumerate()
        .map(|(index, item)| Item::read(index, item))
        .collect::<Result<Vec<_>>>()?;
    let graph = Graph::build(&items, restore_recursive)?;
    let text = graph.write()?;
    let spec =
        parse_dgspec(&text).map_err(|decline| RestoreItemsDecline::Unreadable { decline })?;
    Ok(RestoreGraphFile { text, spec })
}

// ---------------------------------------------------------------------------
// Items.
// ---------------------------------------------------------------------------

/// The item types `GetDependencySpec` reads (its `Type` metadata, compared
/// ignoring case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    RestoreSpec,
    ProjectSpec,
    TargetFrameworkInformation,
    ProjectReference,
    Dependency,
    DownloadDependency,
    FrameworkReference,
    PrunePackageReference,
    CentralPackageVersion,
    NuGetAuditSuppress,
}

impl Kind {
    const ALL: [(&'static str, Kind); 10] = [
        ("RestoreSpec", Kind::RestoreSpec),
        ("ProjectSpec", Kind::ProjectSpec),
        (
            "TargetFrameworkInformation",
            Kind::TargetFrameworkInformation,
        ),
        ("ProjectReference", Kind::ProjectReference),
        ("Dependency", Kind::Dependency),
        ("DownloadDependency", Kind::DownloadDependency),
        ("FrameworkReference", Kind::FrameworkReference),
        ("PrunePackageReference", Kind::PrunePackageReference),
        ("CentralPackageVersion", Kind::CentralPackageVersion),
        ("NuGetAuditSuppress", Kind::NuGetAuditSuppress),
    ];

    /// The metadata an item of this kind may carry: what `GetDependencySpec`
    /// reads from it, and what the restore targets and tasks write on it that
    /// it does not read (each such name noted), besides
    /// [`MSBUILD_TASK_METADATA`].
    fn vocabulary(self) -> &'static [&'static str] {
        match self {
            Kind::RestoreSpec => &["Type", "ProjectUniqueName"],
            Kind::ProjectSpec => &[
                "Type",
                "ProjectUniqueName",
                "Version",
                "ProjectPath",
                "ProjectName",
                "Sources",
                "FallbackFolders",
                "PackagesPath",
                "ProjectStyle",
                "OutputPath",
                "RuntimeIdentifiers",
                "RuntimeSupports",
                "CrossTargeting",
                "RestoreLegacyPackagesDirectory",
                "ValidateRuntimeAssets",
                "SkipContentFileWrite",
                "ConfigFilePaths",
                "TreatWarningsAsErrors",
                "WarningsAsErrors",
                "WarningsNotAsErrors",
                "NoWarn",
                "RestorePackagesWithLockFile",
                "NuGetLockFilePath",
                "RestoreLockedMode",
                "_CentralPackageVersionsEnabled",
                "CentralPackageFloatingVersionsEnabled",
                "CentralPackageVersionOverrideEnabled",
                "CentralPackageTransitivePinningEnabled",
                "NuGetAudit",
                "NuGetAuditLevel",
                "NuGetAuditMode",
                "SdkAnalysisLevel",
                "UsingMicrosoftNETSdk",
                "NETCoreSdkVersion",
                "RestoreUseLegacyDependencyResolver",
                "RestoreDoNotWriteDependencyGraphSpec",
                "RestoreEnableAnalyzerAssets",
                // Read only for the packages.config, tool and unknown styles,
                // which decline.
                "TargetFrameworks",
                "PackagesConfigPath",
                "SolutionDir",
                "RepositoryPath",
            ],
            Kind::TargetFrameworkInformation => &[
                "Type",
                "ProjectUniqueName",
                "TargetFramework",
                "TargetFrameworkMoniker",
                "TargetPlatformMoniker",
                "TargetPlatformMinVersion",
                "PackageTargetFallback",
                "AssetTargetFallback",
                "RuntimeIdentifierGraphPath",
                "RestoreEnablePackagePruning",
                "RestorePackagePruningDefault",
                "NuGetAuditMode",
                "RestoreEnableAnalyzerAssets",
                // Read only for a C++ project, which declines.
                "CLRSupport",
                "WindowsTargetPlatformMinVersion",
                // Written by `_GenerateProjectRestoreGraphPerFramework`, not
                // read: the framework comes from the monikers.
                "TargetFrameworkIdentifier",
                "TargetFrameworkVersion",
                "TargetFrameworkProfile",
                "TargetPlatformIdentifier",
                "TargetPlatformVersion",
            ],
            Kind::ProjectReference => &[
                "Type",
                "ProjectUniqueName",
                "ProjectPath",
                "ProjectReferenceUniqueName",
                "TargetFrameworks",
                "IncludeAssets",
                "ExcludeAssets",
                "PrivateAssets",
            ],
            Kind::Dependency => &[
                "Type",
                "ProjectUniqueName",
                "Id",
                "VersionRange",
                "VersionOverride",
                "TargetFrameworks",
                "IncludeAssets",
                "ExcludeAssets",
                "PrivateAssets",
                "NoWarn",
                "IsImplicitlyDefined",
                "GeneratePathProperty",
                "Aliases",
            ],
            Kind::DownloadDependency
            | Kind::PrunePackageReference
            | Kind::CentralPackageVersion => &[
                "Type",
                "ProjectUniqueName",
                "Id",
                "VersionRange",
                "TargetFrameworks",
            ],
            Kind::FrameworkReference => &[
                "Type",
                "ProjectUniqueName",
                "Id",
                "TargetFrameworks",
                "PrivateAssets",
            ],
            Kind::NuGetAuditSuppress => &["Type", "ProjectUniqueName", "Id", "TargetFrameworks"],
        }
    }
}

/// The metadata the `MSBuild` task stamps on every item a target returns
/// across it, which `GetDependencySpec` does not read.
const MSBUILD_TASK_METADATA: [&str; 3] = [
    "MSBuildSourceProjectFile",
    "MSBuildSourceTargetName",
    "OriginalItemSpec",
];

/// One item, read as `MSBuildTaskItem.GetProperty` reads it: each value
/// trimmed, an empty one absent, names matched ignoring case and keyed by the
/// vocabulary's spelling.
#[derive(Debug)]
struct Item<'a> {
    index: usize,
    identity: &'a str,
    kind: Kind,
    values: BTreeMap<&'static str, &'a str>,
}

impl<'a> Item<'a> {
    fn read(index: usize, item: &'a RestoreGraphItem) -> Result<Item<'a>> {
        let vocabulary = |detail: String| RestoreItemsDecline::Vocabulary {
            item: index,
            detail,
        };
        let ty = item
            .metadata
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("Type"))
            .map(|(_, value)| dotnet_trim(value))
            .unwrap_or_default();
        let kind = Kind::ALL
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ty))
            .map(|&(_, kind)| kind)
            .ok_or_else(|| vocabulary(format!("type {ty:?} is not modelled")))?;
        let mut values = BTreeMap::new();
        let mut seen = Vec::new();
        for (name, value) in &item.metadata {
            if MSBUILD_TASK_METADATA
                .iter()
                .any(|unread| unread.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let known = kind
                .vocabulary()
                .iter()
                .find(|known| known.eq_ignore_ascii_case(name))
                .ok_or_else(|| vocabulary(format!("metadata {name:?} is not modelled")))?;
            if seen.contains(known) {
                return Err(vocabulary(format!("metadata {known} is given twice")));
            }
            seen.push(*known);
            // `bool.TryParse` ignores NULs where `Trim` keeps them. Any other
            // control character is trimmed as .NET trims it, or reaches the
            // writer, which declines it.
            if value.contains('\0') {
                return Err(RestoreItemsDecline::Value {
                    item: index,
                    name: known,
                    value: value.clone(),
                    detail: "a NUL character",
                });
            }
            let value = dotnet_trim(value);
            if !value.is_empty() {
                values.insert(*known, value);
            }
        }
        Ok(Item {
            index,
            identity: &item.identity,
            kind,
            values,
        })
    }

    fn get(&self, name: &'static str) -> Option<&'a str> {
        debug_assert!(
            self.kind.vocabulary().contains(&name),
            "{name} is read but not in {:?}'s vocabulary",
            self.kind
        );
        self.values.get(name).copied()
    }

    /// `MSBuildRestoreUtility.IsPropertyTrue` (`IsPropertyFalse` with
    /// `"False"`): the value equals the word ignoring case.
    fn is(&self, name: &'static str, word: &str) -> bool {
        self.get(name)
            .is_some_and(|value| value.eq_ignore_ascii_case(word))
    }

    fn is_true(&self, name: &'static str) -> bool {
        self.is(name, "True")
    }

    fn decline(&self, name: &'static str, detail: &'static str) -> RestoreItemsDecline {
        RestoreItemsDecline::Value {
            item: self.index,
            name,
            value: self.get(name).unwrap_or_default().to_owned(),
            detail,
        }
    }

    /// Decline when the flag is set: NuGet would record something the typed
    /// model does not hold.
    fn refuse_true(&self, name: &'static str) -> Result<()> {
        if self.is_true(name) {
            Err(self.decline(name, "the model does not hold this setting"))
        } else {
            Ok(())
        }
    }

    fn required(&self, name: &'static str) -> Result<&'a str> {
        self.get(name)
            .ok_or_else(|| self.decline(name, "absent, which the model does not cover"))
    }
}

/// `string.Trim()`: .NET's whitespace is Unicode's `White_Space`, which is
/// also what Rust trims.
fn dotnet_trim(value: &str) -> &str {
    value.trim()
}

/// `MSBuildStringUtility.Split`: split on `separators`, trim each piece, drop
/// the empty ones.
fn split<'a>(value: Option<&'a str>, separators: &[char]) -> Vec<&'a str> {
    value
        .map(|value| {
            value
                .split(|c| separators.contains(&c))
                .map(dotnet_trim)
                .filter(|piece| !piece.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// `bool.TryParse` on a value already trimmed.
fn dotnet_bool(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

/// `MSBuildStringUtility.GetNuGetLogCodes`: split on `;` and `,`, keep each
/// piece that starts with `NU` and names a code ignoring case. A code
/// differing only beyond ASCII never names one, as .NET folds no non-ASCII
/// character to an ASCII one.
fn log_codes(value: Option<&str>) -> Vec<NuGetLogCode> {
    split(value, &[';', ','])
        .into_iter()
        .filter(|piece| {
            piece
                .get(..2)
                .is_some_and(|nu| nu.eq_ignore_ascii_case("NU"))
        })
        .filter_map(|piece| NuGetLogCode::parse(&piece.to_ascii_uppercase()))
        .collect()
}

// ---------------------------------------------------------------------------
// Include flags.
// ---------------------------------------------------------------------------

/// `LibraryIncludeFlags`, as bits.
const FLAG_NAMES: [(&str, u8); 7] = [
    ("Runtime", 1),
    ("Compile", 2),
    ("Build", 4),
    ("Native", 8),
    ("ContentFiles", 16),
    ("Analyzers", 32),
    ("BuildTransitive", 64),
];
const ALL_FLAGS: u8 = 127;
const DEFAULT_SUPPRESS_PARENT: u8 = 4 | 16 | 32;

/// `MSBuildRestoreUtility.GetIncludeFlags`: split on `;`, and if anything is
/// left, the union of the flags each piece names (`LibraryIncludeFlagUtils.GetFlags`,
/// which lower-cases with `ToLowerInvariant` and ignores what it does not
/// know); else `default`. A non-ASCII piece declines, since the invariant
/// lower-casing folds some (`İ`) onto ASCII letters.
fn include_flags(item: &Item, name: &'static str, default: u8) -> Result<u8> {
    let pieces = split(item.get(name), &[';']);
    if pieces.is_empty() {
        return Ok(default);
    }
    let mut flags = 0;
    for piece in pieces {
        if !piece.is_ascii() {
            return Err(item.decline(name, "a non-ASCII flag"));
        }
        let piece = piece.to_ascii_lowercase();
        flags |= match piece.as_str() {
            "all" => ALL_FLAGS,
            "runtime" => 1,
            "compile" => 2,
            "build" => 4,
            "native" => 8,
            "contentfiles" => 16,
            "analyzers" => 32,
            "buildtransitive" => 64 | 4,
            _ => 0,
        };
    }
    Ok(flags)
}

/// `LibraryIncludeFlags.ToString()`, which the writer uses for a
/// dependency's flags: the defined name for `None` and `All`, else the set
/// flags' names in ascending order.
fn flags_enum_string(flags: u8) -> String {
    match flags {
        0 => "None".to_owned(),
        ALL_FLAGS => "All".to_owned(),
        _ => FLAG_NAMES
            .iter()
            .filter(|(_, bit)| flags & bit != 0)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// `LibraryIncludeFlagUtils.GetFlagString`, which the writer uses for a
/// project reference's flags.
fn flags_friendly_string(flags: u8) -> String {
    match flags {
        0 => "none".to_owned(),
        ALL_FLAGS => "all".to_owned(),
        _ => flags_enum_string(flags).to_ascii_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// String comparisons NuGet makes.
// ---------------------------------------------------------------------------

/// `StringComparer.Ordinal`: UTF-16 code-unit order.
fn ordinal(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `StringComparer.OrdinalIgnoreCase` over ASCII, or `None` when either side
/// is not ASCII (the caller declines).
fn ordinal_ignore_case(a: &str, b: &str) -> Option<Ordering> {
    (a.is_ascii() && b.is_ascii()).then(|| {
        a.bytes()
            .map(|c| c.to_ascii_uppercase())
            .cmp(b.bytes().map(|c| c.to_ascii_uppercase()))
    })
}

/// Whether .NET's `OrdinalIgnoreCase` could call `a` and `b` equal: a
/// superset of it, which folds each character every way Unicode does.
fn could_fold_equal(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| {
            x == y || x.to_lowercase().eq(y.to_lowercase()) || x.to_uppercase().eq(y.to_uppercase())
        })
}

/// Sort `values` ordinally, declining unless sorting them ignoring case gives
/// the same order: NuGet sorts paths ordinally on a case-sensitive file
/// system and ignoring case on an insensitive one.
fn sort_paths(values: &mut [&str], what: &str) -> Result<()> {
    values.sort_by(|a, b| ordinal(a, b));
    for pair in values.windows(2) {
        let insensitive = if pair[0].is_ascii() && pair[1].is_ascii() {
            ordinal_ignore_case(pair[0], pair[1])
        } else {
            None
        };
        if insensitive != Some(Ordering::Less) {
            return Err(RestoreItemsDecline::HostDependent {
                detail: format!(
                    "{what} {:?} and {:?} sort differently ignoring case",
                    pair[0], pair[1]
                ),
            });
        }
    }
    Ok(())
}

/// Sort ASCII names ignoring case (`StringComparer.OrdinalIgnoreCase`),
/// stably. A non-ASCII name declines.
fn sort_ignoring_case<T>(entries: &mut [T], key: impl Fn(&T) -> &str, what: &str) -> Result<()> {
    if let Some(entry) = entries.iter().find(|entry| !key(entry).is_ascii()) {
        return Err(RestoreItemsDecline::HostDependent {
            detail: format!("{what} {:?} is not ASCII", key(entry)),
        });
    }
    entries.sort_by(|a, b| ordinal_ignore_case(key(a), key(b)).expect("checked ASCII"));
    Ok(())
}

// ---------------------------------------------------------------------------
// The graph.
// ---------------------------------------------------------------------------

/// A project reference as `AddProjectReferences` builds it.
#[derive(Debug, Clone)]
struct Reference<'a> {
    unique_name: Option<&'a str>,
    path: Option<&'a str>,
    include: u8,
    exclude: u8,
    private: u8,
}

/// A direct dependency, as `AddPackageReferences` builds it.
#[derive(Debug, Clone)]
struct Dependency<'a> {
    name: &'a str,
    /// `None` under central package management when no version applies; the
    /// writer writes `VersionRange.All`.
    range: Option<VersionRange>,
    include: u8,
    suppress_parent: u8,
    no_warn: Vec<NuGetLogCode>,
    auto_referenced: bool,
    version_centrally_managed: bool,
    generate_path_property: bool,
    aliases: Option<&'a str>,
    version_override: Option<VersionRange>,
}

/// One entry of a framework's name-keyed range table (central package
/// versions, packages to prune): the name the writer writes and the range
/// text it writes.
#[derive(Debug, Clone)]
struct RangeEntry<'a> {
    key: &'a str,
    text: String,
}

/// One target framework (`TargetFrameworkInformation`).
#[derive(Debug, Clone)]
struct Framework<'a> {
    alias: &'a str,
    short_name: String,
    imports: Vec<String>,
    asset_target_fallback: bool,
    runtime_identifier_graph_path: Option<&'a str>,
    dependencies: Vec<Dependency<'a>>,
    central_versions: Vec<RangeEntry<'a>>,
    downloads: Vec<(&'a str, VersionRange)>,
    framework_references: Vec<(&'a str, bool)>,
    prune: Vec<RangeEntry<'a>>,
    references: Vec<Reference<'a>>,
}

/// One project (`PackageSpec`), with everything the writer writes.
#[derive(Debug)]
struct Project<'a> {
    unique_name: &'a str,
    name: &'a str,
    path: &'a str,
    version: NuGetVersion,
    packages_path: Option<&'a str>,
    output_path: Option<&'a str>,
    cross_targeting: bool,
    skip_content_file_write: bool,
    central_package_versions: bool,
    transitive_pinning: bool,
    using_microsoft_net_sdk: bool,
    fallback_folders: Vec<&'a str>,
    config_file_paths: Vec<&'a str>,
    sources: Vec<String>,
    frameworks: Vec<Framework<'a>>,
    all_warnings_as_errors: bool,
    warnings_as_errors: Vec<NuGetLogCode>,
    no_warn: Vec<NuGetLogCode>,
    warnings_not_as_errors: Vec<NuGetLogCode>,
    restore_packages_with_lock_file: Option<&'a str>,
    nuget_lock_file_path: Option<&'a str>,
    restore_locked_mode: bool,
    audit: Option<Audit<'a>>,
    sdk_analysis_level: Option<&'a str>,
}

#[derive(Debug)]
struct Audit<'a> {
    enable: Option<&'a str>,
    level: Option<&'a str>,
    mode: Option<&'a str>,
    suppressed: Vec<&'a str>,
}

#[derive(Debug)]
struct Graph<'a> {
    restore: Vec<&'a str>,
    projects: Vec<Project<'a>>,
}

impl<'a> Graph<'a> {
    /// `MSBuildRestoreUtility.GetDependencySpec`, plus the task's
    /// `AddAllProjectsForRestore`.
    fn build(items: &[Item<'a>], restore_recursive: bool) -> Result<Graph<'a>> {
        check_path_casing(items)?;
        let mut restore_specs = Vec::new();
        let mut groups: Vec<(&'a str, Vec<&Item<'a>>)> = Vec::new();
        for item in items {
            let Some(unique_name) = item.get("ProjectUniqueName") else {
                continue;
            };
            if item.kind == Kind::RestoreSpec {
                restore_specs.push(unique_name);
            } else if let Some((_, group)) =
                groups.iter_mut().find(|(name, _)| *name == unique_name)
            {
                group.push(item);
            } else {
                groups.push((unique_name, vec![item]));
            }
        }
        let mut projects = Vec::new();
        for (_, group) in &groups {
            if let Some(project) = Project::build(group)? {
                projects.push(project);
            }
        }
        // `RemoveMissingProjects`: a reference to a path no project has is
        // dropped.
        let paths: Vec<&str> = projects.iter().map(|p| p.path).collect();
        for project in &mut projects {
            for framework in &mut project.frameworks {
                framework
                    .references
                    .retain(|r| r.path.is_some_and(|path| paths.contains(&path)));
            }
        }
        // Every project here is a `PackageReference` one, so each is valid
        // for restore.
        let mut restore: Vec<&str> = if restore_recursive {
            projects.iter().map(|p| p.unique_name).collect()
        } else {
            Vec::new()
        };
        for name in restore_specs {
            if projects.iter().any(|p| p.unique_name == name) && !restore.contains(&name) {
                restore.push(name);
            }
        }
        restore.dedup();
        sort_paths(&mut restore, "restored projects")?;
        let mut order: Vec<&str> = projects.iter().map(|p| p.unique_name).collect();
        sort_paths(&mut order, "projects")?;
        projects.sort_by(|a, b| ordinal(a.unique_name, b.unique_name));
        Ok(Graph { restore, projects })
    }
}

/// NuGet compares project names and paths with
/// `PathUtility.GetStringComparerBasedOnOS`: ignoring case on a
/// case-insensitive file system, ordinally elsewhere. When no two of them
/// could be equal ignoring case without being equal, the two agree.
fn check_path_casing(items: &[Item]) -> Result<()> {
    let mut paths: Vec<&str> = items
        .iter()
        .flat_map(|item| {
            [
                "ProjectUniqueName",
                "ProjectPath",
                "ProjectReferenceUniqueName",
            ]
            .into_iter()
            .filter(|name| item.kind.vocabulary().contains(name))
            .filter_map(|name| item.get(name))
        })
        .collect();
    paths.sort_unstable();
    paths.dedup();
    for (i, a) in paths.iter().enumerate() {
        if let Some(b) = paths[i + 1..].iter().find(|b| could_fold_equal(a, b)) {
            return Err(RestoreItemsDecline::HostDependent {
                detail: format!("project paths {a:?} and {b:?} may be one path ignoring case"),
            });
        }
    }
    Ok(())
}

impl<'a> Project<'a> {
    /// `MSBuildRestoreUtility.GetPackageSpec` over one project's items, or
    /// `None` when they hold no `ProjectSpec`.
    fn build(items: &[&Item<'a>]) -> Result<Option<Project<'a>>> {
        let of = |kind: Kind| items.iter().copied().filter(move |item| item.kind == kind);
        let Some(spec) = of(Kind::ProjectSpec).next() else {
            return Ok(None);
        };
        match spec.get("ProjectStyle") {
            Some(style) if style.eq_ignore_ascii_case("PackageReference") => {}
            _ => {
                return Err(spec.decline(
                    "ProjectStyle",
                    "only the PackageReference style is modelled",
                ));
            }
        }
        let central_package_versions = spec.is_true("_CentralPackageVersionsEnabled");
        if spec.is("CentralPackageVersionOverrideEnabled", "False") {
            return Err(spec.decline(
                "CentralPackageVersionOverrideEnabled",
                "the model does not hold this setting",
            ));
        }
        spec.refuse_true("CentralPackageFloatingVersionsEnabled")?;
        let transitive_pinning = spec.is_true("CentralPackageTransitivePinningEnabled");
        let path = spec.required("ProjectPath")?;
        let name = spec.required("ProjectName")?;
        let unique_name = spec.required("ProjectUniqueName")?;
        let tfis: Vec<&Item> = of(Kind::TargetFrameworkInformation).collect();
        let mut frameworks = target_frameworks(path, &tfis)?;
        let cross_targeting = spec.is_true("CrossTargeting");

        add_project_references(&mut frameworks, of(Kind::ProjectReference), cross_targeting)?;

        let sources = split(spec.get("Sources"), &[';'])
            .into_iter()
            .map(|source| {
                fix_source_path(source).ok_or_else(|| {
                    spec.decline(
                        "Sources",
                        "a file: source NuGet completes per operating system",
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let config_file_paths = split(spec.get("ConfigFilePaths"), &[';']);
        let fallback_folders = split(spec.get("FallbackFolders"), &[';']);

        if central_package_versions {
            add_central_package_versions(
                &mut frameworks,
                of(Kind::CentralPackageVersion),
                cross_targeting,
            )?;
        }
        add_package_references(
            &mut frameworks,
            of(Kind::Dependency),
            central_package_versions,
            cross_targeting,
        )?;
        add_package_downloads(
            &mut frameworks,
            of(Kind::DownloadDependency),
            cross_targeting,
        )?;
        add_framework_references(
            &mut frameworks,
            of(Kind::FrameworkReference),
            cross_targeting,
        )?;
        add_prune_package_references(
            &mut frameworks,
            &tfis,
            of(Kind::PrunePackageReference),
            cross_targeting,
        )?;

        let version = match spec.get("Version") {
            None => NuGetVersion::parse("1.0.0").expect("1.0.0 parses"),
            Some(text) => NuGetVersion::parse(text)
                .map_err(|_| spec.decline("Version", "NuGetVersion.Parse rejects it"))?,
        };
        for name in ["RuntimeIdentifiers", "RuntimeSupports"] {
            if !split(spec.get(name), &[';']).is_empty() {
                return Err(spec.decline(name, "runtime identifiers are not modelled"));
            }
        }
        spec.refuse_true("RestoreLegacyPackagesDirectory")?;
        spec.refuse_true("ValidateRuntimeAssets")?;
        let skip_content_file_write = spec.is_true("SkipContentFileWrite");

        let audit = audit_properties(spec, &tfis, of(Kind::NuGetAuditSuppress))?;

        let using_microsoft_net_sdk = match spec.get("UsingMicrosoftNETSdk") {
            None => false,
            Some(value) => dotnet_bool(value)
                .ok_or_else(|| spec.decline("UsingMicrosoftNETSdk", "bool.TryParse rejects it"))?,
        };
        let sdk_analysis_level = spec.get("SdkAnalysisLevel");
        if let Some(level) = sdk_analysis_level
            && NuGetVersion::parse(level).is_err()
        {
            return Err(spec.decline("SdkAnalysisLevel", "NuGetVersion.TryParse rejects it"));
        }
        if let Some(sdk) = spec.get("NETCoreSdkVersion")
            && NuGetVersion::parse(sdk).is_err()
        {
            return Err(spec.decline("NETCoreSdkVersion", "NuGetVersion.TryParse rejects it"));
        }
        spec.refuse_true("RestoreUseLegacyDependencyResolver")?;
        spec.refuse_true("RestoreDoNotWriteDependencyGraphSpec")?;
        spec.refuse_true("RestoreEnableAnalyzerAssets")?;
        for tfi in &tfis {
            tfi.refuse_true("RestoreEnableAnalyzerAssets")?;
        }

        Ok(Some(Project {
            unique_name,
            name,
            path,
            version,
            packages_path: spec.get("PackagesPath"),
            output_path: spec.get("OutputPath"),
            cross_targeting,
            skip_content_file_write,
            central_package_versions,
            transitive_pinning,
            using_microsoft_net_sdk,
            fallback_folders,
            config_file_paths,
            sources,
            frameworks,
            all_warnings_as_errors: spec.is_true("TreatWarningsAsErrors"),
            warnings_as_errors: log_codes(spec.get("WarningsAsErrors")),
            no_warn: log_codes(spec.get("NoWarn")),
            warnings_not_as_errors: log_codes(spec.get("WarningsNotAsErrors")),
            restore_packages_with_lock_file: spec.get("RestorePackagesWithLockFile"),
            nuget_lock_file_path: spec.get("NuGetLockFilePath"),
            restore_locked_mode: spec.is_true("RestoreLockedMode"),
            audit,
            sdk_analysis_level,
        }))
    }
}

/// `GetTargetFrameworkInformation`: one framework per distinct alias (the
/// `TargetFramework` metadata, compared ordinally), first item first.
fn target_frameworks<'a>(path: &str, tfis: &[&Item<'a>]) -> Result<Vec<Framework<'a>>> {
    for suffix in [".vcxproj", ".jsproj"] {
        if path.len() >= suffix.len()
            && path.is_char_boundary(path.len() - suffix.len())
            && path[path.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            return Err(RestoreItemsDecline::Value {
                item: tfis.first().map_or(0, |tfi| tfi.index),
                name: "ProjectPath",
                value: path.to_owned(),
                detail: "C++ and JavaScript projects are not modelled",
            });
        }
    }
    let mut frameworks: Vec<Framework> = Vec::new();
    for tfi in tfis {
        let alias = tfi.get("TargetFramework").unwrap_or("");
        if frameworks.iter().any(|f| f.alias == alias) {
            continue;
        }
        if let Some(other) = frameworks
            .iter()
            .find(|f| ordinal_ignore_case(f.alias, alias).is_none_or(Ordering::is_eq))
        {
            return Err(RestoreItemsDecline::Value {
                item: tfi.index,
                name: "TargetFramework",
                value: alias.to_owned(),
                detail: if other.alias.is_ascii() && alias.is_ascii() {
                    "two aliases equal ignoring case, which NuGet's alias table refuses"
                } else {
                    "a non-ASCII alias"
                },
            });
        }
        let framework = project_framework(tfi)?;
        if !split(tfi.get("PackageTargetFallback"), &[';']).is_empty() {
            return Err(tfi.decline(
                "PackageTargetFallback",
                "PackageTargetFallback is not modelled",
            ));
        }
        let mut imports = Vec::new();
        for import in split(tfi.get("AssetTargetFallback"), &[';']) {
            let parsed = NuGetFramework::parse(import).map_err(|_| {
                tfi.decline("AssetTargetFallback", "NuGetFramework.Parse rejects it")
            })?;
            imports.push(parsed.short_folder_name().ok_or_else(|| {
                tfi.decline("AssetTargetFallback", "a framework with no short name")
            })?);
        }
        let short_name = framework.short_folder_name().ok_or_else(|| {
            tfi.decline("TargetFrameworkMoniker", "a framework with no short name")
        })?;
        if let Some(other) = frameworks.iter().find(|f| f.short_name == short_name) {
            return Err(RestoreItemsDecline::Value {
                item: tfi.index,
                name: "TargetFrameworkMoniker",
                value: short_name.clone(),
                detail: if other.alias.is_empty() || alias.is_empty() {
                    "two aliases for one framework"
                } else {
                    "two aliases for one framework, which TargetFrameworkInformation's equality may merge"
                },
            });
        }
        frameworks.push(Framework {
            alias,
            short_name,
            asset_target_fallback: !imports.is_empty(),
            imports,
            runtime_identifier_graph_path: tfi.get("RuntimeIdentifierGraphPath"),
            dependencies: Vec::new(),
            central_versions: Vec::new(),
            downloads: Vec::new(),
            framework_references: Vec::new(),
            prune: Vec::new(),
            references: Vec::new(),
        });
    }
    Ok(frameworks)
}

/// `MSBuildProjectFrameworkUtility.GetProjectFramework` for a project that is
/// neither C++ nor JavaScript: `NuGetFramework.ParseComponents` over the
/// framework and platform monikers.
fn project_framework(tfi: &Item) -> Result<NuGetFramework> {
    let platform = tfi.get("TargetPlatformMoniker");
    if let Some(identifier) = platform.and_then(|tpm| split(Some(tpm), &[',']).into_iter().next())
        && identifier.eq_ignore_ascii_case("UAP")
    {
        return Err(tfi.decline("TargetPlatformMoniker", "UAP projects are not modelled"));
    }
    let moniker = tfi.get("TargetFrameworkMoniker").ok_or_else(|| {
        tfi.decline(
            "TargetFrameworkMoniker",
            "absent, which NuGet reads as the unsupported framework",
        )
    })?;
    let framework = NuGetFramework::parse_framework_name(moniker).map_err(|_| {
        tfi.decline(
            "TargetFrameworkMoniker",
            "NuGet rejects this framework name",
        )
    })?;
    if !framework.is_specific_framework() {
        return Err(tfi.decline("TargetFrameworkMoniker", "a special framework"));
    }
    if framework.profile().is_some_and(|p| !p.is_empty())
        || framework.is_pcl()
        || framework.framework().eq_ignore_ascii_case(".NETPortable")
    {
        return Err(tfi.decline("TargetFrameworkMoniker", "profiles are not modelled"));
    }
    let net5_era = framework.framework().eq_ignore_ascii_case(".NETCoreApp")
        && framework
            .version_string()
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 5);
    if net5_era && platform.is_some() {
        return Err(tfi.decline(
            "TargetPlatformMoniker",
            "platform-qualified frameworks are not modelled",
        ));
    }
    if net5_era && framework.platform().is_some() {
        return Err(tfi.decline(
            "TargetFrameworkMoniker",
            "platform-qualified frameworks are not modelled",
        ));
    }
    Ok(framework)
}

/// `GetTargetFrameworkStrings`: the item's `TargetFrameworks`, split on `;`
/// for a cross-targeting project and whole otherwise, deduplicated ignoring
/// case (first spelling kept).
fn item_frameworks<'a>(item: &Item<'a>, cross_targeting: bool) -> Result<Vec<&'a str>> {
    let pieces = if cross_targeting {
        split(item.get("TargetFrameworks"), &[';'])
    } else {
        item.get("TargetFrameworks").into_iter().collect()
    };
    let mut frameworks: Vec<&str> = Vec::new();
    for piece in pieces {
        if !piece.is_ascii() {
            return Err(item.decline("TargetFrameworks", "a non-ASCII framework"));
        }
        if !frameworks.iter().any(|f| f.eq_ignore_ascii_case(piece)) {
            frameworks.push(piece);
        }
    }
    Ok(frameworks)
}

/// The index of the framework whose alias is exactly `alias`
/// (`SingleIndex` with `StringComparison.Ordinal`), declining where NuGet
/// throws.
fn exact_alias(frameworks: &[Framework], item: &Item, alias: &str) -> Result<usize> {
    frameworks
        .iter()
        .position(|f| f.alias == alias)
        .ok_or_else(|| {
            item.decline(
                "TargetFrameworks",
                "a framework the project does not have, which NuGet rejects",
            )
        })
}

/// The index of the framework whose alias equals `alias` ignoring case
/// (aliases equal ignoring case were declined, so it is unique).
fn alias_ignoring_case(frameworks: &[Framework], item: &Item, alias: &str) -> Result<usize> {
    frameworks
        .iter()
        .position(|f| f.alias.eq_ignore_ascii_case(alias))
        .ok_or_else(|| {
            item.decline(
                "TargetFrameworks",
                "a framework the project does not have, which NuGet rejects",
            )
        })
}

/// `AddProjectReferences`.
fn add_project_references<'a, 'i>(
    frameworks: &mut [Framework<'a>],
    items: impl Iterator<Item = &'i Item<'a>>,
    cross_targeting: bool,
) -> Result<()>
where
    'a: 'i,
{
    for item in items {
        let targets = item_frameworks(item, cross_targeting)?;
        let reference = Reference {
            unique_name: item.get("ProjectReferenceUniqueName"),
            path: item.get("ProjectPath"),
            include: include_flags(item, "IncludeAssets", ALL_FLAGS)?,
            exclude: include_flags(item, "ExcludeAssets", 0)?,
            private: include_flags(item, "PrivateAssets", DEFAULT_SUPPRESS_PARENT)?,
        };
        let aliases: Vec<&str> = if targets.is_empty() {
            frameworks.iter().map(|f| f.alias).collect()
        } else {
            targets
        };
        for alias in aliases {
            // A lookup in a dictionary keyed by alias, ordinally: a framework
            // spelt in another case is silently skipped.
            if let Some(framework) = frameworks.iter_mut().find(|f| f.alias == alias)
                && !framework
                    .references
                    .iter()
                    .any(|r| r.unique_name == reference.unique_name)
            {
                framework.references.push(reference.clone());
            }
        }
    }
    Ok(())
}

/// `FixSourcePath` on a non-Windows host, or `None` for a `file:` source it
/// would complete (with a different number of slashes on Windows).
fn fix_source_path(source: &str) -> Option<String> {
    if source.contains("//") {
        return Some(source.to_owned());
    }
    let mut result = source.to_owned();
    for prefix in ["http:", "https:"] {
        result = fix_prefix(&result, prefix);
    }
    if fix_prefix(&result, "file:") != result {
        return None;
    }
    Some(result)
}

fn fix_prefix(s: &str, prefix: &str) -> String {
    let n = prefix.len();
    let starts = |with: &str| {
        s.len() >= with.len()
            && s.is_char_boundary(with.len())
            && s[..with.len()].eq_ignore_ascii_case(with)
    };
    if s.encode_utf16().count() >= n + 2
        && starts(&format!("{prefix}/"))
        && !starts(&format!("{prefix}//"))
    {
        format!("{}//{}", &s[..n], &s[n + 1..])
    } else {
        s.to_owned()
    }
}

/// `VersionRange.Parse`, declining where it throws.
fn parse_range(item: &Item, name: &'static str, text: &str) -> Result<VersionRange> {
    VersionRange::parse(text).map_err(|_| item.decline(name, "VersionRange.Parse rejects it"))
}

/// A package id NuGet's readers accept; anything else declines (the reader
/// of the written spec would refuse it).
fn package_id<'a>(item: &Item<'a>) -> Result<&'a str> {
    let id = item.required("Id")?;
    PackageId::parse(id).map_err(|_| item.decline("Id", "not a valid package id"))?;
    Ok(id)
}

/// `AddCentralPackageVersions` (via `CreateCentralVersionDependencies`).
fn add_central_package_versions<'a, 'i>(
    frameworks: &mut [Framework<'a>],
    items: impl Iterator<Item = &'i Item<'a>>,
    cross_targeting: bool,
) -> Result<()>
where
    'a: 'i,
{
    // `Distinct` by item spec, ignoring case: the first of each survives.
    let mut seen: Vec<&str> = Vec::new();
    for item in items {
        if let Some(other) = seen.iter().find(|s| could_fold_equal(s, item.identity)) {
            if !other.eq_ignore_ascii_case(item.identity)
                || !other.is_ascii()
                || !item.identity.is_ascii()
            {
                return Err(RestoreItemsDecline::HostDependent {
                    detail: format!(
                        "item specs {other:?} and {:?} may be equal ignoring case",
                        item.identity
                    ),
                });
            }
            continue;
        }
        seen.push(item.identity);
        let id = package_id(item)?;
        let text = match item.get("VersionRange") {
            None => None,
            Some(text) => {
                parse_range(item, "VersionRange", text)?;
                Some(text)
            }
        };
        let targets = item_frameworks(item, cross_targeting)?;
        let aliases: Vec<&str> = if targets.is_empty() {
            frameworks.iter().map(|f| f.alias).collect()
        } else {
            targets
        };
        for alias in aliases {
            // NuGet groups by alias ordinally and gives each group to the first
            // framework whose alias matches ignoring case, so an alias spelt in
            // another case replaces that framework's whole table. Only the
            // exact spelling is modelled.
            let index = exact_alias(frameworks, item, alias)?;
            let table = &mut frameworks[index].central_versions;
            let text = text.map_or_else(|| "(, )".to_owned(), str::to_owned);
            match table.iter_mut().find(|e| e.key.eq_ignore_ascii_case(id)) {
                // The table's key keeps its first spelling, but the writer writes
                // each entry's own name, which is the last one's.
                Some(entry) => {
                    entry.key = id;
                    entry.text = text;
                }
                None => table.push(RangeEntry { key: id, text }),
            }
        }
    }
    Ok(())
}

/// `AddPackageReferences`.
fn add_package_references<'a, 'i>(
    frameworks: &mut [Framework<'a>],
    items: impl Iterator<Item = &'i Item<'a>>,
    cpvm: bool,
    cross_targeting: bool,
) -> Result<()>
where
    'a: 'i,
{
    for item in items {
        let no_warn = log_codes(item.get("NoWarn"));
        let include = include_flags(item, "IncludeAssets", ALL_FLAGS)?
            & !include_flags(item, "ExcludeAssets", 0)?;
        let suppress_parent = include_flags(item, "PrivateAssets", DEFAULT_SUPPRESS_PARENT)?;
        let targets = item_frameworks(item, cross_targeting)?;
        let name = package_id(item)?;
        let auto_referenced = item.is_true("IsImplicitlyDefined");
        let version_override = item
            .get("VersionOverride")
            .map(|text| parse_range(item, "VersionOverride", text))
            .transpose()?;
        let written = item
            .get("VersionRange")
            .map(|text| parse_range(item, "VersionRange", text))
            .transpose()?;
        let version_defined = written.is_some();
        // One variable across the frameworks, as NuGet has it: a central
        // version found for one framework stays for the next.
        let mut range = match written {
            Some(range) => Some(range),
            None if cpvm => None,
            None => Some(VersionRange::parse("(, )").expect("all parses")),
        };
        let aliases: Vec<&str> = if targets.is_empty() {
            frameworks.iter().map(|f| f.alias).collect()
        } else {
            targets
        };
        for alias in aliases {
            let central =
                if !version_defined && !auto_referenced && cpvm && version_override.is_none() {
                    let index = alias_ignoring_case(frameworks, item, alias)?;
                    frameworks[index]
                        .central_versions
                        .iter()
                        .find(|e| e.key.eq_ignore_ascii_case(name))
                        .map(|e| e.text.clone())
                } else {
                    None
                };
            if let Some(text) = &central {
                range = Some(VersionRange::parse(text).expect("parsed when the table was built"));
            }
            if let Some(over) = &version_override {
                range = Some(over.clone());
            }
            let index = exact_alias(frameworks, item, alias)?;
            let framework = &mut frameworks[index];
            if framework
                .dependencies
                .iter()
                .any(|d| d.name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            framework.dependencies.push(Dependency {
                name,
                range: range.clone(),
                include,
                suppress_parent,
                no_warn: no_warn.clone(),
                auto_referenced,
                version_centrally_managed: central.is_some(),
                generate_path_property: item.is_true("GeneratePathProperty"),
                aliases: item.get("Aliases"),
                version_override: version_override.clone(),
            });
        }
    }
    Ok(())
}

/// `VersionRange`'s equality (`VersionRangeComparer.Default`).
fn range_eq(a: &VersionRange, b: &VersionRange) -> bool {
    a.is_min_inclusive() == b.is_min_inclusive()
        && a.is_max_inclusive() == b.is_max_inclusive()
        && a.min_version() == b.min_version()
        && a.max_version() == b.max_version()
        && a.float_behavior() == b.float_behavior()
}

/// `AddPackageDownloads`.
fn add_package_downloads<'a, 'i>(
    frameworks: &mut [Framework<'a>],
    items: impl Iterator<Item = &'i Item<'a>>,
    cross_targeting: bool,
) -> Result<()>
where
    'a: 'i,
{
    for item in items {
        let id = item.get("Id");
        let text = item
            .get("VersionRange")
            .ok_or_else(|| item.decline("VersionRange", "absent, which NuGet rejects"))?;
        let id = match id {
            Some(_) => package_id(item)?,
            None => return Err(item.decline("Id", "absent, which the model does not cover")),
        };
        for version in text.split(';').filter(|v| !v.is_empty()) {
            let range = parse_range(item, "VersionRange", version)?;
            let exact = matches!((range.min_version(), range.max_version()), (Some(min), Some(max)) if min == max);
            if !exact {
                return Err(
                    item.decline("VersionRange", "not an exact version, which NuGet rejects")
                );
            }
            for alias in item_frameworks(item, cross_targeting)? {
                let index = exact_alias(frameworks, item, alias)?;
                let downloads = &mut frameworks[index].downloads;
                if !downloads
                    .iter()
                    .any(|(other, r)| other.eq_ignore_ascii_case(id) && range_eq(r, &range))
                {
                    downloads.push((id, range.clone()));
                }
            }
        }
    }
    Ok(())
}

/// `AddFrameworkReferences`.
fn add_framework_references<'a, 'i>(
    frameworks: &mut [Framework<'a>],
    items: impl Iterator<Item = &'i Item<'a>>,
    cross_targeting: bool,
) -> Result<()>
where
    'a: 'i,
{
    for item in items {
        let name = item.required("Id")?;
        if !name.is_ascii() {
            return Err(item.decline("Id", "a non-ASCII framework reference"));
        }
        // `FrameworkDependencyFlagsUtils.GetFlags`: `Enum.TryParse` of each
        // piece, ignoring case, and a piece it refuses adds nothing. It also
        // takes numbers (a piece led by a digit or sign) and comma lists, which
        // are not modelled.
        let mut all = false;
        for piece in split(item.get("PrivateAssets"), &[';']) {
            if piece.eq_ignore_ascii_case("all") {
                all = true;
            } else if piece.contains(',')
                || piece.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+')
            {
                return Err(item.decline(
                    "PrivateAssets",
                    "a numeric or listed flag, which is not modelled",
                ));
            }
        }
        for alias in item_frameworks(item, cross_targeting)? {
            let index = exact_alias(frameworks, item, alias)?;
            let references = &mut frameworks[index].framework_references;
            if !references
                .iter()
                .any(|(other, _)| other.eq_ignore_ascii_case(name))
            {
                references.push((name, all));
            }
        }
    }
    Ok(())
}

/// `AddPrunePackageReferences`.
fn add_prune_package_references<'a, 'i>(
    frameworks: &mut [Framework<'a>],
    tfis: &[&Item<'a>],
    items: impl Iterator<Item = &'i Item<'a>>,
    cross_targeting: bool,
) -> Result<()>
where
    'a: 'i,
{
    let default = tfis
        .iter()
        .any(|tfi| tfi.is_true("RestorePackagePruningDefault"));
    // Keyed by each item's alias ignoring case; a later item for an alias
    // overrides an earlier one.
    let mut enabled: Vec<(&str, bool)> = Vec::new();
    for tfi in tfis {
        let alias = tfi.get("TargetFramework").unwrap_or("");
        let on = tfi
            .get("RestoreEnablePackagePruning")
            .and_then(dotnet_bool)
            .unwrap_or(default);
        match enabled
            .iter_mut()
            .find(|(a, _)| a.eq_ignore_ascii_case(alias))
        {
            Some(entry) => entry.1 = on,
            None => enabled.push((alias, on)),
        }
    }
    let is_enabled = |item: &Item, alias: &str| {
        enabled
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(alias))
            .map(|&(_, on)| on)
            .ok_or_else(|| {
                item.decline(
                    "TargetFrameworks",
                    "a framework the project does not have, which NuGet rejects",
                )
            })
    };
    for item in items {
        let targets = item_frameworks(item, cross_targeting)?;
        let aliases: Vec<&str> = if targets.is_empty() {
            frameworks.iter().map(|f| f.alias).collect()
        } else {
            targets
        };
        for alias in aliases {
            if !is_enabled(item, alias)? {
                continue;
            }
            // `PrunePackageReference.Create` runs only for a framework that
            // prunes, so an entry no framework prunes is never read.
            let id = package_id(item)?;
            let version = item
                .get("VersionRange")
                .ok_or_else(|| item.decline("VersionRange", "absent, which NuGet rejects"))?;
            let text = format!("(,{version}]");
            parse_range(item, "VersionRange", &text)?;
            let index = alias_ignoring_case(frameworks, item, alias)?;
            let table = &mut frameworks[index].prune;
            if !table.iter().any(|e| e.key.eq_ignore_ascii_case(id)) {
                table.push(RangeEntry {
                    key: id,
                    text: text.clone(),
                });
            }
        }
    }
    Ok(())
}

/// `GetRestoreAuditProperties`.
fn audit_properties<'a, 'i>(
    spec: &Item<'a>,
    tfis: &[&Item<'a>],
    suppressions: impl Iterator<Item = &'i Item<'a>>,
) -> Result<Option<Audit<'a>>>
where
    'a: 'i,
{
    let enable = spec.get("NuGetAudit");
    let level = spec.get("NuGetAuditLevel");
    let mode = tfis
        .iter()
        .filter_map(|tfi| tfi.get("NuGetAuditMode"))
        .find(|mode| mode.eq_ignore_ascii_case("all"))
        .or_else(|| spec.get("NuGetAuditMode"));
    let mut suppressed: Vec<&str> = Vec::new();
    for item in suppressions {
        let id = item.required("Id")?;
        if !suppressed.contains(&id) {
            suppressed.push(id);
        }
    }
    Ok(
        (enable.is_some() || level.is_some() || mode.is_some() || !suppressed.is_empty())
            .then_some(Audit {
                enable,
                level,
                mode,
                suppressed,
            }),
    )
}

// ---------------------------------------------------------------------------
// Writing: `DependencyGraphSpec.Save` and `PackageSpecWriter`.
// ---------------------------------------------------------------------------

/// A JSON value as `JsonObjectWriter` writes it.
enum Json {
    Object(Vec<(String, Json)>),
    Array(Vec<Json>),
    String(String),
    Bool(bool),
    Int(i64),
    Null,
}

impl Json {
    fn object() -> Json {
        Json::Object(Vec::new())
    }

    fn set(&mut self, name: impl Into<String>, value: Json) {
        let Json::Object(members) = self else {
            unreachable!("set on a non-object")
        };
        members.push((name.into(), value));
    }

    fn string(&mut self, name: &str, value: impl Into<String>) {
        self.set(name, Json::String(value.into()));
    }

    fn string_if_some(&mut self, name: &str, value: Option<&str>) {
        if let Some(value) = value {
            self.string(name, value);
        }
    }

    fn true_if(&mut self, name: &str, value: bool) {
        if value {
            self.set(name, Json::Bool(true));
        }
    }

    /// `WriteNonEmptyNameArray`.
    fn array_if_any(&mut self, name: &str, values: impl IntoIterator<Item = impl Into<String>>) {
        let values: Vec<Json> = values.into_iter().map(|v| Json::String(v.into())).collect();
        if !values.is_empty() {
            self.set(name, Json::Array(values));
        }
    }

    /// Newtonsoft's `JsonTextWriter` with `Formatting.Indented`: two-space
    /// indent, `"name": value`, an empty object or array on one line.
    fn render(&self, out: &mut String, depth: usize) -> Result<()> {
        let indent = |out: &mut String, depth: usize| {
            out.push('\n');
            out.extend(std::iter::repeat_n("  ", depth));
        };
        match self {
            Json::Object(members) if members.is_empty() => out.push_str("{}"),
            Json::Object(members) => {
                out.push('{');
                for (i, (name, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    escape(out, name)?;
                    out.push_str(": ");
                    value.render(out, depth + 1)?;
                }
                indent(out, depth);
                out.push('}');
            }
            Json::Array(values) if values.is_empty() => out.push_str("[]"),
            Json::Array(values) => {
                out.push('[');
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    value.render(out, depth + 1)?;
                }
                indent(out, depth);
                out.push(']');
            }
            Json::String(value) => escape(out, value)?,
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Json::Int(value) => out.push_str(&value.to_string()),
            Json::Null => out.push_str("null"),
        }
        Ok(())
    }
}

/// A JSON string as Newtonsoft escapes it by default: `"` and `\` escaped,
/// everything else verbatim except the control characters and three line
/// separators it also escapes. Control characters were declined on input;
/// the separators decline here rather than model the escape's spelling.
fn escape(out: &mut String, value: &str) -> Result<()> {
    if let Some(c) = value
        .chars()
        .find(|&c| c.is_control() || matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}'))
    {
        return Err(RestoreItemsDecline::HostDependent {
            detail: format!("a value holds {c:?}, whose escape is not modelled"),
        });
    }
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(())
}

impl Graph<'_> {
    fn write(&self) -> Result<String> {
        let mut root = Json::object();
        root.set("format", Json::Int(1));
        let mut restore = Json::object();
        for name in &self.restore {
            restore.set(*name, Json::object());
        }
        root.set("restore", restore);
        let mut projects = Json::object();
        for project in &self.projects {
            projects.set(project.unique_name, project.write()?);
        }
        root.set("projects", projects);
        let mut out = String::new();
        root.render(&mut out, 0)?;
        Ok(out)
    }
}

impl Project<'_> {
    /// `PackageSpecWriter.Write`.
    fn write(&self) -> Result<Json> {
        let mut spec = Json::object();
        spec.string("version", self.version.to_full_string());
        spec.set("restore", self.write_metadata()?);
        let mut frameworks: Vec<&Framework> = self.frameworks.iter().collect();
        sort_ignoring_case(&mut frameworks, |f| f.alias, "target framework alias")?;
        if !frameworks.is_empty() {
            let mut written = Json::object();
            for framework in frameworks {
                let header = if framework.alias.is_empty() {
                    framework.short_name.as_str()
                } else {
                    framework.alias
                };
                written.set(header, framework.write()?);
            }
            spec.set("frameworks", written);
        }
        Ok(spec)
    }

    /// `SetMSBuildMetadata`.
    fn write_metadata(&self) -> Result<Json> {
        let mut m = Json::object();
        m.string("projectUniqueName", self.unique_name);
        m.string("projectName", self.name);
        m.string("projectPath", self.path);
        m.string_if_some("packagesPath", self.packages_path);
        m.string_if_some("outputPath", self.output_path);
        m.string("projectStyle", "PackageReference");
        m.true_if("crossTargeting", self.cross_targeting);
        m.true_if("skipContentFileWrite", self.skip_content_file_write);
        m.true_if(
            "centralPackageVersionsManagementEnabled",
            self.central_package_versions,
        );
        m.true_if(
            "CentralPackageTransitivePinningEnabled",
            self.transitive_pinning,
        );
        if !self.using_microsoft_net_sdk {
            m.set("UsingMicrosoftNETSdk", Json::Bool(false));
        }
        m.array_if_any("fallbackFolders", self.fallback_folders.iter().copied());
        m.array_if_any("configFilePaths", self.config_file_paths.iter().copied());
        let mut original: Vec<String> = self
            .frameworks
            .iter()
            .map(|f| {
                if f.alias.is_empty() {
                    f.short_name.clone()
                } else {
                    f.alias.to_owned()
                }
            })
            .collect();
        original.sort_by(|a, b| ordinal(a, b));
        m.array_if_any("originalTargetFrameworks", original);
        if !self.sources.is_empty() {
            let mut sources: Vec<&str> = self.sources.iter().map(String::as_str).collect();
            sources.sort_by(|a, b| ordinal(a, b));
            let mut written = Json::object();
            for source in sources {
                written.set(source, Json::object());
            }
            m.set("sources", written);
        }
        let mut frameworks: Vec<&Framework> = self.frameworks.iter().collect();
        sort_ignoring_case(&mut frameworks, |f| f.alias, "target framework alias")?;
        if !frameworks.is_empty() {
            let mut written = Json::object();
            let mut headers: Vec<&str> = Vec::new();
            for framework in frameworks {
                let header = if framework.alias.is_empty() {
                    framework.short_name.as_str()
                } else {
                    framework.alias
                };
                if headers.contains(&header) {
                    continue;
                }
                headers.push(header);
                let mut entry = Json::object();
                entry.string("framework", &framework.short_name);
                if !framework.alias.is_empty() {
                    entry.string("targetAlias", framework.alias);
                }
                let mut references: Vec<&Reference> = framework.references.iter().collect();
                let mut paths: Vec<&str> = references.iter().filter_map(|r| r.path).collect();
                sort_paths(&mut paths, "project references")?;
                references.sort_by(|a, b| ordinal(a.path.unwrap_or(""), b.path.unwrap_or("")));
                let mut written_references = Json::object();
                for reference in references {
                    let unique_name = reference.unique_name.ok_or_else(|| RestoreItemsDecline::HostDependent {
                        detail: "a project reference with no unique name, which NuGet's writer cannot write".to_owned(),
                    })?;
                    let mut r = Json::object();
                    r.string_if_some("projectPath", reference.path);
                    if reference.include != ALL_FLAGS {
                        r.string("includeAssets", flags_friendly_string(reference.include));
                    }
                    if reference.exclude != 0 {
                        r.string("excludeAssets", flags_friendly_string(reference.exclude));
                    }
                    if reference.private != DEFAULT_SUPPRESS_PARENT {
                        r.string("privateAssets", flags_friendly_string(reference.private));
                    }
                    written_references.set(unique_name, r);
                }
                entry.set("projectReferences", written_references);
                written.set(header, entry);
            }
            m.set("frameworks", written);
        }
        if self.all_warnings_as_errors
            || !self.no_warn.is_empty()
            || !self.warnings_as_errors.is_empty()
        {
            let mut w = Json::object();
            w.true_if("allWarningsAsErrors", self.all_warnings_as_errors);
            w.array_if_any("noWarn", sorted_codes(&self.no_warn));
            w.array_if_any("warnAsError", sorted_codes(&self.warnings_as_errors));
            w.array_if_any("warnNotAsError", sorted_codes(&self.warnings_not_as_errors));
            m.set("warningProperties", w);
        }
        if self.restore_packages_with_lock_file.is_some()
            || self.nuget_lock_file_path.is_some()
            || self.restore_locked_mode
        {
            let mut l = Json::object();
            l.string_if_some(
                "restorePackagesWithLockFile",
                self.restore_packages_with_lock_file,
            );
            l.string_if_some("nuGetLockFilePath", self.nuget_lock_file_path);
            l.true_if("restoreLockedMode", self.restore_locked_mode);
            m.set("restoreLockProperties", l);
        }
        if let Some(audit) = &self.audit {
            let mut a = Json::object();
            a.string_if_some("enableAudit", audit.enable);
            a.string_if_some("auditLevel", audit.level);
            a.string_if_some("auditMode", audit.mode);
            if !audit.suppressed.is_empty() {
                let mut s = Json::object();
                for advisory in &audit.suppressed {
                    s.set(*advisory, Json::Null);
                }
                a.set("suppressedAdvisories", s);
            }
            m.set("restoreAuditProperties", a);
        }
        if let Some(level) = self.sdk_analysis_level {
            m.string("SdkAnalysisLevel", level);
        }
        Ok(m)
    }
}

/// A warning list as the writer writes it: distinct, in code order (each
/// code is `NU` and four digits, so the name order is the code order).
fn sorted_codes(codes: &[NuGetLogCode]) -> Vec<&'static str> {
    let mut codes: Vec<NuGetLogCode> = codes.to_vec();
    codes.sort();
    codes.dedup();
    codes.iter().map(|c| c.as_str()).collect()
}

impl Framework<'_> {
    /// One entry of `SetFrameworks`.
    fn write(&self) -> Result<Json> {
        let mut f = Json::object();
        f.string("framework", &self.short_name);
        if !self.alias.is_empty() {
            f.string("targetAlias", self.alias);
        }
        if !self.dependencies.is_empty() {
            let mut dependencies: Vec<&Dependency> = self.dependencies.iter().collect();
            dependencies.sort_by(|a, b| ordinal(a.name, b.name));
            let mut written = Json::object();
            for dependency in dependencies {
                written.set(dependency.name, dependency.write());
            }
            f.set("dependencies", written);
        }
        if !self.central_versions.is_empty() {
            let mut entries: Vec<&RangeEntry> = self.central_versions.iter().collect();
            sort_ignoring_case(&mut entries, |e| e.key, "central package version")?;
            let mut written = Json::object();
            for entry in entries {
                written.string(entry.key, &entry.text);
            }
            f.set("centralPackageVersions", written);
        }
        if !self.imports.is_empty() {
            f.set(
                "imports",
                Json::Array(
                    self.imports
                        .iter()
                        .map(|i| Json::String(i.clone()))
                        .collect(),
                ),
            );
        }
        f.true_if("assetTargetFallback", self.asset_target_fallback);
        f.true_if("warn", self.asset_target_fallback);
        if !self.downloads.is_empty() {
            f.set("downloadDependencies", self.write_downloads()?);
        }
        if !self.framework_references.is_empty() {
            let mut references: Vec<&(&str, bool)> = self.framework_references.iter().collect();
            sort_ignoring_case(&mut references, |r| r.0, "framework reference")?;
            let mut written = Json::object();
            for (name, all) in references {
                let mut r = Json::object();
                r.string("privateAssets", if *all { "all" } else { "none" });
                written.set(*name, r);
            }
            f.set("frameworkReferences", written);
        }
        f.string_if_some(
            "runtimeIdentifierGraphPath",
            self.runtime_identifier_graph_path,
        );
        if !self.prune.is_empty() {
            let mut entries: Vec<&RangeEntry> = self.prune.iter().collect();
            sort_ignoring_case(&mut entries, |e| e.key, "package to prune")?;
            let mut written = Json::object();
            for entry in entries {
                written.string(entry.key, &entry.text);
            }
            f.set("packagesToPrune", written);
        }
        Ok(f)
    }

    /// `SetDownloadDependencies`: grouped by exact name, the groups in the
    /// culture's string order, each group's versions in version order joined
    /// by `;`. The culture's order is the ignore-case order for names made of
    /// letters, digits and dots; anything else declines when there is an
    /// order to decide.
    fn write_downloads(&self) -> Result<Json> {
        let mut groups: Vec<(&str, Vec<&VersionRange>)> = Vec::new();
        for (name, range) in &self.downloads {
            match groups.iter_mut().find(|(n, _)| n == name) {
                Some((_, ranges)) => ranges.push(range),
                None => groups.push((name, vec![range])),
            }
        }
        if groups.len() > 1
            && let Some((name, _)) = groups
                .iter()
                .find(|(n, _)| !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'))
        {
            return Err(RestoreItemsDecline::HostDependent {
                detail: format!("download {name:?} sorts by the culture"),
            });
        }
        if let Some(pair) = groups
            .iter()
            .enumerate()
            .flat_map(|(i, a)| groups[i + 1..].iter().map(move |b| (a.0, b.0)))
            .find(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Err(RestoreItemsDecline::HostDependent {
                detail: format!(
                    "downloads {:?} and {:?} sort by the culture",
                    pair.0, pair.1
                ),
            });
        }
        sort_ignoring_case(&mut groups, |g| g.0, "download")?;
        let mut written = Vec::new();
        for (name, mut ranges) in groups {
            ranges.sort_by(|a, b| a.min_version().cmp(&b.min_version()));
            let version = ranges
                .iter()
                .map(|r| r.to_normalized_string())
                .collect::<Vec<_>>()
                .join(";");
            let mut d = Json::object();
            d.string("name", name);
            d.string("version", version);
            written.push(d);
        }
        Ok(Json::Array(written))
    }
}

impl Dependency<'_> {
    /// One entry of `SetDependencies`. A package dependency is always written
    /// in the expanded form, since its `target` is `Package`.
    fn write(&self) -> Json {
        let mut d = Json::object();
        if self.include != ALL_FLAGS {
            d.string("include", flags_enum_string(self.include));
        }
        if self.suppress_parent != DEFAULT_SUPPRESS_PARENT {
            d.string("suppressParent", flags_enum_string(self.suppress_parent));
        }
        d.string("target", "Package");
        let range = self
            .range
            .clone()
            .unwrap_or_else(|| VersionRange::parse("(, )").expect("all parses"));
        d.string("version", range.to_normalized_string());
        if let Some(over) = &self.version_override {
            d.string("versionOverride", over.to_normalized_string());
        }
        d.true_if("autoReferenced", self.auto_referenced);
        d.array_if_any("noWarn", sorted_codes(&self.no_warn));
        d.true_if("generatePathProperty", self.generate_path_property);
        d.true_if("versionCentrallyManaged", self.version_centrally_managed);
        d.string_if_some("aliases", self.aliases);
        d
    }
}
