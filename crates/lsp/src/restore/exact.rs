//! Whether an offline restore's result is the one the user's own restore
//! would produce.
//!
//! The offline restore reads fewer sources than the user's restore: the
//! packages folder, and the SDK's `library-packs` folders, which its targets
//! add to `RestoreAdditionalProjectSources` after listing that property in
//! `TreatAsLocalProperty`, so they survive the command line's override. The
//! user's restore reads the same folders plus its feeds.
//!
//! NuGet resolves each dependency edge to the exact minimum of its range when
//! the packages folder holds that version, and otherwise to the version
//! nearest the minimum across the folder and the sources
//! (`ResolverUtility.FindLibraryMatchAsync`). So an edge the offline restore
//! resolved to its range's inclusive minimum resolves to that version under
//! any superset of its sources: nothing is nearer. An edge it resolved to
//! anything else may resolve elsewhere under the user's feeds, which may hold
//! a nearer version. When every edge is exact, both restores walk the same
//! graph and decide it the same way; so the result is certified only then.
//!
//! The restore records every inexact edge it walked as a warning in the
//! assets file's log (`UnexpectedDependencyMessages`): `NU1603` for a range
//! whose minimum was not found, `NU1602` for a transitive range with no
//! inclusive minimum, `NU1604` for such a direct one. Reading that record is
//! sound only where NuGet would have written it, so the certificate also
//! requires what makes the record complete:
//!
//! - No `noWarn` in any project of the spec names one of those codes. A
//!   project's own list, a dependency's, and a referenced project's (which
//!   NuGet applies along the paths that project brings) all keep a warning
//!   out of the log.
//! - The restored project's lock file is disabled and redirected, as the
//!   command line asks: NuGet skips `NU1602` and `NU1603` for a project with
//!   a lock file.
//! - No range is floating. NuGet does not warn on one, and a floating range
//!   takes the highest version the sources hold, which the user's feeds may
//!   exceed. The spec carries the direct ranges, and the assets file the
//!   dependencies of every package in the result. (Residual: a floating range
//!   in the nuspec of a package the resolver walked and then evicted is in
//!   neither. nuget.org refuses floating dependency ranges, so only a private
//!   feed's package can carry one.)
//! - No direct range lacks an inclusive minimum. NuGet warns on one (`NU1604`)
//!   and then stops warning about that package anywhere in the graph.

use std::fmt;
use std::path::Path;

use borzoi_nuget::VersionRange;
use borzoi_nuget::dgspec::NuGetLogCode;
use serde_json::Value;

use crate::paths::paths_equal;

/// The warnings NuGet writes for a dependency edge it did not resolve to the
/// inclusive minimum of its range.
const INEXACT_EDGE_CODES: [&str; 3] = ["NU1602", "NU1603", "NU1604"];

/// Why an offline restore's result is not certified as the user's.
#[derive(Debug)]
pub(super) enum Uncertified {
    /// The spec is not JSON, or a field read here has another shape.
    SpecUnread(&'static str),
    /// A `noWarn` entry is not spelt exactly as a code. NuGet's reader parses
    /// such an entry leniently, so it may name an inexact edge's code.
    UnreadCode(String),
    /// The spec has no entry for the restored project.
    ProjectMissing,
    /// The assets file is not JSON of the shape read here.
    AssetsUnread(&'static str),
    /// A `noWarn` could keep an inexact edge out of the log.
    Suppressed { project: String, code: &'static str },
    /// The restored project's lock file was not disabled and redirected.
    LockFileLive,
    /// A range in the spec is floating or has no inclusive minimum.
    InexactRange {
        project: String,
        package: String,
        range: String,
    },
    /// A resolved package depends on a floating range.
    FloatingDependency {
        target: String,
        library: String,
        package: String,
        range: String,
    },
    /// The restore logged an edge it did not resolve to its minimum.
    InexactEdge { code: String },
}

impl fmt::Display for Uncertified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Uncertified::SpecUnread(what) => write!(f, "the restored spec is unread: {what}"),
            Uncertified::UnreadCode(code) => write!(f, "the noWarn entry {code:?} is unread"),
            Uncertified::ProjectMissing => write!(f, "the restored spec omits the project"),
            Uncertified::AssetsUnread(what) => write!(f, "the assets file is unread: {what}"),
            Uncertified::Suppressed { project, code } => {
                write!(f, "{project} suppresses {code}")
            }
            Uncertified::LockFileLive => {
                write!(f, "the project's lock file was not disabled and redirected")
            }
            Uncertified::InexactRange {
                project,
                package,
                range,
            } => write!(
                f,
                "{project} asks for {package} {range}, which has no exact minimum"
            ),
            Uncertified::FloatingDependency {
                target,
                library,
                package,
                range,
            } => write!(
                f,
                "{library} depends on {package} {range}, a floating range, under {target}"
            ),
            Uncertified::InexactEdge { code } => {
                write!(f, "the restore logged {code}: an edge missed its minimum")
            }
        }
    }
}

