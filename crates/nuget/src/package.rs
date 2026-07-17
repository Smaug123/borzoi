//! Exact installed-package lookup and `.nuspec` dependency projection.
//!
//! This module deliberately does not resolve dependency graphs or interpret
//! asset include/exclude lists. It answers only the stage-5 questions: for a
//! package id, which exact versions are committed in a caller-supplied global
//! packages root; for an exact package identity, is the package committed on
//! disk, what dependency groups does its nuspec declare, and which dependency
//! group applies to a project target framework?

use crate::{FrameworkParseError, NuGetFramework, NuGetVersion, RangeParseError, VersionRange};

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// A NuGet package id.
///
/// Package ids are case-insensitive for identity and lowercased in the global
/// packages folder. The original spelling is preserved for display; callers
/// use [`Self::folder_name`] when constructing cache paths.
#[derive(Debug, Clone)]
pub struct PackageId(String);

/// Why a package id was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageIdParseError {
    Empty,
    InvalidBoundary {
        value: String,
    },
    InvalidSeparatorRun {
        value: String,
        index: usize,
    },
    InvalidChar {
        value: String,
        index: usize,
        ch: char,
    },
}

impl fmt::Display for PackageIdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageIdParseError::Empty => f.write_str("package id is empty"),
            PackageIdParseError::InvalidBoundary { value } => {
                write!(
                    f,
                    "package id {value:?} must start and end with an ASCII letter, digit, or '_'"
                )
            }
            PackageIdParseError::InvalidSeparatorRun { value, index } => {
                write!(
                    f,
                    "package id {value:?} contains adjacent '.' or '-' separators at byte {index}"
                )
            }
            PackageIdParseError::InvalidChar { value, index, ch } => {
                write!(
                    f,
                    "package id {value:?} contains invalid character {ch:?} at byte {index}"
                )
            }
        }
    }
}

impl Error for PackageIdParseError {}

impl PackageId {
    /// Parse a package id from a project or nuspec string.
    ///
    /// The accepted grammar is intentionally the normal package-id envelope:
    /// ASCII letters/digits plus `.`, `_`, and `-`, with no path separators
    /// and no leading/trailing or adjacent `.`/`-` separators. That keeps
    /// global-packages path construction local and non-surprising.
    pub fn parse(input: &str) -> Result<PackageId, PackageIdParseError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(PackageIdParseError::Empty);
        }

        let mut previous_dot_dash_separator = false;
        for (index, ch) in trimmed.char_indices() {
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
                return Err(PackageIdParseError::InvalidChar {
                    value: trimmed.to_owned(),
                    index,
                    ch,
                });
            }
            let dot_dash_separator = matches!(ch, '.' | '-');
            if previous_dot_dash_separator && dot_dash_separator {
                return Err(PackageIdParseError::InvalidSeparatorRun {
                    value: trimmed.to_owned(),
                    index,
                });
            }
            previous_dot_dash_separator = dot_dash_separator;
        }

        let valid_boundary = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
        if !trimmed.chars().next().is_some_and(valid_boundary)
            || !trimmed.chars().last().is_some_and(valid_boundary)
        {
            return Err(PackageIdParseError::InvalidBoundary {
                value: trimmed.to_owned(),
            });
        }

        Ok(PackageId(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Folder name used by NuGet's global packages layout.
    pub fn folder_name(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq for PackageId {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for PackageId {}

impl PartialOrd for PackageId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PackageId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .cmp(other.0.bytes().map(|byte| byte.to_ascii_lowercase()))
    }
}

impl Hash for PackageId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for byte in self.0.bytes() {
            byte.to_ascii_lowercase().hash(state);
        }
    }
}

impl FromStr for PackageId {
    type Err = PackageIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PackageId::parse(s)
    }
}

/// An exact package identity. Resolution chooses these later; this module
/// only consumes them.
#[derive(Debug, Clone)]
pub struct PackageIdentity {
    pub id: PackageId,
    pub version: NuGetVersion,
}

impl PackageIdentity {
    pub fn new(id: PackageId, version: NuGetVersion) -> PackageIdentity {
        PackageIdentity { id, version }
    }

    /// The global-packages relative path, e.g. `fsharp.core/10.1.204`.
    pub fn cache_relative_path(&self) -> PathBuf {
        PathBuf::from(self.id.folder_name()).join(self.version_folder_name())
    }

    /// Version folder spelling used by the global packages folder.
    pub fn version_folder_name(&self) -> String {
        self.version.to_normalized_string().to_ascii_lowercase()
    }
}

