use std::path::{Path, PathBuf};

use crate::project_assets::error::ProjectAssetsError;
use crate::project_assets::raw::RawAssets;
use crate::project_assets::tfm;

/// A single symbolic reference discovered in one `project.assets.json`.
///
/// `PackageDll` carries one candidate absolute path per configured
/// `packageFolders` entry, in JSON-document order: NuGet writes the
/// primary global packages folder first, then any fallback folders.
/// The shell is expected to pick the first candidate that exists on
/// disk; only the primary location is hit in the overwhelmingly common
/// single-folder case. `ProjectRef` is the absolute path of a
/// referenced `.fsproj` / `.csproj` plus the *producer's base TFM* that
/// NuGet selected for this consumer (short form, e.g. `netstandard2.0`).
/// `Framework` carries the shared-framework name and the TFM, deferring
/// on-disk resolution to the shell layer.
///
/// **Caveat on `ProjectRef::tfm` and platform-qualified TFMs.** NuGet
/// writes the `framework` field on project-kind target entries as the
/// `.NETFramework`/`.NETCoreApp`/`.NETStandard` long moniker only —
/// without the target-platform suffix. For a `net8.0-windows` consumer
/// referencing a `net8.0-windows` producer, the field reads
/// `.NETCoreApp,Version=v8.0` and round-trips here as `net8.0`. This is
/// not a bug in normalisation: the platform suffix is genuinely not in
/// the consumer's assets file. Recovering the producer's full TFM
/// requires reading the producer's own `project.frameworks` (see Phase
/// 3 of `docs/completed/multi-tfm-resolution-plan.md`). Phase 1 consumers don't
/// dispatch on `tfm` yet, so storing the base form is correct for
/// today and the cross-reference layer can lift it later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    PackageDll {
        candidates: Vec<PathBuf>,
    },
    ProjectRef {
        project_path: PathBuf,
        tfm: String,
        /// The producer's **evaluated** output assembly simple name, from the
        /// project-kind target entry's `compile` asset
        /// (`bin/placeholder/<AssemblyName>.dll`). NuGet's restore evaluated
        /// the producer project to write it, so an `<AssemblyName>` override
        /// is already applied — this is what lets the F# output-DLL locator
        /// find a renamed output instead of assuming the project-file stem.
        /// `None` when the entry has no non-placeholder `compile` asset (a
        /// hand-edited assets file, or a producer with no compile-time
        /// output); the consumer falls back to the stem assumption.
        assembly_name: Option<String>,
    },
    Framework {
        name: String,
        tfm: String,
    },
}

/// Pure enumeration of one parsed `project.assets.json`.
///
/// `assets_file_dir` is the directory containing the assets file. Project
/// reference relative paths in `libraries[..].path` are resolved against
/// the directory one level above (the project's own directory — i.e.
/// `obj/`'s parent), matching how MSBuild writes those paths from the
/// project's perspective. No filesystem access is performed.
pub fn enumerate_one(
    assets: &RawAssets,
    assets_file_dir: &Path,
) -> Result<Vec<Reference>, ProjectAssetsError> {
    let (tfm, target) = single_target(assets)?;
    enumerate_target(assets, assets_file_dir, tfm, target)
}

/// [`enumerate_one`] for a caller-chosen TFM (fsproj 3.3c, plan E3): selects
/// `targets[tfm]` via `lookup_target_for_tfm` (netstandard-alias fallback
/// included) instead of requiring the restore to be single-target — the
/// multi-TFM (`<TargetFrameworks>`) entry case. A target missing for `tfm`
/// (stale restore) is an error: the caller degrades to an empty env rather
/// than serving a *different* TFM's assemblies (plan E6 — under-resolve,
/// never cross-resolve).
pub fn enumerate_one_for_tfm(
    assets: &RawAssets,
    assets_file_dir: &Path,
    tfm: &str,
) -> Result<Vec<Reference>, ProjectAssetsError> {
    let target = lookup_target_for_tfm(assets, tfm)?;
    enumerate_target(assets, assets_file_dir, tfm, target)
}