/// Certify that the offline restore of `project`, which read the
/// dependency-graph spec `spec` and wrote the assets file `assets`, with its
/// lock file redirected to `lock_file`, resolved every edge to the inclusive
/// minimum of its range (see the module docs), or say why not.
pub(super) fn certify_exact_minimums(
    spec: &str,
    assets: &str,
    project: &Path,
    lock_file: &Path,
) -> Result<(), Uncertified> {
    let spec: Value =
        serde_json::from_str(spec).map_err(|_| Uncertified::SpecUnread("not JSON"))?;
    certify_spec(&spec, project, lock_file)?;
    let assets: Value =
        serde_json::from_str(assets).map_err(|_| Uncertified::AssetsUnread("not JSON"))?;
    certify_assets(&assets)
}

/// The spec's half of [`certify_exact_minimums`]: the conditions under which
/// the assets file's log records every inexact edge, and no range in the spec
/// without an exact minimum.
///
/// It reads only the fields that bear on them, and declines when one of those
/// has another shape. The rest of the spec is not read, so a field the SDK
/// writes and `borzoi_nuget::dgspec::parse_dgspec` does not model (a
/// project's `runtimes`, say) does not decline the restore.
fn certify_spec(spec: &Value, project: &Path, lock_file: &Path) -> Result<(), Uncertified> {
    let projects = spec
        .get("projects")
        .and_then(Value::as_object)
        .ok_or(Uncertified::SpecUnread("no `projects` object"))?;
    let (_, restored) = projects
        .iter()
        .find(|(name, _)| paths_equal(Path::new(name), project))
        .ok_or(Uncertified::ProjectMissing)?;
    let lock = optional_object(
        restored.pointer("/restore/restoreLockProperties"),
        "`restoreLockProperties` is not an object",
    )?;
    let lock_field = |name| {
        optional_string(
            lock.and_then(|lock| lock.get(name)),
            "a lock property is not a string",
        )
    };
    let enabled = lock_field("restorePackagesWithLockFile")?
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
    let redirected = lock_field("nuGetLockFilePath")?
        .is_some_and(|path| paths_equal(Path::new(path), lock_file));
    if enabled || !redirected {
        return Err(Uncertified::LockFileLive);
    }

    for (name, candidate) in projects {
        let suppressed = |code| Uncertified::Suppressed {
            project: name.clone(),
            code,
        };
        if let Some(code) = suppressed_code(candidate.pointer("/restore/warningProperties/noWarn"))?
        {
            return Err(suppressed(code));
        }
        let frameworks =
            optional_object(candidate.get("frameworks"), "`frameworks` is not an object")?;
        for framework in frameworks
            .into_iter()
            .flat_map(|frameworks| frameworks.values())
        {
            let dependencies = optional_object(
                framework.get("dependencies"),
                "`dependencies` is not an object",
            )?;
            for (package, dependency) in dependencies.into_iter().flatten() {
                if let Some(code) = suppressed_code(dependency.get("noWarn"))? {
                    return Err(suppressed(code));
                }
                // A direct dependency without a range is read as any version.
                let range =
                    optional_string(dependency.get("version"), "a version is not a string")?;
                exactly_matchable(name, package, range.unwrap_or(""))?;
                let version_override = optional_string(
                    dependency.get("versionOverride"),
                    "a version override is not a string",
                )?;
                if let Some(range) = version_override {
                    exactly_matchable(name, package, range)?;
                }
            }
            let central = optional_object(
                framework.get("centralPackageVersions"),
                "`centralPackageVersions` is not an object",
            )?;
            for (package, range) in central.into_iter().flatten() {
                let range = range
                    .as_str()
                    .ok_or(Uncertified::SpecUnread("a central version is not a string"))?;
                exactly_matchable(name, package, range)?;
            }
        }
    }
    Ok(())
}

