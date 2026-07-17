//! Compile-asset selection: which assemblies in a resolved package a project
//! targeting some framework actually compiles against.
//!
//! This is NuGet's *content model* — `ManagedCodeConventions` +
//! `ContentItemCollection.FindBestItemGroup` — restricted to the compile
//! patterns, followed by the two nuspec-driven adjustments
//! `LockFileUtils.CreateLockFileTargetLibrary` makes before writing
//! `CompileTimeAssemblies` into `project.assets.json`. In outline:
//!
//! 1. Every package file that is not in the package root is an *asset*.
//! 2. Assets are grouped by target framework under two pattern sets:
//!    `ref/{tfm}/…` and `lib/{tfm}/…` (plus the pre-TFM `lib/{assembly}`
//!    shape, whose framework is `.NETFramework,Version=v0.0`).
//! 3. **Ref takes precedence over lib**: the ref groups are searched first,
//!    and lib is consulted *only* if no ref group is compatible with the
//!    project at all. A compatible-but-assembly-less ref group (a `ref/net8.0/`
//!    holding nothing but a readme) therefore yields *no* compile assets and
//!    suppresses `lib/` entirely. This is not a quirk we can round off: it is
//!    how `ref`-only packages deliberately hide their `lib/` implementation.
//! 4. Within a pattern set the compatible group nearest the project framework
//!    wins, and its assemblies (`.dll`/`.exe`/`.winmd`, or the `_._` empty-folder
//!    placeholder) are the compile assets.
//! 5. The nuspec's `<references>` allow-list, if present, then removes any
//!    `lib/`-rooted assembly it does not name.
//!
//! # Correctness envelope
//!
//! The house rule is "resolve identically or degrade", so this declines
//! wherever NuGet's answer is not one we can reproduce exactly:
//!
//! - **Project frameworks** outside [`NuGetFramework::is_resolver_project_framework`]
//!   — the compatibility relation is only differentially exact there.
//! - **Ambiguous groups.** NuGet's group selection reads a hash dictionary in
//!   insertion order, so when two compatible groups tie, *NuGet's own answer
//!   depends on the order the package's files were listed in* (a package with
//!   both `lib/net/a.dll` and `lib/b.dll` genuinely selects a different
//!   assembly under a different file order — verified against the real
//!   `ContentItemCollection`). We produce an answer only when one group
//!   strictly beats every other, and decline otherwise.
//! - **`lib/contract`.** The legacy contract-package hack (`ApplyLibContract`,
//!   plus the synthetic `ref/any/…` assets `ContentItemCollection.Load` injects
//!   for it) is unmodelled.
//! - **Non-ASCII case in a `<references>` allow-list.** NuGet matches reference
//!   names with `OrdinalIgnoreCase`, a case relation Rust does not have: ours is
//!   either too strict or too loose depending on which you reach for. We decide
//!   the cases where the two relations provably agree, and decline the rest.
//!
//! AssetTargetFallback and RID-specific (`runtimes/…`) selection are out of
//! scope for the whole restore plan, and cannot arise here: the caller passes a
//! plain framework, and no compile pattern matches a `runtimes/` path.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::{InstalledPackage, NuGetFramework, PackageNuspec};

/// NuGet's empty-folder placeholder: a deliberately-empty asset group.
const EMPTY_FOLDER: &str = "_._";

/// The file extensions `ManagedCodeConventions`' `assembly` property accepts.
const ASSEMBLY_EXTENSIONS: &[&str] = &[".dll", ".winmd", ".exe"];

/// The compile assets selected for one package and one project framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileAssets {
    /// Package-relative, `/`-separated asset paths, sorted.
    ///
    /// These are exactly the paths restore would write under the target
    /// library's `compile` key — including a `_._` placeholder, which is a
    /// real entry there. Use [`CompileAssets::assemblies`] for the paths that
    /// are actually assemblies.
    ///
    /// The order is ours, not NuGet's: restore emits them in package-file
    /// order, which for a folder on disk is whatever the filesystem enumerated.
    /// Sorting makes the output a deterministic set.
    pub items: Vec<String>,
}

impl CompileAssets {
    /// The selected paths that name an assembly, dropping `_._` placeholders.
    pub fn assemblies(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .map(String::as_str)
            .filter(|path| !is_placeholder(path))
    }