impl PartialEq for PackageIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.version.eq_strict(&other.version)
    }
}

impl Eq for PackageIdentity {}

impl Hash for PackageIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.version.major().hash(state);
        self.version.minor().hash(state);
        self.version.patch().hash(state);
        self.version.revision().hash(state);
        self.version.release_labels().len().hash(state);
        for label in self.version.release_labels() {
            for byte in label.bytes() {
                byte.to_ascii_lowercase().hash(state);
            }
            0xff_u8.hash(state);
        }
    }
}

/// The cache paths NuGet uses for one exact package identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagePaths {
    pub package_dir: PathBuf,
    pub metadata_path: PathBuf,
    pub nuspec_path: PathBuf,
}

impl PackagePaths {
    pub fn new(global_packages_root: &Path, identity: &PackageIdentity) -> PackagePaths {
        let package_dir = global_packages_root.join(identity.cache_relative_path());
        let id_folder = identity.id.folder_name();
        PackagePaths {
            metadata_path: package_dir.join(".nupkg.metadata"),
            nuspec_path: package_dir.join(format!("{id_folder}.nuspec")),
            package_dir,
        }
    }

    /// NuGet writes `.nupkg.metadata` last; without it, the package folder may
    /// be a partial extraction and must not be trusted.
    ///
    /// A missing marker means the package is uncommitted. Other stat failures
    /// are returned so callers do not make decisions against an incomplete
    /// view of the cache.
    pub fn is_committed(&self) -> io::Result<bool> {
        match std::fs::metadata(&self.metadata_path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(source),
        }
    }
}

/// The package data slice 5 needs from an installed cache entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub identity: PackageIdentity,
    pub paths: PackagePaths,
    pub nuspec: PackageNuspec,
}

/// A committed exact package entry in the global packages folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCacheEntry {
    pub identity: PackageIdentity,
    pub paths: PackagePaths,
}

/// A nuspec dependency projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageNuspec {
    /// Dependency groups in nuspec order. Equivalent target frameworks are not
    /// coalesced: NuGet keeps duplicate group entries distinct, and restore
    /// group selection is order-sensitive.
    pub dependency_groups: Vec<PackageDependencyGroup>,
    /// `<references>` groups in nuspec order: the explicit allow-list of
    /// `lib/` assemblies a package wants referenced. Consumed by compile-asset
    /// selection ([`crate::assets`]); empty for the overwhelming majority of
    /// packages, which reference everything in the selected `lib/` folder.
    pub reference_groups: Vec<PackageReferenceGroup>,
}

impl PackageNuspec {
    /// Return the dependency group NuGet would use for `project`.
    ///
    /// This is only target-framework selection. Version resolution, graph
    /// conflict handling, and dependency `include`/`exclude` asset semantics
    /// belong to the resolver and asset-selection slices.
    pub fn select_dependency_group(
        &self,
        project: &NuGetFramework,
    ) -> Option<&PackageDependencyGroup> {
        self.select_dependency_group_index(project)
            .map(|index| &self.dependency_groups[index])
    }

    /// Return the index of the dependency group NuGet would use for `project`.
    ///
    /// The index form is useful because nuspecs may contain duplicate or
    /// equivalent target-framework groups, and NuGet's choice is
    /// order-sensitive.
    pub fn select_dependency_group_index(&self, project: &NuGetFramework) -> Option<usize> {
        let candidates = self
            .dependency_groups
            .iter()
            .map(|group| group.target_framework.clone())
            .collect::<Vec<_>>();
        NuGetFramework::get_nearest_reducer(project, &candidates)
    }
}

/// Dependencies scoped to one target framework. Unscoped dependencies use
/// NuGet's `Any` framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDependencyGroup {
    pub target_framework: NuGetFramework,
    pub dependencies: Vec<PackageDependency>,
}

/// A `<references>` group: the assembly file names a package wants referenced
/// at compile time, scoped to one target framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageReferenceGroup {
    pub target_framework: NuGetFramework,
    /// The `file` attribute of each `<reference>`, in nuspec order. Compared
    /// case-insensitively against the file name of a candidate compile asset.
    pub files: Vec<String>,
}

/// One `<dependency>` item from a nuspec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDependency {
    pub id: PackageId,
    pub version_range: Option<VersionRange>,
    /// Raw `include` attribute, when present. Interpreting the asset list is
    /// resolver work.
    pub include: Option<String>,
    /// Raw `exclude` attribute, when present. Interpreting the asset list is
    /// resolver work.
    pub exclude: Option<String>,
}