/// `value` as an object, if present.
fn optional_object<'a>(
    value: Option<&'a Value>,
    otherwise: &'static str,
) -> Result<Option<&'a serde_json::Map<String, Value>>, Uncertified> {
    value
        .map(|value| value.as_object().ok_or(Uncertified::SpecUnread(otherwise)))
        .transpose()
}

/// `value` as a string, if present.
fn optional_string<'a>(
    value: Option<&'a Value>,
    otherwise: &'static str,
) -> Result<Option<&'a str>, Uncertified> {
    value
        .map(|value| value.as_str().ok_or(Uncertified::SpecUnread(otherwise)))
        .transpose()
}

/// The first code of the `noWarn` list `no_warn` that names an inexact edge,
/// if any. An entry not spelt exactly as a code declines.
fn suppressed_code(no_warn: Option<&Value>) -> Result<Option<&'static str>, Uncertified> {
    let Some(no_warn) = no_warn else {
        return Ok(None);
    };
    let entries = no_warn
        .as_array()
        .ok_or(Uncertified::SpecUnread("a `noWarn` is not an array"))?;
    for entry in entries {
        let text = entry
            .as_str()
            .ok_or(Uncertified::SpecUnread("a `noWarn` entry is not a string"))?;
        let code =
            NuGetLogCode::parse(text).ok_or_else(|| Uncertified::UnreadCode(text.to_owned()))?;
        if let Some(inexact) = INEXACT_EDGE_CODES
            .iter()
            .find(|inexact| **inexact == code.as_str())
        {
            return Ok(Some(inexact));
        }
    }
    Ok(None)
}

/// `Ok` if the range `text` that `project` gives `package` has an inclusive
/// minimum and does not float.
fn exactly_matchable(project: &str, package: &str, text: &str) -> Result<(), Uncertified> {
    let matchable = VersionRange::parse(text).is_ok_and(|range| {
        !range.is_floating() && range.has_lower_bound() && range.is_min_inclusive()
    });
    if matchable {
        Ok(())
    } else {
        Err(Uncertified::InexactRange {
            project: project.to_owned(),
            package: package.to_owned(),
            range: text.to_owned(),
        })
    }
}

