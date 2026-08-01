//! Differential test: **whether and where `Directory.Build.targets` is
//! imported**, over the properties MSBuild lets a project use to move or
//! suppress that import, against the real evaluator.
//!
//! ## Why this is its own harness
//!
//! `fsproj_derived_tfm_diff.rs` asks *what a name evaluates to* at three
//! positions. It cannot see this seam, because every question here is about the
//! **import itself**: did it happen at all, and did it happen before or after
//! its neighbours. A value-witness at a fixed position is blind to an import
//! that moved past it or vanished.
//!
//! The subject is a small, closed set of documented MSBuild hooks, and every
//! one of them is reachable through the real SDK — which is what makes this
//! diffable at all. `CustomBeforeDirectoryBuildTargets` runs inside
//! `Sdk.targets` *before* the real import point;
//! `CustomAfterDirectoryBuildTargets` runs after it;
//! `DirectoryBuildTargetsPath` redirects it; `ImportDirectoryBuildTargets`
//! suppresses it. Their interactions are exactly the cases where "we own this
//! import point" and "the SDK chain owns it" disagree.
//!
//! ## How order is diffed
//!
//! Every participating file **appends** its own marker to a single `Trace`
//! property. The final value is therefore a verbatim log of which files ran and
//! in what order — so a missing import, a duplicated one, and a reordered one
//! are three distinct, legible failures rather than one opaque mismatch.
//!
//! ## The contract
//!
//! The crate's usual one: we commit `Trace` with trusted provenance ⟹ MSBuild
//! evaluates the same document at the same path to the byte-identical value; we
//! decline ⟹ no claim; MSBuild rejects ⟹ we committed nothing.
//!
//! ## What it found
//!
//! Written after a review round on the import-position change caught two
//! defects this file's axes cover and the value-witness harness structurally
//! could not:
//!
//! - a hook that sets `ImportDirectoryBuildTargets=false` from inside
//!   `Sdk.targets` — MSBuild skips the import, and both the pre- and
//!   post-change walkers performed it anyway (a **pre-existing** wrong commit,
//!   not a regression);
//! - a `DirectoryBuildTargetsPath` redirect — the walker's rediscovery
//!   suppression assumed *it* owned the import point, so once the chain owned it
//!   instead the import was suppressed and then replayed late, past
//!   `CustomAfterDirectoryBuildTargets`.
//!
//! Both are ordinary certain-implies-exact violations once the axis is swept.
//! No new contract was needed, only a harness that varies what the other one
//! holds fixed.

mod common;

use std::collections::HashMap;
use std::path::Path;

use borzoi_msbuild::{parse_fsproj_with_imports, resolve_sdk, workloads};
use common::Oracle;
use tempfile::TempDir;

/// A file that appends `marker` to `Trace`, plus any extra property writes.
fn tracer(marker: &str, extra: &str) -> String {
    format!(
        "<Project>\n  <PropertyGroup>\n    <Trace>$(Trace);{marker}</Trace>\n{extra}  \
         </PropertyGroup>\n</Project>\n"
    )
}

/// One case: a name, and the files to materialise in the project directory.
struct Case {
    name: &'static str,
    files: Vec<(String, String)>,
}

/// The project document every case uses. `Directory.Build.props` seeds `Trace`,
/// so the log always starts from a known point.
const PROJECT: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
</Project>
"#;

/// `Directory.Build.props` with caller-supplied extra property writes. This is
/// the only place a hook property can be set and still be visible to
/// `Sdk.targets`: the SDK reads them during its own walk, after the body.
fn props(extra: &str) -> String {
    format!(
        "<Project>\n  <PropertyGroup>\n    <Trace>body</Trace>\n{extra}  \
         </PropertyGroup>\n</Project>\n"
    )
}

fn f(name: &str, body: String) -> (String, String) {
    (name.to_string(), body)
}