    /// Resolve [`Self::assemblies`] against the package's directory on disk.
    pub fn assembly_paths(&self, package_dir: &Path) -> Vec<PathBuf> {
        self.assemblies()
            .map(|item| {
                let mut path = package_dir.to_path_buf();
                path.extend(item.split('/'));
                path
            })
            .collect()
    }
}

/// Why compile-asset selection declined to answer.
#[derive(Debug)]
pub enum AssetSelectionDecline {
    UnsupportedProjectFramework {
        framework: Box<NuGetFramework>,
    },
    /// Two compatible asset groups tie, and NuGet's own pick between them
    /// depends on the order the package's files happened to be enumerated in.
    AmbiguousAssetGroup {
        left: Box<NuGetFramework>,
        right: Box<NuGetFramework>,
    },
    /// The package uses the legacy `lib/contract` shape, which restore rewrites
    /// through a compat path we do not model.
    LibContract {
        path: String,
    },
    /// A nuspec `<references>` entry and an asset's file name differ only by
    /// non-ASCII case, where NuGet's `OrdinalIgnoreCase` and Rust's case folding
    /// are not the same relation — so whether restore keeps the asset is exactly
    /// what we cannot determine.
    UndecidableReferenceName {
        reference: String,
        asset: String,
    },
    /// A package file name is not UTF-8, so we cannot match it against the
    /// content-model patterns the way NuGet (which works in UTF-16) does.
    NonUtf8PackageFile {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for AssetSelectionDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetSelectionDecline::UnsupportedProjectFramework { framework } => write!(
                f,
                "target framework {:?} is outside the asset-selection project framework envelope",
                framework.short_folder_name()
            ),
            AssetSelectionDecline::AmbiguousAssetGroup { left, right } => write!(
                f,
                "asset groups {:?} and {:?} tie for this project framework, so NuGet's own \
                 selection depends on package file order",
                left.short_folder_name(),
                right.short_folder_name()
            ),
            AssetSelectionDecline::LibContract { path } => {
                write!(f, "package uses the legacy lib/contract shape ({path})")
            }
            AssetSelectionDecline::UndecidableReferenceName { reference, asset } => write!(
                f,
                "nuspec reference {reference:?} and asset {asset:?} differ only by non-ASCII \
                 case, which NuGet's OrdinalIgnoreCase and Rust's case folding do not agree on"
            ),
            AssetSelectionDecline::NonUtf8PackageFile { path } => {
                write!(f, "package file name is not UTF-8: {}", path.display())
            }
            AssetSelectionDecline::Io { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
        }
    }
}

impl Error for AssetSelectionDecline {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            AssetSelectionDecline::Io { source, .. } => Some(source),
            AssetSelectionDecline::UnsupportedProjectFramework { .. }
            | AssetSelectionDecline::AmbiguousAssetGroup { .. }
            | AssetSelectionDecline::LibContract { .. }
            | AssetSelectionDecline::UndecidableReferenceName { .. }
            | AssetSelectionDecline::NonUtf8PackageFile { .. } => None,
        }
    }
}

/// Select the compile assets of an installed package for a project framework.
pub fn select_installed_compile_assets(
    package: &InstalledPackage,
    project: &NuGetFramework,
) -> Result<CompileAssets, AssetSelectionDecline> {
    let files = list_package_files(&package.paths.package_dir)?;
    select_compile_assets(&files, &package.nuspec, project)
}

/// Every file under `package_dir`, as package-relative `/`-separated paths.
///
/// This is the view `LocalPackageInfo.Files` gives restore: the package's own
/// files, root-level bookkeeping (`.nuspec`, `.nupkg.metadata`, …) included —
/// the content model discards root-level paths itself.
pub fn list_package_files(package_dir: &Path) -> Result<Vec<String>, AssetSelectionDecline> {
    let mut files = Vec::new();
    let mut stack = vec![(package_dir.to_path_buf(), String::new())];

    while let Some((dir, prefix)) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|source| AssetSelectionDecline::Io {
            path: dir.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| AssetSelectionDecline::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| AssetSelectionDecline::Io {
                    path: path.clone(),
                    source,
                })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err(AssetSelectionDecline::NonUtf8PackageFile { path });
            };
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };

            if file_type.is_dir() {
                stack.push((path, relative));
            } else {
                files.push(relative);
            }
        }
    }

    files.sort();
    Ok(files)
}