/// Shared body of [`enumerate_one`] / [`enumerate_one_for_tfm`]: walk the
/// chosen target group. `tfm` is used for the `project.frameworks` lookup
/// (framework references); in the caller-chosen case a spelling that misses
/// the frameworks key just yields no framework references (under-resolve).
fn enumerate_target(
    assets: &RawAssets,
    assets_file_dir: &Path,
    tfm: &str,
    target: &std::collections::BTreeMap<String, crate::project_assets::raw::RawTargetEntry>,
) -> Result<Vec<Reference>, ProjectAssetsError> {
    if assets.package_folders.is_empty() {
        return Err(ProjectAssetsError::PackageFolderMissing);
    }

    let project_dir = assets_file_dir.parent().unwrap_or(assets_file_dir);

    let mut out = Vec::new();

    for (name_version, entry) in target {
        match entry.kind.as_str() {
            "project" => {
                let library = assets.libraries.get(name_version).ok_or_else(|| {
                    ProjectAssetsError::LibraryEntryMissing {
                        name_version: name_version.clone(),
                    }
                })?;
                // Prefer `msbuildProject` over `path`: NuGet lowercases the
                // directory portion of `path` for project libraries, which
                // produces a non-existent path on case-sensitive filesystems
                // when the actual directory has mixed case.
                let rel = library
                    .msbuild_project
                    .as_deref()
                    .or(library.path.as_deref())
                    .ok_or_else(|| ProjectAssetsError::ProjectRefMissingPath {
                        name_version: name_version.clone(),
                    })?;
                // NuGet writes `framework` on every project-kind target
                // entry. Absence implies a hand-edited or pre-multi-TFM
                // assets file; refuse to guess — downstream sidecar
                // dispatch needs the producer's TFM to configure the
                // right Roslyn workspace.
                let producer_tfm = entry.framework.as_deref().ok_or_else(|| {
                    ProjectAssetsError::ProjectRefUnresolved {
                        name_version: name_version.clone(),
                    }
                })?;
                out.push(Reference::ProjectRef {
                    project_path: project_dir.join(rel),
                    tfm: tfm::long_to_short(producer_tfm),
                    assembly_name: project_assembly_name(entry),
                });
            }
            "package" => {
                let library = assets.libraries.get(name_version).ok_or_else(|| {
                    ProjectAssetsError::LibraryEntryMissing {
                        name_version: name_version.clone(),
                    }
                })?;
                let library_path = library.path.as_deref().ok_or_else(|| {
                    ProjectAssetsError::LibraryEntryMissing {
                        name_version: name_version.clone(),
                    }
                })?;
                let Some(compile) = entry.compile.as_ref() else {
                    continue;
                };
                for rel_dll in compile.keys() {
                    if is_placeholder(rel_dll) {
                        continue;
                    }
                    let candidates: Vec<PathBuf> = assets
                        .package_folders
                        .iter()
                        .map(|folder| folder.join(library_path).join(rel_dll))
                        .collect();
                    out.push(Reference::PackageDll { candidates });
                }
            }
            _ => {
                // Unknown library kind. Ignore — NuGet may add new variants
                // (e.g. "msbuild"-typed source projects in future). A real
                // assets file we couldn't read would be more useful as a
                // partial result than an error.
            }
        }
    }

    if let Some(frameworks) = assets.project.frameworks.get(tfm) {
        for name in frameworks.framework_references.keys() {
            out.push(Reference::Framework {
                name: name.clone(),
                tfm: tfm.to_string(),
            });
        }
    }

    Ok(out)
}

/// Pick the single compile-time TFM and the matching target group.
///
/// The TFM is sourced from `project.frameworks`, not `targets`: when a
/// project is restored with a `RuntimeIdentifier`, NuGet writes both a
/// bare-TFM target (e.g. `net8.0`) and one or more RID-qualified
/// targets (e.g. `net8.0/osx-arm64`), but `project.frameworks` only
/// lists the bare TFM. Gating on `targets.len()` would spuriously
/// reject those projects as multi-targeted.
///
/// When `targets[framework_tfm]` is absent (e.g. `project.frameworks`
/// uses the short alias `netstandard2.0` but `targets` writes the long
/// moniker `.NETStandard,Version=v2.0`), fall back to the sole bare-TFM
/// target. "Bare" means the target key does not contain `/` — that
/// excludes RID-qualified entries like `net8.0/osx-arm64`. If that
/// filtering leaves exactly one target, it's unambiguous (we already
/// know there's only one framework).
fn single_target(
    assets: &RawAssets,
) -> Result<
    (
        &String,
        &std::collections::BTreeMap<String, crate::project_assets::raw::RawTargetEntry>,
    ),
    ProjectAssetsError,
> {
    if assets.project.frameworks.len() != 1 {
        return Err(ProjectAssetsError::MultipleOrNoTargets {
            found: assets.project.frameworks.keys().cloned().collect(),
        });
    }
    let (tfm, _) = assets.project.frameworks.iter().next().expect("len == 1");
    let target = lookup_target_for_tfm(assets, tfm)?;
    Ok((tfm, target))
}

/// Find the `targets[<tfm>]` entry for a specified TFM, with the
/// netstandard-alias fallback.
///
/// Direct match wins. When absent, normalise each (non-RID-qualified)
/// target key via `tfm::long_to_short` and require *exact* equality
/// with the requested `tfm`. That handles the `project.frameworks`
/// short alias against the `targets` long moniker (e.g.
/// `netstandard2.0` ↔ `.NETStandard,Version=v2.0`) while still
/// erroring when no target genuinely matches the request.
///
/// The earlier "sole bare target" fallback was wrong for the
/// `transitive_project_tfms` caller: a stale single-TFM restore
/// (assets has `net8.0`, LSP picks `net9.0`) would silently enumerate
/// the wrong graph. Matching by normalised key surfaces the
/// mismatch as `TargetForTfmMissing` instead.
pub(super) fn lookup_target_for_tfm<'a>(
    assets: &'a RawAssets,
    tfm: &str,
) -> Result<
    &'a std::collections::BTreeMap<String, crate::project_assets::raw::RawTargetEntry>,
    ProjectAssetsError,