/// The case list. Each is a documented MSBuild hook or a combination of them.
fn cases() -> Vec<Case> {
    let dbt = tracer("dbt", "");
    vec![
        Case {
            name: "plain",
            files: vec![
                f("Directory.Build.props", props("")),
                f("Directory.Build.targets", dbt.clone()),
            ],
        },
        // The import point's two documented neighbours, which bracket it.
        Case {
            name: "custom-before-and-after",
            files: vec![
                f(
                    "Directory.Build.props",
                    props(
                        "    <CustomBeforeDirectoryBuildTargets>$(MSBuildThisFileDirectory)Before.targets</CustomBeforeDirectoryBuildTargets>\n\
                             <CustomAfterDirectoryBuildTargets>$(MSBuildThisFileDirectory)After.targets</CustomAfterDirectoryBuildTargets>\n",
                    ),
                ),
                f("Before.targets", tracer("before", "")),
                f("After.targets", tracer("after", "")),
                f("Directory.Build.targets", dbt.clone()),
            ],
        },
        // A hook that turns the gate off from *inside* `Sdk.targets`, before the
        // real import point. MSBuild does not retry a passed import, so the file
        // must not be walked at all.
        Case {
            name: "custom-before-disables-gate",
            files: vec![
                f(
                    "Directory.Build.props",
                    props(
                        "    <CustomBeforeDirectoryBuildTargets>$(MSBuildThisFileDirectory)Off.targets</CustomBeforeDirectoryBuildTargets>\n",
                    ),
                ),
                f(
                    "Off.targets",
                    tracer(
                        "off",
                        "    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>\n",
                    ),
                ),
                f("Directory.Build.targets", dbt.clone()),
            ],
        },
        // The mirror image: a hook that turns the gate *on* after it was off.
        Case {
            name: "custom-before-enables-gate",
            files: vec![
                f(
                    "Directory.Build.props",
                    props(
                        "    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>\n\
                             <CustomBeforeDirectoryBuildTargets>$(MSBuildThisFileDirectory)On.targets</CustomBeforeDirectoryBuildTargets>\n",
                    ),
                ),
                f(
                    "On.targets",
                    tracer(
                        "on",
                        "    <ImportDirectoryBuildTargets>true</ImportDirectoryBuildTargets>\n",
                    ),
                ),
                f("Directory.Build.targets", dbt.clone()),
            ],
        },
        // A redirect, alone and combined with the after-hook. The combination is
        // what pins *ordering*: the redirect changes which file runs, the hook
        // pins where in the sequence it runs.
        Case {
            name: "redirect",
            files: vec![
                f(
                    "Directory.Build.props",
                    props(
                        "    <DirectoryBuildTargetsPath>$(MSBuildThisFileDirectory)Redirected.targets</DirectoryBuildTargetsPath>\n",
                    ),
                ),
                f("Redirected.targets", dbt.clone()),
                // Present but *not* the redirect target, so walking it would be
                // its own distinct failure.
                f("Directory.Build.targets", tracer("unredirected", "")),
            ],
        },
        Case {
            name: "redirect-with-custom-after",
            files: vec![
                f(
                    "Directory.Build.props",
                    props(
                        "    <DirectoryBuildTargetsPath>$(MSBuildThisFileDirectory)Redirected.targets</DirectoryBuildTargetsPath>\n\
                             <CustomAfterDirectoryBuildTargets>$(MSBuildThisFileDirectory)After.targets</CustomAfterDirectoryBuildTargets>\n",
                    ),
                ),
                f("Redirected.targets", dbt.clone()),
                f("After.targets", tracer("after", "")),
            ],
        },
        // Opted out outright, and opted out with the after-hook still present so
        // a spurious import is visible in the ordering rather than only in
        // membership.
        Case {
            name: "gate-off",
            files: vec![
                f(
                    "Directory.Build.props",
                    props("    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>\n"),
                ),
                f("Directory.Build.targets", dbt.clone()),
            ],
        },
        Case {
            name: "gate-off-with-custom-after",
            files: vec![
                f(
                    "Directory.Build.props",
                    props(
                        "    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>\n\
                             <CustomAfterDirectoryBuildTargets>$(MSBuildThisFileDirectory)After.targets</CustomAfterDirectoryBuildTargets>\n",
                    ),
                ),
                f("After.targets", tracer("after", "")),
                f("Directory.Build.targets", dbt.clone()),
            ],
        },
        // No `Directory.Build.targets` on disk at all: the import is a clean
        // skip on both sides, and must not manufacture a diagnostic that
        // withdraws the neighbours' claims.
        Case {
            name: "absent",
            files: vec![f("Directory.Build.props", props(""))],
        },
    ]
}

