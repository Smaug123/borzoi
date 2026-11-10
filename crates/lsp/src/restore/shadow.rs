//! B₁ in shadow: every on-demand restore also hands the spec it read to the
//! spawn-free half B (`borzoi_nuget::dgspec::resolve_dgspec`) and checks B₁'s
//! answer against the restore's own.
//!
//! The restore stays the answer the LSP serves; nothing here changes it. What
//! this buys is B₁ run on every real project the LSP restores, under the
//! user's SDK, packages folder and environment, rather than on the corpus and
//! generated specs alone. B₁'s contract is certain-implies-exact against the
//! restore (`docs/nuget-dgspec-boundary-plan.md`), so a commit must name, per
//! framework, exactly the packages the assets file's target lists, predict
//! exactly the warning codes it logs where it predicts any, and never be made
//! where the restore failed. A [`ShadowVerdict::Disagreed`] is therefore a B₁
//! bug; a decline makes no claim.
//!
//! `tests/all/restore_graph.rs` runs this same comparison against the
//! restore on its fixtures and on every corpus project.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;

use borzoi_nuget::NuGetFramework;
use borzoi_nuget::dgspec::{
    DependencyGraphSpec, DgSpecDecline, DgSpecResolveDecline, NuGetIdentity, ResolvedFramework,
    parse_dgspec, resolve_dgspec,
};
use serde_json::Value;

use super::nuget_identity::IdentityDecline;

/// What the restore produced, as B₁ is checked against it.
#[derive(Debug, Clone, Copy)]
pub struct RestoreRecord<'a> {
    /// Whether the restore succeeded.
    pub succeeded: bool,
    /// The assets file it wrote, which NuGet writes even when resolution
    /// fails.
    pub assets: Option<&'a Value>,
}

/// B₁'s answer, checked against the restore's.
#[derive(Debug)]
pub enum ShadowVerdict {
    /// B₁ was not asked.
    NotRun(NotRun),
    /// B₁ declined: no claim.
    Declined(DgSpecResolveDecline),
    /// B₁ committed, to exactly the restore's closures (and warnings, where it
    /// predicts them).
    Agreed(Vec<ResolvedFramework>),
    /// B₁ committed to something the restore did not do: a B₁ bug.
    Disagreed(Disagreement),
}

/// Why B₁ was not asked.
#[derive(Debug)]
pub enum NotRun {
    /// The restoring NuGet's identity cannot be said.
    NoIdentity(IdentityDecline),
    /// The spec reader declined the spec.
    Spec(DgSpecDecline),
    /// The spec restores other than one project.
    NotEntryOnly(usize),
    /// A committed comparison needs the assets file, and it is absent or not
    /// in the shape the restore writes.
    AssetsUnreadable(String),
}

/// How a B₁ commit differs from the restore.
#[derive(Debug, PartialEq, Eq)]
pub enum Disagreement {
    /// B₁ committed where the restore failed.
    CommittedOnFailedRestore,
    /// The package closures differ, by framework (`id` lower-cased,
    /// normalised version).
    Closures {
        b1: BTreeMap<String, BTreeSet<(String, String)>>,
        restore: BTreeMap<String, BTreeSet<(String, String)>>,
    },
    /// B₁ predicted exactly, and the restore logged other codes.
    Warnings {
        predicted: BTreeSet<String>,
        logged: BTreeSet<String>,
    },
}

impl fmt::Display for Disagreement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommittedOnFailedRestore => f.write_str("B₁ committed, but the restore failed"),
            Self::Closures { b1, restore } => write!(
                f,
                "B₁'s closures are not the restore's: B₁ {b1:?}, restore {restore:?}"
            ),
            Self::Warnings { predicted, logged } => write!(
                f,
                "B₁ predicts warnings {predicted:?}, the restore logged {logged:?}"
            ),
        }
    }
}

