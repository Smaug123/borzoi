//! Closure-wide TFM mapping for multi-TFM dispatch.
//!
//! When a consumer csproj has selected a TFM and the LSP needs to ask
//! the C# sidecar to build the closure, every csproj in that closure
//! must be built under the producer TFM NuGet's restore selected for it
//! — not under the consumer's TFM. The map produced here keys each
//! closure node's absolute csproj path to its short-form TFM and is
//! consumed by the sidecar protocol layer (Phase 3 of
//! `docs/completed/multi-tfm-resolution-plan.md`).
//!
//! Phase 2b cross-references each producer's own declared TFMs to
//! recover the platform suffix that the consumer's assets file does not
//! carry: NuGet writes the `framework` field on a project-kind target
//! entry as the *base* moniker only (e.g. `.NETCoreApp,Version=v8.0`
//! even for a `net8.0-windows` ⇒ `net8.0-windows` reference). Callers
//! supply the producer's declared TFM list as part of the input map and
//! `pick_producer_tfm` (module-private) picks the most-specific match.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::project_assets::enumerate::lookup_target_for_tfm;
use crate::project_assets::error::ProjectAssetsError;
use crate::project_assets::raw::RawAssets;
use crate::project_assets::tfm;

/// Given a producer's *base* TFM (the long→short conversion of the
/// `framework` field on a project-kind target entry), the consumer's
/// own TFM, and the producer's own declared TFM list, return the
/// producer's full short-form TFM with any platform suffix recovered.
///
/// Algorithm:
///
/// 1. Filter the producer's declared TFMs to those whose base equals
///    `base_producer_tfm`.
/// 2. If zero remain, NuGet's restore can't have succeeded under the
///    current producer manifest — error with [`RestoreMismatch`].
/// 3. Otherwise prefer an exact-suffix match (treating bare consumers
///    and bare candidates as `None == None`).
/// 4. If no exact suffix matches *and* no candidate shares the
///    consumer's platform family, fall back to the bare (suffix-less)
///    candidate: NuGet treats `net8.0` as a compatible producer for
///    any platform-qualified `net8.0-X` consumer, so a
///    `net8.0-android` consumer referencing a producer declaring
///    `[net8.0, net8.0-windows]` correctly picks `net8.0`.
/// 5. Otherwise error with [`RestoreMismatch`]. This catches:
///    * stale-restore: the only candidate has an incompatible suffix
///      (e.g. consumer `net8.0` + sole candidate `net8.0-windows`);
///    * version-skew: a same-family different-version candidate exists
///      but we don't model platform-version compatibility yet.
///
/// # Known limitation: platform-version compatibility
///
/// NuGet's actual compatibility rule allows consumer-side platform
/// *versions* to be higher than the producer's (e.g. a
/// `net8.0-windows10.0.19041.0` consumer is compatible with a
/// `net8.0-windows7.0` producer). This helper performs *exact* suffix
/// equality only — it can't pick a same-family different-version
/// candidate. To avoid silently selecting a less-specific bare producer
/// when a version-skew candidate exists, the bare fallback is *gated*
/// by `platform_family` membership: if any candidate's suffix shares
/// the consumer's platform family, the helper refuses the bare fallback
/// and surfaces [`RestoreMismatch`]. A future phase will lift the
/// version-compat restriction by parsing platform name + version
/// separately.
///
/// [`RestoreMismatch`]: ProjectAssetsError::RestoreMismatch
pub(super) fn pick_producer_tfm(
    base_producer_tfm: &str,
    consumer_tfm: &str,
    producer_declared: &[String],
    producer_path: &Path,
) -> Result<String, ProjectAssetsError> {
    let candidates: Vec<&str> = producer_declared
        .iter()
        .map(String::as_str)
        .filter(|t| tfm::split_platform(t).0 == base_producer_tfm)
        .collect();

    if candidates.is_empty() {
        return Err(ProjectAssetsError::RestoreMismatch {
            producer_path: producer_path.to_path_buf(),
            consumer_tfm: consumer_tfm.to_string(),
            base_producer_tfm: base_producer_tfm.to_string(),
            producer_declared: producer_declared.to_vec(),
        });
    }

    let consumer_suffix = tfm::split_platform(consumer_tfm).1;
    // Exact suffix match (treats `None == None` as the bare-consumer
    // case): always safe whether there's one candidate or many.
    if let Some(t) = candidates
        .iter()
        .find(|t| tfm::split_platform(t).1 == consumer_suffix)
    {
        return Ok((*t).to_string());
    }
    // Bare fallback is safe only when no candidate shares the
    // consumer's platform family: e.g. `net8.0-android` consumer
    // with `[net8.0, net8.0-windows]` picks bare (windows is the
    // wrong platform). But for `net8.0-windows10.0` consumer with
    // `[net8.0, net8.0-windows7.0]`, NuGet would pick the
    // windows version-compat producer — we can't, since we don't
    // model version compat, so surface `RestoreMismatch` rather
    // than silently dispatching the bare producer.
    let consumer_family = consumer_suffix.map(tfm::platform_family);
    let same_family_candidate_exists = consumer_family.is_some_and(|fam| {
        candidates
            .iter()
            .filter_map(|t| tfm::split_platform(t).1)
            .any(|s| tfm::platform_family(s) == fam)
    });
    if !same_family_candidate_exists
        && let Some(t) = candidates
            .iter()
            .find(|t| tfm::split_platform(t).1.is_none())
    {
        return Ok((*t).to_string());
    }
    Err(ProjectAssetsError::RestoreMismatch {
        producer_path: producer_path.to_path_buf(),
        consumer_tfm: consumer_tfm.to_string(),
        base_producer_tfm: base_producer_tfm.to_string(),
        producer_declared: producer_declared.to_vec(),
    })
}