/// Select compile assets from an explicit package file list.
///
/// `files` are package-relative and `/`-separated. Pure: no IO, no environment.
pub fn select_compile_assets(
    files: &[String],
    nuspec: &PackageNuspec,
    project: &NuGetFramework,
) -> Result<CompileAssets, AssetSelectionDecline> {
    if !project.is_resolver_project_framework() {
        return Err(AssetSelectionDecline::UnsupportedProjectFramework {
            framework: Box::new(project.clone()),
        });
    }

    // `ContentItemCollection.Load` discards root-level files, and diverts
    // `lib/contract` packages down a compat path we decline on.
    let mut assets = Vec::new();
    for file in files {
        if !is_allowed_library_file(file) {
            continue;
        }
        // NuGet's own trigger is this bare ordinal prefix — `lib/contracts/`
        // and `lib/contract/` alike set `HasContract` and get the synthetic
        // `ref/any…` assets — so the decline tracks it exactly.
        if file.starts_with("lib/contract") {
            return Err(AssetSelectionDecline::LibContract { path: file.clone() });
        }
        if is_valid_asset(file) {
            assets.push(file.as_str());
        }
    }

    let mut items = match find_best_group(&assets, &COMPILE_REF_ASSEMBLIES, project)? {
        Some(group) => group_items(&assets, &group, &COMPILE_REF_ASSEMBLIES),
        None => match find_best_group(&assets, &COMPILE_LIB_ASSEMBLIES, project)? {
            Some(group) => group_items(&assets, &group, &COMPILE_LIB_ASSEMBLIES),
            None => Vec::new(),
        },
    };

    apply_reference_filter(&mut items, nuspec, project)?;

    items.sort_unstable();
    Ok(CompileAssets {
        items: items.into_iter().map(str::to_owned).collect(),
    })
}

/// `LockFileUtils.ApplyReferenceFilter`: a nuspec `<references>` allow-list
/// removes every `lib/`-rooted compile asset it does not name. Assets under
/// `ref/` are untouched, and so — because the prefix test is ordinal — is a
/// `LIB/`-rooted one.
fn apply_reference_filter(
    items: &mut Vec<&str>,
    nuspec: &PackageNuspec,
    project: &NuGetFramework,
) -> Result<(), AssetSelectionDecline> {
    if nuspec.reference_groups.is_empty() {
        return Ok(());
    }
    let candidates = nuspec
        .reference_groups
        .iter()
        .map(|group| group.target_framework.clone())
        .collect::<Vec<_>>();
    let Some(index) = NuGetFramework::get_nearest_reducer(project, &candidates) else {
        return Ok(());
    };
    let allowed = &nuspec.reference_groups[index].files;

    let mut kept = Vec::with_capacity(items.len());
    for item in items.iter() {
        if !item.starts_with("lib/") {
            kept.push(*item);
            continue;
        }
        let name = file_name(item);

        let mut undecidable: Option<&String> = None;
        let mut referenced = false;
        for file in allowed {
            match reference_matches(file, name) {
                Some(true) => {
                    referenced = true;
                    break;
                }
                Some(false) => {}
                None => undecidable = undecidable.or(Some(file)),
            }
        }

        if referenced {
            kept.push(*item);
        } else if let Some(reference) = undecidable {
            // Not matched by anything we can decide, but something we *cannot*
            // decide might match it — so whether NuGet keeps this asset is
            // exactly what we do not know.
            return Err(AssetSelectionDecline::UndecidableReferenceName {
                reference: reference.clone(),
                asset: (*item).to_owned(),
            });
        }
    }

    *items = kept;
    Ok(())
}