/// Why an installed package could not be read.
#[derive(Debug)]
pub enum PackageReadError {
    NotInstalled {
        metadata_path: PathBuf,
    },
    MissingNuspec {
        nuspec_path: PathBuf,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Nuspec {
        path: PathBuf,
        source: PackageNuspecParseError,
    },
}

/// Why a package-cache directory could not be enumerated.
#[derive(Debug)]
pub enum PackageCacheError {
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for PackageCacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageCacheError::Io { path, source } => {
                write!(
                    f,
                    "failed to read package cache at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for PackageCacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PackageCacheError::Io { source, .. } => Some(source),
        }
    }
}

impl fmt::Display for PackageReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageReadError::NotInstalled { metadata_path } => {
                write!(
                    f,
                    "package is not committed in the global packages folder (missing {})",
                    metadata_path.display()
                )
            }
            PackageReadError::MissingNuspec { nuspec_path } => {
                write!(f, "installed package is missing {}", nuspec_path.display())
            }
            PackageReadError::Io { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            PackageReadError::Nuspec { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
        }
    }
}

impl Error for PackageReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PackageReadError::Io { source, .. } => Some(source),
            PackageReadError::Nuspec { source, .. } => Some(source),
            PackageReadError::NotInstalled { .. } | PackageReadError::MissingNuspec { .. } => None,
        }
    }
}

/// Why a nuspec dependency projection failed.
#[derive(Debug)]
pub enum PackageNuspecParseError {
    Xml(roxmltree::Error),
    MissingDependencyId,
    InvalidDependencyId {
        raw: String,
        source: PackageIdParseError,
    },
    InvalidDependencyVersionRange {
        id: String,
        raw: String,
        source: RangeParseError,
    },
    InvalidTargetFramework {
        raw: String,
        source: FrameworkParseError,
    },
}

impl fmt::Display for PackageNuspecParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageNuspecParseError::Xml(e) => write!(f, "invalid XML: {e}"),
            PackageNuspecParseError::MissingDependencyId => {
                f.write_str("nuspec dependency is missing an id")
            }
            PackageNuspecParseError::InvalidDependencyId { raw, source } => {
                write!(f, "invalid dependency id {raw:?}: {source}")
            }
            PackageNuspecParseError::InvalidDependencyVersionRange { id, raw, source } => {
                write!(
                    f,
                    "invalid version range {raw:?} on dependency {id:?}: {source}"
                )
            }
            PackageNuspecParseError::InvalidTargetFramework { raw, source } => {
                write!(
                    f,
                    "invalid dependency-group target framework {raw:?}: {source}"
                )
            }
        }
    }
}

impl Error for PackageNuspecParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PackageNuspecParseError::Xml(source) => Some(source),
            PackageNuspecParseError::InvalidDependencyId { source, .. } => Some(source),
            PackageNuspecParseError::InvalidDependencyVersionRange { source, .. } => Some(source),
            PackageNuspecParseError::InvalidTargetFramework { source, .. } => Some(source),
            PackageNuspecParseError::MissingDependencyId => None,
        }
    }
}

