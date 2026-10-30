//! NuGet's dependency-graph spec (the "dgspec"): the document MSBuild's
//! restore-input targets hand to NuGet's restore, and the boundary
//! `docs/nuget-dgspec-boundary-plan.md` splits restore at.
//!
//! [`parse_dgspec`] reads a dgspec into the values NuGet's own reader
//! (`DependencyGraphSpec.Load`) would hold, or declines. Every type here is
//! NuGet's *interpretation*, not the document's spelling: defaults are filled
//! in, `exclude` is folded into `include`, a `framework` field overrides the
//! alias it sits under. `tests/all/dgspec_diff.rs` holds the parse to
//! certain-implies-exact against the real reader: whenever we return a spec,
//! NuGet loads the same document to the same values.
//!
//! The reader is lenient where we are strict, and the strictness is what
//! keeps the claim cheap to make:
//!
//! - **Only the modelled vocabulary is accepted.** NuGet ignores a key it does
//!   not read and silently drops what it cannot parse (a misspelt include flag,
//!   a lower-case warning code, `"true"` where a boolean belongs). A key we do
//!   not model, or a value outside the spellings we model, declines, naming the
//!   JSON path. So drift in the SDK's dgspec schema shows up as a decline, never
//!   as a value we read differently.
//! - **Repetition declines.** A repeated JSON key, two package ids or other
//!   names equal up to case, a project keyed by anything but its unique name: NuGet keeps the last, keeps both,
//!   or merges, depending on where they sit. None of it occurs in a dgspec the
//!   SDK writes, so it is not worth modelling.
//! - **Only `format: 1` is accepted.** NuGet does not check `format` at all.
//! - **An unsupported framework declines**, since this crate's framework parser
//!   reads some of NuGet's irregular spellings as unsupported on purpose.
//! - Comments and trailing commas, which NuGet accepts, are a syntax decline.
//!   A leading byte-order mark, which NuGet also accepts, is skipped.

mod json;
mod log_code;

pub use json::JsonKind;
pub use log_code::NuGetLogCode;

use json::Json;

use crate::{
    FrameworkParseError, NuGetFramework, NuGetVersion, PackageId, PackageIdParseError,
    RangeParseError, VersionParseError, VersionRange,
};

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

/// A parsed dependency-graph spec.
#[derive(Debug, Clone)]
pub struct DependencyGraphSpec {
    /// The projects to restore, by unique name, in document order. NuGet keeps
    /// a name here even when no project in [`Self::projects`] carries it.
    pub restore: Vec<String>,
    /// Every project in the graph, in document order. Each is keyed by its
    /// unique name, so unique names are distinct.
    pub projects: Vec<PackageSpec>,
}

/// One project (NuGet's `PackageSpec`). Its name and file path are its
/// restore metadata's `projectName` and `projectPath`.
#[derive(Debug, Clone)]
pub struct PackageSpec {
    /// The project's package version; `1.0.0` when the document gives none.
    pub version: NuGetVersion,
    /// Whether [`Self::version`] is that default rather than a written value.
    pub is_default_version: bool,
    pub restore: ProjectRestoreMetadata,
    pub frameworks: Vec<TargetFrameworkInformation>,
}

/// A project's restore metadata (`ProjectRestoreMetadata`).
///
/// Every property NuGet's type has that is not a field here is one whose
/// dgspec key declines, so on an accepted spec it holds NuGet's default.
#[derive(Debug, Clone)]
pub struct ProjectRestoreMetadata {
    pub project_unique_name: String,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub output_path: Option<String>,
    pub packages_path: Option<String>,
    /// Package sources in document order (the source string is also its name).
    pub sources: Vec<String>,
    /// Fallback package folders in document order.
    pub fallback_folders: Vec<String>,
    pub config_file_paths: Vec<String>,
    pub frameworks: Vec<RestoreFrameworkInfo>,
    pub original_target_frameworks: Vec<String>,
    pub cross_targeting: bool,
    pub skip_content_file_write: bool,
    pub warning_properties: WarningProperties,
    pub restore_lock_properties: RestoreLockProperties,
    pub central_package_versions_enabled: bool,
    pub central_package_transitive_pinning_enabled: bool,
    /// `None` when the document has no `restoreAuditProperties`.
    pub restore_audit_properties: Option<RestoreAuditProperties>,
    pub sdk_analysis_level: Option<NuGetVersion>,
    /// `true` unless the document says otherwise.
    pub using_microsoft_net_sdk: bool,
}

/// One target framework's entry in the restore metadata: its project
/// references.
#[derive(Debug, Clone)]
pub struct RestoreFrameworkInfo {
    /// Empty when the document gives none.
    pub target_alias: String,
    /// The `framework` field when present, else the alias it is keyed by.
    /// Never unsupported.
    pub framework: NuGetFramework,
    pub project_references: Vec<ProjectRestoreReference>,
}

/// A `ProjectReference` as restore sees it.
#[derive(Debug, Clone)]
pub struct ProjectRestoreReference {
    pub project_unique_name: String,
    pub project_path: Option<String>,
    pub include_assets: LibraryIncludeFlags,
    pub exclude_assets: LibraryIncludeFlags,
    pub private_assets: LibraryIncludeFlags,
}

/// The project-wide warning policy (`WarningProperties`). Each set is in
/// ascending order.
#[derive(Debug, Clone, Default)]
pub struct WarningProperties {
    pub all_warnings_as_errors: bool,
    pub warnings_as_errors: BTreeSet<NuGetLogCode>,
    pub no_warn: BTreeSet<NuGetLogCode>,
    pub warnings_not_as_errors: BTreeSet<NuGetLogCode>,
}