/// Does a nuspec `<reference file="…">` name this asset?
///
/// NuGet compares with `StringComparer.OrdinalIgnoreCase`, which folds by
/// **uppercasing** each character with the *simple* (one-to-one) Unicode
/// mapping. Rust has no equivalent, and neither of the obvious substitutes is
/// one — each errs in a different, dangerous direction:
///
/// - `eq_ignore_ascii_case` is too strict: it misses `Ä`/`ä`, which NuGet folds,
///   so it would drop an asset restore keeps.
/// - `to_lowercase` is not even the same *relation*, because case folding is not
///   symmetric: `σ` and `ς` both uppercase to `Σ` — NuGet calls them equal —
///   while their lowercase forms stay distinct.
/// - Rust's `to_uppercase` is the right direction but the *full* mapping, so it
///   equates more than NuGet does (`ß`→`SS`, `ı`→`I`), and would keep an asset
///   restore drops.
///
/// So this answers only where the relations provably agree, and the caller
/// declines otherwise. The soundness of the negative arm is the load-bearing
/// claim: any character with a simple uppercase mapping has that same mapping as
/// its full one (the characters whose full mapping expands, like `ß`, have *no*
/// simple mapping and are left alone), so NuGet-equal implies
/// `to_uppercase`-equal, and therefore `to_uppercase`-different implies
/// NuGet-different.
///
/// - `Some(true)`  — ordinally equal, or an ASCII-only pair equal ignoring case.
/// - `Some(false)` — different even under Rust's *fuller* uppercase folding, so
///   different under NuGet's simpler one too.
/// - `None`        — a non-ASCII pair that Rust's folding equates but NuGet's
///   might not. Undecidable; the caller declines.
///
/// Every arm is pinned against the real comparer in
/// `tests/compile_assets.rs`; real packages combine a `<references>` allow-list
/// with a non-ASCII assembly name essentially never, so the undecidable arm
/// costs nothing in practice.
fn reference_matches(reference: &str, asset_file_name: &str) -> Option<bool> {
    if reference == asset_file_name {
        return Some(true);
    }
    if reference.is_ascii() && asset_file_name.is_ascii() {
        return Some(reference.eq_ignore_ascii_case(asset_file_name));
    }
    if reference.to_uppercase() != asset_file_name.to_uppercase() {
        return Some(false);
    }
    None
}

fn file_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(at) => &path[at + 1..],
        None => path,
    }
}

fn is_placeholder(path: &str) -> bool {
    file_name(path) == EMPTY_FOLDER
}

/// `ContentItemCollection.IsValidAsset`: a path in the package root matches no
/// pattern, so it is not an asset at all.
fn is_valid_asset(path: &str) -> bool {
    path.bytes().skip(1).any(|byte| byte == b'/')
}

/// `LocalPackageFileCache.IsAllowedLibraryFile`: the OPC packaging apparatus a
/// `.nupkg` carries is stripped from the file list *before* the content model
/// ever sees it.
///
/// This is not cosmetic. A `.psmdcp` (the OPC core-properties part) inside a
/// framework folder would otherwise form a real asset *group* — and a
/// compatible, assembly-less `ref/net8.0/` group suppresses `lib/` entirely, so
/// a stray marker file would silently cost the package all of its compile
/// assets. All four tests are ordinal, matching NuGet.
fn is_allowed_library_file(path: &str) -> bool {
    !matches!(path, "_rels/.rels" | "[Content_Types].xml")
        && !path.ends_with('/')
        && !path.ends_with(".psmdcp")
}

// ============================================================================
// The content model
// ============================================================================

/// One token of a pattern, mirroring `ManagedCodeConventions`' property
/// definitions.
///
/// NuGet's `{token?}` spelling ("match only") suppresses *materialising* the
/// matched value into the item's property bag; the token must still match. It
/// carries no flag here because the only property we read back is `tfm`, which
/// is never spelled `{tfm?}` in a compile pattern — so for our purposes a
/// match-only token and a plain one behave identically.
#[derive(Debug, Clone, Copy)]
enum Token {
    /// `{tfm}`: a target-framework folder. Its parser *never* rejects a
    /// non-empty name — an unrecognised one becomes a framework named after
    /// itself — so this always consumes exactly one path segment.
    Tfm,
    /// `{assembly}`: a `.dll`/`.exe`/`.winmd` file name, or the `_._`
    /// placeholder.
    Assembly,
    /// `{any}`: any non-empty text, slashes included.
    Any,
}