/// List committed versions for one package id in a global packages root.
///
/// Only canonical NuGet global-packages entries are returned:
/// `{id-lower}/{normalised-version-lower}/.nupkg.metadata`. Invalid,
/// non-canonical, uncommitted, or non-directory children are ignored. IO
/// failures reading the package-id directory or its entries are returned so a
/// caller can conservatively decline resolution.
pub fn list_committed_package_versions(
    global_packages_root: &Path,
    id: &PackageId,
) -> Result<Vec<PackageCacheEntry>, PackageCacheError> {
    let id_dir = global_packages_root.join(id.folder_name());
    let entries = match std::fs::read_dir(&id_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(PackageCacheError::Io {
                path: id_dir,
                source,
            });
        }
    };

    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackageCacheError::Io {
            path: id_dir.clone(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| PackageCacheError::Io {
            path: path.clone(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(folder_name) = file_name.to_str() else {
            continue;
        };
        let Ok(version) = NuGetVersion::parse(folder_name) else {
            continue;
        };
        if folder_name != version.to_normalized_string().to_ascii_lowercase() {
            continue;
        }

        let identity = PackageIdentity::new(id.clone(), version);
        let paths = PackagePaths::new(global_packages_root, &identity);
        if paths
            .is_committed()
            .map_err(|source| PackageCacheError::Io {
                path: paths.metadata_path.clone(),
                source,
            })?
        {
            versions.push(PackageCacheEntry { identity, paths });
        }
    }

    versions.sort_by(|left, right| {
        left.identity
            .version
            .cmp(&right.identity.version)
            .then_with(|| {
                left.identity
                    .version_folder_name()
                    .cmp(&right.identity.version_folder_name())
            })
    });
    Ok(versions)
}

/// Read one exact package from a caller-supplied global packages root.
pub fn read_installed_package(
    global_packages_root: &Path,
    identity: PackageIdentity,
) -> Result<InstalledPackage, PackageReadError> {
    let mut paths = PackagePaths::new(global_packages_root, &identity);
    if !paths
        .is_committed()
        .map_err(|source| PackageReadError::Io {
            path: paths.metadata_path.clone(),
            source,
        })?
    {
        return Err(PackageReadError::NotInstalled {
            metadata_path: paths.metadata_path,
        });
    }
    let Some(nuspec_path) = find_root_nuspec_path(&paths, &identity.id)? else {
        return Err(PackageReadError::MissingNuspec {
            nuspec_path: paths.nuspec_path,
        });
    };
    paths.nuspec_path = nuspec_path;

    let bytes = std::fs::read(&paths.nuspec_path).map_err(|source| PackageReadError::Io {
        path: paths.nuspec_path.clone(),
        source,
    })?;
    let text = decode_nuspec_bytes(&bytes).map_err(|source| PackageReadError::Io {
        path: paths.nuspec_path.clone(),
        source,
    })?;
    let nuspec = parse_nuspec(&text).map_err(|source| PackageReadError::Nuspec {
        path: paths.nuspec_path.clone(),
        source,
    })?;

    Ok(InstalledPackage {
        identity,
        paths,
        nuspec,
    })
}

fn find_root_nuspec_path(
    paths: &PackagePaths,
    id: &PackageId,
) -> Result<Option<PathBuf>, PackageReadError> {
    let original_file_name = format!("{}.nuspec", id.as_str());
    let canonical_file_name = format!("{}.nuspec", id.folder_name());
    let mut canonical_match = None;
    let mut cased_matches = Vec::new();

    let entries = std::fs::read_dir(&paths.package_dir).map_err(|source| PackageReadError::Io {
        path: paths.package_dir.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| PackageReadError::Io {
            path: paths.package_dir.clone(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if file_name == original_file_name {
            return Ok(Some(path));
        }
        if file_name == canonical_file_name {
            canonical_match = Some(path);
            continue;
        }

        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if extension.eq_ignore_ascii_case("nuspec") && stem.eq_ignore_ascii_case(id.as_str()) {
            cased_matches.push(path);
        }
    }

    if let Some(path) = canonical_match {
        return Ok(Some(path));
    }
    cased_matches.sort();
    Ok(cased_matches.into_iter().next())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Utf16Endian {
    Little,
    Big,
}

fn decode_nuspec_bytes(bytes: &[u8]) -> io::Result<String> {
    match detect_utf16(bytes) {
        Some(endian) => decode_utf16(bytes, endian),
        None => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source)),
    }
}

fn detect_utf16(bytes: &[u8]) -> Option<Utf16Endian> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        return Some(Utf16Endian::Little);
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return Some(Utf16Endian::Big);
    }

    let sample_len = bytes.len().min(128) & !1;
    if sample_len < 4 {
        return None;
    }

    // BOM-less UTF-16 XML still exposes alternating NUL bytes through its
    // ASCII declaration or opening markup.
    let mut little_score = 0;
    let mut big_score = 0;
    for pair in bytes[..sample_len].chunks_exact(2) {
        if pair[1] == 0 && is_xml_ascii_byte(pair[0]) {
            little_score += 1;
        }
        if pair[0] == 0 && is_xml_ascii_byte(pair[1]) {
            big_score += 1;
        }
    }

    match little_score.cmp(&big_score) {
        Ordering::Greater if little_score >= 2 => Some(Utf16Endian::Little),
        Ordering::Less if big_score >= 2 => Some(Utf16Endian::Big),
        _ => None,
    }
}

fn is_xml_ascii_byte(byte: u8) -> bool {
    byte == b'\t' || byte == b'\n' || byte == b'\r' || (0x20..=0x7e).contains(&byte)
}

fn decode_utf16(bytes: &[u8], endian: Utf16Endian) -> io::Result<String> {
    let bytes = match endian {
        Utf16Endian::Little if bytes.starts_with(&[0xff, 0xfe]) => &bytes[2..],
        Utf16Endian::Big if bytes.starts_with(&[0xfe, 0xff]) => &bytes[2..],
        _ => bytes,
    };
    if bytes.len() % 2 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UTF-16 nuspec has an odd byte length",
        ));
    }

    let units = bytes
        .chunks_exact(2)
        .map(|pair| match endian {
            Utf16Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
            Utf16Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))
}