#[derive(Debug, Clone, Default)]
pub struct RestoreLockProperties {
    pub restore_packages_with_lock_file: Option<String>,
    pub nuget_lock_file_path: Option<String>,
    pub restore_locked_mode: bool,
}

#[derive(Debug, Clone)]
pub struct RestoreAuditProperties {
    pub enable_audit: Option<String>,
    pub audit_level: Option<String>,
    pub audit_mode: Option<String>,
    /// Advisory URLs in document order; `None` when the key is absent or its
    /// object is empty.
    pub suppressed_advisories: Option<Vec<String>>,
}

/// One target framework of a project (`TargetFrameworkInformation`).
#[derive(Debug, Clone)]
pub struct TargetFrameworkInformation {
    /// Empty when the document gives none.
    pub target_alias: String,
    /// The `framework` field when present, else the alias it is keyed by.
    /// Never unsupported.
    pub framework: NuGetFramework,
    pub dependencies: Vec<LibraryDependency>,
    pub imports: Vec<NuGetFramework>,
    pub asset_target_fallback: bool,
    pub warn: bool,
    pub download_dependencies: Vec<DownloadDependency>,
    pub central_package_versions: Vec<PackageRangeEntry>,
    pub framework_references: Vec<FrameworkReference>,
    pub packages_to_prune: Vec<PackageRangeEntry>,
    pub runtime_identifier_graph_path: Option<String>,
}

/// A direct dependency (`LibraryDependency`) as the dgspec declares it.
#[derive(Debug, Clone)]
pub struct LibraryDependency {
    pub id: PackageId,
    pub range: VersionRange,
    pub target: LibraryDependencyTarget,
    /// The `include` flags less the `exclude` flags.
    pub include: LibraryIncludeFlags,
    pub suppress_parent: LibraryIncludeFlags,
    /// In document order.
    pub no_warn: Vec<NuGetLogCode>,
    pub auto_referenced: bool,
    pub version_centrally_managed: bool,
    pub generate_path_property: bool,
    pub aliases: Option<String>,
    pub version_override: Option<VersionRange>,
}

/// A `PackageDownload`: a package restore fetches but does not reference.
#[derive(Debug, Clone)]
pub struct DownloadDependency {
    pub id: PackageId,
    pub range: VersionRange,
}

/// A package id paired with a range: a central package version, or a package
/// to prune.
#[derive(Debug, Clone)]
pub struct PackageRangeEntry {
    pub id: PackageId,
    pub range: VersionRange,
}

/// A `FrameworkReference`.
#[derive(Debug, Clone)]
pub struct FrameworkReference {
    pub name: String,
    pub private_assets: FrameworkDependencyFlags,
}

/// NuGet's `LibraryIncludeFlags`: which asset kinds a dependency contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LibraryIncludeFlags(u8);

impl LibraryIncludeFlags {
    pub const NONE: LibraryIncludeFlags = LibraryIncludeFlags(0);
    pub const RUNTIME: LibraryIncludeFlags = LibraryIncludeFlags(1);
    pub const COMPILE: LibraryIncludeFlags = LibraryIncludeFlags(2);
    pub const BUILD: LibraryIncludeFlags = LibraryIncludeFlags(4);
    pub const NATIVE: LibraryIncludeFlags = LibraryIncludeFlags(8);
    pub const CONTENT_FILES: LibraryIncludeFlags = LibraryIncludeFlags(16);
    pub const ANALYZERS: LibraryIncludeFlags = LibraryIncludeFlags(32);
    pub const BUILD_TRANSITIVE: LibraryIncludeFlags = LibraryIncludeFlags(64);
    pub const ALL: LibraryIncludeFlags = LibraryIncludeFlags(127);
    /// What `suppressParent` means when the document omits it.
    pub const DEFAULT_SUPPRESS_PARENT: LibraryIncludeFlags =
        LibraryIncludeFlags(Self::BUILD.0 | Self::CONTENT_FILES.0 | Self::ANALYZERS.0);

    pub fn bits(self) -> u8 {
        self.0
    }

    fn union(self, other: LibraryIncludeFlags) -> LibraryIncludeFlags {
        LibraryIncludeFlags(self.0 | other.0)
    }

    fn without(self, other: LibraryIncludeFlags) -> LibraryIncludeFlags {
        LibraryIncludeFlags(self.0 & !other.0)
    }

    /// The flags the name `token` stands for, compared ignoring ASCII case.
    /// `BuildTransitive` stands for itself *and* `Build`, in every flags field
    /// (so excluding it excludes both).
    fn named(token: &str) -> Option<LibraryIncludeFlags> {
        const NAMES: &[(&str, LibraryIncludeFlags)] = &[
            ("None", LibraryIncludeFlags::NONE),
            ("Runtime", LibraryIncludeFlags::RUNTIME),
            ("Compile", LibraryIncludeFlags::COMPILE),
            ("Build", LibraryIncludeFlags::BUILD),
            ("Native", LibraryIncludeFlags::NATIVE),
            ("ContentFiles", LibraryIncludeFlags::CONTENT_FILES),
            ("Analyzers", LibraryIncludeFlags::ANALYZERS),
            (
                "BuildTransitive",
                LibraryIncludeFlags(
                    LibraryIncludeFlags::BUILD_TRANSITIVE.0 | LibraryIncludeFlags::BUILD.0,
                ),
            ),
            ("All", LibraryIncludeFlags::ALL),
        ];
        NAMES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(token))
            .map(|&(_, flags)| flags)
    }
}