#[derive(Debug, Clone, Copy)]
enum Segment {
    /// Matched case-insensitively, as NuGet's `LiteralSegment` does.
    Literal(&'static str),
    /// A token, terminated by `delimiter` (`\0` = "to the end of the path").
    Token { token: Token, delimiter: u8 },
}

/// The properties a matched pattern contributes. Only the framework matters for
/// grouping; `tfm_raw` is carried because it is part of a group's *identity* in
/// NuGet (a `lib/{assembly}` group and a `lib/{tfm}/` group can hold the same
/// framework yet remain two distinct groups, precisely because one has it).
#[derive(Debug, Clone)]
struct Properties {
    tfm: NuGetFramework,
    tfm_raw: Option<&'static str>,
}

impl PartialEq for Properties {
    fn eq(&self, other: &Self) -> bool {
        self.tfm == other.tfm && self.tfm_raw == other.tfm_raw
    }
}

/// `ManagedCodeConventions.NetTFMTable`: the defaults a pre-TFM `lib/foo.dll`
/// pattern applies — `.NETFramework,Version=v0.0`, plus the `tfm_raw` marker.
fn net_tfm_defaults() -> Properties {
    Properties {
        tfm: NuGetFramework::parse_folder("net").expect("`net` is .NETFramework v0.0"),
        tfm_raw: Some("net0"),
    }
}

#[derive(Debug, Clone, Copy)]
struct Pattern {
    segments: &'static [Segment],
    /// Whether the pattern applies `NetTFMTable`'s defaults on a match.
    net_tfm_defaults: bool,
}

/// A `PatternSet`: how assets are grouped, and which of a group's assets are
/// items of it. NuGet keeps these separate for exactly the reason step 3 of the
/// module docs describes — a group can exist while holding no items.
struct PatternSet {
    group_patterns: &'static [Pattern],
    path_patterns: &'static [Pattern],
}

const REF_TFM_GROUP: &[Segment] = &[
    Segment::Literal("ref/"),
    Segment::Token {
        token: Token::Tfm,
        delimiter: b'/',
    },
    Segment::Literal("/"),
    Segment::Token {
        token: Token::Any,
        delimiter: 0,
    },
];
const REF_TFM_PATH: &[Segment] = &[
    Segment::Literal("ref/"),
    Segment::Token {
        token: Token::Tfm,
        delimiter: b'/',
    },
    Segment::Literal("/"),
    Segment::Token {
        token: Token::Assembly,
        delimiter: 0,
    },
];
const LIB_TFM_GROUP: &[Segment] = &[
    Segment::Literal("lib/"),
    Segment::Token {
        token: Token::Tfm,
        delimiter: b'/',
    },
    Segment::Literal("/"),
    Segment::Token {
        token: Token::Any,
        delimiter: 0,
    },
];
const LIB_TFM_PATH: &[Segment] = &[
    Segment::Literal("lib/"),
    Segment::Token {
        token: Token::Tfm,
        delimiter: b'/',
    },
    Segment::Literal("/"),
    Segment::Token {
        token: Token::Assembly,
        delimiter: 0,
    },
];
const LIB_FLAT_GROUP: &[Segment] = &[
    Segment::Literal("lib/"),
    Segment::Token {
        token: Token::Assembly,
        delimiter: 0,
    },
];
const LIB_FLAT_PATH: &[Segment] = &[
    Segment::Literal("lib/"),
    Segment::Token {
        token: Token::Assembly,
        delimiter: 0,
    },
];

/// `ManagedCodePatterns.CompileRefAssemblies`.
const COMPILE_REF_ASSEMBLIES: PatternSet = PatternSet {
    group_patterns: &[Pattern {
        segments: REF_TFM_GROUP,
        net_tfm_defaults: false,
    }],
    path_patterns: &[Pattern {
        segments: REF_TFM_PATH,
        net_tfm_defaults: false,
    }],
};

/// `ManagedCodePatterns.CompileLibAssemblies`.
const COMPILE_LIB_ASSEMBLIES: PatternSet = PatternSet {
    group_patterns: &[
        Pattern {
            segments: LIB_TFM_GROUP,
            net_tfm_defaults: false,
        },
        Pattern {
            segments: LIB_FLAT_GROUP,
            net_tfm_defaults: true,
        },
    ],
    path_patterns: &[
        Pattern {
            segments: LIB_TFM_PATH,
            net_tfm_defaults: false,
        },
        Pattern {
            segments: LIB_FLAT_PATH,
            net_tfm_defaults: true,
        },
    ],
};

/// `PatternExpression.Match`. The subtlety worth naming: on a token whose
/// candidate text does not parse, NuGet does not fail — it extends the
/// candidate *past* the delimiter and tries again. That only bites on paths
/// with empty segments, but it is the reason this is a scan and not a split.
fn match_pattern(path: &str, pattern: &Pattern) -> Option<Properties> {
    let bytes = path.as_bytes();
    let mut tfm: Option<NuGetFramework> = None;
    let mut start = 0usize;

    for segment in pattern.segments {
        match segment {
            Segment::Literal(literal) => {
                let end = start + literal.len();
                if end > bytes.len() || !bytes[start..end].eq_ignore_ascii_case(literal.as_bytes())
                {
                    return None;
                }
                start = end;
            }
            Segment::Token { token, delimiter } => {
                let mut scan = start;
                let mut matched = None;
                while scan != bytes.len() {
                    let delimiter_index = if *delimiter == 0 {
                        bytes.len()
                    } else {
                        match bytes[scan + 1..]
                            .iter()
                            .position(|byte| byte == delimiter)
                            .map(|at| scan + 1 + at)
                        {
                            Some(at) => at,
                            // No delimiter left: the token cannot be terminated.
                            None => break,
                        }
                    };
                    let candidate = &path[start..delimiter_index];
                    if let Some(value) = lookup(*token, candidate) {
                        if let TokenValue::Framework(framework) = value {
                            tfm = Some(framework);
                        }
                        matched = Some(delimiter_index);
                        break;
                    }
                    scan = delimiter_index;
                }
                start = matched?;
            }
        }
    }

    if start != bytes.len() {
        return None;
    }

    if pattern.net_tfm_defaults {
        let defaults = net_tfm_defaults();
        return Some(Properties {
            tfm: tfm.unwrap_or(defaults.tfm),
            tfm_raw: defaults.tfm_raw,
        });
    }
    Some(Properties {
        tfm: tfm?,
        tfm_raw: None,
    })
}

enum TokenValue {
    Framework(NuGetFramework),
    Matched,
}

/// `ContentPropertyDefinition.TryLookup`.
fn lookup(token: Token, text: &str) -> Option<TokenValue> {
    if text.is_empty() {
        return None;
    }
    match token {
        Token::Tfm => Some(TokenValue::Framework(folder_framework(text))),
        Token::Assembly => is_assembly(text).then_some(TokenValue::Matched),
        Token::Any => Some(TokenValue::Matched),
    }
}

/// The `assembly` property: a recognised extension, or the `_._` placeholder.
/// The extension test only applies to a single path segment (`FileExtensions`
/// with `FileExtensionAllowSubFolders` unset), and `_._` is matched ordinally.
fn is_assembly(text: &str) -> bool {
    if text == EMPTY_FOLDER {
        return true;
    }
    if text.contains('/') || text.contains('\\') {
        return false;
    }
    ASSEMBLY_EXTENSIONS.iter().any(|extension| {
        text.len() >= extension.len()
            && text.as_bytes()[text.len() - extension.len()..]
                .eq_ignore_ascii_case(extension.as_bytes())
    })
}

/// `ManagedCodeConventions.TargetFrameworkName_ParserCore`, plus the
/// `DotnetAnyTable` replacement that precedes it.
fn folder_framework(name: &str) -> NuGetFramework {
    // `DotnetAnyTable` rewrites the folder `any` to
    // `FrameworkConstants.CommonFrameworks.DotNet` — `.NETPlatform` at the
    // *empty* version. Note that this is not what the string `dotnet` would
    // *parse* to (that is .NETPlatform 5.0): the table holds the constant. The
    // difference is invisible in a package with only one of them, since both
    // are compatible with exactly the same projects, and decisive in one with
    // both — `lib/any/` and `lib/dotnet5.0/` are then two groups, and the
    // nearer wins.
    //
    // The table is keyed *ordinally*, so only the lowercase spelling is
    // rewritten: `Any` misses it entirely and parses to NuGet's Any framework,
    // which is compatible with every project.
    if name == "any" {
        return NuGetFramework::dot_net_platform_empty_version();
    }
    if let Ok(framework) = NuGetFramework::parse_folder(name)
        && !framework.is_unsupported()
    {
        return framework;
    }
    // NuGet falls back to `ParseFrameworkName` — the long `FrameworkName` form
    // — before giving up. A folder name can legally contain a comma, so this
    // arm is reachable, if only ever adversarially.
    if let Ok(framework) = NuGetFramework::parse(name)
        && !framework.is_unsupported()
    {
        return framework;
    }
    NuGetFramework::unknown_identifier(name)
}

/// An asset group: the framework it targets, and the assets that fell into it.
struct Group {
    properties: Properties,
    assets: Vec<usize>,
}

/// `ContentItemCollection.PopulateItemGroups`, in NuGet's insertion order
/// (asset-major, then group pattern).
fn populate_groups(assets: &[&str], patterns: &PatternSet) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (index, asset) in assets.iter().enumerate() {
        for pattern in patterns.group_patterns {
            let Some(properties) = match_pattern(asset, pattern) else {
                continue;
            };
            match groups
                .iter_mut()
                .find(|group| group.properties == properties)
            {
                Some(group) => group.assets.push(index),
                None => groups.push(Group {
                    properties,
                    assets: vec![index],
                }),
            }
        }
    }
    groups
}