/// Parse just the dependency groups from a nuspec document.
pub fn parse_nuspec(text: &str) -> Result<PackageNuspec, PackageNuspecParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let doc = roxmltree::Document::parse(text).map_err(PackageNuspecParseError::Xml)?;
    let any_framework = NuGetFramework::parse("any").expect("literal Any framework parses");
    let mut dependency_groups = Vec::new();
    let root = doc.root_element();
    let nuspec_namespace = root.tag_name().namespace();

    let Some(metadata) = root
        .children()
        .find(|n| element_named(*n, nuspec_namespace, "metadata"))
    else {
        return Ok(PackageNuspec {
            dependency_groups,
            reference_groups: Vec::new(),
        });
    };
    let reference_groups = parse_reference_groups(metadata, nuspec_namespace, &any_framework)?;
    let dependency_sections = metadata
        .children()
        .filter(|n| element_named(*n, nuspec_namespace, "dependencies"))
        .collect::<Vec<_>>();

    let has_group = dependency_sections.iter().any(|dependencies| {
        dependencies
            .children()
            .any(|child| element_named(child, nuspec_namespace, "group"))
    });

    if has_group {
        for child in dependency_sections
            .iter()
            .flat_map(|dependencies| dependencies.children())
            .filter(|child| element_named(*child, nuspec_namespace, "group"))
        {
            dependency_groups.push(parse_dependency_group(
                child,
                nuspec_namespace,
                &any_framework,
            )?);
        }
    } else {
        let package_dependencies = dependency_sections
            .iter()
            .flat_map(|dependencies| dependencies.children())
            .filter(|child| element_named(*child, nuspec_namespace, "dependency"))
            .map(parse_dependency)
            .collect::<Result<Vec<_>, _>>()?;
        if !package_dependencies.is_empty() {
            dependency_groups.push(PackageDependencyGroup {
                target_framework: any_framework,
                dependencies: package_dependencies,
            });
        }
    }

    Ok(PackageNuspec {
        dependency_groups,
        reference_groups,
    })
}

/// `NuspecReader.GetReferenceGroups`: `<references><group>` entries win
/// outright, and the pre-2.5 flat `<references><reference>` list is read only
/// when no group exists at all. A `<reference>` without a `file` is dropped,
/// and a flat list that yields no files produces no group.
fn parse_reference_groups(
    metadata: roxmltree::Node<'_, '_>,
    nuspec_namespace: Option<&str>,
    any_framework: &NuGetFramework,
) -> Result<Vec<PackageReferenceGroup>, PackageNuspecParseError> {
    let sections = metadata
        .children()
        .filter(|n| element_named(*n, nuspec_namespace, "references"))
        .collect::<Vec<_>>();

    let mut groups = Vec::new();
    for group in sections
        .iter()
        .flat_map(|section| section.children())
        .filter(|child| element_named(*child, nuspec_namespace, "group"))
    {
        let target_framework = match group.attribute("targetFramework") {
            None | Some("") => any_framework.clone(),
            Some(raw) => NuGetFramework::parse(raw).map_err(|source| {
                PackageNuspecParseError::InvalidTargetFramework {
                    raw: raw.to_owned(),
                    source,
                }
            })?,
        };
        groups.push(PackageReferenceGroup {
            target_framework,
            files: reference_files(group, nuspec_namespace),
        });
    }
    if !groups.is_empty() {
        return Ok(groups);
    }

    let files = sections
        .iter()
        .flat_map(|section| reference_files(*section, nuspec_namespace))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![PackageReferenceGroup {
        target_framework: any_framework.clone(),
        files,
    }])
}