/// NuGet's `LibraryDependencyTarget`: which kinds of library may satisfy a
/// dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LibraryDependencyTarget(u8);

impl LibraryDependencyTarget {
    pub const PACKAGE: LibraryDependencyTarget = LibraryDependencyTarget(1);
    pub const PROJECT: LibraryDependencyTarget = LibraryDependencyTarget(2);
    /// What the reader assigns a dependency whose `target` is absent:
    /// `PackageProjectExternal | Assembly | WinMD`, which is not `All` (63)
    /// because it leaves out `Reference`.
    pub const UNSPECIFIED: LibraryDependencyTarget = LibraryDependencyTarget(47);

    pub fn bits(self) -> u8 {
        self.0
    }
}

/// NuGet's `FrameworkDependencyFlags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameworkDependencyFlags(u16);

impl FrameworkDependencyFlags {
    pub const NONE: FrameworkDependencyFlags = FrameworkDependencyFlags(0);
    pub const ALL: FrameworkDependencyFlags = FrameworkDependencyFlags(65535);

    pub fn bits(self) -> u16 {
        self.0
    }
}

/// Where in the document a decline arose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JsonPath(Vec<PathSegment>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

impl JsonPath {
    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    fn key(&self, key: &str) -> JsonPath {
        let mut segments = self.0.clone();
        segments.push(PathSegment::Key(key.to_owned()));
        JsonPath(segments)
    }

    fn index(&self, index: usize) -> JsonPath {
        let mut segments = self.0.clone();
        segments.push(PathSegment::Index(index));
        JsonPath(segments)
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for segment in &self.0 {
            match segment {
                PathSegment::Key(key) => write!(f, "[{key:?}]")?,
                PathSegment::Index(index) => write!(f, "[{index}]")?,
            }
        }
        Ok(())
    }
}

/// Why [`parse_dgspec`] declined, and where.
#[derive(Debug, Clone)]
pub struct DgSpecDecline {
    pub path: JsonPath,
    pub reason: DgSpecDeclineReason,
}

#[derive(Debug, Clone)]
pub enum DgSpecDeclineReason {
    /// Not a JSON document `serde_json` accepts (this includes comments and
    /// trailing commas, which NuGet's reader allows).
    Syntax(String),
    /// A key outside the modelled vocabulary; `path` ends with it.
    UnmodelledKey,
    /// An object repeats a key; `path` ends with it.
    DuplicateKey,
    /// A required key is missing from the object at `path`.
    MissingKey(&'static str),
    WrongType {
        expected: JsonKind,
        found: JsonKind,
    },
    /// `format` is not the integer 1.
    UnsupportedFormat,
    /// `projectStyle` is not `PackageReference`.
    UnsupportedProjectStyle(String),
    /// A project's key is not its `projectUniqueName`, which is the name NuGet
    /// uses (it ignores the key).
    ProjectKeyMismatch {
        unique_name: String,
    },
    /// Two package ids at one level are equal ignoring case; `path` is the
    /// second.
    DuplicatePackageId,
    /// A name the reader may not keep distinct from a sibling's (see
    /// `Fields::into_named_members`).
    AmbiguousName,
    /// A framework this crate parses as unsupported. Where NuGet's parser
    /// salvages an irregular platform suffix (`net8.0-1`), ours deliberately
    /// does not (see [`NuGetFramework`]), so an unsupported framework here
    /// may not be NuGet's reading.
    UnsupportedFramework,
    Version(VersionParseError),
    Range(RangeParseError),
    Framework(FrameworkParseError),
    PackageId(PackageIdParseError),
    /// A package id with surrounding whitespace, which [`PackageId::parse`]
    /// would trim and NuGet keeps.
    UntrimmedPackageId,
    /// A flags string containing a token outside the modelled vocabulary.
    UnrecognisedFlag(String),
    /// A `target` outside the modelled vocabulary.
    UnrecognisedTarget(String),
    /// A warning code not spelled exactly as one of [`NuGetLogCode`]'s names.
    UnrecognisedLogCode(String),
    /// An `imports` entry that is empty or not a specific framework.
    UnsupportedImport,
    /// A framework reference with an empty name, which the reader refuses.
    EmptyFrameworkReferenceName,
}

impl fmt::Display for DgSpecDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = &self.path;
        match &self.reason {
            DgSpecDeclineReason::Syntax(message) => write!(f, "not JSON: {message}"),
            DgSpecDeclineReason::UnmodelledKey => write!(f, "{path}: key is not modelled"),
            DgSpecDeclineReason::DuplicateKey => write!(f, "{path}: key appears more than once"),
            DgSpecDeclineReason::MissingKey(key) => write!(f, "{path}: missing {key:?}"),
            DgSpecDeclineReason::WrongType { expected, found } => {
                write!(f, "{path}: expected {expected}, found {found}")
            }
            DgSpecDeclineReason::UnsupportedFormat => write!(f, "{path}: only format 1 is read"),
            DgSpecDeclineReason::UnsupportedProjectStyle(style) => {
                write!(f, "{path}: project style {style:?} is not PackageReference")
            }
            DgSpecDeclineReason::ProjectKeyMismatch { unique_name } => {
                write!(
                    f,
                    "{path}: key differs from projectUniqueName {unique_name:?}"
                )
            }
            DgSpecDeclineReason::DuplicatePackageId => {
                write!(f, "{path}: package id repeats, ignoring case")
            }
            DgSpecDeclineReason::AmbiguousName => {
                write!(f, "{path}: name may be merged with a sibling's")
            }
            DgSpecDeclineReason::UnsupportedFramework => {
                write!(f, "{path}: framework is unsupported")
            }
            DgSpecDeclineReason::Version(error) => write!(f, "{path}: {error}"),
            DgSpecDeclineReason::Range(error) => write!(f, "{path}: {error}"),
            DgSpecDeclineReason::Framework(error) => write!(f, "{path}: {error}"),
            DgSpecDeclineReason::PackageId(error) => write!(f, "{path}: {error}"),
            DgSpecDeclineReason::UntrimmedPackageId => {
                write!(f, "{path}: package id has surrounding whitespace")
            }
            DgSpecDeclineReason::UnrecognisedFlag(token) => {
                write!(f, "{path}: unrecognised flag {token:?}")
            }
            DgSpecDeclineReason::UnrecognisedTarget(target) => {
                write!(f, "{path}: unrecognised dependency target {target:?}")
            }
            DgSpecDeclineReason::UnrecognisedLogCode(code) => {
                write!(f, "{path}: unrecognised warning code {code:?}")
            }
            DgSpecDeclineReason::UnsupportedImport => {
                write!(f, "{path}: import is empty or not a specific framework")
            }
            DgSpecDeclineReason::EmptyFrameworkReferenceName => {
                write!(f, "{path}: framework reference has an empty name")
            }
        }
    }
}