impl ShadowVerdict {
    /// A short name for the verdict, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotRun(_) => "not-run",
            Self::Declined(_) => "declined",
            Self::Agreed(_) => "agreed",
            Self::Disagreed(_) => "disagreed",
        }
    }
}

/// B₁ over the spec text the restore read, checked against the restore.
/// `environment` is the one the restore ran under.
pub fn shadow(
    identity: Result<NuGetIdentity, IdentityDecline>,
    spec: &str,
    restore: RestoreRecord<'_>,
    environment: &BTreeMap<OsString, OsString>,
) -> ShadowVerdict {
    let identity = match identity {
        Ok(identity) => identity,
        Err(decline) => return ShadowVerdict::NotRun(NotRun::NoIdentity(decline)),
    };
    match parse_dgspec(spec) {
        Ok(spec) => shadow_parsed(&identity, &spec, restore, environment),
        Err(decline) => ShadowVerdict::NotRun(NotRun::Spec(decline)),
    }
}

/// [`shadow`], over a spec already read.
pub fn shadow_parsed(
    identity: &NuGetIdentity,
    spec: &DependencyGraphSpec,
    restore: RestoreRecord<'_>,
    environment: &BTreeMap<OsString, OsString>,
) -> ShadowVerdict {
    let [entry] = spec.restore.as_slice() else {
        return ShadowVerdict::NotRun(NotRun::NotEntryOnly(spec.restore.len()));
    };
    match resolve_dgspec(identity, spec, entry, environment) {
        Err(decline) => ShadowVerdict::Declined(decline),
        Ok(frameworks) => match compare(&frameworks, restore) {
            Ok(()) => ShadowVerdict::Agreed(frameworks),
            Err(Comparison::Disagreed(disagreement)) => ShadowVerdict::Disagreed(disagreement),
            Err(Comparison::Unreadable(why)) => {
                ShadowVerdict::NotRun(NotRun::AssetsUnreadable(why))
            }
        },
    }
}

/// Why a B₁ commit does not match the restore.
enum Comparison {
    Disagreed(Disagreement),
    Unreadable(String),
}

/// The framework a target or a resolved framework is keyed by. Targets are
/// matched by framework, not by name: an assets file of version 4 names a
/// target by its alias, one of version 3 by `NuGetFramework.ToString()`
/// (`.NETStandard,Version=v2.0`), and which version restore writes moves with
/// the spec's `SdkAnalysisLevel`.
///
/// A framework NuGet reads as unsupported has no key: the parser salvages an
/// unsupported framework from a name that names none, and two such names
/// would share one.
fn key(framework: &NuGetFramework) -> Result<String, Comparison> {
    if framework.is_unsupported() {
        return Err(Comparison::Unreadable(format!(
            "framework {framework:?} is unsupported"
        )));
    }
    framework.short_folder_name().ok_or_else(|| {
        Comparison::Unreadable(format!("framework {framework:?} has no short folder name"))
    })
}

/// Insert `packages` under `key`, which no earlier framework may hold.
fn insert_once(
    map: &mut BTreeMap<String, BTreeSet<(String, String)>>,
    key: String,
    packages: BTreeSet<(String, String)>,
) -> Result<(), Comparison> {
    match map.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(packages);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(entry) => Err(Comparison::Unreadable(
            format!("two frameworks share the key {}", entry.key()),
        )),
    }
}