fn reference_files(node: roxmltree::Node<'_, '_>, nuspec_namespace: Option<&str>) -> Vec<String> {
    node.children()
        .filter(|child| element_named(*child, nuspec_namespace, "reference"))
        .filter_map(|child| child.attribute("file"))
        .filter(|file| !file.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_dependency_group(
    node: roxmltree::Node<'_, '_>,
    nuspec_namespace: Option<&str>,
    any_framework: &NuGetFramework,
) -> Result<PackageDependencyGroup, PackageNuspecParseError> {
    let target_framework = match node.attribute("targetFramework") {
        Some("") => any_framework.clone(),
        Some(raw) => NuGetFramework::parse(raw).map_err(|source| {
            PackageNuspecParseError::InvalidTargetFramework {
                raw: raw.to_owned(),
                source,
            }
        })?,
        None => any_framework.clone(),
    };
    let dependencies = node
        .children()
        .filter(|child| element_named(*child, nuspec_namespace, "dependency"))
        .map(parse_dependency)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PackageDependencyGroup {
        target_framework,
        dependencies,
    })
}

fn parse_dependency(
    node: roxmltree::Node<'_, '_>,
) -> Result<PackageDependency, PackageNuspecParseError> {
    let raw_id = node
        .attribute("id")
        .ok_or(PackageNuspecParseError::MissingDependencyId)?;
    let id = PackageId::parse(raw_id).map_err(|source| {
        PackageNuspecParseError::InvalidDependencyId {
            raw: raw_id.to_owned(),
            source,
        }
    })?;
    let version_range = node.attribute("version").map(str::trim).map_or_else(
        || Ok(all_versions_range()),
        |raw| {
            if raw.is_empty() {
                Ok(all_versions_range())
            } else {
                VersionRange::parse(raw).map_err(|source| {
                    PackageNuspecParseError::InvalidDependencyVersionRange {
                        id: id.as_str().to_owned(),
                        raw: raw.to_owned(),
                        source,
                    }
                })
            }
        },
    )?;

    Ok(PackageDependency {
        id,
        version_range: Some(version_range),
        include: non_empty_attr(node, "include"),
        exclude: non_empty_attr(node, "exclude"),
    })
}

fn all_versions_range() -> VersionRange {
    VersionRange::parse("(, )").expect("literal all-versions range parses")
}

fn non_empty_attr(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.attribute(name)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn element_named(
    node: roxmltree::Node<'_, '_>,
    nuspec_namespace: Option<&str>,
    local_name: &str,
) -> bool {
    node.is_element()
        && node.tag_name().name() == local_name
        && node.tag_name().namespace() == nuspec_namespace
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(s: &str) -> NuGetVersion {
        NuGetVersion::parse(s).expect("version parses")
    }

    fn framework(s: &str) -> NuGetFramework {
        NuGetFramework::parse(s).expect("framework parses")
    }

    #[test]
    fn package_identity_uses_global_packages_folder_spelling() {
        let identity = PackageIdentity::new(
            PackageId::parse("FSharp.Core").unwrap(),
            version("10.1.204+BuiLD"),
        );

        assert_eq!(
            identity.cache_relative_path(),
            PathBuf::from("fsharp.core").join("10.1.204")
        );
    }

    #[test]
    fn package_identity_uses_strict_version_identity() {
        let left =
            PackageIdentity::new(PackageId::parse("FSharp.Core").unwrap(), version("1.0--0"));
        let right =
            PackageIdentity::new(PackageId::parse("fsharp.core").unwrap(), version("1.0-0"));

        assert_eq!(left.version.cmp(&right.version), Ordering::Equal);
        assert_ne!(left, right);
        assert_ne!(left.cache_relative_path(), right.cache_relative_path());

        let mut identities = std::collections::HashSet::new();
        identities.insert(left);
        identities.insert(right);
        assert_eq!(identities.len(), 2);
    }

    #[test]
    fn package_id_rejects_pathlike_shapes() {
        for bad in [
            "",
            " ",
            "../FSharp.Core",
            "FSharp/Core",
            ".hidden",
            "trailing-",
        ] {
            assert!(
                PackageId::parse(bad).is_err(),
                "{bad:?} should not be a package id"
            );
        }
    }

    #[test]
    fn package_id_rejects_adjacent_dot_dash_separators() {
        for bad in ["A..B", "A--B", "A.-B", "A-.B"] {
            assert!(
                PackageId::parse(bad).is_err(),
                "{bad:?} should not be a package id"
            );
        }
    }

    #[test]
    fn package_id_identity_is_case_insensitive() {
        let upper = PackageId::parse("FSharp.Core").unwrap();
        let lower = PackageId::parse("fsharp.core").unwrap();

        assert_eq!(upper, lower);
        assert_eq!(upper.cmp(&lower), Ordering::Equal);

        let mut ids = std::collections::BTreeSet::new();
        ids.insert(upper);
        ids.insert(lower);
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn parses_flat_dependencies_as_any_group() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <dependency id="Alpha" version="[1.0, 2.0)" include="Compile" />
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 1);
        assert!(nuspec.dependency_groups[0].target_framework.is_any());
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0].id,
            PackageId::parse("Alpha").unwrap()
        );
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0]
                .version_range
                .as_ref()
                .unwrap()
                .to_normalized_string(),
            "[1.0.0, 2.0.0)"
        );
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0]
                .include
                .as_deref(),
            Some("Compile")
        );
    }

    #[test]
    fn parses_framework_groups() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="net8.0">
        <dependency id="Beta.Core" version="1.2.3" exclude="Build,Analyzers" />
      </group>
      <group targetFramework=".NETStandard2.0" />
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 2);
        assert_eq!(
            nuspec.dependency_groups[0]
                .target_framework
                .short_folder_name()
                .as_deref(),
            Some("net8.0")
        );
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0]
                .exclude
                .as_deref(),
            Some("Build,Analyzers")
        );
        assert_eq!(
            nuspec.dependency_groups[1]
                .target_framework
                .short_folder_name()
                .as_deref(),
            Some("netstandard2.0")
        );
        assert!(nuspec.dependency_groups[1].dependencies.is_empty());
    }

    #[test]
    fn preserves_duplicate_equivalent_dependency_groups() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="net8.0">
        <dependency id="First" version="1.0" />
      </group>
      <group targetFramework=".NETCoreApp,Version=v8.0">
        <dependency id="Second" version="2.0" />
      </group>
      <group targetFramework="net8.0">
        <dependency id="Third" version="3.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 3);
        let ids = nuspec
            .dependency_groups
            .iter()
            .map(|group| group.dependencies[0].id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["First", "Second", "Third"]);
    }

    #[test]
    fn selects_exact_dependency_group_over_compatible_fallbacks() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="netstandard2.0">
        <dependency id="Fallback" version="1.0" />
      </group>
      <group targetFramework="net8.0" />
      <group>
        <dependency id="Any" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("net8.0")),
            Some(1)
        );
        assert!(
            nuspec
                .select_dependency_group(&framework("net8.0"))
                .unwrap()
                .dependencies
                .is_empty()
        );
    }

    #[test]
    fn selects_compatible_dependency_group_when_exact_group_is_absent() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="netstandard2.0">
        <dependency id="Fallback" version="1.0" />
      </group>
      <group targetFramework="net472">
        <dependency id="FrameworkOnly" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        let selected = nuspec
            .select_dependency_group(&framework("net8.0"))
            .expect("netstandard is compatible with net8.0");
        assert_eq!(selected.dependencies[0].id.as_str(), "Fallback");
    }

    #[test]
    fn selects_nuget_nearest_dependency_group_for_heterogeneous_legacy_frameworks() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="netstandard1.2">
        <dependency id="NetStandard" version="1.0" />
      </group>
      <group targetFramework="win8">
        <dependency id="Windows" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("uap10.0")),
            Some(1)
        );
        let selected = nuspec
            .select_dependency_group(&framework("uap10.0"))
            .expect("win8 is compatible with uap10.0");
        assert_eq!(selected.dependencies[0].id.as_str(), "Windows");
    }

    #[test]
    fn selects_legacy_netcore_5_dependency_groups_before_any_fallback() {
        for (target_framework, dependency_id) in [
            ("netstandard1.0", "NetStandard"),
            ("dotnet5.0", "DotNet"),
            ("win8", "Windows"),
        ] {
            let nuspec = parse_nuspec(&format!(
                r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="{target_framework}">
        <dependency id="{dependency_id}" version="1.0" />
      </group>
      <group>
        <dependency id="Any" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#
            ))
            .unwrap();

            let selected = nuspec
                .select_dependency_group(&framework("netcore50"))
                .expect("NuGet selects the legacy-compatible dependency group");
            assert_eq!(selected.dependencies[0].id.as_str(), dependency_id);
        }
    }

    #[test]
    fn selects_any_dependency_group_when_no_specific_group_matches() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group>
        <dependency id="Any" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        let selected = nuspec
            .select_dependency_group(&framework("net8.0"))
            .expect("Any applies to specific project frameworks");
        assert_eq!(selected.dependencies[0].id.as_str(), "Any");
    }

    #[test]
    fn selects_unscoped_dependency_group_for_any_project_framework() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group>
        <dependency id="Any" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("any")),
            Some(0)
        );
    }

    #[test]
    fn selects_unscoped_dependency_group_for_unsupported_project_framework() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group>
        <dependency id="Any" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("unsupported")),
            Some(0)
        );
    }

    #[test]
    fn selects_exact_agnostic_dependency_group_for_agnostic_project_framework() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group>
        <dependency id="Any" version="1.0" />
      </group>
      <group targetFramework="agnostic">
        <dependency id="Agnostic" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("agnostic")),
            Some(1)
        );
    }

    #[test]
    fn selects_first_duplicate_equivalent_dependency_group() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="net8.0">
        <dependency id="First" version="1.0" />
      </group>
      <group targetFramework=".NETCoreApp,Version=v8.0">
        <dependency id="Second" version="2.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("net8.0")),
            Some(0)
        );
    }

    #[test]
    fn selects_no_dependency_group_when_none_is_compatible() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="net472">
        <dependency id="FrameworkOnly" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(
            nuspec.select_dependency_group_index(&framework("net8.0")),
            None
        );
    }

    #[test]
    fn parses_empty_dependency_group_target_framework_as_any() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework="">
        <dependency id="Gamma" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 1);
        assert!(nuspec.dependency_groups[0].target_framework.is_any());
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0].id,
            PackageId::parse("Gamma").unwrap()
        );
    }

    #[test]
    fn parses_whitespace_dependency_group_target_framework_as_unsupported() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <group targetFramework=" ">
        <dependency id="Delta" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 1);
        assert!(
            nuspec.dependency_groups[0]
                .target_framework
                .is_unsupported()
        );
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0].id,
            PackageId::parse("Delta").unwrap()
        );
    }

    #[test]
    fn parses_unversioned_dependencies_as_all_versions_range() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <dependency id="MissingVersion" />
      <dependency id="EmptyVersion" version="" />
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 1);
        let dependencies = &nuspec.dependency_groups[0].dependencies;
        assert_eq!(dependencies.len(), 2);
        assert_eq!(
            dependencies[0]
                .version_range
                .as_ref()
                .map(VersionRange::to_normalized_string)
                .as_deref(),
            Some("(, )")
        );
        assert_eq!(
            dependencies[1]
                .version_range
                .as_ref()
                .map(VersionRange::to_normalized_string)
                .as_deref(),
            Some("(, )")
        );
    }

    #[test]
    fn accumulates_flat_dependencies_from_split_sections() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <dependency id="First" version="1.0" />
    </dependencies>
    <dependencies>
      <dependency id="Second" version="2.0" />
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 1);
        assert!(nuspec.dependency_groups[0].target_framework.is_any());
        let ids = nuspec.dependency_groups[0]
            .dependencies
            .iter()
            .map(|dependency| dependency.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["First", "Second"]);
    }

    #[test]
    fn grouped_dependencies_in_split_sections_ignore_all_direct_dependencies() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <dependency id="IgnoredFirst" version="1.0" />
    </dependencies>
    <dependencies>
      <group targetFramework="net6.0">
        <dependency id="FirstGroup" version="2.0" />
      </group>
    </dependencies>
    <dependencies>
      <dependency id="IgnoredSecond" version="3.0" />
      <group targetFramework="net8.0">
        <dependency id="SecondGroup" version="4.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 2);
        assert_eq!(
            nuspec.dependency_groups[0]
                .target_framework
                .short_folder_name()
                .as_deref(),
            Some("net6.0")
        );
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0].id,
            PackageId::parse("FirstGroup").unwrap()
        );
        assert_eq!(
            nuspec.dependency_groups[1]
                .target_framework
                .short_folder_name()
                .as_deref(),
            Some("net8.0")
        );
        assert_eq!(
            nuspec.dependency_groups[1].dependencies[0].id,
            PackageId::parse("SecondGroup").unwrap()
        );
    }

    #[test]
    fn grouped_dependencies_ignore_direct_siblings() {
        let nuspec = parse_nuspec(
            r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <dependencies>
      <dependency id="Ignored" version="1.0" />
      <group>
        <dependency id="AnyGroup" version="[3.0]" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
        )
        .unwrap();

        assert_eq!(nuspec.dependency_groups.len(), 1);
        assert!(nuspec.dependency_groups[0].target_framework.is_any());
        assert_eq!(
            nuspec.dependency_groups[0].dependencies[0].id,
            PackageId::parse("AnyGroup").unwrap()
        );
    }
}