/// The assets file's half of [`certify_exact_minimums`]: no logged inexact
/// edge, and no resolved package depending on a floating range.
fn certify_assets(assets: &serde_json::Value) -> Result<(), Uncertified> {
    if let Some(logs) = assets.get("logs") {
        let logs = logs
            .as_array()
            .ok_or(Uncertified::AssetsUnread("`logs` is not an array"))?;
        for log in logs {
            let code = log
                .get("code")
                .and_then(serde_json::Value::as_str)
                .ok_or(Uncertified::AssetsUnread("a log has no code"))?;
            if INEXACT_EDGE_CODES.contains(&code) {
                return Err(Uncertified::InexactEdge {
                    code: code.to_owned(),
                });
            }
        }
    }
    let targets = assets
        .get("targets")
        .and_then(serde_json::Value::as_object)
        .ok_or(Uncertified::AssetsUnread("no `targets` object"))?;
    for (target, libraries) in targets {
        let libraries = libraries
            .as_object()
            .ok_or(Uncertified::AssetsUnread("a target is not an object"))?;
        for (library, entry) in libraries {
            let Some(dependencies) = entry.get("dependencies") else {
                continue;
            };
            let dependencies = dependencies
                .as_object()
                .ok_or(Uncertified::AssetsUnread("`dependencies` is not an object"))?;
            for (package, range) in dependencies {
                let range = range.as_str().ok_or(Uncertified::AssetsUnread(
                    "a dependency range is not a string",
                ))?;
                // A version holds no `*`, so a range holds one exactly when it
                // floats.
                if range.contains('*') {
                    return Err(Uncertified::FloatingDependency {
                        target: target.clone(),
                        library: library.clone(),
                        package: package.clone(),
                        range: range.to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{INEXACT_EDGE_CODES, Uncertified, certify_assets, certify_spec};
    use proptest::prelude::*;
    use serde_json::{Value, json};
    use std::path::Path;

    const PROJECT: &str = "/repo/app/App.fsproj";
    const REFERENCED: &str = "/repo/lib/Lib.fsproj";
    const LOCK: &str = "/scratch/packages.lock.json";

    /// A spec shaped as the SDK writes one for the offline restore of
    /// [`PROJECT`], which references [`REFERENCED`]: every edge exact, nothing
    /// suppressed, the lock file disabled and redirected.
    fn spec() -> Value {
        let project = |dependency: &str| {
            json!({
                "version": "1.0.0",
                "restore": {
                    "projectUniqueName": "unused",
                    "warningProperties": {"warnAsError": ["NU1605"], "noWarn": ["NU1701"]},
                    "restoreLockProperties": {
                        "restorePackagesWithLockFile": "false",
                        "nuGetLockFilePath": LOCK,
                    },
                },
                "frameworks": {
                    "net10.0": {
                        "targetAlias": "net10.0",
                        "dependencies": {
                            dependency: {"target": "Package", "version": "[10.1.204, )"},
                        },
                        "centralPackageVersions": {"Other": "[1.0.0]"},
                    },
                },
                "runtimes": {"osx-arm64": {"#import": []}},
            })
        };
        json!({
            "format": 1,
            "restore": {PROJECT: {}},
            "projects": {PROJECT: project("FSharp.Core"), REFERENCED: project("Lib.Dep")},
        })
    }

    fn certify(spec: &Value) -> Result<(), Uncertified> {
        certify_spec(spec, Path::new(PROJECT), Path::new(LOCK))
    }

    /// `spec()` with the value at `pointer` set to `value`, adding the last
    /// key if it is absent.
    fn with(pointer: &str, value: Value) -> Value {
        let mut spec = spec();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        spec.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("no object at {parent}"))
            .insert(key.replace("~1", "/"), value);
        spec
    }

    const DEPENDENCY: &str =
        "/projects/~1repo~1app~1App.fsproj/frameworks/net10.0/dependencies/FSharp.Core";

    #[test]
    fn an_exact_spec_is_certified_whatever_else_it_holds() {
        certify(&spec()).unwrap();
    }

    #[test]
    fn the_restored_project_must_be_in_the_spec() {
        let mut spec = spec();
        spec["projects"].as_object_mut().unwrap().remove(PROJECT);
        assert!(matches!(certify(&spec), Err(Uncertified::ProjectMissing)));
    }

    #[test]
    fn a_live_lock_file_is_not_certified() {
        let lock = "/projects/~1repo~1app~1App.fsproj/restore/restoreLockProperties";
        for spec in [
            with(
                &format!("{lock}/restorePackagesWithLockFile"),
                json!(" True "),
            ),
            with(
                &format!("{lock}/nuGetLockFilePath"),
                json!("/repo/app/packages.lock.json"),
            ),
            with(lock, json!({})),
        ] {
            assert!(
                matches!(certify(&spec), Err(Uncertified::LockFileLive)),
                "{spec}"
            );
        }
    }

    #[test]
    fn a_range_without_an_exact_minimum_is_not_certified() {
        for (field, range) in [
            ("version", json!("10.*")),
            ("version", json!("[10.1.*, )")),
            ("version", json!("(10.1.204, )")),
            ("version", json!("(, 11.0.0)")),
            ("version", json!("")),
            ("versionOverride", json!("10.*")),
        ] {
            let spec = with(&format!("{DEPENDENCY}/{field}"), range.clone());
            assert!(
                matches!(certify(&spec), Err(Uncertified::InexactRange { .. })),
                "{field} {range}"
            );
        }
        let mut spec = spec();
        spec.pointer_mut(DEPENDENCY)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("version");
        assert!(matches!(
            certify(&spec),
            Err(Uncertified::InexactRange { .. })
        ));
        let central =
            "/projects/~1repo~1lib~1Lib.fsproj/frameworks/net10.0/centralPackageVersions/Other";
        assert!(matches!(
            certify(&with(central, json!("1.*"))),
            Err(Uncertified::InexactRange { .. })
        ));
    }

    #[test]
    fn a_no_warn_entry_not_spelt_as_a_code_is_not_certified() {
        for entry in ["nu1603", "1603", " NU1603", "NU1603,NU1701"] {
            let spec = with(&format!("{DEPENDENCY}/noWarn"), json!([entry]));
            assert!(
                matches!(certify(&spec), Err(Uncertified::UnreadCode(_))),
                "{entry}"
            );
        }
    }

    proptest! {
        /// Every `noWarn` list, of either project and at either level, keeps
        /// the result uncertified exactly when it names an inexact edge's code.
        #[test]
        fn a_no_warn_list_decides_exactly_by_the_inexact_codes(
            project in proptest::sample::select(vec![PROJECT, REFERENCED]),
            dependency_level in any::<bool>(),
            codes in proptest::collection::vec(
                proptest::sample::select(
                    INEXACT_EDGE_CODES.iter().chain(OTHER_CODES.iter()).copied().collect::<Vec<_>>(),
                ),
                0..5,
            ),
        ) {
            let escaped = project.replace('/', "~1");
            let pointer = if dependency_level {
                let package = if project == PROJECT { "FSharp.Core" } else { "Lib.Dep" };
                format!("/projects/{escaped}/frameworks/net10.0/dependencies/{package}/noWarn")
            } else {
                format!("/projects/{escaped}/restore/warningProperties/noWarn")
            };
            let spec = with(&pointer, json!(codes));
            let inexact = codes.iter().any(|code| INEXACT_EDGE_CODES.contains(code));
            let verdict = certify(&spec);
            prop_assert_eq!(
                matches!(verdict, Err(Uncertified::Suppressed { .. })),
                inexact,
                "{} {:?} gave {:?}",
                pointer,
                codes,
                verdict
            );
            prop_assert_eq!(verdict.is_ok(), !inexact);
        }
    }

    /// Codes the restore logs in an ordinary result, beside the inexact ones.
    const OTHER_CODES: [&str; 4] = ["NU1701", "NU1608", "NU1605", "NU1900"];

    fn assets_with(codes: &[&str], dependency_range: &str) -> serde_json::Value {
        json!({
            "version": 3,
            "targets": {
                "net10.0": {
                    "A/1.0.0": {"type": "package", "dependencies": {"B": dependency_range}},
                    "B/2.0.0": {"type": "package"},
                },
            },
            "logs": codes
                .iter()
                .map(|code| json!({"code": code, "level": "Warning", "message": "m"}))
                .collect::<Vec<_>>(),
        })
    }

    proptest! {
        /// The log certifies a result exactly when it names no inexact edge.
        #[test]
        fn the_log_certifies_exactly_when_no_edge_is_inexact(
            codes in proptest::collection::vec(
                proptest::sample::select(
                    INEXACT_EDGE_CODES.iter().chain(OTHER_CODES.iter()).copied().collect::<Vec<_>>(),
                ),
                0..6,
            ),
        ) {
            let inexact = codes.iter().any(|code| INEXACT_EDGE_CODES.contains(code));
            let verdict = certify_assets(&assets_with(&codes, "2.0.0"));
            prop_assert_eq!(
                verdict.is_err(),
                inexact,
                "codes {:?} gave {:?}",
                codes,
                verdict
            );
            let named = matches!(verdict, Err(Uncertified::InexactEdge { .. }));
            prop_assert_eq!(named, inexact);
        }

        /// A transitive range is refused exactly when it floats.
        #[test]
        fn a_transitive_range_is_refused_exactly_when_it_floats(
            range in proptest::sample::select(vec![
                "2.0.0", "[2.0.0, )", "[2.0.0, 3.0.0)", "[2.0.0]",
                "2.*", "2.0.*", "*", "[2.0.0-*, )", "2.0.0-beta.*",
            ]),
        ) {
            let verdict = certify_assets(&assets_with(&[], range));
            prop_assert_eq!(verdict.is_err(), range.contains('*'), "{} gave {:?}", range, verdict);
        }
    }

    #[test]
    fn an_assets_file_without_logs_is_read_as_logging_nothing() {
        let mut assets = assets_with(&[], "2.0.0");
        assets.as_object_mut().unwrap().remove("logs");
        assert!(certify_assets(&assets).is_ok());
    }

    #[test]
    fn an_unread_shape_is_not_certified() {
        for assets in [
            json!({"targets": {}, "logs": {}}),
            json!({"targets": {}, "logs": [{"level": "Warning"}]}),
            json!({"logs": []}),
            json!({"targets": {"net10.0": {"A/1.0.0": {"dependencies": {"B": 2}}}}}),
        ] {
            assert!(
                matches!(certify_assets(&assets), Err(Uncertified::AssetsUnread(_))),
                "{assets}"
            );
        }
    }
}