/// Evaluate one case both ways and return `(ours-or-None-if-declined, theirs)`.
fn evaluate(
    oracle: &mut Oracle,
    dir: &Path,
    case: &Case,
) -> (Option<String>, Option<String>, String) {
    // A fresh directory per case: a stale `Directory.Build.targets` from the
    // previous one would silently change the subject.
    if dir.exists() {
        std::fs::remove_dir_all(dir).expect("clear case directory");
    }
    std::fs::create_dir_all(dir).expect("create case directory");
    for (name, body) in &case.files {
        std::fs::write(dir.join(name), body)
            .unwrap_or_else(|e| panic!("write {name} for {}: {e}", case.name));
    }
    let project_path = dir.join("Demo.fsproj");
    std::fs::write(&project_path, PROJECT).expect("write project");

    let dotnet_root = common::dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = common::workload_env_from_process();
    let resolver = |name: &str| {
        resolve_sdk(
            &dotnet_root,
            None,
            name,
            None,
            None,
            &workloads::WorkloadEnvironment {
                user_dotnet_root: user_dotnet_root.as_deref(),
                overrides_present,
                global_json_pins_workload_set: false,
            },
        )
    };
    let parsed = parse_fsproj_with_imports(
        PROJECT,
        &project_path,
        &HashMap::new(),
        &common::oracle_environment(),
        Some(&resolver as &borzoi_msbuild::SdkResolver<'_>),
        None,
    )
    .expect("well-formed XML parses");

    let ours = if parsed.property_provenance_untrusted("Trace") {
        None
    } else {
        parsed.properties.get("Trace").cloned()
    };
    let theirs = oracle
        .project(PROJECT, &["Trace".to_string()], Some(&project_path), &[])
        .map(|t| t["Trace"].clone());
    (ours, theirs, project_path.display().to_string())
}

/// Certain-implies-exact over the whole case list, reported as a worklist.
///
/// Collected rather than panicked on at the first case: one modelling mistake
/// in this seam produces several rows, and stopping at the first makes a family
/// look like an instance.
#[test]
fn the_directory_build_targets_import_is_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    let mut committed = 0usize;
    let mut declined = 0usize;
    let mut divergences = Vec::new();

    for case in cases() {
        let (ours, theirs, path) = evaluate(&mut oracle, &dir, &case);
        match (&ours, &theirs) {
            (Some(ours), None) => divergences.push(format!(
                "  {}: MSBuild rejects the project, we committed {ours:?}",
                case.name
            )),
            (Some(ours), Some(theirs)) if ours != theirs => {
                committed += 1;
                divergences.push(format!(
                    "  {}: ours {ours:?}  msbuild {theirs:?}   ({path})",
                    case.name
                ));
            }
            (Some(_), Some(_)) => committed += 1,
            (None, _) => declined += 1,
        }
    }
    eprintln!("directory-build import: {committed} committed, {declined} declined");

    assert!(
        divergences.is_empty(),
        "certain-implies-exact violated in {} of the import cases:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    // Anti-vacuity: declining every case satisfies the contract perfectly.
    assert!(
        committed >= 7,
        "only {committed} cases committed — the walker may have started \
         declining this seam wholesale, which passes vacuously"
    );
}