impl Error for DgSpecDecline {}

type Parsed<T> = Result<T, DgSpecDecline>;

fn decline<T>(path: JsonPath, reason: DgSpecDeclineReason) -> Parsed<T> {
    Err(DgSpecDecline { path, reason })
}

/// Read a dependency-graph spec, as NuGet's `DependencyGraphSpec.Load` would,
/// or decline (see the module docs for what declines).
pub fn parse_dgspec(text: &str) -> Result<DependencyGraphSpec, DgSpecDecline> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let root = match json::parse(text) {
        Ok(root) => root,
        Err(error) => {
            return decline(
                JsonPath::default(),
                DgSpecDeclineReason::Syntax(error.to_string()),
            );
        }
    };
    let path = JsonPath::default();
    let mut fields = Fields::new(&path, &root)?;

    match fields.take("format") {
        Some((format_path, Json::Number(n))) if n.as_u64() == Some(1) => {
            let _ = format_path;
        }
        Some((format_path, _)) => {
            return decline(format_path, DgSpecDeclineReason::UnsupportedFormat);
        }
        None => return decline(path.clone(), DgSpecDeclineReason::MissingKey("format")),
    }

    let mut restore = Vec::new();
    if let Some((restore_path, value)) = fields.take("restore") {
        for (entry_path, name, entry) in Fields::new(&restore_path, value)?.into_named_members()? {
            Fields::new(&entry_path, entry)?.finish()?;
            restore.push(name.to_owned());
        }
    }

    let mut projects: Vec<PackageSpec> = Vec::new();
    if let Some((projects_path, value)) = fields.take("projects") {
        for (project_path, key, project) in
            Fields::new(&projects_path, value)?.into_named_members()?
        {
            let spec = package_spec(&project_path, project)?;
            if spec.restore.project_unique_name != key {
                return decline(
                    project_path,
                    DgSpecDeclineReason::ProjectKeyMismatch {
                        unique_name: spec.restore.project_unique_name,
                    },
                );
            }
            projects.push(spec);
        }
    }
    fields.finish()?;

    Ok(DependencyGraphSpec { restore, projects })
}

fn package_spec(path: &JsonPath, value: &Json) -> Parsed<PackageSpec> {
    let mut fields = Fields::new(path, value)?;
    let (version, is_default_version) = match fields.take("version") {
        Some((p, v)) => (version(&p, v)?, false),
        None => (
            NuGetVersion::parse("1.0.0").expect("1.0.0 is a version"),
            true,
        ),
    };
    let restore = match fields.take("restore") {
        Some((p, v)) => restore_metadata(&p, v)?,
        None => return decline(path.clone(), DgSpecDeclineReason::MissingKey("restore")),
    };
    let mut frameworks = Vec::new();
    if let Some((frameworks_path, v)) = fields.take("frameworks") {
        for (p, alias, f) in Fields::new(&frameworks_path, v)?.into_named_members()? {
            frameworks.push(target_framework(&p, alias, f)?);
        }
    }
    fields.finish()?;
    Ok(PackageSpec {
        version,
        is_default_version,
        restore,
        frameworks,
    })
}

