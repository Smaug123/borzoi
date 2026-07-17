pub mod closure;
pub mod enumerate;
pub mod error;
pub mod framework;
pub mod raw;
pub mod tfm;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub use closure::transitive_project_tfms;
pub use enumerate::{Reference, enumerate_one, enumerate_one_for_tfm};
pub use error::ProjectAssetsError;
pub use framework::resolve_framework;
pub use raw::{RawAssets, RawLibrary, RawProject, RawProjectFramework, RawTargetEntry};

use crate::project_assets::enumerate::lookup_target_for_tfm;

/// The result of recursively resolving a project's compile-time references.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedAssemblies {
    /// Absolute paths to compile-time DLLs from NuGet packages, deduplicated.
    pub package_dlls: Vec<PathBuf>,
    /// Absolute paths to reference assemblies from shared frameworks.
    pub framework_dlls: Vec<PathBuf>,
    /// The producer TFM (short form, e.g. `net10.0`) NuGet's restore selected
    /// for each project reference, keyed by the *canonicalised* project path.
    /// The key set doubles as the assets-recorded project-reference set.
    /// Sourced from each project-kind target entry's `framework` field (see
    /// [`enumerate::Reference::ProjectRef`]). The C#-sidecar consumer needs it
    /// to build a `.csproj` reference under the TFM its consumer selected; the
    /// F# consumer ignores it (a built output DLL is TFM-invariant in the common
    /// case). A ref whose canonicalisation failed is absent.
    pub project_ref_tfms: BTreeMap<PathBuf, String>,
    /// The producer's **evaluated** output assembly simple name per project
    /// reference, keyed like [`Self::project_ref_tfms`]. Sourced from the
    /// project-kind target entry's `compile` asset (see
    /// [`enumerate::Reference::ProjectRef`]); NuGet evaluated the producer to
    /// write it, so `<AssemblyName>` overrides are already applied. The F#
    /// output-DLL locator uses it to find renamed outputs; a ref absent here
    /// (no usable compile asset) degrades to the project-file-stem assumption.
    pub project_ref_assembly_names: BTreeMap<PathBuf, String>,
}

/// Walk a `project.assets.json` and everything it transitively references.
///
/// Reads `root_assets_path`, then for every project reference inside it
/// follows the link to that project's own `obj/project.assets.json`,
/// accumulating compile-time DLLs and framework-pack DLLs as it goes.
/// Cycles are broken by visiting each assets file at most once
/// (compared by canonicalised path).
///
/// `dotnet_root` is the directory containing `packs/{name}.Ref/...` for
/// shared framework reference assemblies — usually `/usr/local/share/dotnet`
/// on macOS or `$DOTNET_ROOT` if set. The caller passes it in explicitly;
/// this layer does no env-var sniffing.
pub fn resolve_assemblies(
    root_assets_path: &Path,
    dotnet_root: &Path,
) -> Result<ResolvedAssemblies, ProjectAssetsError> {
    resolve_assemblies_impl(root_assets_path, dotnet_root, true, None)
}

/// [`resolve_assemblies`] minus the transitive `<ProjectReference>` walk.
/// Reads `root_assets_path` alone — accumulating its package and framework
/// DLLs, recording project-ref producer TFMs in
/// [`ResolvedAssemblies::project_ref_tfms`] without descending into them.
///
/// Use when the caller doesn't need the transitive closure *and* would
/// rather degrade gracefully under partial restores: a referenced sibling
/// project that hasn't been restored makes [`resolve_assemblies`] fail with
/// [`ProjectAssetsError::MissingTransitiveAssets`], whereas this function
/// returns the root project's own DLLs regardless. The LSP semantic layer
/// is the canonical caller — it sources project-reference *edges* from the
/// parsed graph (assets contribute only artifacts) and needs the
/// package/framework set to stay available even when a sibling project is
/// mid-restore.
pub fn resolve_assemblies_root_only(
    root_assets_path: &Path,
    dotnet_root: &Path,
) -> Result<ResolvedAssemblies, ProjectAssetsError> {
    resolve_assemblies_impl(root_assets_path, dotnet_root, false, None)
}

/// [`resolve_assemblies_root_only`] for a caller-chosen TFM (fsproj 3.3c,
/// plan E3): the assets target is selected by `tfm` (via
/// [`enumerate::enumerate_one_for_tfm`], alias fallback included) instead of
/// requiring the restore to be single-target — the multi-TFM entry case. A
/// single-TFM restore whose one target matches `tfm` resolves identically to
/// [`resolve_assemblies_root_only`]; a restore *without* a target for `tfm`
/// errors (plan E6: the caller degrades to an empty env rather than serving a
/// different TFM's assemblies against a parse evaluated under `tfm`).
pub fn resolve_assemblies_for_tfm(
    root_assets_path: &Path,
    dotnet_root: &Path,
    tfm: &str,
) -> Result<ResolvedAssemblies, ProjectAssetsError> {
    resolve_assemblies_impl(root_assets_path, dotnet_root, false, Some(tfm))
}