/// Whether B₁'s committed `frameworks` are exactly what the restore did.
fn compare(frameworks: &[ResolvedFramework], restore: RestoreRecord<'_>) -> Result<(), Comparison> {
    if !restore.succeeded {
        return Err(Comparison::Disagreed(
            Disagreement::CommittedOnFailedRestore,
        ));
    }
    let unreadable = |why: &str| Comparison::Unreadable(why.to_owned());
    let assets = restore.assets.ok_or_else(|| unreadable("no assets file"))?;
    let targets = assets["targets"]
        .as_object()
        .ok_or_else(|| unreadable("the assets file has no targets"))?;
    let mut theirs: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for (name, libraries) in targets {
        let framework = NuGetFramework::parse(name)
            .map_err(|e| Comparison::Unreadable(format!("target {name:?}: {e:?}")))?;
        let libraries = libraries
            .as_object()
            .ok_or_else(|| unreadable("a target does not map libraries"))?;
        let mut packages = BTreeSet::new();
        for (library, value) in libraries {
            if value["type"] != "package" {
                continue;
            }
            let (id, version) = library
                .split_once('/')
                .ok_or_else(|| unreadable("a library is not `id/version`"))?;
            packages.insert((id.to_ascii_lowercase(), version.to_owned()));
        }
        insert_once(&mut theirs, key(&framework)?, packages)?;
    }
    let mut ours: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for resolved in frameworks {
        let packages = resolved
            .closure
            .packages
            .iter()
            .map(|package| {
                (
                    package.identity.id.as_str().to_ascii_lowercase(),
                    package.identity.version.to_normalized_string(),
                )
            })
            .collect();
        insert_once(&mut ours, key(&resolved.framework)?, packages)?;
    }
    if ours != theirs {
        return Err(Comparison::Disagreed(Disagreement::Closures {
            b1: ours,
            restore: theirs,
        }));
    }
    // Where B₁ predicts every framework's warnings, they are the codes the
    // restore logged.
    let Some(predicted) = frameworks
        .iter()
        .map(|framework| framework.warnings.as_ref())
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(());
    };
    let predicted: BTreeSet<String> = predicted
        .into_iter()
        .flatten()
        .map(|code| code.as_str().to_owned())
        .collect();
    let logged: BTreeSet<String> = assets["logs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|log| log["code"].as_str().map(str::to_owned))
        .collect();
    if predicted != logged {
        return Err(Comparison::Disagreed(Disagreement::Warnings {
            predicted,
            logged,
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use borzoi_nuget::dgspec::NuGetLogCode;
    use borzoi_nuget::{
        NuGetVersion, PackageId, PackageIdentity, PackagePaths, ResolvedPackage,
        ResolvedPackageClosure,
    };
    use proptest::prelude::*;
    use serde_json::{Map, Value, json};

    use super::{Comparison, Disagreement, RestoreRecord, compare};
    use borzoi_nuget::NuGetFramework;
    use borzoi_nuget::dgspec::ResolvedFramework;

    /// The ids and versions the generated closures draw from.
    const IDS: [&str; 4] = ["FSharp.Core", "NETStandard.Library", "System.Memory", "A.B"];
    const VERSIONS: [&str; 3] = ["1.0.0", "4.5.5", "10.1.0-beta.1"];
    /// Each framework with the two spellings an assets file may give its
    /// target: version 4's alias and version 3's `NuGetFramework.ToString()`.
    const FRAMEWORKS: [(&str, &str); 3] = [
        ("net8.0", ".NETCoreApp,Version=v8.0"),
        ("netstandard2.0", ".NETStandard,Version=v2.0"),
        ("net10.0", ".NETCoreApp,Version=v10.0"),
    ];
    const CODES: [&str; 3] = ["NU1603", "NU1608", "NU1510"];

    fn package(id: &str, version: &str) -> ResolvedPackage {
        let identity = PackageIdentity::new(
            PackageId::parse(id).unwrap(),
            NuGetVersion::parse(version).unwrap(),
        );
        ResolvedPackage {
            paths: PackagePaths::new(Path::new("/packages"), &identity),
            identity,
        }
    }

    fn framework(
        alias: &str,
        packages: &[(usize, usize)],
        warnings: Option<&[usize]>,
    ) -> ResolvedFramework {
        ResolvedFramework {
            target_alias: alias.to_owned(),
            framework: NuGetFramework::parse(alias).unwrap(),
            closure: ResolvedPackageClosure {
                packages: packages
                    .iter()
                    .map(|&(id, version)| package(IDS[id], VERSIONS[version]))
                    .collect(),
                resolved_dependencies: Vec::new(),
                examined: Vec::new(),
            },
            warnings: warnings.map(|codes| {
                codes
                    .iter()
                    .map(|&code| NuGetLogCode::parse(CODES[code]).unwrap())
                    .collect()
            }),
        }
    }

    /// The assets file a restore writes for `frameworks`: each target under
    /// the name its version gives it, a project library beside the packages
    /// (which the comparison must ignore), and every predicted code logged.
    fn assets_for(frameworks: &[ResolvedFramework], long_names: bool) -> Value {
        let mut targets = Map::new();
        for resolved in frameworks {
            let (alias, long) = FRAMEWORKS
                .iter()
                .find(|(alias, _)| *alias == resolved.target_alias)
                .unwrap();
            let mut libraries = Map::new();
            for package in &resolved.closure.packages {
                libraries.insert(
                    format!(
                        "{}/{}",
                        package.identity.id.as_str(),
                        package.identity.version.to_normalized_string()
                    ),
                    json!({ "type": "package" }),
                );
            }
            libraries.insert("Lib/1.0.0".to_owned(), json!({ "type": "project" }));
            let name = if long_names { long } else { alias };
            targets.insert((*name).to_owned(), Value::Object(libraries));
        }
        let logs: Vec<Value> = frameworks
            .iter()
            .flat_map(|framework| framework.warnings.iter().flatten())
            .map(|code| json!({ "code": code.as_str(), "level": "Warning" }))
            .collect();
        json!({ "version": 3, "targets": targets, "logs": logs })
    }

    fn agree(frameworks: &[ResolvedFramework], assets: &Value) -> Result<(), Disagreement> {
        match compare(
            frameworks,
            RestoreRecord {
                succeeded: true,
                assets: Some(assets),
            },
        ) {
            Ok(()) => Ok(()),
            Err(Comparison::Disagreed(disagreement)) => Err(disagreement),
            Err(Comparison::Unreadable(why)) => panic!("unreadable: {why}"),
        }
    }

    prop_compose! {
        /// One to three distinct frameworks, each with a closure over distinct
        /// ids and its warnings, predicted or not.
        fn frameworks()(
            chosen in proptest::sample::subsequence(vec![0usize, 1, 2], 1..=3),
            closures in proptest::collection::vec(
                proptest::collection::btree_map(0..IDS.len(), 0..VERSIONS.len(), 0..=4),
                3,
            ),
            warnings in proptest::collection::vec(
                proptest::option::of(proptest::collection::btree_set(0..CODES.len(), 0..=2)),
                3,
            ),
        ) -> Vec<ResolvedFramework> {
            chosen
                .into_iter()
                .map(|index| {
                    let packages: Vec<(usize, usize)> = closures[index].clone().into_iter().collect();
                    let codes: Option<Vec<usize>> =
                        warnings[index].clone().map(|codes| codes.into_iter().collect());
                    framework(FRAMEWORKS[index].0, &packages, codes.as_deref())
                })
                .collect()
        }
    }

    proptest! {
        /// The restore that did what B₁ says agrees, whichever spelling the
        /// assets file gives its targets.
        #[test]
        fn the_restore_b1_describes_agrees(frameworks in frameworks(), long in any::<bool>()) {
            prop_assert_eq!(agree(&frameworks, &assets_for(&frameworks, long)), Ok(()));
        }

        /// Any one change to the restore's closures disagrees: a package
        /// dropped, added, or at another version, in any framework, or a
        /// framework missing or extra.
        #[test]
        fn every_change_to_a_closure_disagrees(
            frameworks in frameworks(),
            which in any::<proptest::sample::Index>(),
            change in 0..5usize,
            id in 0..IDS.len(),
            version in 0..VERSIONS.len(),
        ) {
            let mut restore = frameworks.clone();
            let target = which.index(restore.len());
            let packages = &mut restore[target].closure.packages;
            match change {
                0 if !packages.is_empty() => { packages.remove(which.index(packages.len())); }
                1 => {
                    let new = package(IDS[id], VERSIONS[version]);
                    if packages.iter().any(|p| p.identity == new.identity) {
                        return Ok(());
                    }
                    packages.push(new);
                }
                2 if !packages.is_empty() => {
                    let at = which.index(packages.len());
                    let id = packages[at].identity.id.as_str().to_owned();
                    let other = VERSIONS
                        .iter()
                        .find(|v| NuGetVersion::parse(v).unwrap() != packages[at].identity.version)
                        .unwrap();
                    packages[at] = package(&id, other);
                }
                3 if restore.len() > 1 => { restore.remove(target); }
                4 => {
                    let Some((alias, _)) = FRAMEWORKS
                        .iter()
                        .find(|(alias, _)| restore.iter().all(|f| f.target_alias != *alias))
                    else {
                        return Ok(());
                    };
                    restore.push(framework(alias, &[], None));
                }
                _ => return Ok(()),
            }
            let verdict = agree(&frameworks, &assets_for(&restore, false));
            let disagrees = matches!(verdict, Err(Disagreement::Closures { .. }));
            prop_assert!(disagrees, "{:?}", verdict);
        }

        /// Where B₁ predicts every framework's warnings, a code it predicts
        /// that the restore did not log, or one logged that it did not
        /// predict, disagrees; where it predicts none exactly, the log is not
        /// compared.
        #[test]
        fn a_changed_log_disagrees_exactly_where_b1_predicts(
            frameworks in frameworks(),
            code in 0..CODES.len(),
        ) {
            let mut assets = assets_for(&frameworks, false);
            let logs = assets["logs"].as_array_mut().unwrap();
            let logged = logs.iter().any(|log| log["code"] == CODES[code]);
            if logged {
                logs.retain(|log| log["code"] != CODES[code]);
            } else {
                logs.push(json!({ "code": CODES[code], "level": "Warning" }));
            }
            let predicts = frameworks.iter().all(|f| f.warnings.is_some());
            let verdict = agree(&frameworks, &assets);
            if predicts {
                let disagrees = matches!(verdict, Err(Disagreement::Warnings { .. }));
                prop_assert!(disagrees, "{:?}", verdict);
            } else {
                prop_assert_eq!(verdict, Ok(()));
            }
        }

        /// A commit on a restore that failed disagrees, whatever it wrote.
        #[test]
        fn a_commit_on_a_failed_restore_disagrees(frameworks in frameworks()) {
            let assets = assets_for(&frameworks, false);
            let verdict = compare(
                &frameworks,
                RestoreRecord { succeeded: false, assets: Some(&assets) },
            );
            let disagrees = matches!(
                verdict,
                Err(Comparison::Disagreed(Disagreement::CommittedOnFailedRestore))
            );
            prop_assert!(disagrees);
        }
    }

    /// Without an assets file, or with one not in the restore's shape, a
    /// commit is not compared, rather than read as agreeing or disagreeing.
    #[test]
    fn an_unreadable_assets_file_is_not_compared() {
        let frameworks = [framework("net8.0", &[(0, 0)], Some(&[]))];
        let shapes = [
            None,
            Some(json!({})),
            Some(json!({ "targets": [] })),
            Some(json!({ "targets": { "net8.0": [] } })),
            Some(json!({ "targets": { "not a framework!": {} } })),
            Some(json!({ "targets": { "net8.0": {}, ".NETCoreApp,Version=v8.0": {} } })),
            Some(json!({ "targets": { "net8.0": { "FSharp.Core": { "type": "package" } } } })),
        ];
        for assets in &shapes {
            let verdict = compare(
                &frameworks,
                RestoreRecord {
                    succeeded: true,
                    assets: assets.as_ref(),
                },
            );
            assert!(
                matches!(verdict, Err(Comparison::Unreadable(_))),
                "{assets:?}"
            );
        }
    }
}