fn restore_metadata(path: &JsonPath, value: &Json) -> Parsed<ProjectRestoreMetadata> {
    let mut fields = Fields::new(path, value)?;
    let project_unique_name = match fields.take("projectUniqueName") {
        Some((p, v)) => string(&p, v)?,
        None => {
            return decline(
                path.clone(),
                DgSpecDeclineReason::MissingKey("projectUniqueName"),
            );
        }
    };
    match fields.take("projectStyle") {
        Some((p, v)) => {
            let style = string(&p, v)?;
            if style != "PackageReference" {
                return decline(p, DgSpecDeclineReason::UnsupportedProjectStyle(style));
            }
        }
        None => {
            return decline(
                path.clone(),
                DgSpecDeclineReason::MissingKey("projectStyle"),
            );
        }
    }
    let project_name = fields.optional("projectName", string)?;
    let project_path = fields.optional("projectPath", string)?;
    let output_path = fields.optional("outputPath", string)?;
    let packages_path = fields.optional("packagesPath", string)?;

    let mut sources = Vec::new();
    if let Some((sources_path, v)) = fields.take("sources") {
        for (p, source, options) in Fields::new(&sources_path, v)?.into_named_members()? {
            Fields::new(&p, options)?.finish()?;
            sources.push(source.to_owned());
        }
    }
    let fallback_folders = fields
        .optional("fallbackFolders", strings)?
        .unwrap_or_default();
    let config_file_paths = fields
        .optional("configFilePaths", strings)?
        .unwrap_or_default();
    let original_target_frameworks = fields
        .optional("originalTargetFrameworks", strings)?
        .unwrap_or_default();

    let mut frameworks = Vec::new();
    if let Some((frameworks_path, v)) = fields.take("frameworks") {
        for (p, alias, f) in Fields::new(&frameworks_path, v)?.into_named_members()? {
            frameworks.push(restore_framework(&p, alias, f)?);
        }
    }

    let cross_targeting = fields.flag("crossTargeting")?;
    let skip_content_file_write = fields.flag("skipContentFileWrite")?;
    let central_package_versions_enabled =
        fields.flag("centralPackageVersionsManagementEnabled")?;
    let central_package_transitive_pinning_enabled =
        fields.flag("CentralPackageTransitivePinningEnabled")?;
    let using_microsoft_net_sdk = fields
        .optional("UsingMicrosoftNETSdk", boolean)?
        .unwrap_or(true);
    let sdk_analysis_level = fields.optional("SdkAnalysisLevel", version)?;
    let warning_properties = fields
        .optional("warningProperties", warning_properties)?
        .unwrap_or_default();
    let restore_lock_properties = fields
        .optional("restoreLockProperties", restore_lock_properties)?
        .unwrap_or_default();
    let restore_audit_properties =
        fields.optional("restoreAuditProperties", restore_audit_properties)?;
    fields.finish()?;

    Ok(ProjectRestoreMetadata {
        project_unique_name,
        project_name,
        project_path,
        output_path,
        packages_path,
        sources,
        fallback_folders,
        config_file_paths,
        frameworks,
        original_target_frameworks,
        cross_targeting,
        skip_content_file_write,
        warning_properties,
        restore_lock_properties,
        central_package_versions_enabled,
        central_package_transitive_pinning_enabled,
        restore_audit_properties,
        sdk_analysis_level,
        using_microsoft_net_sdk,
    })
}

fn restore_framework(path: &JsonPath, alias: &str, value: &Json) -> Parsed<RestoreFrameworkInfo> {
    let mut fields = Fields::new(path, value)?;
    let target_alias = fields.optional("targetAlias", string)?.unwrap_or_default();
    let framework = effective_framework(path, alias, &mut fields)?;
    let mut project_references = Vec::new();
    if let Some((references_path, v)) = fields.take("projectReferences") {
        for (p, unique_name, reference) in Fields::new(&references_path, v)?.into_named_members()? {
            let mut reference_fields = Fields::new(&p, reference)?;
            let project_path = reference_fields.optional("projectPath", string)?;
            let include_assets = reference_fields
                .optional("includeAssets", reference_flags)?
                .flatten()
                .unwrap_or(LibraryIncludeFlags::ALL);
            let exclude_assets = reference_fields
                .optional("excludeAssets", reference_flags)?
                .flatten()
                .unwrap_or(LibraryIncludeFlags::NONE);
            let private_assets = reference_fields
                .optional("privateAssets", reference_flags)?
                .flatten()
                .unwrap_or(LibraryIncludeFlags::DEFAULT_SUPPRESS_PARENT);
            reference_fields.finish()?;
            project_references.push(ProjectRestoreReference {
                project_unique_name: unique_name.to_owned(),
                project_path,
                include_assets,
                exclude_assets,
                private_assets,
            });
        }
    }
    fields.finish()?;
    Ok(RestoreFrameworkInfo {
        target_alias,
        framework,
        project_references,
    })
}

fn warning_properties(path: &JsonPath, value: &Json) -> Parsed<WarningProperties> {
    let mut fields = Fields::new(path, value)?;
    let all_warnings_as_errors = fields.flag("allWarningsAsErrors")?;
    let mut set = |key: &'static str| -> Parsed<BTreeSet<NuGetLogCode>> {
        Ok(fields
            .optional(key, log_codes)?
            .unwrap_or_default()
            .into_iter()
            .collect())
    };
    let warnings_as_errors = set("warnAsError")?;
    let no_warn = set("noWarn")?;
    let warnings_not_as_errors = set("warnNotAsError")?;
    fields.finish()?;
    Ok(WarningProperties {
        all_warnings_as_errors,
        warnings_as_errors,
        no_warn,
        warnings_not_as_errors,
    })
}

fn restore_lock_properties(path: &JsonPath, value: &Json) -> Parsed<RestoreLockProperties> {
    let mut fields = Fields::new(path, value)?;
    let restore_packages_with_lock_file = fields.optional("restorePackagesWithLockFile", string)?;
    let nuget_lock_file_path = fields.optional("nuGetLockFilePath", string)?;
    let restore_locked_mode = fields.flag("restoreLockedMode")?;
    fields.finish()?;
    Ok(RestoreLockProperties {
        restore_packages_with_lock_file,
        nuget_lock_file_path,
        restore_locked_mode,
    })
}

