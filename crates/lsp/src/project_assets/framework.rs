use std::path::{Path, PathBuf};

use crate::project_assets::error::ProjectAssetsError;

/// Locate the reference assemblies for a shared framework pack on disk.
///
/// Searches two kinds of root, in this order, for a `{name}.Ref` ref pack:
///
/// 1. `{dotnet_root}/packs/{name}.Ref/` — the pack the running SDK bundles.
/// 2. `{pf}/{name-lowercased}.ref/` for each NuGet package folder `pf` — the
///    targeting pack NuGet restores as a `downloadDependency` (the assets'
///    `packageFolders`). This is how a project targeting an *older* TFM than
///    the running SDK (e.g. `net6.0` developed with the .NET 10 SDK) gets its
///    reference assemblies: the SDK ships only its own TFM's pack, while the
///    older TFM's pack lands under `~/.nuget/packages/microsoft.netcore.app.ref/`.
///
/// Both layouts are `{version}/ref/{tfm}/*.dll`. The SDK's bundled pack is
/// **authoritative**: if it has any version with a matching `ref/{tfm}/`, the
/// highest such is used and the NuGet folders are ignored — mirroring MSBuild,
/// where the bundled pack is what builds and the `downloadDependency` only fills
/// in TFMs the running SDK doesn't ship. Only when the SDK pack has no matching
/// TFM do the NuGet folders' packs come into play (highest matching version).
/// In all cases every `*.dll` in the chosen `ref/{tfm}/` is returned.
///
/// This is *not* full MSBuild rollForward logic — it just picks the highest
/// locally-available pack that contains a `ref/{tfm}` directory. Good enough for
/// an LSP that wants to know which assemblies the compiler would resolve.
///
/// # Errors
///
/// - [`ProjectAssetsError::FrameworkPackNotFound`] when *no* candidate root
///   directory exists at all (`searched` reports the SDK packs path).
/// - [`ProjectAssetsError::FrameworkRefForTfmMissing`] when a root exists but
///   none holds a version with a matching `ref/{tfm}/`.
///
/// # Unsupported: Windows-desktop frameworks and platform TFMs
///
/// Some framework reference names don't directly correspond to a pack
/// directory and need a mapping (e.g. `Microsoft.WindowsDesktop.App.WPF`
/// and `Microsoft.WindowsDesktop.App.WindowsForms` both resolve to
/// `Microsoft.WindowsDesktop.App.Ref`). We don't carry that table —
/// projects referencing WPF or Windows Forms will see
/// `FrameworkPackNotFound`.
///
/// Similarly, platform-TFM suffixes (`net8.0-windows`, `net8.0-android`,
/// `net8.0-ios`, etc.) are passed straight through to the `ref/{tfm}`
/// probe, but ref-packs file BCL assemblies under the bare TFM
/// (`ref/net8.0`). Platform-TFM projects will see
/// `FrameworkRefForTfmMissing`. Both gaps are deliberate: this LSP
/// targets cross-platform F# work, not Windows-desktop or
/// mobile-platform development.
pub fn resolve_framework(
    dotnet_root: &Path,
    package_folders: &[PathBuf],
    name: &str,
    tfm: &str,
) -> Result<Vec<PathBuf>, ProjectAssetsError> {
    // The SDK's bundled pack is authoritative; the NuGet-restored targeting
    // packs (lowercased `{name}.ref` under each package folder) are fallbacks
    // searched only when the SDK pack lacks this TFM. `sdk_pack` doubles as the
    // `searched` path reported when nothing exists at all.
    let sdk_pack = dotnet_root.join("packs").join(format!("{name}.Ref"));
    let nuget_dir = format!("{}.ref", name.to_lowercase());
    let nuget_roots: Vec<PathBuf> = package_folders
        .iter()
        .map(|pf| pf.join(&nuget_dir))
        .collect();

    // Scan the SDK pack first; if it has the TFM, it wins outright. Otherwise
    // fall back to the NuGet roots.
    let (sdk_existed, sdk_best) = best_version_with_tfm(std::slice::from_ref(&sdk_pack), tfm)?;
    let version_dir = match sdk_best {
        Some((_, dir)) => dir,
        None => {
            let (nuget_existed, nuget_best) = best_version_with_tfm(&nuget_roots, tfm)?;
            match nuget_best {
                Some((_, dir)) => dir,
                // A pack dir existed but held no matching TFM vs. no candidate
                // dir at all (the framework isn't installed anywhere we look).
                None if sdk_existed || nuget_existed => {
                    return Err(ProjectAssetsError::FrameworkRefForTfmMissing {
                        name: name.to_string(),
                        tfm: tfm.to_string(),
                    });
                }
                None => {
                    return Err(ProjectAssetsError::FrameworkPackNotFound {
                        name: name.to_string(),
                        searched: sdk_pack,
                    });
                }
            }
        }
    };

    let ref_dir = version_dir.join("ref").join(tfm);
    let mut dlls = Vec::new();
    for entry in std::fs::read_dir(&ref_dir).map_err(|e| ProjectAssetsError::Io {
        path: ref_dir.clone(),
        source: e,
    })? {
        let entry = entry.map_err(|e| ProjectAssetsError::Io {
            path: ref_dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "dll") {
            dlls.push(path);
        }
    }
    dlls.sort();
    Ok(dlls)
}

/// Scan each candidate `{name}.Ref` root for the highest version directory whose
/// `ref/{tfm}/` exists. Returns `(any_root_existed, best)` — `any_root_existed`
/// distinguishes "no candidate dir present at all" from "present but holding no
/// matching TFM", so the caller can pick the right error.
fn best_version_with_tfm(
    roots: &[PathBuf],
    tfm: &str,
) -> Result<(bool, Option<(VersionKey, PathBuf)>), ProjectAssetsError> {
    let mut best: Option<(VersionKey, PathBuf)> = None;
    let mut any_root_existed = false;
    for root in roots {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(ProjectAssetsError::Io {
                    path: root.clone(),
                    source: e,
                });
            }
        };
        any_root_existed = true;
        for entry in entries {
            let entry = entry.map_err(|e| ProjectAssetsError::Io {
                path: root.clone(),
                source: e,
            })?;
            let path = entry.path();
            let Some(version) = parse_version(entry.file_name().to_string_lossy().as_ref()) else {
                continue;
            };
            if !path.join("ref").join(tfm).is_dir() {
                continue;
            }
            match &best {
                Some((best_v, _)) if best_v >= &version => {}
                _ => best = Some((version, path)),
            }
        }
    }
    Ok((any_root_existed, best))
}

