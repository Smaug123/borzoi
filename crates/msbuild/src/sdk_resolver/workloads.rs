//! Resolution of the two MSBuild **workload locator SDKs** —
//! `Microsoft.NET.SDK.WorkloadManifestTargetsLocator` and
//! `Microsoft.NET.SDK.WorkloadAutoImportPropsLocator` — against the
//! canonical dotnet on-disk layout, under the "resolve identically or
//! degrade" policy of `docs/completed/sdk-chain-exactness-plan.md` (D2).
//!
//! In MSBuild these names are claimed by the workload MSBuild SDK
//! resolver, which returns a *list* of directories; `<Import
//! Project="P" Sdk="…Locator"/>` then imports `P` relative to each (an
//! empty list means the import cleanly contributes nothing). The
//! algorithm here transcribes the authoritative dotnet/sdk sources —
//! `SdkDirectoryWorkloadManifestProvider.cs` and
//! `CachingWorkloadResolver.cs` (fetched 2026-07-09) — and was
//! additionally ground-truthed against `dotnet msbuild -preprocess` on
//! the nix dotnet 10.0.300 layout (probe record in
//! docs/completed/sdk-chain-exactness-plan.md):
//!
//! * The **primary pass** enumerates every id directory in the *host
//!   feature band*'s `sdk-manifests/{band}` directory — known-list
//!   membership is not required there. When a `userlocal` marker
//!   (`{dotnet}/metadata/workloads/{band}/userlocal`) is present and
//!   the user-local root has an `sdk-manifests` directory, that root
//!   participates too and *shadows* the dotnet root for same-named id
//!   directories. Pack scanning is gated differently: the user root's
//!   `packs` tree is consulted whenever the marker is present and the
//!   directory exists, with or without manifests
//!   (`WorkloadResolver.Create` vs the manifest provider).
//! * Ids from `KnownWorkloadManifests.txt` (fallback filename
//!   `IncludedWorkloadManifests.txt`) that the primary pass did not
//!   find go through **band fallback**: the *dotnet* manifests root
//!   only (never the user-local root), bands strictly below the host
//!   band, highest band wins. An id found nowhere is silently skipped
//!   (`samsung.net.sdk.tizen` on the probed layout).
//! * Inside an id directory, versioned subdirectories containing
//!   `WorkloadManifest.json` take precedence over a flat manifest, and
//!   the **highest version wins**; a directory resolving to no
//!   manifest is skipped. A hardcoded outdated-id set (and the
//!   `workloadsets` folder name) is skipped everywhere.
//! * Import order is known-list line order first, then the remaining
//!   ids ordinal-case-insensitively alphabetical — this matches the
//!   observed `-preprocess` import order.
//! * The targets locator keeps only manifest directories that contain
//!   `WorkloadManifest.targets`; the auto-import locator returns the
//!   `{pack}/Sdk` folders of installed SDK-kind packs whose
//!   `Sdk/AutoImport.props` exists.
//!
//! What still degrades with [`SdkResolveError::UnsupportedLayout`]
//! rather than resolving: a `workloadsets` directory or an
//! install-state pin (both can override manifest versions through
//! machinery we don't model), a `global.json` that engages
//! workload-set selection (`sdk.workloadVersion` — it selects a
//! workload set or fails the real evaluation when the set is
//! missing), a `userlocal` marker when the caller supplied no
//! user-local root (we cannot see what a real build would), a
//! prerelease host band (prerelease band strings follow rules we
//! don't model), workload environment overrides, any manifest
//! version directory name `SdkVersion` cannot parse (we cannot
//! reproduce MSBuild's comparison for those), and any directory or
//! known-list file that exists but cannot be enumerated/read (the
//! real provider throws there; an empty answer would be wrongly
//! certified as exact).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{SdkResolveError, SdkVersion};
use crate::SdkResolution;

pub const WORKLOAD_MANIFEST_TARGETS_LOCATOR: &str =
    "Microsoft.NET.SDK.WorkloadManifestTargetsLocator";