fn restore_audit_properties(path: &JsonPath, value: &Json) -> Parsed<RestoreAuditProperties> {
    let mut fields = Fields::new(path, value)?;
    let enable_audit = fields.optional("enableAudit", string)?;
    let audit_level = fields.optional("auditLevel", string)?;
    let audit_mode = fields.optional("auditMode", string)?;
    let suppressed_advisories = match fields.take("suppressedAdvisories") {
        Some((p, v)) => {
            let mut advisories = Vec::new();
            for (advisory_path, url, options) in Fields::new(&p, v)?.into_named_members()? {
                // The SDK writes `null`; the reader takes an empty object too.
                if *options != Json::Null {
                    Fields::new(&advisory_path, options)?.finish()?;
                }
                advisories.push(url.to_owned());
            }
            (!advisories.is_empty()).then_some(advisories)
        }
        None => None,
    };
    fields.finish()?;
    Ok(RestoreAuditProperties {
        enable_audit,
        audit_level,
        audit_mode,
        suppressed_advisories,
    })
}

fn target_framework(
    path: &JsonPath,
    alias: &str,
    value: &Json,
) -> Parsed<TargetFrameworkInformation> {
    let mut fields = Fields::new(path, value)?;
    let target_alias = fields.optional("targetAlias", string)?.unwrap_or_default();
    let framework = effective_framework(path, alias, &mut fields)?;

    let mut dependencies = Vec::new();
    if let Some((dependencies_path, v)) = fields.take("dependencies") {
        let mut seen = Ids::default();
        for (p, name, dependency) in Fields::new(&dependencies_path, v)?.into_members() {
            let id = package_id(&p, name)?;
            seen.insert(&p, &id)?;
            dependencies.push(library_dependency(&p, id, dependency)?);
        }
    }
    let imports = fields
        .optional("imports", |p, v| array(p, v, import))?
        .unwrap_or_default();
    let asset_target_fallback = fields.flag("assetTargetFallback")?;
    let warn = fields.flag("warn")?;

    let mut download_dependencies = Vec::new();
    if let Some((downloads_path, v)) = fields.take("downloadDependencies") {
        let mut seen = Ids::default();
        for (index, item) in as_array(&downloads_path, v)?.iter().enumerate() {
            let p = downloads_path.index(index);
            let mut item_fields = Fields::new(&p, item)?;
            let id = match item_fields.take("name") {
                Some((name_path, name)) => package_id(&name_path, &string(&name_path, name)?)?,
                None => return decline(p, DgSpecDeclineReason::MissingKey("name")),
            };
            seen.insert(&p, &id)?;
            let range = match item_fields.take("version") {
                Some((range_path, range)) => range_value(&range_path, range)?,
                None => return decline(p, DgSpecDeclineReason::MissingKey("version")),
            };
            item_fields.finish()?;
            download_dependencies.push(DownloadDependency { id, range });
        }
    }

    let central_package_versions = fields
        .optional("centralPackageVersions", package_ranges)?
        .unwrap_or_default();
    let packages_to_prune = fields
        .optional("packagesToPrune", package_ranges)?
        .unwrap_or_default();

    let mut framework_references = Vec::new();
    if let Some((references_path, v)) = fields.take("frameworkReferences") {
        for (p, name, reference) in Fields::new(&references_path, v)?.into_named_members()? {
            if name.is_empty() {
                return decline(p, DgSpecDeclineReason::EmptyFrameworkReferenceName);
            }
            let mut reference_fields = Fields::new(&p, reference)?;
            let private_assets = reference_fields
                .optional("privateAssets", framework_dependency_flags)?
                .unwrap_or(FrameworkDependencyFlags::NONE);
            reference_fields.finish()?;
            framework_references.push(FrameworkReference {
                name: name.to_owned(),
                private_assets,
            });
        }
    }
    let runtime_identifier_graph_path = fields.optional("runtimeIdentifierGraphPath", string)?;
    fields.finish()?;

    Ok(TargetFrameworkInformation {
        target_alias,
        framework,
        dependencies,
        imports,
        asset_target_fallback,
        warn,
        download_dependencies,
        central_package_versions,
        framework_references,
        packages_to_prune,
        runtime_identifier_graph_path,
    })
}

fn library_dependency(path: &JsonPath, id: PackageId, value: &Json) -> Parsed<LibraryDependency> {
    let mut fields = Fields::new(path, value)?;
    let range = match fields.take("version") {
        Some((p, v)) => range_value(&p, v)?,
        // The reader refuses a package dependency without a range.
        None => return decline(path.clone(), DgSpecDeclineReason::MissingKey("version")),
    };
    let target = fields
        .optional("target", dependency_target)?
        .unwrap_or(LibraryDependencyTarget::UNSPECIFIED);
    let include = fields
        .optional("include", include_flags)?
        .unwrap_or(LibraryIncludeFlags::ALL);
    let exclude = fields
        .optional("exclude", include_flags)?
        .unwrap_or(LibraryIncludeFlags::NONE);
    let suppress_parent = fields
        .optional("suppressParent", include_flags)?
        .unwrap_or(LibraryIncludeFlags::DEFAULT_SUPPRESS_PARENT);
    let no_warn = fields.optional("noWarn", log_codes)?.unwrap_or_default();
    let auto_referenced = fields.flag("autoReferenced")?;
    let version_centrally_managed = fields.flag("versionCentrallyManaged")?;
    let generate_path_property = fields.flag("generatePathProperty")?;
    let aliases = fields.optional("aliases", string)?;
    let version_override = fields.optional("versionOverride", range_value)?;
    fields.finish()?;
    Ok(LibraryDependency {
        id,
        range,
        target,
        include: include.without(exclude),
        suppress_parent,
        no_warn,
        auto_referenced,
        version_centrally_managed,
        generate_path_property,
        aliases,
        version_override,
    })
}

