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
//!
//! ## The gate's *vocabulary*, and the one route with no project oracle
//!
//! The hooks above are swept with literal `true`/`false`. That leaves the
//! comparison itself untested — `'$(ImportDirectoryBuild*)' == 'true'` is an
//! MSBuild `==`, so `on`, `yes` and `!false` open it too. Two tests cover it,
//! and the split between them is the point:
//!
//! - [`the_directory_build_props_gate_vocabulary_is_exact_or_declined`] sweeps
//!   the vocabulary through a whole SDK-resolved evaluation. This is genuine
//!   coverage of the composition, but it is **blind to the walker's own gate
//!   predicate** — with the chain present, a wrongly-closed hand-rolled gate
//!   just stops suppressing the chain's rediscovery import, and `condition.rs`
//!   decides it correctly instead. The errors cancel.
//! - [`the_gate_predicate_matches_msbuild_equality`] therefore diffs the
//!   predicate against MSBuild's `==` directly. The predicate decides the
//!   import only when the SDK chain is *absent* — which is exactly when
//!   MSBuild cannot evaluate the project either, so no whole-project oracle
//!   for that route can exist. Diffing the reimplemented comparison is what is
//!   left, and it is enough.
//!
//! Both were checked by mutation (`is_msbuild_true` reverted to
//! `eq_ignore_ascii_case("true")`): the first stays green, the second reports
//! 54 divergences. A differential that cannot fail is worse than none, because
//! it reads as coverage.

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

/// Gate values for `ImportDirectoryBuildProps`, as `(case name, value)`.
///
/// See [`the_directory_build_props_gate_vocabulary_is_exact_or_declined`] for
/// why the sweep targets the *props* gate as a **global**, and why sweeping the
/// targets gate instead cannot see the defect it exists for.
///
/// The alphabet is MSBuild's boolean vocabulary, its negations, its near-misses
/// (`0`/`1`, which `==` does *not* admit as booleans), and the whitespace
/// spellings — blank compares equal to empty, while a padded `" true "` does
/// not compare true.
const GATE_VALUES: &[(&str, &str)] = &[
    ("gate-true", "true"),
    ("gate-TRUE", "TRUE"),
    ("gate-on", "on"),
    ("gate-ON", "ON"),
    ("gate-yes", "yes"),
    ("gate-bang-false", "!false"),
    ("gate-bang-off", "!off"),
    ("gate-bang-no", "!no"),
    ("gate-false", "false"),
    ("gate-off", "off"),
    ("gate-no", "no"),
    ("gate-bang-true", "!true"),
    ("gate-zero", "0"),
    ("gate-one", "1"),
    ("gate-double-bang", "!!true"),
    ("gate-padded-true", " true "),
    ("gate-blank", "   "),
    ("gate-empty", ""),
    ("gate-nonsense", "nonsense"),
];

/// The case list. Each is a documented MSBuild hook or a combination of them.
/// The gate's *value* axis is swept separately, over [`GATE_VALUES`].
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

/// The project document the gate sweep uses. Unlike [`PROJECT`] the body
/// appends to `Trace` itself, so the property exists on both sides whether or
/// not `Directory.Build.props` was imported — the two outcomes are then
/// `";dbp;body"` and `";body"` rather than a value-versus-absent comparison,
/// which would confuse "not imported" with "declined".
const GATE_PROJECT: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <Trace>$(Trace);body</Trace>
  </PropertyGroup>
</Project>
"#;

/// Evaluate the gate sweep's document under one global gate value.
fn evaluate_gate(oracle: &mut Oracle, dir: &Path, value: &str) -> (Option<String>, Option<String>) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).expect("clear case directory");
    }
    std::fs::create_dir_all(dir).expect("create case directory");
    std::fs::write(dir.join("Directory.Build.props"), tracer("dbp", ""))
        .expect("write Directory.Build.props");
    let project_path = dir.join("Demo.fsproj");
    std::fs::write(&project_path, GATE_PROJECT).expect("write project");

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
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), value.to_string());
    let parsed = parse_fsproj_with_imports(
        GATE_PROJECT,
        &project_path,
        &extras,
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
    let globals = [("ImportDirectoryBuildProps".to_string(), value.to_string())];
    let theirs = oracle
        .project(
            GATE_PROJECT,
            &["Trace".to_string()],
            Some(&project_path),
            &globals,
        )
        .map(|t| t["Trace"].clone());
    (ours, theirs)
}