pub const WORKLOAD_AUTO_IMPORT_PROPS_LOCATOR: &str =
    "Microsoft.NET.SDK.WorkloadAutoImportPropsLocator";

/// `SdkDirectoryWorkloadManifestProvider._outdatedManifestIds`: id
/// directories MSBuild skips wherever it encounters them.
const OUTDATED_MANIFEST_IDS: &[&str] = &[
    "microsoft.net.workload.android",
    "microsoft.net.workload.blazorwebassembly",
    "microsoft.net.workload.ios",
    "microsoft.net.workload.maccatalyst",
    "microsoft.net.workload.macos",
    "microsoft.net.workload.tvos",
    "microsoft.net.workload.mono.toolchain",
];

/// Whether `sdk_name` is one of the workload locator SDKs (MSBuild SDK
/// name comparisons are case-insensitive).
pub fn is_workload_locator(sdk_name: &str) -> bool {
    sdk_name.eq_ignore_ascii_case(WORKLOAD_MANIFEST_TARGETS_LOCATOR)
        || sdk_name.eq_ignore_ascii_case(WORKLOAD_AUTO_IMPORT_PROPS_LOCATOR)
}

/// Host-supplied workload context. The msbuild crate never reads the
/// process environment itself (dependency rejection): the shell
/// snapshots what the real resolver would consult and passes it in.
pub struct WorkloadEnvironment<'a> {
    /// The user-local dotnet root (`{DOTNET_CLI_HOME ?? HOME}/.dotnet`),
    /// consulted only when the dotnet root carries a `userlocal`
    /// marker. `None` while the marker is present ⇒ degrade (a real
    /// build would consult manifests we cannot see).
    pub user_dotnet_root: Option<&'a Path>,
    /// True when any workload-resolution environment override is set
    /// (`DOTNETSDK_WORKLOAD_MANIFEST_ROOTS`,
    /// `DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS`,
    /// `DOTNETSDK_WORKLOAD_PACK_ROOTS`): those redirect resolution in ways
    /// we do not model, so their presence degrades.
    pub overrides_present: bool,
    /// True when the `global.json` governing the evaluation engages
    /// workload-set selection (see `GlobalJson::pins_workload_set`):
    /// MSBuild passes the discovered `global.json` path into its
    /// workload manifest provider, and an `sdk.workloadVersion` pin
    /// there selects a workload set — or fails the evaluation when
    /// that set is not installed. Either way the manifest enumeration
    /// modelled here is not what a real build would use, so degrade.
    pub global_json_pins_workload_set: bool,
}

fn unsupported(reason: impl Into<String>) -> SdkResolveError {
    SdkResolveError::UnsupportedLayout {
        reason: reason.into(),
    }
}

/// Enumerate the **subdirectories** of a directory, distinguishing
/// "absent" from "unreadable". Every upstream scan this mirrors uses
/// `Directory.GetDirectories`, which returns only directory entries — so a
/// regular file (`.DS_Store`, a stray manifest-shaped file) is *not* yielded
/// here: otherwise it would be parsed as a feature band (spurious degrade) or
/// treated as an id directory that shadows the real dotnet-root manifest.
///
/// A missing path (or a non-directory) yields an empty listing — the
/// dotnet provider guards its enumerations with `Directory.Exists`,
/// which reports false for those. Any other error (permissions, I/O)
/// degrades: upstream the enumeration would throw and fail the
/// evaluation, so treating the directory as empty could certify a
/// wrong import set as exact. (In the corner cases where CLR
/// `Directory.Exists` itself swallows the error and skips, degrading
/// is merely conservative.) Entry-iteration and per-entry type-probe
/// errors degrade for the same reason. The type probe follows symlinks
/// (`metadata`, not `file_type`), matching `Directory.GetDirectories`,
/// which includes a symlink to a directory and excludes one to a file.
fn read_dir_or_degrade(dir: &Path) -> Result<Vec<std::fs::DirEntry>, SdkResolveError> {
    use std::io::ErrorKind;
    let enumeration_failed =
        |err: std::io::Error| unsupported(format!("cannot enumerate {}: {err}", dir.display()));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(err) => return Err(enumeration_failed(err)),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(enumeration_failed)?;
        // `std::fs::metadata` (target-following), NOT `DirEntry::metadata`,
        // which on Unix is `symlink_metadata` and would report a symlink to a
        // directory as a non-directory. `Directory.GetDirectories` includes a
        // symlinked directory (package-manager and user-local layouts create
        // them), so we must follow the link. A dangling symlink errors here and
        // degrades — conservative, and MSBuild's own later enumeration of it
        // would fail too.
        if std::fs::metadata(entry.path())
            .map_err(enumeration_failed)?
            .is_dir()
        {
            dirs.push(entry);
        }
    }
    Ok(dirs)
}