/// The `framework` field when present, else the alias key it sits under.
fn effective_framework(
    path: &JsonPath,
    alias: &str,
    fields: &mut Fields,
) -> Parsed<NuGetFramework> {
    match fields.take("framework") {
        Some((p, v)) => framework_value(&p, v),
        None => framework_text(path, alias),
    }
}

/// One `imports` entry. The reader skips an empty one and refuses the whole
/// document over one that is not a specific framework (unsupported, `any`,
/// `agnostic`); both decline here.
fn import(path: &JsonPath, value: &Json) -> Parsed<NuGetFramework> {
    let framework = framework_value(path, value)?;
    if !framework.is_specific_framework() {
        return decline(path.clone(), DgSpecDeclineReason::UnsupportedImport);
    }
    Ok(framework)
}

fn package_ranges(path: &JsonPath, value: &Json) -> Parsed<Vec<PackageRangeEntry>> {
    let mut entries = Vec::new();
    let mut seen = Ids::default();
    for (p, name, range) in Fields::new(path, value)?.into_members() {
        let id = package_id(&p, name)?;
        seen.insert(&p, &id)?;
        entries.push(PackageRangeEntry {
            id,
            range: range_value(&p, range)?,
        });
    }
    Ok(entries)
}

/// Package ids seen so far at one level, to decline a case-insensitive repeat.
#[derive(Default)]
struct Ids(BTreeSet<PackageId>);

impl Ids {
    fn insert(&mut self, path: &JsonPath, id: &PackageId) -> Parsed<()> {
        if self.0.insert(id.clone()) {
            Ok(())
        } else {
            decline(path.clone(), DgSpecDeclineReason::DuplicatePackageId)
        }
    }
}

fn package_id(path: &JsonPath, text: &str) -> Parsed<PackageId> {
    let id = PackageId::parse(text)
        .or_else(|error| decline(path.clone(), DgSpecDeclineReason::PackageId(error)))?;
    if id.as_str() != text {
        return decline(path.clone(), DgSpecDeclineReason::UntrimmedPackageId);
    }
    Ok(id)
}

/// A comma-separated list of [`LibraryIncludeFlags`] names, ignoring ASCII case
/// and the spaces around each name. The empty string is no flags. Only spaces
/// are trimmed: the reader does not recognise a name next to a tab.
fn include_flags(path: &JsonPath, value: &Json) -> Parsed<LibraryIncludeFlags> {
    let text = string(path, value)?;
    let mut flags = LibraryIncludeFlags::NONE;
    if text.is_empty() {
        return Ok(flags);
    }
    for token in text.split(',') {
        let token = token.trim_matches(' ');
        match LibraryIncludeFlags::named(token) {
            Some(named) => flags = flags.union(named),
            None => {
                return decline(
                    path.clone(),
                    DgSpecDeclineReason::UnrecognisedFlag(token.to_owned()),
                );
            }
        }
    }
    Ok(flags)
}

/// A project reference's flags: [`include_flags`], except that the empty
/// string means the field's default rather than no flags.
fn reference_flags(path: &JsonPath, value: &Json) -> Parsed<Option<LibraryIncludeFlags>> {
    if *value == Json::String(String::new()) {
        return Ok(None);
    }
    include_flags(path, value).map(Some)
}

fn dependency_target(path: &JsonPath, value: &Json) -> Parsed<LibraryDependencyTarget> {
    let text = string(path, value)?;
    if text.eq_ignore_ascii_case("Package") {
        Ok(LibraryDependencyTarget::PACKAGE)
    } else if text.eq_ignore_ascii_case("Project") {
        Ok(LibraryDependencyTarget::PROJECT)
    } else {
        decline(path.clone(), DgSpecDeclineReason::UnrecognisedTarget(text))
    }
}

fn framework_dependency_flags(path: &JsonPath, value: &Json) -> Parsed<FrameworkDependencyFlags> {
    let text = string(path, value)?;
    if text.eq_ignore_ascii_case("all") {
        Ok(FrameworkDependencyFlags::ALL)
    } else if text.eq_ignore_ascii_case("none") {
        Ok(FrameworkDependencyFlags::NONE)
    } else {
        decline(path.clone(), DgSpecDeclineReason::UnrecognisedFlag(text))
    }
}

fn log_codes(path: &JsonPath, value: &Json) -> Parsed<Vec<NuGetLogCode>> {
    array(path, value, |p, v| {
        let text = string(p, v)?;
        NuGetLogCode::parse(&text)
            .ok_or(())
            .or_else(|()| decline(p.clone(), DgSpecDeclineReason::UnrecognisedLogCode(text)))
    })
}

fn version(path: &JsonPath, value: &Json) -> Parsed<NuGetVersion> {
    let text = string(path, value)?;
    NuGetVersion::parse(&text)
        .or_else(|error| decline(path.clone(), DgSpecDeclineReason::Version(error)))
}