/// Certain-implies-exact over MSBuild's **boolean vocabulary** at the
/// `Directory.Build.props` gate, through a whole SDK-resolved evaluation.
///
/// ## What this does and does not cover
///
/// It covers the composition — gate value, rediscovery suppression, splice
/// position, chain ownership — across the vocabulary rather than the two
/// literal spellings the hook cases above use.
///
/// It does **not** cover `should_import_default_true`, the walker's own
/// reimplementation of the gate, and cannot: with the SDK resolved, a closed
/// hand-rolled gate simply stops suppressing the chain's own rediscovery
/// import, so the real `Microsoft.Common.props` condition runs through
/// `condition.rs` — which models the vocabulary correctly — and the two errors
/// cancel exactly. Confirmed by mutation: replacing the predicate with
/// `eq_ignore_ascii_case("true")` leaves this test green.
///
/// That is not a gap to be widened away. The predicate decides the import
/// precisely when the SDK chain is *absent*, and a project whose SDK does not
/// resolve is one the real MSBuild cannot evaluate either — so there is no
/// whole-project oracle for the route that uses it, in principle and not just
/// here. It is diffed directly instead, by
/// [`the_gate_predicate_matches_msbuild_equality`].
#[test]
fn the_directory_build_props_gate_vocabulary_is_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    let mut committed = 0usize;
    let mut imported = 0usize;
    let mut divergences = Vec::new();

    for (name, value) in GATE_VALUES {
        let (ours, theirs) = evaluate_gate(&mut oracle, &dir, value);
        match (&ours, &theirs) {
            (Some(ours), None) => divergences.push(format!(
                "  {name} ({value:?}): MSBuild rejects the project, we committed {ours:?}"
            )),
            (Some(ours), Some(theirs)) if ours != theirs => {
                committed += 1;
                divergences.push(format!(
                    "  {name} ({value:?}): ours {ours:?}  msbuild {theirs:?}"
                ));
            }
            (Some(ours), Some(_)) => {
                committed += 1;
                if ours.contains("dbp") {
                    imported += 1;
                }
            }
            (None, _) => {}
        }
    }
    eprintln!("directory-build props gate: {committed} committed, {imported} of them imported");

    assert!(
        divergences.is_empty(),
        "certain-implies-exact violated in {} of the gate values:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    // Anti-vacuity, both directions. Declining every value satisfies the
    // contract, and so does agreeing on a sweep where the gate never opens
    // (or never closes) — the alphabet is only doing work if both outcomes
    // are actually represented.
    assert_eq!(
        committed,
        GATE_VALUES.len(),
        "every gate value must be committed; declining hides the comparison"
    );
    assert!(
        imported > 0 && imported < committed,
        "the sweep must contain both opened and closed gates, got {imported} \
         imported of {committed}"
    );
}