/// Resolve one locator SDK. `sdk_version_dir` is the host SDK version
/// directory the entry evaluation resolved against — the locators
/// answer relative to the *running* SDK, so the caller passes the same
/// directory its `Microsoft.NET.Sdk` resolution came from.
pub fn resolve_workload_locator(
    sdk_name: &str,
    dotnet_root: &Path,
    sdk_version_dir: &Path,
    env: &WorkloadEnvironment<'_>,
) -> Result<SdkResolution, SdkResolveError> {
    debug_assert!(is_workload_locator(sdk_name));
    if env.overrides_present {
        return Err(unsupported(
            "a workload environment override variable is set",
        ));
    }
    if env.global_json_pins_workload_set {
        return Err(unsupported(
            "global.json pins a workload set (sdk.workloadVersion)",
        ));
    }
    let band = host_feature_band(sdk_version_dir)?;
    let roots = workload_roots(dotnet_root, &band, env)?;
    let manifest_roots = enumerate_manifest_roots(sdk_version_dir, &band, &roots)?;

    if sdk_name.eq_ignore_ascii_case(WORKLOAD_MANIFEST_TARGETS_LOCATOR) {
        // `CachingWorkloadResolver` keeps only the manifest directories
        // that actually contain the targets file; the rest are silently
        // skipped, not an error.
        let with_targets: Vec<PathBuf> = manifest_roots
            .into_iter()
            .filter(|dir| dir.join("WorkloadManifest.targets").is_file())
            .collect();
        return Ok(SdkResolution::Roots(with_targets));
    }

    // Auto-import locator: MSBuild returns the `{pack}/Sdk` folder of
    // every *installed* SDK-kind workload pack whose
    // `Sdk/AutoImport.props` exists. Whatever the precise "installed"
    // semantics, MSBuild can only import files that exist — so when no
    // candidate `AutoImport.props` exists under any pack root, the
    // empty result is exact. Any candidate on disk means a workload may
    // be installed, and we degrade rather than model the
    // manifest→pack→installation-record chain.
    for root in roots.pack_roots() {
        if let Some(found) = find_any_auto_import_props(&root.join("packs"))? {
            return Err(unsupported(format!(
                "a workload pack ships AutoImport.props (a workload may \
                 be installed): {}",
                found.display()
            )));
        }
    }
    Ok(SdkResolution::Roots(Vec::new()))
}

/// The feature band of the host SDK (`10.0.301` → `10.0.300`), as both
/// the directory-name string and the comparable version. Prerelease
/// host SDKs use prerelease-qualified bands whose exact rules we don't
/// model — degrade.
fn host_feature_band(sdk_version_dir: &Path) -> Result<(String, SdkVersion), SdkResolveError> {
    let name = sdk_version_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| unsupported("host SDK directory has a non-UTF-8 name"))?;
    let version = SdkVersion::parse(name)
        .ok_or_else(|| unsupported(format!("host SDK directory is not a version: {name}")))?;
    if version.is_prerelease() {
        return Err(unsupported(format!(
            "prerelease host SDK bands are not modelled: {name}"
        )));
    }
    let band_string = format!(
        "{}.{}.{}",
        version.major(),
        version.minor(),
        version.feature_band() * 100
    );
    let band = SdkVersion::parse(&band_string).expect("bands built from numerics parse");
    Ok((band_string, band))
}