> {
    if let Some(target) = assets.targets.get(tfm) {
        return Ok(target);
    }
    let matches: Vec<&std::collections::BTreeMap<String, _>> = assets
        .targets
        .iter()
        .filter(|(k, _)| !k.contains('/') && tfm::long_to_short(k) == tfm)
        .map(|(_, v)| v)
        .collect();
    if let [single] = matches.as_slice() {
        return Ok(single);
    }
    Err(ProjectAssetsError::TargetForTfmMissing {
        tfm: tfm.to_string(),
        found: assets.targets.keys().cloned().collect(),
    })
}

fn is_placeholder(rel: &str) -> bool {
    Path::new(rel).file_name().is_some_and(|n| n == "_._")
}

/// The producer's output assembly simple name recorded on a project-kind
/// target entry: the file stem of its `compile` asset
/// (`bin/placeholder/<AssemblyName>.dll`). A project produces one primary
/// output, so a well-formed entry has exactly one non-placeholder compile
/// asset; anything else (none, or several distinct stems) yields `None` —
/// refuse to guess rather than pick one (D5: under-resolve, never wrong).
fn project_assembly_name(entry: &crate::project_assets::raw::RawTargetEntry) -> Option<String> {
    let compile = entry.compile.as_ref()?;
    let mut stems = compile
        .keys()
        .filter(|rel| !is_placeholder(rel))
        .filter_map(|rel| Path::new(rel).file_stem())
        .filter_map(|stem| stem.to_str());
    let first = stems.next()?;
    if stems.any(|other| other != first) {
        return None;
    }
    Some(first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_assets::raw::{
        RawAssets, RawLibrary, RawProject, RawProjectFramework, RawTargetEntry,
    };
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    fn load_fixture(name: &str) -> RawAssets {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/project_assets")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    #[test]
    fn fixture_enumerates_expected_compile_dlls() {
        let assets = load_fixture("single_tfm.json");
        let refs = enumerate_one(&assets, Path::new("/tmp/dummy")).unwrap();

        // The fixture has a single packageFolder, so each PackageDll's
        // `candidates` is a one-element vec — pull the sole candidate.
        let package_dlls: Vec<&Path> = refs
            .iter()
            .filter_map(|r| match r {
                Reference::PackageDll { candidates } => {
                    assert_eq!(candidates.len(), 1, "single-folder fixture");
                    Some(candidates[0].as_path())
                }
                _ => None,
            })
            .collect();

        // FCS, FSharp.Core, FSharp.SystemTextJson — compile assets we know
        // are present from the deserialisation test.
        assert!(
            package_dlls.iter().any(|p| p.ends_with(
                "fsharp.compiler.service/43.12.204/lib/netstandard2.0/FSharp.Compiler.Service.dll"
            )),
            "missing FSharp.Compiler.Service.dll, got {package_dlls:?}"
        );
        assert!(
            package_dlls
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "FSharp.Core.dll")
                    && p.to_string_lossy().contains("fsharp.core/10.1.204")),
            "missing FSharp.Core.dll, got {package_dlls:?}"
        );

        // All paths must be rooted at the package folder.
        for p in &package_dlls {
            assert!(
                p.starts_with("/Users/patrick/.nuget/packages/"),
                "not under packages folder: {}",
                p.display()
            );
        }

        // No `_._` leaks.
        for p in &package_dlls {
            assert!(
                p.file_name().is_none_or(|n| n != "_._"),
                "_._ leaked: {}",
                p.display()
            );
        }

        // Exactly one Framework reference (Microsoft.NETCore.App).
        let frameworks: Vec<&str> = refs
            .iter()
            .filter_map(|r| match r {
                Reference::Framework { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(frameworks, vec!["Microsoft.NETCore.App"]);
    }

    fn build_assets(targets: BTreeMap<String, BTreeMap<String, RawTargetEntry>>) -> RawAssets {
        let package_folders = vec![PathBuf::from("/pkgs/")];
        let mut frameworks = BTreeMap::new();
        for tfm in targets.keys() {
            // Skip RID-qualified target keys so the project.frameworks
            // map only ever contains bare TFMs, as real assets files do.
            if tfm.contains('/') {
                continue;
            }
            frameworks.insert(
                tfm.clone(),
                RawProjectFramework {
                    framework_references: BTreeMap::new(),
                },
            );
        }
        RawAssets {
            version: 3,
            targets,
            libraries: BTreeMap::new(),
            package_folders,
            project: RawProject { frameworks },
        }
    }

    #[test]
    fn multiple_tfms_errors() {
        let mut targets = BTreeMap::new();
        targets.insert("net6.0".to_string(), BTreeMap::new());
        targets.insert("net8.0".to_string(), BTreeMap::new());
        let assets = build_assets(targets);

        match enumerate_one(&assets, Path::new("/tmp")) {
            Err(ProjectAssetsError::MultipleOrNoTargets { found }) => {
                assert_eq!(found, vec!["net6.0".to_string(), "net8.0".to_string()]);
            }
            other => panic!("expected MultipleOrNoTargets, got {other:?}"),
        }
    }

    #[test]
    fn no_tfms_errors() {
        let assets = build_assets(BTreeMap::new());
        match enumerate_one(&assets, Path::new("/tmp")) {
            Err(ProjectAssetsError::MultipleOrNoTargets { found }) => {
                assert!(found.is_empty());
            }
            other => panic!("expected MultipleOrNoTargets, got {other:?}"),
        }
    }

    #[test]
    fn rid_qualified_target_does_not_count_as_extra_tfm() {
        // When restored with a RuntimeIdentifier, NuGet writes both
        // `net8.0` and `net8.0/osx-arm64` as targets but only `net8.0`
        // in project.frameworks. The gate should pass and the bare
        // TFM's target should be used.
        let mut pkg_compile = BTreeMap::new();
        pkg_compile.insert(
            "lib/net8.0/Bare.dll".to_string(),
            serde_json::Value::Object(Default::default()),
        );
        let mut bare_target = BTreeMap::new();
        bare_target.insert(
            "Bare/1.0.0".to_string(),
            RawTargetEntry {
                kind: "package".to_string(),
                compile: Some(pkg_compile),
                framework: None,
            },
        );

        let mut rid_compile = BTreeMap::new();
        rid_compile.insert(
            "lib/net8.0/Rid.dll".to_string(),
            serde_json::Value::Object(Default::default()),
        );
        let mut rid_target = BTreeMap::new();
        rid_target.insert(
            "Bare/1.0.0".to_string(),
            RawTargetEntry {
                kind: "package".to_string(),
                compile: Some(rid_compile),
                framework: None,
            },
        );

        let mut targets = BTreeMap::new();
        targets.insert("net8.0".to_string(), bare_target);
        targets.insert("net8.0/osx-arm64".to_string(), rid_target);
        let mut assets = build_assets(targets);
        assets.libraries.insert(
            "Bare/1.0.0".to_string(),
            RawLibrary {
                kind: "package".to_string(),
                path: Some("bare/1.0.0".to_string()),
                msbuild_project: None,
            },
        );

        let refs = enumerate_one(&assets, Path::new("/tmp")).expect("RID target must not error");
        let dlls: Vec<&Path> = refs
            .iter()
            .filter_map(|r| match r {
                Reference::PackageDll { candidates } => Some(candidates[0].as_path()),
                _ => None,
            })
            .collect();
        // Only the bare TFM's target was consulted; the RID-qualified one
        // would have produced Rid.dll instead.
        assert_eq!(
            dlls,
            vec![Path::new("/pkgs/bare/1.0.0/lib/net8.0/Bare.dll")]
        );
    }

    #[test]
    fn netstandard_alias_tfm_falls_back_to_sole_target() {
        // F# netstandard libraries hit this every time: project.frameworks
        // lists the short alias `netstandard2.0`, but `targets` has only the
        // long moniker `.NETStandard,Version=v2.0`. With no fallback, every
        // netstandard library would fail to enumerate. Verify the sole bare
        // target is picked up.
        let mut pkg_compile = BTreeMap::new();
        pkg_compile.insert(
            "lib/netstandard2.0/Foo.dll".to_string(),
            serde_json::Value::Object(Default::default()),
        );
        let mut target = BTreeMap::new();
        target.insert(
            "Foo/1.0.0".to_string(),
            RawTargetEntry {
                kind: "package".to_string(),
                compile: Some(pkg_compile),
                framework: None,
            },
        );

        let mut targets = BTreeMap::new();
        targets.insert(".NETStandard,Version=v2.0".to_string(), target);
        let mut assets = build_assets(targets);
        // build_assets won't have populated frameworks with our long-moniker
        // key (it skipped `,`-containing keys via its no-`/` check, but
        // `.NETStandard,Version=v2.0` has no `/`, so build_assets includes
        // it). Replace frameworks with the short alias instead.
        let mut frameworks = BTreeMap::new();
        frameworks.insert(
            "netstandard2.0".to_string(),
            RawProjectFramework {
                framework_references: BTreeMap::new(),
            },
        );
        assets.project = RawProject { frameworks };
        assets.libraries.insert(
            "Foo/1.0.0".to_string(),
            RawLibrary {
                kind: "package".to_string(),
                path: Some("foo/1.0.0".to_string()),
                msbuild_project: None,
            },
        );

        let refs = enumerate_one(&assets, Path::new("/tmp")).expect("alias fallback must succeed");
        let dlls: Vec<&Path> = refs
            .iter()
            .filter_map(|r| match r {
                Reference::PackageDll { candidates } => Some(candidates[0].as_path()),
                _ => None,
            })
            .collect();
        assert_eq!(
            dlls,
            vec![Path::new("/pkgs/foo/1.0.0/lib/netstandard2.0/Foo.dll")]
        );
    }

    #[test]
    fn target_missing_and_no_unique_bare_target_errors() {
        // Two non-matching bare targets and no key match: ambiguous, error.
        let mut targets = BTreeMap::new();
        targets.insert("net6.0".to_string(), BTreeMap::new());
        targets.insert("net7.0".to_string(), BTreeMap::new());
        let mut assets = build_assets(targets);
        // Force the project framework to be a single TFM that doesn't
        // match any target key; build_assets would otherwise produce
        // two frameworks.
        let mut frameworks = BTreeMap::new();
        frameworks.insert(
            "net8.0".to_string(),
            RawProjectFramework {
                framework_references: BTreeMap::new(),
            },
        );
        assets.project = RawProject { frameworks };

        match enumerate_one(&assets, Path::new("/tmp")) {
            Err(ProjectAssetsError::TargetForTfmMissing { tfm, found }) => {
                assert_eq!(tfm, "net8.0");
                assert_eq!(found, vec!["net6.0".to_string(), "net7.0".to_string()]);
            }
            other => panic!("expected TargetForTfmMissing, got {other:?}"),
        }
    }

    #[test]
    fn package_folders_preserve_document_order_through_serde() {
        // Two packageFolders in non-lexicographic JSON order. After
        // deserialising and using `enumerate_one`, the first key (per
        // JSON order) must be the one used to root package DLLs, not
        // the lexicographically smallest. Use raw JSON text rather
        // than the `json!` macro: `serde_json::Value::Object` sorts
        // its keys (unless the `preserve_order` feature is on), so
        // round-tripping through Value would lose the property we
        // want to assert. Direct `from_str` parses character-by-
        // character and preserves order via MapAccess.
        let json = r#"{
            "version": 3,
            "targets": {
                "net8.0": {
                    "Foo/1.0.0": {
                        "type": "package",
                        "compile": { "lib/net8.0/Foo.dll": {} }
                    }
                }
            },
            "libraries": {
                "Foo/1.0.0": { "type": "package", "path": "foo/1.0.0" }
            },
            "packageFolders": {
                "/zzz-primary/": {},
                "/aaa-fallback/": {}
            },
            "project": { "frameworks": { "net8.0": {} } }
        }"#;
        let assets: RawAssets = serde_json::from_str(json).unwrap();
        assert_eq!(
            assets.package_folders,
            vec![
                PathBuf::from("/zzz-primary/"),
                PathBuf::from("/aaa-fallback/")
            ]
        );

        let refs = enumerate_one(&assets, Path::new("/tmp")).unwrap();
        // Each PackageDll has one candidate per packageFolder, in
        // the same JSON order: primary first, fallback second. The
        // shell layer picks the first existing one (or falls back
        // to the primary), but at the enumerate layer we just expose
        // the candidates.
        let candidates: Vec<&[PathBuf]> = refs
            .iter()
            .filter_map(|r| match r {
                Reference::PackageDll { candidates } => Some(candidates.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(candidates.len(), 1, "one Foo.dll expected");
        assert_eq!(
            candidates[0],
            &[
                PathBuf::from("/zzz-primary/foo/1.0.0/lib/net8.0/Foo.dll"),
                PathBuf::from("/aaa-fallback/foo/1.0.0/lib/net8.0/Foo.dll"),
            ]
        );
    }

    #[test]
    fn project_ref_resolves_relative_to_project_dir() {
        let mut target = BTreeMap::new();
        target.insert(
            "Other/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: Some("net8.0".to_string()),
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0".to_string(), target);
        let mut assets = build_assets(targets);
        assets.libraries.insert(
            "Other/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: Some("../Other/Other.fsproj".to_string()),
                msbuild_project: None,
            },
        );

        // Project dir is one level above the assets file dir.
        let refs = enumerate_one(&assets, Path::new("/repo/MyProj/obj")).unwrap();
        assert_eq!(
            refs,
            vec![Reference::ProjectRef {
                project_path: PathBuf::from("/repo/MyProj/../Other/Other.fsproj"),
                tfm: "net8.0".to_string(),
                assembly_name: None,
            }]
        );
    }

    #[test]
    fn project_ref_prefers_msbuild_project_over_path() {
        // NuGet lowercases the directory in `path` for project libraries
        // but preserves the on-disk case in `msbuildProject`. The
        // resolver must follow `msbuildProject` so the resulting path
        // is correct on case-sensitive filesystems.
        let mut target = BTreeMap::new();
        target.insert(
            "Other/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: Some("net8.0".to_string()),
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0".to_string(), target);
        let mut assets = build_assets(targets);
        assets.libraries.insert(
            "Other/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: Some("../mixedcase/Other.fsproj".to_string()),
                msbuild_project: Some("../MixedCase/Other.fsproj".to_string()),
            },
        );

        let refs = enumerate_one(&assets, Path::new("/repo/MyProj/obj")).unwrap();
        assert_eq!(
            refs,
            vec![Reference::ProjectRef {
                project_path: PathBuf::from("/repo/MyProj/../MixedCase/Other.fsproj"),
                tfm: "net8.0".to_string(),
                assembly_name: None,
            }]
        );
    }

    #[test]
    fn project_ref_extracts_evaluated_assembly_name_from_compile_asset() {
        // NuGet records the producer's *evaluated* output name as the
        // project-kind entry's compile asset (`bin/placeholder/<name>.dll`),
        // so an `<AssemblyName>` override surfaces here — the name need not
        // match the project-file stem.
        let mut compile = BTreeMap::new();
        compile.insert(
            "bin/placeholder/RenamedOutput.dll".to_string(),
            serde_json::Value::Object(Default::default()),
        );
        let mut target = BTreeMap::new();
        target.insert(
            "Other/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: Some(compile),
                framework: Some("net8.0".to_string()),
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0".to_string(), target);
        let mut assets = build_assets(targets);
        assets.libraries.insert(
            "Other/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: Some("../Other/Other.fsproj".to_string()),
                msbuild_project: None,
            },
        );

        let refs = enumerate_one(&assets, Path::new("/repo/MyProj/obj")).unwrap();
        assert_eq!(
            refs,
            vec![Reference::ProjectRef {
                project_path: PathBuf::from("/repo/MyProj/../Other/Other.fsproj"),
                tfm: "net8.0".to_string(),
                assembly_name: Some("RenamedOutput".to_string()),
            }]
        );
    }

    #[test]
    fn project_assembly_name_refuses_placeholder_and_ambiguity() {
        let entry = |keys: &[&str]| RawTargetEntry {
            kind: "project".to_string(),
            compile: Some(
                keys.iter()
                    .map(|k| (k.to_string(), serde_json::Value::Object(Default::default())))
                    .collect::<BTreeMap<_, _>>(),
            ),
            framework: Some("net8.0".to_string()),
        };
        // A `_._` placeholder is not an output name.
        assert_eq!(
            project_assembly_name(&entry(&["bin/placeholder/_._"])),
            None
        );
        // Two distinct stems: refuse to guess.
        assert_eq!(
            project_assembly_name(&entry(&["bin/placeholder/A.dll", "bin/placeholder/B.dll"])),
            None
        );
        // The same stem twice (e.g. two asset paths for one output) is fine.
        assert_eq!(
            project_assembly_name(&entry(&["bin/one/A.dll", "bin/two/A.dll"])),
            Some("A".to_string())
        );
        // No compile section at all.
        assert_eq!(
            project_assembly_name(&RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: Some("net8.0".to_string()),
            }),
            None
        );
    }

    #[test]
    fn project_ref_missing_framework_field_errors() {
        // The framework field is mandatory on project-kind target entries
        // because the multi-TFM sidecar path can't pick the producer's
        // workspace without it. Absence must be an error, not a silent
        // fallback to the consumer's TFM (which would give wrong results
        // exactly in the multi-TFM scenario we care about).
        let mut target = BTreeMap::new();
        target.insert(
            "Other/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: None,
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0".to_string(), target);
        let mut assets = build_assets(targets);
        assets.libraries.insert(
            "Other/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: Some("../Other/Other.fsproj".to_string()),
                msbuild_project: None,
            },
        );

        match enumerate_one(&assets, Path::new("/repo/MyProj/obj")) {
            Err(ProjectAssetsError::ProjectRefUnresolved { name_version }) => {
                assert_eq!(name_version, "Other/1.0.0");
            }
            other => panic!("expected ProjectRefUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn platform_qualified_consumer_records_base_producer_tfm() {
        // Empirically observed `dotnet restore` behaviour: when a
        // `net8.0-windows` consumer references a `net8.0-windows`
        // producer, NuGet writes the producer's `framework` field as
        // `.NETCoreApp,Version=v8.0` only — no `-windows` suffix. The
        // consumer's TFM key (`net8.0-windows7.0`) preserves the
        // platform, but the project library's framework field does not.
        //
        // This test pins the behaviour so phase 3 (the cross-reference
        // step that lifts the producer's full TFM by consulting its
        // own `project.frameworks`) can identify the precise locus to
        // change. Until then, `tfm` records the *base* moniker only.
        let mut target = BTreeMap::new();
        target.insert(
            "Lib/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: Some(".NETCoreApp,Version=v8.0".to_string()),
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0-windows7.0".to_string(), target);
        let mut assets = build_assets(targets);
        // build_assets seeded `project.frameworks` from the targets
        // keys; that's already correct here (consumer TFM is the
        // platform-qualified one).
        assets.libraries.insert(
            "Lib/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: Some("../Lib/Lib.csproj".to_string()),
                msbuild_project: Some("../Lib/Lib.csproj".to_string()),
            },
        );

        let refs = enumerate_one(&assets, Path::new("/repo/App/obj")).unwrap();
        match refs.as_slice() {
            [Reference::ProjectRef { tfm, .. }] => {
                assert_eq!(
                    tfm, "net8.0",
                    "phase 1 records the base TFM only — recovering the platform suffix is a phase-3 cross-reference"
                );
            }
            other => panic!("expected one ProjectRef, got {other:?}"),
        }
    }

    #[test]
    fn fixture_with_long_framework_moniker_converts_to_short() {
        // multi_tfm_proj_ref.json has a project ref whose `framework`
        // is `.NETStandard,Version=v2.0`. After enumeration the
        // ProjectRef must carry the short alias.
        let assets = load_fixture("multi_tfm_proj_ref.json");
        let refs = enumerate_one(&assets, Path::new("/repo/Consumer/obj")).unwrap();
        let project_refs: Vec<&Reference> = refs
            .iter()
            .filter(|r| matches!(r, Reference::ProjectRef { .. }))
            .collect();
        assert_eq!(project_refs.len(), 1);
        match project_refs[0] {
            Reference::ProjectRef {
                project_path,
                tfm,
                assembly_name: _,
            } => {
                assert_eq!(tfm, "netstandard2.0");
                assert!(
                    project_path.ends_with("OtherProject/OtherProject.fsproj"),
                    "msbuildProject casing must be preserved: {}",
                    project_path.display()
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn placeholder_compile_entries_are_skipped() {
        let mut compile = BTreeMap::new();
        compile.insert(
            "lib/net8.0/_._".to_string(),
            serde_json::Value::Object(Default::default()),
        );
        compile.insert(
            "lib/net8.0/Real.dll".to_string(),
            serde_json::Value::Object(Default::default()),
        );
        let mut target = BTreeMap::new();
        target.insert(
            "Foo/1.0.0".to_string(),
            RawTargetEntry {
                kind: "package".to_string(),
                compile: Some(compile),
                framework: None,
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0".to_string(), target);
        let mut assets = build_assets(targets);
        assets.libraries.insert(
            "Foo/1.0.0".to_string(),
            RawLibrary {
                kind: "package".to_string(),
                path: Some("foo/1.0.0".to_string()),
                msbuild_project: None,
            },
        );

        let refs = enumerate_one(&assets, Path::new("/tmp")).unwrap();
        let dlls: Vec<&Path> = refs
            .iter()
            .filter_map(|r| match r {
                Reference::PackageDll { candidates } => Some(candidates[0].as_path()),
                _ => None,
            })
            .collect();
        assert_eq!(dlls, vec![Path::new("/pkgs/foo/1.0.0/lib/net8.0/Real.dll")]);
    }

    // ---- proptest generators ----

    fn arb_id() -> impl Strategy<Value = String> {
        "[A-Z][a-z]{0,5}"
    }

    fn arb_version() -> impl Strategy<Value = String> {
        (0u32..5, 0u32..10, 0u32..50).prop_map(|(a, b, c)| format!("{a}.{b}.{c}"))
    }

    fn arb_compile_key(idx: usize, with_placeholder: bool) -> String {
        if with_placeholder {
            "lib/net8.0/_._".to_string()
        } else {
            format!("lib/net8.0/A{idx}.dll")
        }
    }

    fn arb_package_entry(
        idx: usize,
    ) -> impl Strategy<Value = (String, RawTargetEntry, RawLibrary)> {
        (
            arb_id(),
            arb_version(),
            0usize..4,
            proptest::bool::weighted(0.3),
        )
            .prop_map(move |(name, version, n_dlls, include_placeholder)| {
                let name_version = format!("{name}{idx}/{version}");
                let mut compile = BTreeMap::new();
                for d in 0..n_dlls {
                    compile.insert(
                        arb_compile_key(d, false),
                        serde_json::Value::Object(Default::default()),
                    );
                }
                if include_placeholder {
                    compile.insert(
                        arb_compile_key(0, true),
                        serde_json::Value::Object(Default::default()),
                    );
                }
                let entry = RawTargetEntry {
                    kind: "package".to_string(),
                    compile: Some(compile),
                    framework: None,
                };
                let library = RawLibrary {
                    kind: "package".to_string(),
                    path: Some(format!("{}/{}", name.to_lowercase(), version)),
                    msbuild_project: None,
                };
                (name_version, entry, library)
            })
    }

    fn arb_project_entry(
        idx: usize,
    ) -> impl Strategy<Value = (String, RawTargetEntry, RawLibrary)> {
        // Pick from a mix of short and long forms so the type_filter
        // property exercises long→short conversion as well as straight
        // pass-through. Picking deterministically per `idx` keeps each
        // generated assets file stable (no proptest-state-dependence on
        // tfm).
        (arb_id(), arb_version()).prop_map(move |(name, version)| {
            let name_version = format!("P{name}{idx}/{version}");
            let framework = match idx % 3 {
                0 => "net8.0",
                1 => ".NETStandard,Version=v2.0",
                _ => ".NETCoreApp,Version=v3.1",
            };
            let entry = RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: Some(framework.to_string()),
            };
            let library = RawLibrary {
                kind: "project".to_string(),
                path: Some(format!("../{name}{idx}/{name}{idx}.fsproj")),
                msbuild_project: None,
            };
            (name_version, entry, library)
        })
    }

    /// Build a `RawAssets` with a given number of TFMs (`tfm_count`). When
    /// `tfm_count == 1`, the target contains the generated packages and
    /// project refs; for other counts, targets are empty (we only need
    /// non-one counts to exercise the gate).
    fn arb_assets(tfm_count: usize) -> impl Strategy<Value = RawAssets> {
        let pkg_strat = proptest::collection::vec(arb_package_entry(0), 0..3);
        let proj_strat = proptest::collection::vec(arb_project_entry(0), 0..2);
        let fw_strat = proptest::collection::vec(arb_id(), 0..2);

        (pkg_strat, proj_strat, fw_strat).prop_map(move |(pkgs, projs, fws)| {
            let mut libraries = BTreeMap::new();
            let mut target = BTreeMap::new();
            for (i, (nv, entry, lib)) in pkgs.into_iter().enumerate() {
                let nv = format!("Pkg{i}{nv}");
                target.insert(nv.clone(), entry);
                libraries.insert(nv, lib);
            }
            for (i, (nv, entry, lib)) in projs.into_iter().enumerate() {
                let nv = format!("Proj{i}{nv}");
                target.insert(nv.clone(), entry);
                libraries.insert(nv, lib);
            }

            let mut framework_references = BTreeMap::new();
            for (i, name) in fws.into_iter().enumerate() {
                framework_references.insert(
                    format!("Fw{i}{name}"),
                    serde_json::Value::Object(Default::default()),
                );
            }

            let mut targets = BTreeMap::new();
            let mut frameworks = BTreeMap::new();
            for i in 0..tfm_count {
                let tfm = format!("net{}.0", 6 + i);
                targets.insert(
                    tfm.clone(),
                    if i == 0 {
                        target.clone()
                    } else {
                        BTreeMap::new()
                    },
                );
                frameworks.insert(
                    tfm,
                    RawProjectFramework {
                        framework_references: if i == 0 {
                            framework_references.clone()
                        } else {
                            BTreeMap::new()
                        },
                    },
                );
            }

            let package_folders = vec![PathBuf::from("/cache/")];

            RawAssets {
                version: 3,
                targets,
                libraries,
                package_folders,
                project: RawProject { frameworks },
            }
        })
    }

    // ---- properties ----

    proptest! {
        #[test]
        fn roundtrip(assets in arb_assets(1)) {
            let serialised = serde_json::to_string(&assets).unwrap();
            let parsed: RawAssets = serde_json::from_str(&serialised).unwrap();
            prop_assert_eq!(parsed, assets);
        }

        #[test]
        fn tfm_gate(
            input in (0usize..4).prop_flat_map(|n| arb_assets(n).prop_map(move |a| (n, a)))
        ) {
            let (tfm_count, assets) = input;
            let result = enumerate_one(&assets, Path::new("/tmp"));
            if tfm_count == 1 {
                prop_assert!(result.is_ok(), "expected Ok for one TFM, got {:?}", result);
            } else {
                prop_assert!(
                    matches!(result, Err(ProjectAssetsError::MultipleOrNoTargets { .. })),
                    "expected MultipleOrNoTargets for {} TFMs, got {:?}",
                    tfm_count,
                    result
                );
            }
        }

        #[test]
        fn package_paths_rooted_at_package_folder(assets in arb_assets(1)) {
            let refs = enumerate_one(&assets, Path::new("/tmp")).unwrap();
            // Every candidate path on a `PackageDll` must be rooted at
            // one of the configured packageFolders, in the same order.
            for r in &refs {
                if let Reference::PackageDll { candidates } = r {
                    prop_assert_eq!(candidates.len(), assets.package_folders.len());
                    for (cand, folder) in candidates.iter().zip(assets.package_folders.iter()) {
                        prop_assert!(
                            cand.starts_with(folder),
                            "{} not under {}",
                            cand.display(),
                            folder.display()
                        );
                    }
                }
            }
        }

        #[test]
        fn placeholder_never_leaks(assets in arb_assets(1)) {
            let refs = enumerate_one(&assets, Path::new("/tmp")).unwrap();
            for r in &refs {
                if let Reference::PackageDll { candidates } = r {
                    for cand in candidates {
                        prop_assert!(
                            cand.file_name().is_none_or(|n| n != "_._"),
                            "_._ leaked into output: {}",
                            cand.display()
                        );
                    }
                }
            }
        }

        #[test]
        fn type_filter(assets in arb_assets(1)) {
            let refs = enumerate_one(&assets, Path::new("/tmp")).unwrap();
            let (_tfm, target) = single_target(&assets).unwrap();
            for r in &refs {
                match r {
                    Reference::PackageDll { candidates } => {
                        // Reconstruct which library this came from: the first candidate
                        // path must contain a libraries[k].path component for some k
                        // whose target entry has kind "package".
                        let abs_str = candidates[0].to_string_lossy().to_string();
                        let mut matched_package = false;
                        for (nv, entry) in target {
                            if entry.kind != "package" { continue; }
                            let Some(lib_path) = assets.libraries.get(nv).and_then(|l| l.path.as_deref()) else {
                                continue;
                            };
                            if abs_str.contains(lib_path) {
                                matched_package = true;
                                break;
                            }
                        }
                        prop_assert!(matched_package, "PackageDll {} not traceable to a package-typed library", abs_str);
                    }
                    Reference::ProjectRef { project_path, tfm: produced_tfm, assembly_name: _ } => {
                        let proj_str = project_path.to_string_lossy().to_string();
                        let mut matched_project = false;
                        for (nv, entry) in target {
                            if entry.kind != "project" { continue; }
                            let Some(lib) = assets.libraries.get(nv) else { continue; };
                            let Some(p) = lib.path.as_deref() else { continue; };
                            if proj_str.ends_with(p) {
                                matched_project = true;
                                // The produced tfm must equal the long→short
                                // conversion of whatever was in the target
                                // entry's `framework` field; absence is an
                                // error the enumerator would have surfaced
                                // before we got here.
                                let raw = entry.framework.as_deref().expect("project entry must have framework");
                                prop_assert_eq!(produced_tfm, &super::tfm::long_to_short(raw));
                                break;
                            }
                        }
                        prop_assert!(matched_project, "ProjectRef {} not traceable to a project-typed library", proj_str);
                    }
                    Reference::Framework { .. } => {}
                }
            }
        }

        #[test]
        fn determinism(assets in arb_assets(1)) {
            let a = enumerate_one(&assets, Path::new("/tmp")).unwrap();
            let b = enumerate_one(&assets, Path::new("/tmp")).unwrap();
            prop_assert_eq!(a, b);
        }
    }
}