/// A gate value drawn from the boolean vocabulary's neighbourhood: the words
/// themselves under random casing, `!`-negations to arbitrary depth, padding,
/// and near-miss junk built from the same letters. Hand-picking here is what
/// let the defect through in the first place — `on`/`yes` are only obvious
/// once you already know the answer.
fn gen_gate_value(rng: &mut common::SplitMix64) -> String {
    const WORDS: &[&str] = &["true", "false", "on", "off", "yes", "no", "0", "1", ""];
    const LETTERS: &[char] = &[
        't', 'r', 'u', 'e', 'f', 'a', 'l', 's', 'o', 'n', 'y', 'O', 'N',
    ];
    let mut value = if rng.below(4) == 0 {
        // Junk over the same letters: catches a predicate that accepts a
        // prefix, a substring, or anything else short of the whole word.
        (0..1 + rng.below(5))
            .map(|_| *rng.pick(LETTERS))
            .collect::<String>()
    } else {
        let word = *rng.pick(WORDS);
        // Random casing — the comparison is case-insensitive, so every
        // spelling must agree with the canonical one.
        word.chars()
            .map(|c| {
                if rng.below(2) == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect::<String>()
    };
    for _ in 0..rng.below(3) {
        value.insert(0, '!');
    }
    match rng.below(8) {
        0 => value.insert(0, ' '),
        1 => value.push(' '),
        _ => {}
    }
    value
}

/// Diff the walker's gate predicate directly against MSBuild's `==`.
///
/// This is the gate that catches the defect
/// [`the_directory_build_props_gate_vocabulary_is_exact_or_declined`]
/// structurally cannot (see its docs): `should_import_default_true` decides the
/// import only when the SDK chain is absent, and a project with no resolvable
/// SDK has no whole-project oracle. What *is* diffable is the comparison being
/// reimplemented — `'$(Prop)' == 'true'`, the literal condition text from
/// `Microsoft.Common.props` — so the predicate is checked against the real
/// evaluator on that condition, value by value.
///
/// The obligation is equality, not implication. A gate predicate that is
/// merely *conservative* is not safe in either direction here: reading true as
/// false loses the file's properties and items, and reading false as true
/// invents them, and neither shows up as an uncertainty.
#[test]
fn the_gate_predicate_matches_msbuild_equality() {
    const CONDITION: &str = "'$(X)' == 'true'";
    let mut oracle = Oracle::spawn();
    let mut rng = common::SplitMix64(0x9a7d_e1f0_4c33_b105);
    let mut divergences = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut opened: Vec<String> = Vec::new();
    let mut closed = 0usize;

    // The vocabulary's own spellings, then a generated sweep of its
    // neighbourhood.
    let fixed = GATE_VALUES.iter().map(|(_, v)| (*v).to_string());
    let generated = (0..4_000).map(|_| gen_gate_value(&mut rng));
    for value in fixed.chain(generated.collect::<Vec<_>>()) {
        if seen.insert(value.clone(), ()).is_some() {
            continue;
        }
        let ours = borzoi_msbuild::test_support::is_msbuild_true(&value);
        let props = [("X".to_string(), value.clone())];
        let Some(theirs) = oracle.eval(CONDITION, &props) else {
            // A quoted comparison over a property is always legal MSBuild, so
            // a rejection means the harness fed something it should not have.
            divergences.push(format!("  {value:?}: MSBuild rejected {CONDITION}"));
            continue;
        };
        if ours != theirs {
            divergences.push(format!("  {value:?}: ours {ours}, msbuild {theirs}"));
        }
        if theirs {
            opened.push(value.clone());
        } else {
            closed += 1;
        }
    }
    let total = opened.len() + closed;
    eprintln!(
        "gate predicate: {total} distinct values, {} true, {closed} false",
        opened.len()
    );

    assert!(
        divergences.is_empty(),
        "the gate predicate disagrees with MSBuild `==` on {} of {total} values:\n{}",
        divergences.len(),
        divergences
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Anti-vacuity. A generator that only ever produced junk would agree
    // trivially, since everything outside the vocabulary is false — so require
    // that the sweep reaches truths *beyond the literal word*, which is
    // precisely the region a string-equality predicate gets wrong. Stated as
    // coverage of the region rather than as a count: the true-space here is
    // small and closed (~70 distinct strings, being the vocabulary under
    // arbitrary casing and `!`-negation), so any count floor would sit just
    // under a ceiling the generator cannot exceed.
    let beyond_the_literal_word: Vec<&String> = opened
        .iter()
        .filter(|v| !v.eq_ignore_ascii_case("true"))
        .collect();
    assert!(
        !beyond_the_literal_word.is_empty(),
        "every value that opened the gate was a spelling of \"true\" — the \
         sweep never reached `on`/`yes`/`!false`, so it cannot see the defect \
         it exists for"
    );
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