/// The consultation roots. Manifest enumeration and pack scanning gate
/// the user-local root *differently* upstream, so the two views are
/// kept separate:
///
/// * `SdkDirectoryWorkloadManifestProvider` adds the user root to the
///   manifest roots only when `{user}/sdk-manifests` exists;
/// * `WorkloadResolver.Create` adds it to the pack roots whenever the
///   directory itself exists — a user profile can hold installed packs
///   without holding any manifests.
///
/// In the primary manifest pass the user root shadows the dotnet root;
/// band fallback only ever consults the dotnet root.
struct WorkloadRoots {
    /// The user-local root, present only when the `userlocal` marker
    /// gates it in and the directory exists on disk (and it is not the
    /// dotnet root itself — a user-local dotnet install resolves
    /// against itself once).
    user: Option<UserRoot>,
    dotnet_root: PathBuf,
}

struct UserRoot {
    path: PathBuf,
    /// Whether `{path}/sdk-manifests` exists — the extra gate
    /// `SdkDirectoryWorkloadManifestProvider` puts on manifest-root
    /// participation.
    participates_in_manifests: bool,
}

impl WorkloadRoots {
    /// Primary-pass manifest roots in *precedence* order (highest
    /// first): the user-local root when it participates, then the
    /// dotnet root.
    fn manifest_roots(&self) -> impl Iterator<Item = &Path> {
        self.user
            .as_ref()
            .filter(|user| user.participates_in_manifests)
            .map(|user| user.path.as_path())
            .into_iter()
            .chain(std::iter::once(self.dotnet_root.as_path()))
    }

    /// Roots whose `packs` directory the auto-import scan consults,
    /// precedence order.
    fn pack_roots(&self) -> impl Iterator<Item = &Path> {
        self.user
            .as_ref()
            .map(|user| user.path.as_path())
            .into_iter()
            .chain(std::iter::once(self.dotnet_root.as_path()))
    }

    /// Every consulted root, for the conservative degrade sweeps
    /// (install-state pins, MSI markers, workload sets).
    fn all(&self) -> impl Iterator<Item = &Path> {
        self.pack_roots()
    }
}

fn workload_roots(
    dotnet_root: &Path,
    band: &(String, SdkVersion),
    env: &WorkloadEnvironment<'_>,
) -> Result<WorkloadRoots, SdkResolveError> {
    let marker = dotnet_root
        .join("metadata")
        .join("workloads")
        .join(&band.0)
        .join("userlocal");
    let mut user = None;
    // CLR `File.Exists` (what `WorkloadFileBasedInstall.IsUserLocal` uses)
    // is false for a directory or a symlink to one, so `is_file` — not
    // `exists` — faithfully transcribes the marker check. A malformed
    // `userlocal` *directory* must read as "no marker" (global install),
    // not as a present marker that would consult/shadow the user root.
    if marker.is_file() {
        match env.user_dotnet_root {
            Some(user_root) => {
                if user_root != dotnet_root && user_root.is_dir() {
                    user = Some(UserRoot {
                        path: user_root.to_path_buf(),
                        participates_in_manifests: user_root.join("sdk-manifests").is_dir(),
                    });
                }
            }
            None => {
                return Err(unsupported(
                    "userlocal workload marker present but no user-local \
                     root was supplied",
                ));
            }
        }
    }
    let roots = WorkloadRoots {
        user,
        dotnet_root: dotnet_root.to_path_buf(),
    };
    for root in roots.all() {
        let workloads_metadata = root.join("metadata").join("workloads");
        // An MSI-based install keeps its install state under
        // ProgramData, outside anything we are handed — degrade on the
        // marker (`GetWorkloadInstallType` in dotnet/sdk).
        let msi_marker = workloads_metadata
            .join(&band.0)
            .join("installertype")
            .join("msi");
        // `GetWorkloadInstallType` tests this marker with CLR `File.Exists`
        // too, so `is_file` (not `exists`): a directory named `msi` is not
        // the marker.
        if msi_marker.is_file() {
            return Err(unsupported(format!(
                "MSI-based workload install: {}",
                msi_marker.display()
            )));
        }
        // A file-based install-state pin overrides manifest versions;
        // degrade. The real path carries a process-architecture segment
        // (`metadata/workloads/{arch}/{band}/InstallState/default.json`,
        // `WorkloadInstallType.GetInstallStateFolder`); sweep every
        // arch directory rather than guess which architecture the real
        // build's dotnet process would report, and keep the older
        // arch-less location as a candidate too.
        let mut candidates = vec![
            workloads_metadata
                .join(&band.0)
                .join("InstallState")
                .join("default.json"),
        ];
        for entry in read_dir_or_degrade(&workloads_metadata)? {
            candidates.push(
                entry
                    .path()
                    .join(&band.0)
                    .join("InstallState")
                    .join("default.json"),
            );
        }
        for install_state in candidates {
            if install_state.is_file() {
                return Err(unsupported(format!(
                    "workload install-state pin present: {}",
                    install_state.display()
                )));
            }
        }
    }
    Ok(roots)
}