/// `ContentItemCollection.FindBestItemGroup` for a single criteria entry
/// (`{tfm: project, rid: null}`; no compile pattern yields a `rid`, so the
/// null-rid constraint is vacuous).
///
/// Where NuGet keeps whichever group its dictionary happened to hand it first
/// among tied ones, we decline: see the module's correctness envelope.
fn find_best_group(
    assets: &[&str],
    patterns: &PatternSet,
    project: &NuGetFramework,
) -> Result<Option<Group>, AssetSelectionDecline> {
    let groups = populate_groups(assets, patterns);
    let valid = groups
        .into_iter()
        .filter(|group| is_criteria_satisfied(project, &group.properties.tfm))
        .collect::<Vec<_>>();

    let Some(best) = valid.iter().fold(None::<&Group>, |best, group| match best {
        None => Some(group),
        Some(best) => {
            if compare(project, &best.properties.tfm, &group.properties.tfm) > 0 {
                Some(group)
            } else {
                Some(best)
            }
        }
    }) else {
        return Ok(None);
    };

    // The winner must beat every other group *strictly*. NuGet's comparison is
    // not guaranteed transitive, so anything short of a Condorcet winner leaves
    // its answer dependent on the order it enumerated the groups.
    for group in &valid {
        if std::ptr::eq(group, best) {
            continue;
        }
        if compare(project, &best.properties.tfm, &group.properties.tfm) >= 0 {
            return Err(AssetSelectionDecline::AmbiguousAssetGroup {
                left: Box::new(best.properties.tfm.clone()),
                right: Box::new(group.properties.tfm.clone()),
            });
        }
    }

    let best = Group {
        properties: best.properties.clone(),
        assets: best.assets.clone(),
    };
    Ok(Some(best))
}