/// Sort key for a ref-pack version directory.
///
/// Components in order of significance:
/// 1. Numeric `MAJOR.MINOR.PATCH...` parts (lex over `Vec<u32>` matches numeric order).
/// 2. `is_stable`: stable releases sort above prereleases sharing the same numeric prefix
///    (SemVer §11: `1.0.0-alpha < 1.0.0`).
/// 3. `prerelease`: lexical tiebreaker among prereleases (`-alpha < -beta`). Not a full
///    SemVer §11.4 prerelease comparator — close enough for picking the highest installed
///    preview when multiple coexist.
type VersionKey = (Vec<u32>, bool, String);

/// Parse a ref-pack directory name into a sort key, accepting SemVer prerelease
/// suffixes like `10.0.0-preview.7.25380.108`. Returns `None` if the leading
/// dot-separated numeric portion is empty or unparseable.
fn parse_version(s: &str) -> Option<VersionKey> {
    let (numeric, prerelease) = match s.split_once('-') {
        Some((n, p)) => (n, Some(p)),
        None => (s, None),
    };
    let parts: Result<Vec<u32>, _> = numeric.split('.').map(|p| p.parse::<u32>()).collect();
    let parts = parts.ok()?;
    if parts.is_empty() {
        return None;
    }
    Some((
        parts,
        prerelease.is_none(),
        prerelease.unwrap_or("").to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn picks_highest_version_with_matching_tfm() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("8.0.1/ref/net8.0/System.dll"));
        touch(&pack.join("8.0.11/ref/net8.0/System.dll"));
        touch(&pack.join("8.0.11/ref/net8.0/System.Console.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0").unwrap();
        let expected = vec![
            pack.join("8.0.11/ref/net8.0/System.Console.dll"),
            pack.join("8.0.11/ref/net8.0/System.dll"),
        ];
        assert_eq!(dlls, expected);
    }

    #[test]
    fn unrelated_subdirs_are_ignored() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("8.0.1/ref/net8.0/System.dll"));
        fs::create_dir_all(pack.join("not-a-version/ref/net8.0")).unwrap();
        touch(&pack.join("not-a-version/ref/net8.0/Ghost.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0").unwrap();
        assert_eq!(dlls, vec![pack.join("8.0.1/ref/net8.0/System.dll")]);
    }

    #[test]
    fn versions_without_matching_tfm_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("9.0.0/ref/net9.0/System.dll"));
        touch(&pack.join("8.0.1/ref/net8.0/System.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0").unwrap();
        assert_eq!(dlls, vec![pack.join("8.0.1/ref/net8.0/System.dll")]);
    }

    #[test]
    fn non_dll_files_are_dropped() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("8.0.1/ref/net8.0/System.dll"));
        touch(&pack.join("8.0.1/ref/net8.0/System.xml"));
        touch(&pack.join("8.0.1/ref/net8.0/FrameworkList.xml"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0").unwrap();
        assert_eq!(dlls, vec![pack.join("8.0.1/ref/net8.0/System.dll")]);
    }

    #[test]
    fn missing_pack_is_reported() {
        let tmp = TempDir::new().unwrap();
        match resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0") {
            Err(ProjectAssetsError::FrameworkPackNotFound { name, searched }) => {
                assert_eq!(name, "Microsoft.NETCore.App");
                assert_eq!(searched, tmp.path().join("packs/Microsoft.NETCore.App.Ref"));
            }
            other => panic!("expected FrameworkPackNotFound, got {other:?}"),
        }
    }

    #[test]
    fn resolves_from_nuget_package_folder_when_sdk_pack_absent() {
        // The net6.0-targeting-pack-via-NuGet case: no SDK packs dir at all; the
        // ref pack lives under a NuGet package folder as `{name-lowercased}.ref`.
        let tmp = TempDir::new().unwrap();
        let nuget = tmp.path().join("nuget");
        touch(&nuget.join("microsoft.netcore.app.ref/6.0.36/ref/net6.0/System.Console.dll"));

        let dlls = resolve_framework(
            tmp.path(),
            std::slice::from_ref(&nuget),
            "Microsoft.NETCore.App",
            "net6.0",
        )
        .unwrap();
        assert_eq!(
            dlls,
            vec![nuget.join("microsoft.netcore.app.ref/6.0.36/ref/net6.0/System.Console.dll")]
        );
    }

    #[test]
    fn sdk_pack_lacking_tfm_falls_back_to_nuget() {
        // The real WoofWare shape: the SDK bundles only its own TFM's pack
        // (net10.0), while the project's net6.0 ref pack is in NuGet.
        let tmp = TempDir::new().unwrap();
        touch(
            &tmp.path()
                .join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0/System.dll"),
        );
        let nuget = tmp.path().join("nuget");
        touch(&nuget.join("microsoft.netcore.app.ref/6.0.36/ref/net6.0/System.dll"));

        let dlls = resolve_framework(
            tmp.path(),
            std::slice::from_ref(&nuget),
            "Microsoft.NETCore.App",
            "net6.0",
        )
        .unwrap();
        assert_eq!(
            dlls,
            vec![nuget.join("microsoft.netcore.app.ref/6.0.36/ref/net6.0/System.dll")]
        );
    }

    #[test]
    fn sdk_pack_wins_over_nuget_even_when_nuget_is_higher() {
        // The SDK's bundled pack is authoritative: when it has the TFM it wins
        // outright and the NuGet fallback is ignored, even if NuGet holds a
        // numerically-higher version. (Mirrors MSBuild: the bundled pack builds;
        // the downloadDependency only fills TFMs the SDK doesn't ship.)
        let tmp = TempDir::new().unwrap();
        touch(
            &tmp.path()
                .join("packs/Microsoft.NETCore.App.Ref/8.0.1/ref/net8.0/Sdk.dll"),
        );
        let nuget = tmp.path().join("nuget");
        touch(&nuget.join("microsoft.netcore.app.ref/8.0.36/ref/net8.0/Nuget.dll"));

        let dlls = resolve_framework(
            tmp.path(),
            std::slice::from_ref(&nuget),
            "Microsoft.NETCore.App",
            "net8.0",
        )
        .unwrap();
        assert_eq!(
            dlls,
            vec![
                tmp.path()
                    .join("packs/Microsoft.NETCore.App.Ref/8.0.1/ref/net8.0/Sdk.dll")
            ]
        );
    }

    #[test]
    fn tfm_missing_across_all_roots_is_reported() {
        // A nuget root exists but holds no matching tfm → RefForTfmMissing
        // (not PackNotFound, since a root did exist).
        let tmp = TempDir::new().unwrap();
        let nuget = tmp.path().join("nuget");
        touch(&nuget.join("microsoft.netcore.app.ref/9.0.0/ref/net9.0/System.dll"));

        match resolve_framework(tmp.path(), &[nuget], "Microsoft.NETCore.App", "net6.0") {
            Err(ProjectAssetsError::FrameworkRefForTfmMissing { tfm, .. }) => {
                assert_eq!(tfm, "net6.0");
            }
            other => panic!("expected FrameworkRefForTfmMissing, got {other:?}"),
        }
    }

    #[test]
    fn no_version_for_tfm_is_reported() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("9.0.0/ref/net9.0/System.dll"));

        match resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0") {
            Err(ProjectAssetsError::FrameworkRefForTfmMissing { name, tfm }) => {
                assert_eq!(name, "Microsoft.NETCore.App");
                assert_eq!(tfm, "net8.0");
            }
            other => panic!("expected FrameworkRefForTfmMissing, got {other:?}"),
        }
    }

    #[test]
    fn prerelease_version_is_accepted_when_only_option() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("10.0.0-preview.7.25380.108/ref/net10.0/System.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net10.0").unwrap();
        assert_eq!(
            dlls,
            vec![pack.join("10.0.0-preview.7.25380.108/ref/net10.0/System.dll")]
        );
    }

    #[test]
    fn stable_version_beats_prerelease_with_same_numeric() {
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("10.0.0-preview.7.25380.108/ref/net10.0/Preview.dll"));
        touch(&pack.join("10.0.0/ref/net10.0/Stable.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net10.0").unwrap();
        assert_eq!(dlls, vec![pack.join("10.0.0/ref/net10.0/Stable.dll")]);
    }

    #[test]
    fn higher_numeric_prerelease_beats_lower_stable() {
        // SemVer: 10.0.0-preview > 9.0.0 because the numeric prefix is larger.
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("9.0.0/ref/net10.0/Old.dll"));
        touch(&pack.join("10.0.0-preview.7.25380.108/ref/net10.0/Preview.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net10.0").unwrap();
        assert_eq!(
            dlls,
            vec![pack.join("10.0.0-preview.7.25380.108/ref/net10.0/Preview.dll")]
        );
    }

    #[test]
    fn version_comparison_is_numeric_not_lexicographic() {
        // "10.0.1" > "9.0.0" numerically; lex order would prefer "9.0.0".
        let tmp = TempDir::new().unwrap();
        let pack = tmp.path().join("packs/Microsoft.NETCore.App.Ref");
        touch(&pack.join("9.0.0/ref/net8.0/Old.dll"));
        touch(&pack.join("10.0.1/ref/net8.0/New.dll"));

        let dlls = resolve_framework(tmp.path(), &[], "Microsoft.NETCore.App", "net8.0").unwrap();
        assert_eq!(dlls, vec![pack.join("10.0.1/ref/net8.0/New.dll")]);
    }
}