/// Build a map from each closure node's absolute csproj path to the
/// short-form TFM NuGet's restore selected for it, with platform
/// suffixes recovered from each producer's own declared TFMs.
///
/// `top_assets` is the parsed `project.assets.json` for the consumer.
/// `consumer_tfm` is the TFM the LSP has chosen for the consumer
/// (short form, e.g. `net10.0`). `top_project_path` is the absolute
/// path to the consumer's `.csproj`/`.fsproj`; it appears in the result
/// map under `consumer_tfm` so the caller has one uniform place to
/// look up every node — top included.
///
/// `producer_declared_tfms` is keyed by each producer csproj's absolute
/// path (matching how this helper computes the path:
/// `top_project_path.parent().unwrap().join(rel)`, where `rel` is the
/// producer's `msbuildProject` or fallback `path` entry). The value is
/// that producer's declared short-form TFM list (its
/// `project.frameworks` keys). The platform-suffix recovery walks this
/// map; see `pick_producer_tfm` (module-private) for the matching rules.
///
/// Returns a `BTreeMap` for deterministic iteration order in tests and
/// in the wire protocol.
///
/// # Errors
///
/// - `TargetForTfmMissing` when `targets` has neither a direct match for
///   `consumer_tfm` nor exactly one bare target. The netstandard-alias
///   case (project.frameworks says `netstandard2.0`, targets says
///   `.NETStandard,Version=v2.0`) is the only fallback this helper
///   indulges; anything else means the LSP's chosen TFM disagrees with
///   what restore produced, which is the user's signal to re-run
///   `dotnet restore`.
/// - `LibraryEntryMissing` when a target entry has no matching
///   `libraries` entry.
/// - `ProjectRefMissingPath` when a project-kind library has neither
///   `msbuildProject` nor `path`.
/// - `ProjectRefUnresolved` when a project-kind target entry has no
///   `framework` field. Same recovery as in `enumerate_one`: refuse to
///   guess, surface the staleness.
/// - `ProducerAssetsNotProvided` when the producer-declared-TFMs map
///   lacks an entry for a project-kind closure node. Caller bug.
/// - `RestoreMismatch` when the cross-reference between the base
///   producer TFM and the producer's declared TFMs fails — see
///   `pick_producer_tfm` (module-private) for the details.
pub fn transitive_project_tfms(
    top_assets: &RawAssets,
    consumer_tfm: &str,
    top_project_path: &Path,
    producer_declared_tfms: &BTreeMap<PathBuf, Vec<String>>,
) -> Result<BTreeMap<PathBuf, String>, ProjectAssetsError> {
    let target = lookup_target_for_tfm(top_assets, consumer_tfm)?;
    let project_dir = top_project_path
        .parent()
        .expect("top csproj path has a parent directory");

    let mut out = BTreeMap::new();
    out.insert(top_project_path.to_path_buf(), consumer_tfm.to_string());

    for (name_version, entry) in target {
        if entry.kind != "project" {
            continue;
        }
        let library = top_assets.libraries.get(name_version).ok_or_else(|| {
            ProjectAssetsError::LibraryEntryMissing {
                name_version: name_version.clone(),
            }
        })?;
        // Prefer `msbuildProject` over `path`: NuGet lowercases the
        // directory portion of `path` for project libraries, which gives
        // a non-existent path on case-sensitive filesystems when the
        // actual directory has mixed case.
        let rel = library
            .msbuild_project
            .as_deref()
            .or(library.path.as_deref())
            .ok_or_else(|| ProjectAssetsError::ProjectRefMissingPath {
                name_version: name_version.clone(),
            })?;
        let producer_long_tfm =
            entry
                .framework
                .as_deref()
                .ok_or_else(|| ProjectAssetsError::ProjectRefUnresolved {
                    name_version: name_version.clone(),
                })?;
        let producer_path = project_dir.join(rel);
        // The recorded `framework` field is usually the bare base in long
        // form, but defend against an already-short and platform-qualified
        // value (NuGet versions we don't model, or hand edits): strip any
        // platform suffix so the base-match filter in `pick_producer_tfm`
        // is comparing apples to apples.
        let full_producer_tfm = tfm::long_to_short(producer_long_tfm);
        let (base_producer_tfm, _) = tfm::split_platform(&full_producer_tfm);
        let producer_declared = producer_declared_tfms.get(&producer_path).ok_or_else(|| {
            ProjectAssetsError::ProducerAssetsNotProvided {
                producer_path: producer_path.clone(),
            }
        })?;
        let recovered = pick_producer_tfm(
            base_producer_tfm,
            consumer_tfm,
            producer_declared,
            &producer_path,
        )?;
        out.insert(producer_path, recovered);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_assets::raw::{
        RawAssets, RawLibrary, RawProject, RawProjectFramework, RawTargetEntry,
    };
    use proptest::prelude::*;

    fn load_fixture(name: &str) -> RawAssets {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/project_assets")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    /// One row in a synthetic target/libraries pair for tests.
    struct Row {
        name_version: &'static str,
        kind: &'static str,
        framework: Option<&'static str>,
        msbuild_project: Option<&'static str>,
        path: Option<&'static str>,
    }

    /// Build an assets file with the given TFM listed in `targets` and
    /// `project.frameworks`, populated with `rows`.
    fn build_assets_single_tfm(tfm_key: &str, rows: &[Row]) -> RawAssets {
        let mut target = BTreeMap::new();
        let mut libraries = BTreeMap::new();
        for row in rows {
            target.insert(
                row.name_version.to_string(),
                RawTargetEntry {
                    kind: row.kind.to_string(),
                    compile: None,
                    framework: row.framework.map(str::to_string),
                },
            );
            libraries.insert(
                row.name_version.to_string(),
                RawLibrary {
                    kind: row.kind.to_string(),
                    path: row.path.map(str::to_string),
                    msbuild_project: row.msbuild_project.map(str::to_string),
                },
            );
        }

        let mut targets = BTreeMap::new();
        targets.insert(tfm_key.to_string(), target);

        let mut frameworks = BTreeMap::new();
        frameworks.insert(
            tfm_key.to_string(),
            RawProjectFramework {
                framework_references: BTreeMap::new(),
            },
        );

        RawAssets {
            version: 3,
            targets,
            libraries,
            package_folders: vec![PathBuf::from("/pkgs/")],
            project: RawProject { frameworks },
        }
    }

    fn project_row(
        name_version: &'static str,
        framework: Option<&'static str>,
        msbuild_project: Option<&'static str>,
        path: Option<&'static str>,
    ) -> Row {
        Row {
            name_version,
            kind: "project",
            framework,
            msbuild_project,
            path,
        }
    }

    /// Build a `BTreeMap<PathBuf, Vec<String>>` mapping each producer
    /// csproj path to its declared short-form TFM list. The path string
    /// must match the helper's path-computation rule
    /// (`top.parent().unwrap().join(rel)`); we pass the literal
    /// pre-joined form here rather than canonicalising, matching how the
    /// helper itself stores paths (no fs access).
    fn producer_map(entries: &[(&str, &[&str])]) -> BTreeMap<PathBuf, Vec<String>> {
        entries
            .iter()
            .map(|(p, tfms)| {
                (
                    PathBuf::from(*p),
                    tfms.iter().map(|s| (*s).to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn includes_top_csproj_keyed_by_consumer_tfm() {
        // No project refs at all; the result is still non-empty because
        // the top csproj seeds the map.
        let assets = build_assets_single_tfm("net10.0", &[]);
        let top = Path::new("/repo/App/App.fsproj");

        let map = transitive_project_tfms(&assets, "net10.0", top, &producer_map(&[])).unwrap();
        assert_eq!(
            map,
            BTreeMap::from([(top.to_path_buf(), "net10.0".to_string())])
        );
    }

    #[test]
    fn records_producer_tfm_in_short_form() {
        // Long monikers in the assets file must round-trip to short form
        // in the map. This is the headline behaviour the helper provides.
        let assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETStandard,Version=v2.0"),
                Some("../Leaf/Leaf.csproj"),
                Some("../leaf/Leaf.csproj"),
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");

        let producers = producer_map(&[("/repo/App/../Leaf/Leaf.csproj", &["netstandard2.0"])]);
        let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
        let expected = BTreeMap::from([
            (top.to_path_buf(), "net10.0".to_string()),
            (
                PathBuf::from("/repo/App/../Leaf/Leaf.csproj"),
                "netstandard2.0".to_string(),
            ),
        ]);
        assert_eq!(map, expected);
    }

    #[test]
    fn skips_package_kind_entries() {
        let assets = build_assets_single_tfm(
            "net10.0",
            &[
                Row {
                    name_version: "Pkg/1.0.0",
                    kind: "package",
                    framework: None,
                    msbuild_project: None,
                    path: Some("pkg/1.0.0"),
                },
                project_row(
                    "Proj/1.0.0",
                    Some("net8.0"),
                    Some("../Proj/Proj.fsproj"),
                    None,
                ),
            ],
        );
        let top = Path::new("/repo/App/App.fsproj");

        let producers = producer_map(&[("/repo/App/../Proj/Proj.fsproj", &["net8.0"])]);
        let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
        // Only the top + the project ref; the package was ignored.
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(Path::new("/repo/App/../Proj/Proj.fsproj")),
            Some(&"net8.0".to_string())
        );
    }

    #[test]
    fn prefers_msbuild_project_over_path() {
        // NuGet lowercases the directory in `path` for project libraries.
        // The map must follow `msbuildProject` for case-correctness.
        let assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some("net8.0"),
                Some("../MixedCase/Leaf.csproj"),
                Some("../mixedcase/Leaf.csproj"),
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");

        let producers = producer_map(&[("/repo/App/../MixedCase/Leaf.csproj", &["net8.0"])]);
        let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
        assert!(map.contains_key(Path::new("/repo/App/../MixedCase/Leaf.csproj")));
        assert!(!map.contains_key(Path::new("/repo/App/../mixedcase/Leaf.csproj")));
    }

    #[test]
    fn falls_back_to_path_when_msbuild_project_absent() {
        let assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some("net8.0"),
                None,
                Some("../Leaf/Leaf.csproj"),
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");

        let producers = producer_map(&[("/repo/App/../Leaf/Leaf.csproj", &["net8.0"])]);
        let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
        assert!(map.contains_key(Path::new("/repo/App/../Leaf/Leaf.csproj")));
    }

    #[test]
    fn missing_framework_field_is_unresolved() {
        let assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                None,
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");

        match transitive_project_tfms(&assets, "net10.0", top, &producer_map(&[])) {
            Err(ProjectAssetsError::ProjectRefUnresolved { name_version }) => {
                assert_eq!(name_version, "Leaf/1.0.0");
            }
            other => panic!("expected ProjectRefUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn missing_library_entry_errors() {
        // Target lists a project-kind entry but no matching libraries
        // entry — assets file is malformed; refuse to guess.
        let mut assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some("net8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        assets.libraries.remove("Leaf/1.0.0");

        match transitive_project_tfms(
            &assets,
            "net10.0",
            Path::new("/repo/App/App.fsproj"),
            &producer_map(&[]),
        ) {
            Err(ProjectAssetsError::LibraryEntryMissing { name_version }) => {
                assert_eq!(name_version, "Leaf/1.0.0");
            }
            other => panic!("expected LibraryEntryMissing, got {other:?}"),
        }
    }

    #[test]
    fn library_without_path_or_msbuild_project_errors() {
        let mut assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some("net8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        // Strip both path fields.
        let entry = assets.libraries.get_mut("Leaf/1.0.0").unwrap();
        entry.path = None;
        entry.msbuild_project = None;

        match transitive_project_tfms(
            &assets,
            "net10.0",
            Path::new("/repo/App/App.fsproj"),
            &producer_map(&[]),
        ) {
            Err(ProjectAssetsError::ProjectRefMissingPath { name_version }) => {
                assert_eq!(name_version, "Leaf/1.0.0");
            }
            other => panic!("expected ProjectRefMissingPath, got {other:?}"),
        }
    }

    #[test]
    fn stale_single_tfm_restore_does_not_silently_match() {
        // Regression: a stale single-TFM restore where assets has only
        // `net8.0` and the LSP picks `net9.0` would previously silently
        // enumerate the `net8.0` graph and tag the top csproj as `net9.0`,
        // because the old "sole bare target" fallback didn't check that
        // the target's TFM actually matched. The lookup must surface a
        // mismatch so the user re-runs `dotnet restore`.
        let assets = build_assets_single_tfm("net8.0", &[]);

        match transitive_project_tfms(
            &assets,
            "net9.0",
            Path::new("/repo/App/App.fsproj"),
            &producer_map(&[]),
        ) {
            Err(ProjectAssetsError::TargetForTfmMissing { tfm, found }) => {
                assert_eq!(tfm, "net9.0");
                assert_eq!(found, vec!["net8.0".to_string()]);
            }
            other => panic!("expected TargetForTfmMissing, got {other:?}"),
        }
    }

    #[test]
    fn consumer_tfm_not_in_targets_errors() {
        // Multi-TFM assets: targets has net8.0 and net10.0, caller asks
        // for net9.0. Bare-target fallback doesn't fire (two bare
        // targets), so the error must surface.
        let mut assets = build_assets_single_tfm("net8.0", &[]);
        assets
            .targets
            .insert("net10.0".to_string(), BTreeMap::new());
        assets.project.frameworks.insert(
            "net10.0".to_string(),
            RawProjectFramework {
                framework_references: BTreeMap::new(),
            },
        );

        match transitive_project_tfms(
            &assets,
            "net9.0",
            Path::new("/repo/App/App.fsproj"),
            &producer_map(&[]),
        ) {
            Err(ProjectAssetsError::TargetForTfmMissing { tfm, found }) => {
                assert_eq!(tfm, "net9.0");
                assert_eq!(found, vec!["net10.0".to_string(), "net8.0".to_string()]);
            }
            other => panic!("expected TargetForTfmMissing, got {other:?}"),
        }
    }

    #[test]
    fn netstandard_alias_falls_back_to_sole_bare_target() {
        // Single-TFM netstandard project: project.frameworks says
        // `netstandard2.0`, targets says `.NETStandard,Version=v2.0`.
        // The caller (LSP) passes the short alias; lookup must fall
        // back to the sole bare target.
        let mut target = BTreeMap::new();
        target.insert(
            "Leaf/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                framework: Some(".NETStandard,Version=v2.0".to_string()),
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert(".NETStandard,Version=v2.0".to_string(), target);

        let mut frameworks = BTreeMap::new();
        frameworks.insert(
            "netstandard2.0".to_string(),
            RawProjectFramework {
                framework_references: BTreeMap::new(),
            },
        );

        let mut libraries = BTreeMap::new();
        libraries.insert(
            "Leaf/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: Some("../Leaf/Leaf.fsproj".to_string()),
                msbuild_project: None,
            },
        );

        let assets = RawAssets {
            version: 3,
            targets,
            libraries,
            package_folders: vec![PathBuf::from("/pkgs/")],
            project: RawProject { frameworks },
        };

        let producers = producer_map(&[("/repo/App/../Leaf/Leaf.fsproj", &["netstandard2.0"])]);
        let map = transitive_project_tfms(
            &assets,
            "netstandard2.0",
            Path::new("/repo/App/App.fsproj"),
            &producers,
        )
        .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(Path::new("/repo/App/App.fsproj")),
            Some(&"netstandard2.0".to_string())
        );
        assert_eq!(
            map.get(Path::new("/repo/App/../Leaf/Leaf.fsproj")),
            Some(&"netstandard2.0".to_string())
        );
    }

    #[test]
    fn platform_suffix_recovered_single_producer_declaration() {
        // Headline Phase 2b behaviour: the producer's *only* declared
        // TFM is `net8.0-windows`, the consumer's assets recorded the
        // base `.NETCoreApp,Version=v8.0` (which long→short to
        // `net8.0`), and we recover the `-windows` suffix from the
        // producer's own declaration. The producer's full TFM ends up
        // in the result map verbatim.
        let assets = build_assets_single_tfm(
            "net8.0-windows",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[("/repo/App/../Leaf/Leaf.csproj", &["net8.0-windows"])]);

        let map = transitive_project_tfms(&assets, "net8.0-windows", top, &producers).unwrap();
        assert_eq!(
            map.get(Path::new("/repo/App/../Leaf/Leaf.csproj")),
            Some(&"net8.0-windows".to_string()),
            "platform suffix must be recovered from the producer's declaration"
        );
    }

    #[test]
    fn multi_match_picks_consumer_suffix() {
        // Producer declares both `net8.0` and `net8.0-windows`. The
        // consumer's assets recorded the base `net8.0`, but the
        // consumer's TFM is `net8.0-windows`, so we pick the windows
        // variant to match the consumer's platform.
        let assets = build_assets_single_tfm(
            "net8.0-windows",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[(
            "/repo/App/../Leaf/Leaf.csproj",
            &["net8.0", "net8.0-windows"],
        )]);

        let map = transitive_project_tfms(&assets, "net8.0-windows", top, &producers).unwrap();
        assert_eq!(
            map.get(Path::new("/repo/App/../Leaf/Leaf.csproj")),
            Some(&"net8.0-windows".to_string()),
        );
    }

    #[test]
    fn multi_match_picks_bare_when_consumer_is_bare() {
        // Same producer declarations, but consumer is the bare `net8.0`.
        // We must pick the bare producer entry, not the platform-
        // qualified one — restore would have dispatched the bare
        // producer build for a bare consumer.
        let assets = build_assets_single_tfm(
            "net8.0",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[(
            "/repo/App/../Leaf/Leaf.csproj",
            &["net8.0", "net8.0-windows"],
        )]);

        let map = transitive_project_tfms(&assets, "net8.0", top, &producers).unwrap();
        assert_eq!(
            map.get(Path::new("/repo/App/../Leaf/Leaf.csproj")),
            Some(&"net8.0".to_string()),
        );
    }

    #[test]
    fn no_matching_base_errors_with_restore_mismatch() {
        // Producer declares only `netstandard2.0` but consumer's assets
        // recorded base producer TFM `net8.0`. This can only happen if
        // the producer's `<TargetFrameworks>` was edited since restore;
        // surface `RestoreMismatch` so the user re-runs restore.
        let assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[("/repo/App/../Leaf/Leaf.csproj", &["netstandard2.0"])]);

        match transitive_project_tfms(&assets, "net10.0", top, &producers) {
            Err(ProjectAssetsError::RestoreMismatch {
                producer_path,
                consumer_tfm,
                base_producer_tfm,
                producer_declared,
            }) => {
                assert_eq!(
                    producer_path,
                    PathBuf::from("/repo/App/../Leaf/Leaf.csproj")
                );
                assert_eq!(consumer_tfm, "net10.0");
                assert_eq!(base_producer_tfm, "net8.0");
                assert_eq!(producer_declared, vec!["netstandard2.0".to_string()]);
            }
            other => panic!("expected RestoreMismatch, got {other:?}"),
        }
    }

    #[test]
    fn already_short_platform_qualified_framework_is_normalised() {
        // Defensive case: NuGet *empirically* writes only the bare base
        // moniker into `framework` (long form, e.g.
        // `.NETCoreApp,Version=v8.0`), but assets files written by tools
        // we don't model — or hand edits — might carry a short
        // platform-qualified value like `net8.0-windows7.0` directly.
        // `long_to_short` is a no-op on already-short input, so without
        // suffix-stripping the base-match filter sees `net8.0-windows7.0`
        // as both base and declared and fails to match anything (the
        // declared list always exposes split bases). Strip on the
        // consumer side so the comparison is base-to-base.
        let mut target = BTreeMap::new();
        target.insert(
            "Leaf/1.0.0".to_string(),
            RawTargetEntry {
                kind: "project".to_string(),
                compile: None,
                // Already-short, platform-qualified framework field —
                // exactly the case the defensiveness handles.
                framework: Some("net8.0-windows7.0".to_string()),
            },
        );
        let mut targets = BTreeMap::new();
        targets.insert("net8.0-windows7.0".to_string(), target);

        let mut frameworks = BTreeMap::new();
        frameworks.insert(
            "net8.0-windows7.0".to_string(),
            RawProjectFramework {
                framework_references: BTreeMap::new(),
            },
        );

        let mut libraries = BTreeMap::new();
        libraries.insert(
            "Leaf/1.0.0".to_string(),
            RawLibrary {
                kind: "project".to_string(),
                path: None,
                msbuild_project: Some("../Leaf/Leaf.csproj".to_string()),
            },
        );

        let assets = RawAssets {
            version: 3,
            targets,
            libraries,
            package_folders: vec![PathBuf::from("/pkgs/")],
            project: RawProject { frameworks },
        };

        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[("/repo/App/../Leaf/Leaf.csproj", &["net8.0-windows7.0"])]);

        let map = transitive_project_tfms(&assets, "net8.0-windows7.0", top, &producers).unwrap();
        assert_eq!(
            map.get(Path::new("/repo/App/../Leaf/Leaf.csproj")),
            Some(&"net8.0-windows7.0".to_string()),
        );
    }

    #[test]
    fn platform_consumer_falls_back_to_bare_producer() {
        // Consumer is `net8.0-android`; producer declares both `net8.0`
        // and `net8.0-windows`. NuGet's restore picks the bare `net8.0`
        // (a platform-qualified consumer is compatible with a bare base
        // producer when no exact suffix match exists). The picker must
        // mirror that behaviour rather than surfacing `RestoreMismatch`.
        let assets = build_assets_single_tfm(
            "net8.0-android",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[(
            "/repo/App/../Leaf/Leaf.csproj",
            &["net8.0", "net8.0-windows"],
        )]);

        let map = transitive_project_tfms(&assets, "net8.0-android", top, &producers).unwrap();
        assert_eq!(
            map.get(Path::new("/repo/App/../Leaf/Leaf.csproj")),
            Some(&"net8.0".to_string()),
            "platform consumer with no exact suffix match must fall back to the bare producer entry"
        );
    }

    #[test]
    fn singleton_incompatible_candidate_errors_not_silently_returned() {
        // Regression: a stale restore where the producer's only matching-
        // base declaration is platform-qualified but the consumer is bare
        // (or differently platform-qualified) must surface
        // `RestoreMismatch`. NuGet would have refused to restore this in
        // the first place, so encountering it means the assets file is
        // stale and the resolver must not silently dispatch the wrong
        // producer TFM. Tests both consumer-side cases (bare and
        // platform-mismatched) in one match.
        for consumer in ["net8.0", "net8.0-android"] {
            let assets = build_assets_single_tfm(
                consumer,
                &[project_row(
                    "Leaf/1.0.0",
                    Some(".NETCoreApp,Version=v8.0"),
                    Some("../Leaf/Leaf.csproj"),
                    None,
                )],
            );
            let top = Path::new("/repo/App/App.fsproj");
            let producers = producer_map(&[("/repo/App/../Leaf/Leaf.csproj", &["net8.0-windows"])]);

            match transitive_project_tfms(&assets, consumer, top, &producers) {
                Err(ProjectAssetsError::RestoreMismatch {
                    producer_declared, ..
                }) => {
                    assert_eq!(producer_declared, vec!["net8.0-windows".to_string()]);
                }
                other => panic!("expected RestoreMismatch for consumer {consumer}, got {other:?}",),
            }
        }
    }

    #[test]
    fn platform_version_skew_with_same_family_errors_not_bare() {
        // Regression for the gated bare fallback: consumer is
        // `net8.0-windows10.0.19041.0`; producer declares
        // `[net8.0, net8.0-windows7.0]`. NuGet picks `net8.0-windows7.0`
        // (windows family, compatible version). We don't model version
        // compatibility, so picking bare `net8.0` would silently build
        // the producer for the wrong target framework. Surface
        // `RestoreMismatch` instead so the user knows our resolver is
        // out of its depth.
        let assets = build_assets_single_tfm(
            "net8.0-windows10.0.19041.0",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[(
            "/repo/App/../Leaf/Leaf.csproj",
            &["net8.0", "net8.0-windows7.0"],
        )]);

        match transitive_project_tfms(&assets, "net8.0-windows10.0.19041.0", top, &producers) {
            Err(ProjectAssetsError::RestoreMismatch {
                producer_declared, ..
            }) => {
                assert_eq!(
                    producer_declared,
                    vec!["net8.0".to_string(), "net8.0-windows7.0".to_string()],
                );
            }
            other => panic!("expected RestoreMismatch (version-skew), got {other:?}"),
        }
    }

    #[test]
    fn multi_match_no_consumer_suffix_match_errors() {
        // Producer declares `net8.0-windows` and `net8.0-android` (no
        // bare entry). Consumer is `net8.0-linux`. Neither producer
        // entry matches the consumer's suffix and there is no bare
        // candidate to fall back to; surface `RestoreMismatch`.
        let assets = build_assets_single_tfm(
            "net8.0-linux",
            &[project_row(
                "Leaf/1.0.0",
                Some(".NETCoreApp,Version=v8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");
        let producers = producer_map(&[(
            "/repo/App/../Leaf/Leaf.csproj",
            &["net8.0-windows", "net8.0-android"],
        )]);

        match transitive_project_tfms(&assets, "net8.0-linux", top, &producers) {
            Err(ProjectAssetsError::RestoreMismatch {
                producer_declared, ..
            }) => {
                assert_eq!(
                    producer_declared,
                    vec!["net8.0-windows".to_string(), "net8.0-android".to_string()]
                );
            }
            other => panic!("expected RestoreMismatch, got {other:?}"),
        }
    }

    #[test]
    fn missing_producer_entry_errors() {
        // Caller forgot to populate the producer map for a closure
        // node. Treat as a caller programming error (Phase 3 wiring
        // will load every producer transitively before calling).
        let assets = build_assets_single_tfm(
            "net10.0",
            &[project_row(
                "Leaf/1.0.0",
                Some("net8.0"),
                Some("../Leaf/Leaf.csproj"),
                None,
            )],
        );
        let top = Path::new("/repo/App/App.fsproj");

        // Empty producer map even though there is a project ref.
        match transitive_project_tfms(&assets, "net10.0", top, &producer_map(&[])) {
            Err(ProjectAssetsError::ProducerAssetsNotProvided { producer_path }) => {
                assert_eq!(
                    producer_path,
                    PathBuf::from("/repo/App/../Leaf/Leaf.csproj")
                );
            }
            other => panic!("expected ProducerAssetsNotProvided, got {other:?}"),
        }
    }

    #[test]
    fn top_csproj_does_not_need_producer_entry() {
        // The top is the consumer, not a producer. Its TFM is the
        // caller-supplied `consumer_tfm`. So even with an empty
        // producer map, a top-only closure must succeed.
        let assets = build_assets_single_tfm("net10.0", &[]);
        let top = Path::new("/repo/App/App.fsproj");

        let map = transitive_project_tfms(&assets, "net10.0", top, &producer_map(&[])).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(top), Some(&"net10.0".to_string()));
    }

    #[test]
    fn fixture_multi_tfm_proj_ref_produces_expected_map() {
        // End-to-end pin on the shared fixture so changes to the long→short
        // converter or path-resolution rules show up here.
        let assets = load_fixture("multi_tfm_proj_ref.json");
        let top = Path::new("/repo/Consumer/Consumer.fsproj");

        let producers = producer_map(&[(
            "/repo/Consumer/../OtherProject/OtherProject.fsproj",
            &["netstandard2.0"],
        )]);
        let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
        let expected = BTreeMap::from([
            (top.to_path_buf(), "net10.0".to_string()),
            (
                PathBuf::from("/repo/Consumer/../OtherProject/OtherProject.fsproj"),
                "netstandard2.0".to_string(),
            ),
        ]);
        assert_eq!(map, expected);
    }

    // ---- proptest generators ----

    fn arb_id() -> impl Strategy<Value = String> {
        "[A-Z][a-z]{0,5}"
    }

    fn arb_version() -> impl Strategy<Value = String> {
        (0u32..5, 0u32..10, 0u32..50).prop_map(|(a, b, c)| format!("{a}.{b}.{c}"))
    }

    fn arb_long_tfm() -> impl Strategy<Value = String> {
        prop_oneof![
            (0u32..5, 0u32..10).prop_map(|(a, b)| format!(".NETStandard,Version=v{a}.{b}")),
            (5u32..15, 0u32..10).prop_map(|(a, b)| format!(".NETCoreApp,Version=v{a}.{b}")),
            (0u32..5, 0u32..10).prop_map(|(a, b)| format!("net{a}.{b}")),
        ]
    }

    fn arb_proj_row() -> impl Strategy<Value = (String, String, String)> {
        // (name_version, framework, msbuild_rel_path)
        (arb_id(), arb_version(), arb_long_tfm()).prop_map(|(name, ver, fw)| {
            let nv = format!("{name}/{ver}");
            let rel = format!("../{name}/{name}.fsproj");
            (nv, fw, rel)
        })
    }

    /// Generate a multi-TFM assets file with the given consumer TFM
    /// always present, plus 0-3 project refs under it. Returns both the
    /// assets and a producer-declared-TFMs map keyed exactly the way
    /// the helper computes paths. Each producer declares a singleton
    /// list containing the short form of the row's generated framework
    /// — so the helper's `pick_producer_tfm` always succeeds and the
    /// result matches Phase 2a's old base-only behaviour, exercising
    /// the "no platform suffix to recover" branch.
    ///
    /// Each generated row uses the row index in both the
    /// name/version and the msbuildProject path. That keeps every row
    /// uniquely keyed *and* uniquely pathed — without the path
    /// disambiguation, two rows that happened to share a generated
    /// name would emit the same relative path and collapse to one
    /// map entry, which is a real-world impossibility (each
    /// `<ProjectReference>` is a different csproj on disk).
    fn arb_multi_tfm_assets(
        consumer: &'static str,
        top: &'static Path,
    ) -> impl Strategy<Value = (RawAssets, BTreeMap<PathBuf, Vec<String>>)> {
        proptest::collection::vec(arb_proj_row(), 0..4).prop_map(move |rows| {
            let mut target = BTreeMap::new();
            let mut libraries = BTreeMap::new();
            let mut producers: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
            let project_dir = top.parent().unwrap();
            for (i, (nv, fw, _rel)) in rows.iter().enumerate() {
                let nv = format!("P{i}{nv}");
                let rel = format!("../Proj{i}/Proj{i}.fsproj");
                target.insert(
                    nv.clone(),
                    RawTargetEntry {
                        kind: "project".to_string(),
                        compile: None,
                        framework: Some(fw.clone()),
                    },
                );
                libraries.insert(
                    nv,
                    RawLibrary {
                        kind: "project".to_string(),
                        path: None,
                        msbuild_project: Some(rel.clone()),
                    },
                );
                producers.insert(project_dir.join(&rel), vec![tfm::long_to_short(fw)]);
            }

            let mut targets = BTreeMap::new();
            let mut frameworks = BTreeMap::new();
            // Throw in one other empty TFM so the lookup actually has to
            // pick the right one and doesn't accidentally bare-fallback.
            targets.insert(consumer.to_string(), target);
            targets.insert("net6.0".to_string(), BTreeMap::new());
            frameworks.insert(
                consumer.to_string(),
                RawProjectFramework {
                    framework_references: BTreeMap::new(),
                },
            );
            frameworks.insert(
                "net6.0".to_string(),
                RawProjectFramework {
                    framework_references: BTreeMap::new(),
                },
            );

            let assets = RawAssets {
                version: 3,
                targets,
                libraries,
                package_folders: vec![PathBuf::from("/pkgs/")],
                project: RawProject { frameworks },
            };
            (assets, producers)
        })
    }

    /// Reference implementation: naïve walk over `targets[consumer]`
    /// computing the expected map. The helper must produce the same.
    fn expected_map(assets: &RawAssets, consumer: &str, top: &Path) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        out.insert(top.to_path_buf(), consumer.to_string());
        let project_dir = top.parent().unwrap();
        let Some(target) = assets.targets.get(consumer) else {
            return out;
        };
        for (nv, entry) in target {
            if entry.kind != "project" {
                continue;
            }
            let lib = assets.libraries.get(nv).expect("test setup populates libs");
            let rel = lib
                .msbuild_project
                .as_deref()
                .or(lib.path.as_deref())
                .expect("test setup includes a path");
            let framework = entry.framework.as_deref().expect("test setup");
            out.insert(project_dir.join(rel), tfm::long_to_short(framework));
        }
        out
    }

    const PROPTEST_TOP: &str = "/repo/Consumer/Consumer.fsproj";

    /// Reference implementation of `pick_producer_tfm`, written
    /// straight from the plan's prose so it can serve as the property
    /// oracle. Returns `None` to mean "error"; the test compares
    /// presence as well as the picked value.
    fn pick_producer_tfm_reference(
        base_producer_tfm: &str,
        consumer_tfm: &str,
        producer_declared: &[String],
    ) -> Option<String> {
        let consumer_suffix = tfm::split_platform(consumer_tfm).1;
        let candidates: Vec<&str> = producer_declared
            .iter()
            .map(String::as_str)
            .filter(|t| tfm::split_platform(t).0 == base_producer_tfm)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        if let Some(t) = candidates
            .iter()
            .find(|t| tfm::split_platform(t).1 == consumer_suffix)
        {
            return Some((*t).to_string());
        }
        // Bare fallback only when no candidate shares the consumer's
        // platform family (and the consumer is itself platform-qualified
        // — a bare consumer with a non-bare sole candidate is a stale
        // restore, surface RestoreMismatch).
        let consumer_family = consumer_suffix.map(tfm::platform_family);
        let same_family = consumer_family.is_some_and(|fam| {
            candidates
                .iter()
                .filter_map(|t| tfm::split_platform(t).1)
                .any(|s| tfm::platform_family(s) == fam)
        });
        if !same_family
            && let Some(t) = candidates
                .iter()
                .find(|t| tfm::split_platform(t).1.is_none())
        {
            return Some((*t).to_string());
        }
        None
    }

    /// Strategy for a short-form TFM, optionally with a platform suffix.
    fn arb_short_tfm() -> impl Strategy<Value = String> {
        let base = prop_oneof![
            (0u32..5, 0u32..10).prop_map(|(a, b)| format!("netstandard{a}.{b}")),
            (5u32..15, 0u32..10).prop_map(|(a, b)| format!("net{a}.{b}")),
        ];
        let plat = prop_oneof![Just(None), Just(Some("windows")), Just(Some("android")),];
        (base, plat).prop_map(|(b, p)| match p {
            Some(s) => format!("{b}-{s}"),
            None => b,
        })
    }

    proptest! {
        // pick_producer_tfm matches its reference implementation on every
        // (base, consumer, producer-declared) triple. The reference is
        // tiny and written from the plan's wording, so this catches any
        // drift between intent and implementation.
        #[test]
        fn pick_producer_tfm_matches_reference(
            base in "net[0-9]\\.0|netstandard2\\.0",
            consumer in arb_short_tfm(),
            declared in proptest::collection::vec(arb_short_tfm(), 0..5),
        ) {
            let path = Path::new("/repo/Producer/Producer.csproj");
            let actual = pick_producer_tfm(&base, &consumer, &declared, path);
            let expected = pick_producer_tfm_reference(&base, &consumer, &declared);
            match (actual, expected) {
                (Ok(a), Some(e)) => prop_assert_eq!(a, e),
                (Err(ProjectAssetsError::RestoreMismatch { .. }), None) => {}
                (a, e) => prop_assert!(
                    false,
                    "mismatch: actual={:?}, expected={:?}",
                    a, e
                ),
            }
        }

        // Whatever pick_producer_tfm returns must itself be a string
        // already in the producer's declared list. The function can't
        // synthesise a new TFM — recovery means selecting an existing
        // declaration.
        #[test]
        fn pick_producer_tfm_result_is_declared(
            base in "net[0-9]\\.0",
            consumer in arb_short_tfm(),
            declared in proptest::collection::vec(arb_short_tfm(), 1..5),
        ) {
            let path = Path::new("/repo/Producer/Producer.csproj");
            if let Ok(picked) = pick_producer_tfm(&base, &consumer, &declared, path) {
                prop_assert!(declared.contains(&picked));
            }
        }

        // Reference-implementation property: the helper agrees with the
        // naïve walk on every well-formed input. Catches mistakes in
        // path joining, long→short conversion, the inclusion of the
        // top csproj, and the skip-non-project rule in one stroke.
        // Producer-declared list contains exactly the row's framework,
        // so platform-suffix recovery reduces to the base case and we
        // can keep comparing against the bare reference walk.
        #[test]
        fn matches_reference_walk((assets, producers) in arb_multi_tfm_assets("net10.0", Path::new(PROPTEST_TOP))) {
            let top = Path::new(PROPTEST_TOP);
            let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
            prop_assert_eq!(map, expected_map(&assets, "net10.0", top));
        }

        // The map always contains the top csproj keyed by the consumer
        // TFM. Independent property because it can catch regressions
        // even if `expected_map` itself drifts.
        #[test]
        fn top_csproj_always_present((assets, producers) in arb_multi_tfm_assets("net10.0", Path::new(PROPTEST_TOP))) {
            let top = Path::new(PROPTEST_TOP);
            let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
            prop_assert_eq!(
                map.get(top),
                Some(&"net10.0".to_string())
            );
        }

        // The map size equals 1 + the number of project-kind entries in
        // targets[consumer]. Validates that we don't double-count when
        // a target name collides with another TFM's, and that we don't
        // accidentally include package entries.
        #[test]
        fn map_size_matches_project_count((assets, producers) in arb_multi_tfm_assets("net10.0", Path::new(PROPTEST_TOP))) {
            let top = Path::new(PROPTEST_TOP);
            let map = transitive_project_tfms(&assets, "net10.0", top, &producers).unwrap();
            let project_count = assets
                .targets
                .get("net10.0")
                .unwrap()
                .values()
                .filter(|e| e.kind == "project")
                .count();
            prop_assert_eq!(map.len(), 1 + project_count);
        }
    }
}