/// The assets of `group` that are *items* of it: those matching one of the
/// pattern set's path patterns. An asset can belong to a group (it lives under
/// the folder) without being an item of it (it is not an assembly).
fn group_items<'a>(assets: &[&'a str], group: &Group, patterns: &PatternSet) -> Vec<&'a str> {
    group
        .assets
        .iter()
        .filter(|&&index| {
            patterns
                .path_patterns
                .iter()
                .any(|pattern| match_pattern(assets[index], pattern).is_some())
        })
        .map(|&index| assets[index])
        .collect()
}

/// `ManagedCodeConventions.TargetFrameworkName_CompatibilityTest`.
fn is_criteria_satisfied(criteria: &NuGetFramework, available: &NuGetFramework) -> bool {
    // A convention with no framework in the path uses Any, which is compatible
    // with every project — checked before the criteria's own Any-ness, so an
    // Any *criteria* still accepts an Any candidate.
    if available.is_any() {
        return true;
    }
    if criteria.is_any() {
        return false;
    }
    NuGetFramework::is_compatible(criteria, available)
}

/// `ContentPropertyDefinition.Compare`: which of two candidate frameworks the
/// project should prefer. Negative keeps `left`, positive takes `right`, zero
/// is a tie.
///
/// Coverage wins first — a candidate that *accepts* the other is strictly more
/// general and so a worse fit — and `FrameworkReducer.GetNearest` breaks the
/// remaining ties.
fn compare(project: &NuGetFramework, left: &NuGetFramework, right: &NuGetFramework) -> i32 {
    let left_covers_right = is_criteria_satisfied(left, right);
    let right_covers_left = is_criteria_satisfied(right, left);
    if left_covers_right && !right_covers_left {
        return -1;
    }
    if right_covers_left && !left_covers_right {
        return 1;
    }
    if left == right {
        return 0;
    }
    match NuGetFramework::get_nearest_reducer(project, &[left.clone(), right.clone()]) {
        Some(0) => -1,
        Some(1) => 1,
        _ => 0,
    }
}