/// One resolved manifest directory plus the id MSBuild orders it by.
struct ResolvedManifest {
    id: String,
    directory: PathBuf,
}

/// Enumerate the workload manifest directories exactly as
/// `SdkDirectoryWorkloadManifestProvider.GetManifests` does: the
/// primary pass over the host band directory of every root (user root
/// shadowing dotnet), band fallback against the dotnet root for known
/// ids the primary pass missed, then known-list order followed by
/// alphabetical.
fn enumerate_manifest_roots(
    sdk_version_dir: &Path,
    band: &(String, SdkVersion),
    roots: &WorkloadRoots,
) -> Result<Vec<PathBuf>, SdkResolveError> {
    // Known-id list: `KnownWorkloadManifests.txt`, falling back to the
    // older `IncludedWorkloadManifests.txt`; a missing file just means
    // no fallback pass and no preferential ordering. A file that
    // exists but cannot be read degrades — upstream `File.ReadAllLines`
    // would throw and fail the evaluation, and treating the list as
    // absent would silently change ordering and fallback.
    let mut known_ids: Option<Vec<String>> = None;
    for name in [
        "KnownWorkloadManifests.txt",
        "IncludedWorkloadManifests.txt",
    ] {
        let path = sdk_version_dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                // Upstream reads this with `File.ReadAllLines`, whose
                // BOM-detecting `StreamReader` consumes a leading UTF-8 BOM.
                // Rust's `read_to_string` keeps it, and `str::trim` does *not*
                // remove U+FEFF (no longer White_Space), so without this the
                // first id would carry the BOM and fail its lookup / safe-id
                // check, degrading a valid layout.
                let contents = contents.strip_prefix('\u{feff}').unwrap_or(&contents);
                known_ids = Some(
                    contents
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect(),
                );
                break;
            }
            // `IsADirectory`: upstream guards with `File.Exists`, which
            // is false for a directory, so that shape is "absent" too.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory
                ) => {}
            Err(err) => {
                return Err(unsupported(format!(
                    "cannot read {}: {err}",
                    path.display()
                )));
            }
        }
    }

    // Workload sets can pin manifest versions through selection
    // machinery (global.json workload versions, install state) we don't
    // model; the presence of any `workloadsets` directory under any
    // participating root degrades, conservatively across every band.
    for root in roots.all() {
        for entry in read_dir_or_degrade(&root.join("sdk-manifests"))? {
            let sets = entry.path().join("workloadsets");
            if sets.is_dir() {
                return Err(unsupported(format!(
                    "workload set present: {}",
                    sets.display()
                )));
            }
        }
    }

    // Primary pass: every id directory in `{root}/sdk-manifests/{band}`,
    // shadowed **by directory name** — the highest-precedence root wins by name,
    // exactly as `SdkDirectoryWorkloadManifestProvider.GetManifests` overwrites
    // its `directoriesWithManifests` dictionary (roots processed via `.Reverse()`
    // so the user-local root, added last, wins) and only *then* probes each
    // winning directory. Crucially the shadow is by name, *before* validating
    // contents: a winner that holds no resolvable manifest is dropped and is
    // *not* backfilled from a lower-precedence root's same-band copy (MSBuild's
    // `ProbeDirectory` simply skips it). It instead becomes eligible for the
    // dotnet-root band fallback below — which searches only bands *below* the
    // host band, never the shadowed same-band dotnet copy.
    let mut winners: HashMap<String, (String, PathBuf)> = HashMap::new();
    for root in roots.manifest_roots() {
        let band_dir = root.join("sdk-manifests").join(&band.0);
        for entry in read_dir_or_degrade(&band_dir)? {
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // First insertion wins: `manifest_roots()` yields highest precedence
            // first, so a later (lower-precedence) root never displaces a name a
            // higher root already claimed — resolvable or not.
            winners
                .entry(id.to_ascii_lowercase())
                .or_insert_with(|| (id, entry.path()));
        }
    }
    let mut found: HashMap<String, ResolvedManifest> = HashMap::new();
    for (key, (id, dir)) in winners {
        if is_skipped_manifest_id(&id) {
            continue;
        }
        if let Some(directory) = resolve_manifest_directory(&dir)? {
            found.insert(key, ResolvedManifest { id, directory });
        }
    }

    // Band fallback for known ids the primary pass missed: dotnet root
    // only, bands strictly below the host band, highest matching band
    // wins (whether or not it resolves to a manifest).
    //
    // MSBuild calls `FallbackForMissingManifest` only for ids that are actually
    // missing, and it is *that* call which enumerates the band directories (and
    // can throw on a stray non-version name). So the enumeration — and its
    // degrade — must be gated on there being a missing id: a fully-resolved set
    // never triggers it, even if a stray directory happens to sit alongside.
    let any_missing = known_ids
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .any(|id| !found.contains_key(&id.to_ascii_lowercase()) && !is_skipped_manifest_id(id));
    if let Some(known) = &known_ids
        && any_missing
    {
        let manifests_dir = roots.dotnet_root.join("sdk-manifests");
        let mut fallback_bands: Vec<(SdkVersion, PathBuf)> = Vec::new();
        for entry in read_dir_or_degrade(&manifests_dir)? {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(version) = SdkVersion::parse(&name) else {
                // `FallbackForMissingManifest` constructs `new SdkFeatureBand`
                // for *every* directory under `sdk-manifests` — with no
                // try/catch — so a name that isn't a valid feature band throws a
                // `FormatException` and fails the real resolution. Silently
                // skipping it here would return a *supposedly exact* locator
                // result for a layout MSBuild rejects. `SdkVersion::parse`
                // accepts the same prerelease/non-canonical version spellings
                // `SdkFeatureBand` does, so a parse failure is exactly the
                // throwing case: degrade rather than guess.
                return Err(unsupported(format!(
                    "non-version directory under sdk-manifests would fail \
                     feature-band enumeration: {name}"
                )));
            };
            if version < band.1 {
                fallback_bands.push((version, entry.path()));
            }
        }
        fallback_bands.sort_by(|a, b| b.0.cmp(&a.0));
        for id in known {
            let key = id.to_ascii_lowercase();
            if found.contains_key(&key) || is_skipped_manifest_id(id) {
                continue;
            }
            if !is_safe_manifest_id(id) {
                return Err(unsupported(format!(
                    "manifest id with unsafe path characters: {id}"
                )));
            }
            for (_, band_dir) in &fallback_bands {
                let id_dir = band_dir.join(id);
                if !id_dir.is_dir() {
                    continue;
                }
                // `FallbackForMissingManifest` filters unresolvable
                // candidates *before* picking the highest band, so a
                // band whose id directory holds no manifest is passed
                // over in favour of an older band that resolves.
                if let Some(directory) = resolve_manifest_directory(&id_dir)? {
                    found.insert(
                        key.clone(),
                        ResolvedManifest {
                            id: id.clone(),
                            directory,
                        },
                    );
                    break;
                }
            }
        }
    }

    // Stable order: known-list line order first, then the rest
    // alphabetically (ordinal, case-insensitive).
    let known_order: HashMap<String, usize> = known_ids
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(index, id)| (id.to_ascii_lowercase(), index))
        .collect();
    let mut manifests: Vec<ResolvedManifest> = found.into_values().collect();
    manifests.sort_by(|a, b| {
        let ka = known_order.get(&a.id.to_ascii_lowercase());
        let kb = known_order.get(&b.id.to_ascii_lowercase());
        match (ka, kb) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.id.to_ascii_lowercase().cmp(&b.id.to_ascii_lowercase()),
        }
    });
    Ok(manifests.into_iter().map(|m| m.directory).collect())
}