fn resolve_assemblies_impl(
    root_assets_path: &Path,
    dotnet_root: &Path,
    recurse: bool,
    root_tfm: Option<&str>,
) -> Result<ResolvedAssemblies, ProjectAssetsError> {
    // A chosen TFM applies to the *root* assets file only; since the two
    // callers that pass one never recurse, "every visited file is the root"
    // holds and the enumeration below can dispatch on `root_tfm` alone.
    debug_assert!(root_tfm.is_none() || !recurse);
    let mut package_dlls: BTreeSet<PathBuf> = BTreeSet::new();
    let mut framework_dlls: BTreeSet<PathBuf> = BTreeSet::new();
    let mut project_ref_tfms: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut project_ref_assembly_names: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut frameworks_resolved: BTreeSet<(String, String)> = BTreeSet::new();

    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    let mut worklist: Vec<PathBuf> = vec![root_assets_path.to_path_buf()];

    while let Some(assets_path) = worklist.pop() {
        let canon = std::fs::canonicalize(&assets_path).map_err(|e| ProjectAssetsError::Io {
            path: assets_path.clone(),
            source: e,
        })?;
        if !visited.insert(canon.clone()) {
            continue;
        }
        let text = std::fs::read_to_string(&canon).map_err(|e| ProjectAssetsError::Io {
            path: canon.clone(),
            source: e,
        })?;
        let assets: RawAssets =
            serde_json::from_str(&text).map_err(|e| ProjectAssetsError::Json {
                path: canon.clone(),
                source: e,
            })?;
        let assets_dir = canon
            .parent()
            .expect("assets file has parent")
            .to_path_buf();
        let refs = match root_tfm {
            Some(tfm) => enumerate::enumerate_one_for_tfm(&assets, &assets_dir, tfm)?,
            None => enumerate_one(&assets, &assets_dir)?,
        };

        for r in refs {
            match r {
                Reference::PackageDll { candidates } => {
                    package_dlls.insert(pick_existing(&candidates));
                }
                Reference::ProjectRef {
                    project_path,
                    tfm,
                    assembly_name,
                } => {
                    // Root-only mode: record the project-ref target if it
                    // exists, but never recurse into it. Missing targets
                    // don't surface as `MissingTransitiveAssets` either —
                    // the whole point of root-only is that callers don't
                    // depend on the transitive closure.
                    if !recurse {
                        if let Ok(canon_proj) = std::fs::canonicalize(&project_path) {
                            if let Some(name) = assembly_name {
                                project_ref_assembly_names.insert(canon_proj.clone(), name);
                            }
                            project_ref_tfms.insert(canon_proj, tfm);
                        }
                        continue;
                    }
                    let canon_proj = std::fs::canonicalize(&project_path).map_err(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            ProjectAssetsError::MissingTransitiveAssets {
                                project_path: project_path.clone(),
                            }
                        } else {
                            ProjectAssetsError::Io {
                                path: project_path.clone(),
                                source: e,
                            }
                        }
                    })?;
                    let next_assets = canon_proj
                        .parent()
                        .expect("project file has parent")
                        .join("obj")
                        .join("project.assets.json");
                    if let Some(name) = assembly_name {
                        project_ref_assembly_names.insert(canon_proj.clone(), name);
                    }
                    project_ref_tfms.insert(canon_proj.clone(), tfm);
                    if !next_assets.exists() {
                        return Err(ProjectAssetsError::MissingTransitiveAssets {
                            project_path: canon_proj,
                        });
                    }
                    worklist.push(next_assets);
                }
                Reference::Framework { name, tfm } => {
                    // Skip a framework we already resolved *successfully*. The
                    // dedup is recorded only on `Ok` below — not here — because
                    // resolution now depends on this assets file's
                    // `package_folders`: a *miss* in one project of a reference
                    // closure must not poison a later project that shares the
                    // framework but has a package folder where the NuGet
                    // targeting pack does live.
                    if frameworks_resolved.contains(&(name.clone(), tfm.clone())) {
                        continue;
                    }
                    // Non-fatal: a framework reference we can't locate (no ref
                    // pack for this TFM in the SDK packs dir *or* the NuGet
                    // package folders) costs us that framework's BCL types, but
                    // must NOT discard the package DLLs already resolved —
                    // go-to-definition into FSharp.Core (a NuGet package) is
                    // independent of the BCL targeting pack. Skipping here (D5:
                    // under-resolve, never wrong) instead of aborting is what
                    // lets a project targeting an older TFM than the installed
                    // SDK still resolve its package references.
                    match framework::resolve_framework(
                        dotnet_root,
                        &assets.package_folders,
                        &name,
                        &tfm,
                    ) {
                        Ok(dlls) => {
                            frameworks_resolved.insert((name, tfm));
                            framework_dlls.extend(dlls);
                        }
                        Err(err) => {
                            tracing::warn!(
                                framework = %name,
                                tfm = %tfm,
                                error = %err,
                                "could not resolve framework reference; its BCL types will be unavailable, package references unaffected"
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(ResolvedAssemblies {
        package_dlls: package_dlls.into_iter().collect(),
        framework_dlls: framework_dlls.into_iter().collect(),
        project_ref_tfms,
        project_ref_assembly_names,
    })
}

/// Build the closure-wide TFM map from on-disk `project.assets.json`
/// files alone. Disk-side counterpart to [`transitive_project_tfms`]:
///
/// 1. Read `obj/project.assets.json` next to `top_csproj_path`.
/// 2. For every project-kind target entry, look the producer csproj up
///    via `msbuildProject` (preferred) or `path`, then read *that*
///    producer's own assets file to extract the keys of
///    `project.frameworks` — the producer's declared TFM list.
/// 3. Hand the resulting `producer_declared_tfms` map to
///    [`transitive_project_tfms`] for the actual resolution (including
///    Phase 2b platform-suffix recovery).
///
/// The returned map keys every closure node — the top csproj included —
/// to the short-form TFM NuGet selected for it. NuGet's restore already
/// flattens `<ProjectReference>` transitively into the consumer's
/// `targets`, so a one-level loop here covers an arbitrarily deep
/// chain.
///
/// Path keys are built as `top_csproj_path.parent().join(rel)` and are
/// *not* canonicalised. This matches the keying convention of
/// [`transitive_project_tfms`], so a caller can use either function and
/// get path-equal keys for the same producers.
///
/// # Errors
///
/// - [`ProjectAssetsError::Io`] / [`ProjectAssetsError::Json`] if any
///   assets file is unreadable or malformed. The path on the error is
///   the offending file (top *or* producer), so the caller can tell
///   which `dotnet restore` to re-run.
/// - Plus every variant [`transitive_project_tfms`] can surface,
///   verbatim (e.g. [`ProjectAssetsError::RestoreMismatch`] when a
///   producer's declared TFMs have drifted from what restore recorded).
pub fn resolve_transitive_project_tfms(
    top_csproj_path: &Path,
    consumer_tfm: &str,
) -> Result<BTreeMap<PathBuf, String>, ProjectAssetsError> {
    let top_assets = read_raw_assets(&assets_path_for(top_csproj_path))?;
    let target = lookup_target_for_tfm(&top_assets, consumer_tfm)?;
    let top_dir = top_csproj_path
        .parent()
        .expect("top csproj path has a parent directory");

    let mut declared: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (name_version, entry) in target {
        if entry.kind != "project" {
            continue;
        }
        let library = top_assets.libraries.get(name_version).ok_or_else(|| {
            ProjectAssetsError::LibraryEntryMissing {
                name_version: name_version.clone(),
            }
        })?;
        // Same precedence as `transitive_project_tfms`: `msbuildProject`
        // preserves on-disk casing on case-sensitive filesystems where
        // NuGet's lowercased `path` would canonicalise to a missing file.
        let rel = library
            .msbuild_project
            .as_deref()
            .or(library.path.as_deref())
            .ok_or_else(|| ProjectAssetsError::ProjectRefMissingPath {
                name_version: name_version.clone(),
            })?;
        let producer_csproj = top_dir.join(rel);
        // Two `name_version`s can point to the same csproj if NuGet
        // recorded it under multiple ids (rare but legal). Reading the
        // producer's assets twice would be wasteful and the second pass
        // would just overwrite identical data; skip.
        if declared.contains_key(&producer_csproj) {
            continue;
        }
        let producer_assets = read_raw_assets(&assets_path_for(&producer_csproj))?;
        let tfms: Vec<String> = producer_assets.project.frameworks.keys().cloned().collect();
        declared.insert(producer_csproj, tfms);
    }
    transitive_project_tfms(&top_assets, consumer_tfm, top_csproj_path, &declared)
}

fn assets_path_for(csproj_path: &Path) -> PathBuf {
    csproj_path
        .parent()
        .expect("csproj path has a parent directory")
        .join("obj")
        .join("project.assets.json")
}

fn read_raw_assets(path: &Path) -> Result<RawAssets, ProjectAssetsError> {
    let text = std::fs::read_to_string(path).map_err(|e| ProjectAssetsError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_str(&text).map_err(|e| ProjectAssetsError::Json {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Pick the first candidate that exists on disk, falling back to the
/// first candidate (the path under the primary `packageFolders` entry)
/// when nothing exists. Caller controls the order: NuGet writes the
/// primary global packages folder first, then any fallback folders.
///
/// `candidates` is invariantly non-empty: `enumerate_one` errors with
/// `PackageFolderMissing` when the assets file has zero packageFolders,
/// and otherwise emits one candidate per folder.
fn pick_existing(candidates: &[PathBuf]) -> PathBuf {
    for c in candidates {
        if c.exists() {
            return c.clone();
        }
    }
    candidates[0].clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: write a minimal one-TFM assets file with the given packages
    /// (package_id -> compile DLL relative paths under lib/), the given
    /// project references (relative paths to .fsproj files), and an optional
    /// framework reference name.
    #[allow(clippy::too_many_arguments)]
    fn write_assets(
        path: &Path,
        tfm: &str,
        package_folder: &Path,
        packages: &[(&str, &[&str])],
        project_refs: &[&str],
        framework_ref: Option<&str>,
    ) {
        let mut target = serde_json::Map::new();
        let mut libraries = serde_json::Map::new();

        for (id, dlls) in packages {
            let mut compile = serde_json::Map::new();
            for dll in *dlls {
                compile.insert((*dll).to_string(), serde_json::json!({}));
            }
            target.insert(
                (*id).to_string(),
                serde_json::json!({
                    "type": "package",
                    "compile": compile,
                }),
            );
            // libraries.path is conventionally lowercase "name/version"
            libraries.insert(
                (*id).to_string(),
                serde_json::json!({
                    "type": "package",
                    "path": id.to_lowercase(),
                }),
            );
        }

        for (i, proj) in project_refs.iter().enumerate() {
            let id = format!("ProjRef{i}/1.0.0");
            // Mirror real NuGet output: every project-kind target entry
            // carries `framework` (the producer's TFM). For these
            // synthetic fixtures the producer is on the same TFM as the
            // consumer — multi-TFM coverage lives in the dedicated
            // fixture and the enumerate.rs tests.
            target.insert(
                id.clone(),
                serde_json::json!({ "type": "project", "framework": tfm }),
            );
            libraries.insert(
                id,
                serde_json::json!({
                    "type": "project",
                    "path": proj,
                }),
            );
        }

        let frameworks_for_project = if let Some(name) = framework_ref {
            serde_json::json!({ tfm: { "frameworkReferences": { name: {} } } })
        } else {
            serde_json::json!({ tfm: {} })
        };

        let doc = serde_json::json!({
            "version": 3,
            "targets": { tfm: target },
            "libraries": libraries,
            "packageFolders": { package_folder.to_str().unwrap(): {} },
            "project": { "frameworks": frameworks_for_project },
        });

        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    /// One package row in [`write_assets_multi_tfm`]: `(package id/version,
    /// compile DLL relative paths)`.
    type PackageRow<'a> = (&'a str, &'a [&'a str]);

    /// Multi-TFM assets file: one `targets` group and one `project.frameworks`
    /// entry per `(tfm, packages)` pair. The shape a
    /// `<TargetFrameworks>a;b</TargetFrameworks>` restore writes.
    fn write_assets_multi_tfm(
        path: &Path,
        package_folder: &Path,
        per_tfm: &[(&str, &[PackageRow<'_>])],
    ) {
        let mut targets = serde_json::Map::new();
        let mut libraries = serde_json::Map::new();
        let mut frameworks = serde_json::Map::new();
        for (tfm, packages) in per_tfm {
            let mut target = serde_json::Map::new();
            for (id, dlls) in *packages {
                let mut compile = serde_json::Map::new();
                for dll in *dlls {
                    compile.insert((*dll).to_string(), serde_json::json!({}));
                }
                target.insert(
                    (*id).to_string(),
                    serde_json::json!({ "type": "package", "compile": compile }),
                );
                libraries.insert(
                    (*id).to_string(),
                    serde_json::json!({ "type": "package", "path": id.to_lowercase() }),
                );
            }
            targets.insert((*tfm).to_string(), serde_json::Value::Object(target));
            frameworks.insert((*tfm).to_string(), serde_json::json!({}));
        }
        let doc = serde_json::json!({
            "version": 3,
            "targets": targets,
            "libraries": libraries,
            "packageFolders": { package_folder.to_str().unwrap(): {} },
            "project": { "frameworks": frameworks },
        });
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    // ---- resolve_assemblies_for_tfm (fsproj 3.3c stage 2, plan E3/E6) ----

    #[test]
    fn for_tfm_selects_the_requested_target_of_a_multi_tfm_restore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();
        let assets = root.join("A/obj/project.assets.json");
        write_assets_multi_tfm(
            &assets,
            &pkgs,
            &[
                ("net8.0", &[("P8/1.0.0", &["lib/net8.0/P8.dll"])]),
                ("net10.0", &[("P10/1.0.0", &["lib/net10.0/P10.dll"])]),
            ],
        );
        let dotnet = root.join("dotnet");

        // Regression pin: the TFM-less path still refuses multi-target restores.
        assert!(matches!(
            resolve_assemblies_root_only(&assets, &dotnet),
            Err(ProjectAssetsError::MultipleOrNoTargets { .. })
        ));

        let r8 = resolve_assemblies_for_tfm(&assets, &dotnet, "net8.0").expect("net8.0 target");
        assert_eq!(
            r8.package_dlls,
            vec![pkgs.join("p8/1.0.0/lib/net8.0/P8.dll")]
        );
        let r10 = resolve_assemblies_for_tfm(&assets, &dotnet, "net10.0").expect("net10.0 target");
        assert_eq!(
            r10.package_dlls,
            vec![pkgs.join("p10/1.0.0/lib/net10.0/P10.dll")]
        );
    }

    #[test]
    fn for_tfm_missing_target_is_an_error_never_another_tfm() {
        // E6: a stale restore under-resolves; it must never serve a different
        // TFM's assemblies (that would break the parse/env coherence E5
        // guarantees).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();
        let assets = root.join("A/obj/project.assets.json");
        write_assets(
            &assets,
            "net8.0",
            &pkgs,
            &[("P8/1.0.0", &["lib/net8.0/P8.dll"])],
            &[],
            None,
        );
        assert!(matches!(
            resolve_assemblies_for_tfm(&assets, &root.join("dotnet"), "net9.0"),
            Err(ProjectAssetsError::TargetForTfmMissing { .. })
        ));
    }

    #[test]
    fn for_tfm_on_single_tfm_restore_matches_root_only() {
        // Never-regress: when the restore has exactly one target and the
        // caller asks for it, both paths agree exactly.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();
        touch(&root.join("B/B.fsproj"));
        let assets = root.join("A/obj/project.assets.json");
        write_assets(
            &assets,
            "net8.0",
            &pkgs,
            &[("P1/1.0.0", &["lib/net8.0/P1.dll"])],
            &["../B/B.fsproj"],
            Some("Microsoft.NETCore.App"),
        );
        let dotnet = root.join("dotnet");
        let pack = dotnet.join("packs/Microsoft.NETCore.App.Ref/8.0.0/ref/net8.0");
        touch(&pack.join("System.dll"));

        let via_tfm = resolve_assemblies_for_tfm(&assets, &dotnet, "net8.0").expect("for_tfm");
        let via_single = resolve_assemblies_root_only(&assets, &dotnet).expect("root_only");
        assert_eq!(via_tfm, via_single);
    }

    #[test]
    fn resolves_packages_frameworks_and_project_refs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        // Project A references Project B and pulls in package P1.
        // Project B pulls in package P2.
        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        touch(&proj_a);
        touch(&proj_b);

        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net8.0",
            &pkgs,
            &[("P1/1.0.0", &["lib/net8.0/P1.dll"])],
            &["../B/B.fsproj"],
            Some("Microsoft.NETCore.App"),
        );
        write_assets(
            &root.join("B/obj/project.assets.json"),
            "net8.0",
            &pkgs,
            &[("P2/2.0.0", &["lib/net8.0/P2.dll"])],
            &[],
            Some("Microsoft.NETCore.App"),
        );

        // Fake dotnet_root.
        let dotnet_root = root.join("dotnet");
        let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/9.0.0/ref/net8.0");
        touch(&pack.join("System.dll"));

        let result = resolve_assemblies(&root.join("A/obj/project.assets.json"), &dotnet_root)
            .expect("resolve_assemblies");

        let canon_proj_b = fs::canonicalize(&proj_b).unwrap();
        assert_eq!(
            result.project_ref_tfms.keys().collect::<Vec<_>>(),
            vec![&canon_proj_b]
        );
        // The producer TFM is retained per ref, keyed by the same canonical
        // path (A references B at net8.0 — the `framework` on B's target entry).
        assert_eq!(
            result
                .project_ref_tfms
                .get(&canon_proj_b)
                .map(String::as_str),
            Some("net8.0"),
        );

        // Both P1 and P2 must appear, deduplicated and sorted. Package paths
        // are built by joining the literal packageFolders entry with
        // libraries[..].path and the compile key — no filesystem touch.
        let expected_packages = vec![
            pkgs.join("p1/1.0.0/lib/net8.0/P1.dll"),
            pkgs.join("p2/2.0.0/lib/net8.0/P2.dll"),
        ];
        assert_eq!(result.package_dlls, expected_packages);

        // Framework DLL resolved once even though both projects list the same
        // framework reference (dedup by (name, tfm)).
        assert_eq!(result.framework_dlls, vec![pack.join("System.dll")]);
    }

    #[test]
    fn unresolvable_framework_is_non_fatal_and_keeps_packages() {
        // The headline fix: a project whose framework reference can't be
        // resolved anywhere (no SDK packs dir, no NuGet ref pack) must still
        // return its package DLLs — FSharp.Core go-to-def is independent of the
        // BCL targeting pack. Before the fix this returned `Err` and the env was
        // emptied wholesale.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        touch(&root.join("A/A.fsproj"));
        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net6.0",
            &pkgs,
            &[("FSharp.Core/6.0.1", &["lib/netstandard2.1/FSharp.Core.dll"])],
            &[],
            Some("Microsoft.NETCore.App"),
        );
        // dotnet_root has no packs; pkgs has no ref pack — framework unresolvable.
        let result = resolve_assemblies_root_only(
            &root.join("A/obj/project.assets.json"),
            &root.join("dotnet"),
        )
        .expect("framework miss must not fail the whole resolution");
        assert!(
            result
                .package_dlls
                .iter()
                .any(|p| p.ends_with("FSharp.Core.dll")),
            "FSharp.Core must survive an unresolvable framework; got {:?}",
            result.package_dlls
        );
        assert!(result.framework_dlls.is_empty());
    }

    #[test]
    fn root_only_records_project_ref_tfm() {
        // Root-only mode records the ref target and its producer TFM without
        // recursing — B's own assets need not exist. This is the path
        // `semantic::build_assembly_env` drives for C# refs (it needs the TFM
        // to build the csproj under the consumer-selected framework).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();
        let proj_b = root.join("B/B.csproj");
        touch(&proj_b);
        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &["../B/B.csproj"],
            None,
        );
        let result = resolve_assemblies_root_only(
            &root.join("A/obj/project.assets.json"),
            &root.join("dotnet"),
        )
        .expect("resolve_assemblies_root_only");
        let canon_b = fs::canonicalize(&proj_b).unwrap();
        assert_eq!(
            result.project_ref_tfms.keys().collect::<Vec<_>>(),
            vec![&canon_b]
        );
        assert_eq!(
            result.project_ref_tfms.get(&canon_b).map(String::as_str),
            Some("net10.0"),
        );
    }

    #[test]
    fn framework_resolved_from_nuget_download_dependency() {
        // The net6.0-under-newer-SDK case end to end: the targeting pack lives
        // under the NuGet package folder (a `downloadDependency`), not the SDK
        // packs dir. `resolve_assemblies` must find it via `packageFolders`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        touch(&pkgs.join("microsoft.netcore.app.ref/6.0.36/ref/net6.0/System.Console.dll"));
        touch(&root.join("A/A.fsproj"));
        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net6.0",
            &pkgs,
            &[],
            &[],
            Some("Microsoft.NETCore.App"),
        );
        // dotnet_root has no net6.0 pack (simulating a newer SDK).
        let result = resolve_assemblies_root_only(
            &root.join("A/obj/project.assets.json"),
            &root.join("dotnet"),
        )
        .expect("resolve_assemblies_root_only");
        assert!(
            result
                .framework_dlls
                .iter()
                .any(|p| p.ends_with("System.Console.dll")),
            "BCL ref must resolve from the NuGet download-dependency pack; got {:?}",
            result.framework_dlls
        );
    }

    #[test]
    fn framework_miss_in_one_project_does_not_poison_a_later_one() {
        // Recursive closure: A (whose package folder lacks the targeting pack)
        // references B (whose package folder has it), both targeting net6.0 with
        // the same framework reference. A's miss must not record the (name, tfm)
        // and skip B — the framework must still resolve from B's package folder.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs_a = root.join("pkgs_a"); // no ref pack
        let pkgs_b = root.join("pkgs_b");
        touch(&pkgs_b.join("microsoft.netcore.app.ref/6.0.36/ref/net6.0/System.Console.dll"));
        touch(&root.join("A/A.fsproj"));
        touch(&root.join("B/B.fsproj"));
        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net6.0",
            &pkgs_a,
            &[],
            &["../B/B.fsproj"],
            Some("Microsoft.NETCore.App"),
        );
        write_assets(
            &root.join("B/obj/project.assets.json"),
            "net6.0",
            &pkgs_b,
            &[],
            &[],
            Some("Microsoft.NETCore.App"),
        );
        // dotnet_root has no packs, so resolution must come from B's package folder.
        let result = resolve_assemblies(
            &root.join("A/obj/project.assets.json"),
            &root.join("dotnet"),
        )
        .expect("resolve_assemblies");
        assert!(
            result
                .framework_dlls
                .iter()
                .any(|p| p.ends_with("System.Console.dll")),
            "the framework must resolve from project B's package folder; got {:?}",
            result.framework_dlls
        );
    }

    #[test]
    fn project_ref_cycle_terminates() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        touch(&proj_a);
        touch(&proj_b);

        // A -> B -> A. MSBuild rejects cycles at build time but if the
        // assets files happen to encode one, the resolver must terminate.
        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net8.0",
            &pkgs,
            &[],
            &["../B/B.fsproj"],
            None,
        );
        write_assets(
            &root.join("B/obj/project.assets.json"),
            "net8.0",
            &pkgs,
            &[],
            &["../A/A.fsproj"],
            None,
        );

        let result = resolve_assemblies(
            &root.join("A/obj/project.assets.json"),
            &root.join("dotnet"),
        )
        .expect("resolve_assemblies");

        let canon_a = fs::canonicalize(&proj_a).unwrap();
        let canon_b = fs::canonicalize(&proj_b).unwrap();
        let mut expected = vec![canon_a, canon_b];
        expected.sort();
        assert_eq!(
            result.project_ref_tfms.keys().cloned().collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn falls_back_to_secondary_package_folder_when_primary_lacks_dll() {
        // Two packageFolders configured (NuGet fallback folder scenario);
        // the DLL only exists under the second one. resolve_assemblies
        // must walk the list and pick the existing path, not blindly use
        // the primary folder.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let primary = root.join("primary-empty");
        let fallback = root.join("fallback-has-it");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&fallback).unwrap();

        // Only the fallback folder actually contains the DLL.
        touch(&fallback.join("p/1.0.0/lib/net8.0/P.dll"));

        // Write the assets file by hand so packageFolders preserves
        // document order. The `write_assets` helper goes through
        // serde_json::Value which would sort the keys.
        let assets_path = root.join("A/obj/project.assets.json");
        fs::create_dir_all(assets_path.parent().unwrap()).unwrap();
        let json = format!(
            r#"{{
                "version": 3,
                "targets": {{
                    "net8.0": {{
                        "P/1.0.0": {{
                            "type": "package",
                            "compile": {{ "lib/net8.0/P.dll": {{}} }}
                        }}
                    }}
                }},
                "libraries": {{
                    "P/1.0.0": {{ "type": "package", "path": "p/1.0.0" }}
                }},
                "packageFolders": {{
                    "{}": {{}},
                    "{}": {{}}
                }},
                "project": {{ "frameworks": {{ "net8.0": {{}} }} }}
            }}"#,
            primary.to_str().unwrap(),
            fallback.to_str().unwrap(),
        );
        fs::write(&assets_path, json).unwrap();

        let result = resolve_assemblies(&assets_path, &root.join("dotnet")).unwrap();
        assert_eq!(
            result.package_dlls,
            vec![fallback.join("p/1.0.0/lib/net8.0/P.dll")]
        );
    }

    #[test]
    fn missing_transitive_assets_is_reported() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_b = root.join("B/B.fsproj");
        touch(&proj_b);
        // Note: no B/obj/project.assets.json — B was not restored.

        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net8.0",
            &pkgs,
            &[],
            &["../B/B.fsproj"],
            None,
        );

        match resolve_assemblies(
            &root.join("A/obj/project.assets.json"),
            &root.join("dotnet"),
        ) {
            Err(ProjectAssetsError::MissingTransitiveAssets { project_path }) => {
                assert_eq!(project_path, fs::canonicalize(&proj_b).unwrap());
            }
            other => panic!("expected MissingTransitiveAssets, got {other:?}"),
        }
    }

    /// `resolve_assemblies_root_only` must succeed in the partial-restore
    /// case where `resolve_assemblies` errors — the LSP semantic layer
    /// depends on this so an unrestored sibling project doesn't blank the
    /// root's framework / package DLLs (codex Stage 6 finding).
    #[test]
    fn root_only_succeeds_when_transitive_assets_are_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        // Stub a package whose DLL exists on disk under `pkgs/`.
        let dll = pkgs.join("pkg.a/1.0.0/lib/net8.0/Pkg.A.dll");
        fs::create_dir_all(dll.parent().unwrap()).unwrap();
        fs::write(&dll, b"").unwrap();

        // The sibling project file exists but its `obj/project.assets.json`
        // does *not* — `resolve_assemblies` errors with
        // MissingTransitiveAssets, `resolve_assemblies_root_only` ignores it.
        let proj_b = root.join("B/B.fsproj");
        touch(&proj_b);

        let assets = root.join("A/obj/project.assets.json");
        write_assets(
            &assets,
            "net8.0",
            &pkgs,
            &[("Pkg.A/1.0.0", &["lib/net8.0/Pkg.A.dll"])],
            &["../B/B.fsproj"],
            None,
        );

        // Sanity: the eager walker still errors here, matching the test above.
        assert!(matches!(
            resolve_assemblies(&assets, &root.join("dotnet")),
            Err(ProjectAssetsError::MissingTransitiveAssets { .. })
        ));

        // The root-only walker succeeds and returns the root's package DLL.
        // Project-ref targets are recorded (when they exist on disk) but never
        // descended into.
        let resolved = resolve_assemblies_root_only(&assets, &root.join("dotnet"))
            .expect("root-only succeeds despite missing transitive assets");
        assert!(
            resolved
                .package_dlls
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "Pkg.A.dll")),
            "expected the root's package DLL to survive; got {:#?}",
            resolved.package_dlls,
        );
    }

    /// Single-csproj closure: no project refs, so the returned map is the
    /// singleton `{top: consumer_tfm}`. Pins the trivial path because
    /// every end-to-end caller hits it for the no-refs case.
    #[test]
    fn resolve_transitive_project_tfms_singleton_no_refs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        touch(&proj_a);
        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &[],
            None,
        );

        let map =
            resolve_transitive_project_tfms(&proj_a, "net10.0").expect("resolve singleton closure");
        assert_eq!(
            map,
            BTreeMap::from([(proj_a.clone(), "net10.0".to_string())]),
        );
    }

    /// Two-csproj closure A→B. Both producers were restored under the
    /// same TFM as the consumer; the helper picks that up via each
    /// producer's `project.frameworks` keys without the caller having to
    /// pass declared-TFMs by hand.
    #[test]
    fn resolve_transitive_project_tfms_walks_one_level() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        touch(&proj_a);
        touch(&proj_b);

        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &["../B/B.fsproj"],
            None,
        );
        write_assets(
            &root.join("B/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &[],
            None,
        );

        let map = resolve_transitive_project_tfms(&proj_a, "net10.0").expect("resolve A→B closure");
        // Path keys mirror `transitive_project_tfms`: top is the exact
        // path the caller passed in (no canonicalisation), producer is
        // `<top_dir>/<rel>`.
        let expected_b = proj_a.parent().unwrap().join("../B/B.fsproj");
        assert_eq!(
            map,
            BTreeMap::from([
                (proj_a, "net10.0".to_string()),
                (expected_b, "net10.0".to_string()),
            ]),
        );
    }

    /// Transitive closure A→B→C: NuGet restore flattens C into A's own
    /// `targets` (this is the contract of `project.assets.json`), so a
    /// single pass through A's target list is enough — we do not need
    /// to recurse into B's targets to find C. The fixture is written
    /// to match that NuGet contract.
    #[test]
    fn resolve_transitive_project_tfms_three_node_chain() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        let proj_c = root.join("C/C.fsproj");
        touch(&proj_a);
        touch(&proj_b);
        touch(&proj_c);

        // A's assets file lists *both* B and C in its targets, matching
        // what NuGet would write for an A→B→C chain.
        write_three_project_assets(
            &root.join("A/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[
                ("ProjRef0/1.0.0", "../B/B.fsproj"),
                ("ProjRef1/1.0.0", "../C/C.fsproj"),
            ],
        );
        write_assets(
            &root.join("B/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &["../C/C.fsproj"],
            None,
        );
        write_assets(
            &root.join("C/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &[],
            None,
        );

        let map =
            resolve_transitive_project_tfms(&proj_a, "net10.0").expect("resolve A→B→C closure");
        let expected_b = proj_a.parent().unwrap().join("../B/B.fsproj");
        let expected_c = proj_a.parent().unwrap().join("../C/C.fsproj");
        assert_eq!(
            map,
            BTreeMap::from([
                (proj_a, "net10.0".to_string()),
                (expected_b, "net10.0".to_string()),
                (expected_c, "net10.0".to_string()),
            ]),
        );
    }

    /// Multi-TFM producer: consumer A is on `net10.0`; producer B
    /// declares `[net8.0, net6.0]` and NuGet picked `net8.0` for the
    /// consumer (recorded in A's assets file as the producer's
    /// `framework`). The helper consults B's own assets to learn the
    /// declared TFMs and the Phase 2b picker reproduces NuGet's choice.
    #[test]
    fn resolve_transitive_project_tfms_picks_producer_tfm_from_disk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        touch(&proj_a);
        touch(&proj_b);

        // A is on net10.0; its target entry for B records `framework:
        // ".NETCoreApp,Version=v8.0"` (the producer TFM NuGet picked).
        // Hand-write A's assets file because `write_assets` always pins
        // the producer TFM to the consumer's.
        write_consumer_assets_with_producer_framework(
            &root.join("A/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            "ProjRef0/1.0.0",
            "../B/B.fsproj",
            ".NETCoreApp,Version=v8.0",
        );
        // B is the multi-TFM producer; its own assets file declares
        // `[net6.0, net8.0]` in `project.frameworks`.
        write_multi_tfm_producer_assets(
            &root.join("B/obj/project.assets.json"),
            &["net6.0", "net8.0"],
            &pkgs,
        );

        let map = resolve_transitive_project_tfms(&proj_a, "net10.0")
            .expect("resolve A→B (multi-TFM B) closure");
        let expected_b = proj_a.parent().unwrap().join("../B/B.fsproj");
        assert_eq!(
            map,
            BTreeMap::from([
                (proj_a, "net10.0".to_string()),
                (expected_b, "net8.0".to_string()),
            ]),
        );
    }

    /// Producer wasn't restored: its `obj/project.assets.json` is
    /// missing on disk. Surface that as the standard `Io` error keyed
    /// on the *producer's* assets path so the caller can tell which
    /// `dotnet restore` to run, not the top one.
    #[test]
    fn resolve_transitive_project_tfms_missing_producer_assets_is_io_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        touch(&proj_a);
        touch(&proj_b);
        // No B/obj/project.assets.json — B has never been restored.

        write_assets(
            &root.join("A/obj/project.assets.json"),
            "net10.0",
            &pkgs,
            &[],
            &["../B/B.fsproj"],
            None,
        );

        match resolve_transitive_project_tfms(&proj_a, "net10.0") {
            Err(ProjectAssetsError::Io { path, source }) => {
                let expected = root.join("A/../B/obj/project.assets.json");
                assert_eq!(
                    path, expected,
                    "Io error path must point at the missing producer assets file, not the top one"
                );
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io NotFound on producer assets, got {other:?}"),
        }
    }

    /// Top assets file is missing entirely. Same shape as the
    /// producer-missing case but the path is the top's.
    #[test]
    fn resolve_transitive_project_tfms_missing_top_assets_is_io_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let proj_a = root.join("A/A.fsproj");
        touch(&proj_a);
        // No A/obj/project.assets.json.

        match resolve_transitive_project_tfms(&proj_a, "net10.0") {
            Err(ProjectAssetsError::Io { path, source }) => {
                assert_eq!(path, root.join("A/obj/project.assets.json"));
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io NotFound on top assets, got {other:?}"),
        }
    }

    /// Multi-hop platform-suffix mix: A on `net8.0-windows` references
    /// a bare-only B, which itself references a multi-TFM C declaring
    /// `[net8.0, net8.0-windows]`. NuGet flattens A→B→C into A's target
    /// list and records *only the base* `.NETCoreApp,Version=v8.0` in
    /// each `framework` field (per Phase 2b's documented invariant —
    /// confirmed empirically with `dotnet restore` against this exact
    /// shape).
    ///
    /// A pre-merge codex review (sidecar-callers-derive-project-tfms,
    /// 2026-05-27) worried this would resolve C to `net8.0-windows`
    /// instead of `net8.0`, on the theory that the immediate parent B
    /// is bare and C should inherit B's choice. An empirical experiment
    /// (set up A→B→C exactly as here, ran `dotnet build`) showed
    /// otherwise: MSBuild builds C *twice* (once as `net8.0` for B,
    /// once as `net8.0-windows` for A's direct view via the flattened
    /// ref), and csc compiles A with `/reference` pointing at the
    /// `net8.0-windows/ref/C.dll`. So `net8.0-windows` *is* the right
    /// answer for the root consumer's compile-time view of C, and the
    /// current `pick_producer_tfm` semantics — thread the root TFM
    /// into every entry of the flat target list — match MSBuild's
    /// actual behaviour.
    ///
    /// The sidecar's one-DLL-per-project emit model can't faithfully
    /// represent C-built-under-two-TFMs; resolving A's view to the
    /// windows variant is the right single-emit answer for this
    /// closure. This test pins that behaviour and the experimental
    /// reasoning behind it so a future refactor doesn't quietly flip
    /// the result.
    #[test]
    fn resolve_transitive_project_tfms_multi_hop_suffix_mix_threads_root_tfm() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();

        let proj_a = root.join("A/A.fsproj");
        let proj_b = root.join("B/B.fsproj");
        let proj_c = root.join("C/C.fsproj");
        touch(&proj_a);
        touch(&proj_b);
        touch(&proj_c);

        // A is `net8.0-windows`; its flattened target lists B and C,
        // each with the bare-net8.0 framework field NuGet writes
        // (the base moniker, irrespective of which variant MSBuild
        // actually selects per consumer). Hand-write the document
        // because the existing helpers pin the producer framework to
        // the consumer's TFM, which is the *opposite* of the shape
        // under test.
        let a_assets = root.join("A/obj/project.assets.json");
        fs::create_dir_all(a_assets.parent().unwrap()).unwrap();
        let a_doc = serde_json::json!({
            "version": 3,
            "targets": { "net8.0-windows": {
                "ProjRef0/1.0.0": {
                    "type": "project",
                    "framework": ".NETCoreApp,Version=v8.0",
                },
                "ProjRef1/1.0.0": {
                    "type": "project",
                    "framework": ".NETCoreApp,Version=v8.0",
                },
            }},
            "libraries": {
                "ProjRef0/1.0.0": { "type": "project", "path": "../B/B.fsproj" },
                "ProjRef1/1.0.0": { "type": "project", "path": "../C/C.fsproj" },
            },
            "packageFolders": { pkgs.to_str().unwrap(): {} },
            "project": { "frameworks": { "net8.0-windows": {} } },
        });
        fs::write(&a_assets, serde_json::to_string_pretty(&a_doc).unwrap()).unwrap();

        // B is bare-only. C declares both TFMs.
        write_multi_tfm_producer_assets(
            &root.join("B/obj/project.assets.json"),
            &["net8.0"],
            &pkgs,
        );
        write_multi_tfm_producer_assets(
            &root.join("C/obj/project.assets.json"),
            &["net8.0", "net8.0-windows"],
            &pkgs,
        );

        let map = resolve_transitive_project_tfms(&proj_a, "net8.0-windows")
            .expect("resolve A→B→C closure with platform-suffix mix");
        let expected_b = proj_a.parent().unwrap().join("../B/B.fsproj");
        let expected_c = proj_a.parent().unwrap().join("../C/C.fsproj");
        assert_eq!(
            map,
            BTreeMap::from([
                (proj_a, "net8.0-windows".to_string()),
                (expected_b, "net8.0".to_string()),
                (expected_c, "net8.0-windows".to_string()),
            ]),
            "A's compile-time view of C goes through the flattened ref \
             at A's TFM; csc gets `net8.0-windows/ref/C.dll`. The \
             single-emit model picks that variant. See the docstring \
             for the empirical justification.",
        );
    }

    /// Hand-write the consumer's assets with one project-kind target
    /// entry whose `framework` is the caller-controlled producer TFM
    /// (in long form, as NuGet writes it).
    fn write_consumer_assets_with_producer_framework(
        path: &Path,
        consumer_tfm: &str,
        package_folder: &Path,
        proj_ref_id: &str,
        proj_ref_rel_path: &str,
        producer_framework_long: &str,
    ) {
        let doc = serde_json::json!({
            "version": 3,
            "targets": { consumer_tfm: {
                proj_ref_id: {
                    "type": "project",
                    "framework": producer_framework_long,
                },
            }},
            "libraries": {
                proj_ref_id: { "type": "project", "path": proj_ref_rel_path },
            },
            "packageFolders": { package_folder.to_str().unwrap(): {} },
            "project": { "frameworks": { consumer_tfm: {} } },
        });
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    /// Producer-side assets file: declares multiple TFMs in
    /// `project.frameworks` but is otherwise inert (no targets the
    /// consumer-side walker cares about; we only read
    /// `project.frameworks.keys()`).
    fn write_multi_tfm_producer_assets(path: &Path, declared_tfms: &[&str], package_folder: &Path) {
        let mut frameworks = serde_json::Map::new();
        let mut targets = serde_json::Map::new();
        for tfm in declared_tfms {
            frameworks.insert((*tfm).to_string(), serde_json::json!({}));
            targets.insert((*tfm).to_string(), serde_json::json!({}));
        }
        let doc = serde_json::json!({
            "version": 3,
            "targets": targets,
            "libraries": {},
            "packageFolders": { package_folder.to_str().unwrap(): {} },
            "project": { "frameworks": frameworks },
        });
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    /// `write_assets` numbers its `ProjRef{i}/1.0.0` ids by closure
    /// position, but the three-node chain test wants two named project
    /// refs at the *consumer's* assets level (B and C, both flat),
    /// matching NuGet's transitive-flattening contract. This helper
    /// writes that shape directly.
    fn write_three_project_assets(
        path: &Path,
        consumer_tfm: &str,
        package_folder: &Path,
        proj_refs: &[(&str, &str)],
    ) {
        let mut target = serde_json::Map::new();
        let mut libraries = serde_json::Map::new();
        for (id, rel) in proj_refs {
            target.insert(
                (*id).to_string(),
                serde_json::json!({ "type": "project", "framework": consumer_tfm }),
            );
            libraries.insert(
                (*id).to_string(),
                serde_json::json!({ "type": "project", "path": rel }),
            );
        }
        let doc = serde_json::json!({
            "version": 3,
            "targets": { consumer_tfm: target },
            "libraries": libraries,
            "packageFolders": { package_folder.to_str().unwrap(): {} },
            "project": { "frameworks": { consumer_tfm: {} } },
        });
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }
}