fn range_value(path: &JsonPath, value: &Json) -> Parsed<VersionRange> {
    let text = string(path, value)?;
    VersionRange::parse(&text)
        .or_else(|error| decline(path.clone(), DgSpecDeclineReason::Range(error)))
}

fn framework_value(path: &JsonPath, value: &Json) -> Parsed<NuGetFramework> {
    framework_text(path, &string(path, value)?)
}

fn framework_text(path: &JsonPath, text: &str) -> Parsed<NuGetFramework> {
    let framework = NuGetFramework::parse(text)
        .or_else(|error| decline(path.clone(), DgSpecDeclineReason::Framework(error)))?;
    if framework.is_unsupported() {
        return decline(path.clone(), DgSpecDeclineReason::UnsupportedFramework);
    }
    Ok(framework)
}

fn string(path: &JsonPath, value: &Json) -> Parsed<String> {
    match value {
        Json::String(s) => Ok(s.clone()),
        other => wrong_type(path, JsonKind::String, other),
    }
}

fn boolean(path: &JsonPath, value: &Json) -> Parsed<bool> {
    match value {
        Json::Bool(b) => Ok(*b),
        other => wrong_type(path, JsonKind::Bool, other),
    }
}

fn strings(path: &JsonPath, value: &Json) -> Parsed<Vec<String>> {
    array(path, value, string)
}

fn as_array<'a>(path: &JsonPath, value: &'a Json) -> Parsed<&'a [Json]> {
    match value {
        Json::Array(items) => Ok(items),
        other => wrong_type(path, JsonKind::Array, other),
    }
}

fn array<T>(
    path: &JsonPath,
    value: &Json,
    item: impl Fn(&JsonPath, &Json) -> Parsed<T>,
) -> Parsed<Vec<T>> {
    as_array(path, value)?
        .iter()
        .enumerate()
        .map(|(index, v)| item(&path.index(index), v))
        .collect()
}

fn wrong_type<T>(path: &JsonPath, expected: JsonKind, found: &Json) -> Parsed<T> {
    decline(
        path.clone(),
        DgSpecDeclineReason::WrongType {
            expected,
            found: found.kind(),
        },
    )
}

/// The members of one JSON object, read key by key. [`Self::finish`] declines
/// on any member nothing took, so every object's vocabulary is closed.
struct Fields<'a> {
    path: JsonPath,
    members: &'a [(String, Json)],
    taken: Vec<bool>,
}

impl<'a> Fields<'a> {
    /// The object at `path`, having declined if it is not an object or repeats
    /// a key.
    fn new(path: &JsonPath, value: &'a Json) -> Parsed<Fields<'a>> {
        let Json::Object(members) = value else {
            return wrong_type(path, JsonKind::Object, value);
        };
        let mut keys = BTreeSet::new();
        for (key, _) in members {
            if !keys.insert(key.as_str()) {
                return decline(path.key(key), DgSpecDeclineReason::DuplicateKey);
            }
        }
        Ok(Fields {
            path: path.clone(),
            members,
            taken: vec![false; members.len()],
        })
    }

    fn take(&mut self, key: &str) -> Option<(JsonPath, &'a Json)> {
        let index = self.members.iter().position(|(k, _)| k == key)?;
        self.taken[index] = true;
        Some((self.path.key(key), &self.members[index].1))
    }

    fn optional<T>(
        &mut self,
        key: &str,
        read: impl FnOnce(&JsonPath, &'a Json) -> Parsed<T>,
    ) -> Parsed<Option<T>> {
        self.take(key).map(|(p, v)| read(&p, v)).transpose()
    }

    /// A boolean that is `false` when absent.
    fn flag(&mut self, key: &str) -> Parsed<bool> {
        Ok(self.optional(key, boolean)?.unwrap_or(false))
    }

    /// [`Self::into_members`], having declined if two keys could be one name to
    /// the reader. NuGet compares many free-form names (project paths, aliases,
    /// framework references) ignoring case, some only on a case-insensitive
    /// file system, and merges or drops a repeat. So a repeat ignoring ASCII
    /// case declines; and since .NET's case-insensitive comparison and Rust's
    /// case mapping differ outside ASCII, so does any name with a non-ASCII
    /// cased letter once there are two names to confuse.
    fn into_named_members(self) -> Parsed<impl Iterator<Item = (JsonPath, &'a str, &'a Json)>> {
        let has_cased_non_ascii = |key: &str| {
            key.chars().any(|c| {
                !c.is_ascii()
                    && (c.to_lowercase().ne(std::iter::once(c))
                        || c.to_uppercase().ne(std::iter::once(c)))
            })
        };
        let mut folded = BTreeSet::new();
        for (key, _) in self.members {
            if (self.members.len() > 1 && has_cased_non_ascii(key))
                || !folded.insert(key.to_ascii_lowercase())
            {
                return decline(self.path.key(key), DgSpecDeclineReason::AmbiguousName);
            }
        }
        Ok(self.into_members())
    }

    /// Every member as `(path, key, value)`, for objects keyed by name rather
    /// than by a fixed vocabulary.
    fn into_members(self) -> impl Iterator<Item = (JsonPath, &'a str, &'a Json)> {
        let path = self.path;
        self.members
            .iter()
            .map(move |(key, value)| (path.key(key), key.as_str(), value))
    }

    fn finish(self) -> Parsed<()> {
        match self.taken.iter().position(|taken| !taken) {
            Some(index) => decline(
                self.path.key(&self.members[index].0),
                DgSpecDeclineReason::UnmodelledKey,
            ),
            None => Ok(()),
        }
    }
}