/// `SdkDirectoryWorkloadManifestProvider.ResolveManifestDirectory`:
/// versioned subdirectories containing `WorkloadManifest.json` beat a
/// flat manifest, highest version wins; nothing resolvable means the
/// directory is skipped (`Ok(None)`), not an error. A version directory
/// name our `SdkVersion` cannot parse degrades — we cannot reproduce
/// MSBuild's comparison for it.
fn resolve_manifest_directory(id_dir: &Path) -> Result<Option<PathBuf>, SdkResolveError> {
    let mut best: Option<(SdkVersion, PathBuf)> = None;
    for entry in read_dir_or_degrade(id_dir)? {
        let candidate = entry.path();
        if !candidate.join("WorkloadManifest.json").is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(version) = SdkVersion::parse(&name) else {
            return Err(unsupported(format!(
                "unparseable workload manifest version directory: {}",
                candidate.display()
            )));
        };
        if best.as_ref().is_none_or(|(v, _)| version > *v) {
            best = Some((version, candidate));
        }
    }
    if let Some((_, directory)) = best {
        return Ok(Some(directory));
    }
    if id_dir.join("WorkloadManifest.json").is_file() {
        return Ok(Some(id_dir.to_path_buf()));
    }
    Ok(None)
}

/// Ids MSBuild skips wherever encountered: the hardcoded outdated set
/// and the `workloadsets` folder name.
fn is_skipped_manifest_id(id: &str) -> bool {
    id.eq_ignore_ascii_case("workloadsets")
        || OUTDATED_MANIFEST_IDS
            .iter()
            .any(|outdated| id.eq_ignore_ascii_case(outdated))
}

/// Manifest ids come from a file we read off disk and are spliced into
/// paths; hold them to the same containment rules as SDK names.
fn is_safe_manifest_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// The first `packs/*/*/Sdk/AutoImport.props` (the layout
/// `CachingWorkloadResolver` imports from) — or a bare
/// `packs/*/*/AutoImport.props`, held conservatively as a candidate
/// too — under `packs_dir`, if any. An unreadable directory degrades
/// (`Err`): treating it as empty would certify the zero-import answer
/// off evidence we could not actually collect.
fn find_any_auto_import_props(packs_dir: &Path) -> Result<Option<PathBuf>, SdkResolveError> {
    for pack in read_dir_or_degrade(packs_dir)? {
        for version in read_dir_or_degrade(&pack.path())? {
            for candidate in [
                version.path().join("Sdk").join("AutoImport.props"),
                version.path().join("AutoImport.props"),
            ] {
                if candidate.is_file() {
                    return Ok(Some(candidate));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
