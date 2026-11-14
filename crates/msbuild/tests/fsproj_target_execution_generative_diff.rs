//! Generative differential: **certainty implies exactness** for the target
//! executor (stage A₁.3c of `docs/nuget-dgspec-boundary-plan.md`).
//!
//! The executor runs targets on the walk's end state and answers with the
//! targets that executed, in order, and the properties and items the run
//! leaves, or declines. This harness writes small projects over what decides
//! a run:
//! - the schedule: `DependsOnTargets` (literal, or read from a property),
//!   `BeforeTargets` and `AfterTargets`, cycles among them, and a target's
//!   `Condition` on properties earlier targets write;
//! - target bodies: `PropertyGroup`s and `ItemGroup` `Include`s with
//!   conditions and `$(…)` values, `Message`, and what the executor declines
//!   (another task, `Inputs`/`Outputs`);
//! - an untrusted property (written under a condition the walk cannot decide).
//!
//! Every run the executor commits to must be MSBuild's (the oracle's
//! `runTargets` op): the same targets in the same order, the same properties,
//! the same items with the same metadata. A build MSBuild fails must decline.

mod common;

use std::collections::{BTreeMap, HashMap};

use borzoi_msbuild::observed::host_fs;
use borzoi_msbuild::test_support::{ExecutionProbe, ExecutionRead, ReadProbe, parse_fsproj_reads};
use common::{Oracle, SplitMix64};

const TARGETS: [&str; 4] = ["T0", "T1", "T2", "T3"];
const PROPERTIES: [&str; 3] = ["A", "B", "C"];
/// Values a target writes.
const VALUES: [&str; 6] = ["x", "y", "$(A)z", "$(B)", "", "$(Gated)"];
const CONDITIONS: [&str; 6] = [
    "'$(A)' == 'x'",
    "'$(A)' != 'x'",
    "'$(B)' == ''",
    "'$(C)' == 'y'",
    "'$(Gated)' == 'g'",
    "Exists('nowhere')",
];
/// A condition MSBuild accepts and this evaluator cannot decide, so that
/// `Gated` is untrusted.
const UNDECIDABLE: &str = "$([System.String]::IsNullOrWhiteSpace(''))";

fn condition(rng: &mut SplitMix64) -> String {
    if rng.below(3) == 0 {
        // `Exists` (which the executor declines) only now and then.
        let pick = if rng.below(10) == 0 {
            CONDITIONS.len() - 1
        } else {
            rng.below(CONDITIONS.len() - 1)
        };
        format!(" Condition=\"{}\"", CONDITIONS[pick])
    } else {
        String::new()
    }
}

/// Targets other than the `of`th: mostly on the `later` side of it, so that
/// most graphs are acyclic (a target depends on later ones, and hooks onto
/// earlier ones), and now and then any.
fn others(rng: &mut SplitMix64, of: usize, later: bool) -> Vec<&'static str> {
    let any = rng.below(6) == 0;
    TARGETS
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != of && (any || (*i > of) == later) && rng.below(2) == 0)
        .map(|(_, t)| *t)
        .collect()
}

fn project(rng: &mut SplitMix64) -> String {
    let mut out = String::from("<Project>\n  <PropertyGroup>\n");
    out.push_str(&format!("    <A>{}</A>\n", rng.pick(&["x", "y"])));
    if rng.below(2) == 0 {
        out.push_str("    <B>b</B>\n");
    }
    if rng.below(3) == 0 {
        out.push_str(&format!(
            "    <Gated Condition=\"{UNDECIDABLE}\">g</Gated>\n"
        ));
    }
    out.push_str("    <Deps>T2;T3</Deps>\n    <Hook>T1</Hook>\n  </PropertyGroup>\n");
    if rng.below(2) == 0 {
        out.push_str("  <ItemGroup>\n    <I Include=\"e\" M=\"m\" />\n  </ItemGroup>\n");
    }
    for index in 0..TARGETS.len() {
        target(rng, index, &mut out);
    }
    // A later definition of a name replaces the earlier one, and stands
    // where it does.
    if rng.below(4) == 0 {
        let index = rng.below(TARGETS.len());
        target(rng, index, &mut out);
    }
    out.push_str("</Project>\n");
    out
}

fn target(rng: &mut SplitMix64, index: usize, out: &mut String) {
    let name = TARGETS[index];
    {
        let mut attributes = condition(rng);
        if rng.below(4) != 0 {
            let depends = if rng.below(6) == 0 {
                "$(Deps)".to_owned()
            } else {
                others(rng, index, true).join(";")
            };
            attributes.push_str(&format!(" DependsOnTargets=\"{depends}\""));
        }
        for hook in ["BeforeTargets", "AfterTargets"] {
            if rng.below(4) == 0 {
                let named = if rng.below(8) == 0 {
                    vec!["$(Hook)"]
                } else {
                    others(rng, index, false)
                };
                if !named.is_empty() {
                    attributes.push_str(&format!(" {hook}=\"{}\"", named.join(";")));
                }
            }
        }
        if rng.below(40) == 0 {
            attributes.push_str(" Inputs=\"a\" Outputs=\"b\"");
        }
        out.push_str(&format!("  <Target Name=\"{name}\"{attributes}>\n"));
        for _ in 0..rng.below(3) {
            match rng.below(12) {
                0..=6 => {
                    out.push_str(&format!("    <PropertyGroup{}>\n", condition(rng)));
                    for _ in 0..1 + rng.below(2) {
                        let property = rng.pick(&PROPERTIES);
                        out.push_str(&format!(
                            "      <{property}{}>{}</{property}>\n",
                            condition(rng),
                            rng.pick(&VALUES)
                        ));
                    }
                    out.push_str("    </PropertyGroup>\n");
                }
                7 | 8 => out.push_str(&format!(
                    "    <ItemGroup>\n      <I Include=\"{};k\" M=\"{}\"{} />\n    </ItemGroup>\n",
                    rng.pick(&VALUES),
                    rng.pick(&VALUES),
                    condition(rng)
                )),
                9 | 10 => out.push_str("    <Message Text=\"$(A)\" Importance=\"low\" />\n"),
                _ => out.push_str("    <Error Condition=\"false\" Text=\"no\" />\n"),
            }
        }
        out.push_str("  </Target>\n");
    }
}

/// Whether a target in `xml` can bring itself up again through the targets it
/// runs before or after, or a literal `DependsOnTargets`, the last definition
/// of each name counting. MSBuild does not
/// finish such a build (two targets `AfterTargets` each other hang it), so the
/// oracle is not asked; the executor must decline.
fn triggers_cycle(xml: &str) -> bool {
    let document = roxmltree::Document::parse(xml).unwrap();
    // The last definition of each name counts.
    let mut last: BTreeMap<&str, roxmltree::Node<'_, '_>> = BTreeMap::new();
    for target in document.descendants().filter(|n| n.has_tag_name("Target")) {
        last.insert(target.attribute("Name").unwrap(), target);
    }
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for target in last.into_values() {
        let name = target.attribute("Name").unwrap().to_owned();
        let list = |attribute: &str| -> Vec<String> {
            target
                .attribute(attribute)
                // Hooks expand against the evaluation; a dependency read from a
                // property is decided when it comes up, and a cycle through it
                // fails the build rather than hanging it.
                .map(|raw| raw.replace("$(Hook)", "T1"))
                .filter(|raw| !raw.contains('$'))
                .map(|raw| {
                    raw.split(';')
                        .filter(|n| !n.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        edges
            .entry(name.clone())
            .or_default()
            .extend(list("DependsOnTargets"));
        for before in list("BeforeTargets") {
            edges.entry(before).or_default().push(name.clone());
        }
        for after in list("AfterTargets") {
            edges.entry(after).or_default().push(name.clone());
        }
    }
    fn reaches(
        from: &str,
        to: &str,
        edges: &BTreeMap<String, Vec<String>>,
        seen: &mut Vec<String>,
    ) -> bool {
        edges.get(from).into_iter().flatten().any(|next| {
            next == to
                || (!seen.contains(next) && {
                    seen.push(next.clone());
                    reaches(next, to, edges, seen)
                })
        })
    }
    TARGETS
        .iter()
        .any(|t| reaches(t, t, &edges, &mut Vec::new()))
}

#[derive(Default)]
struct Tally {
    cases: usize,
    rejected: usize,
    cycles: usize,
    failed: usize,
    committed: usize,
    committed_with_targets: usize,
}

fn check(oracle: &mut Oracle, xml: &str, tally: &mut Tally) {
    tally.cases += 1;
    let dir = tempfile::tempdir().unwrap();
    let path = host_fs::canonicalize(dir.path()).unwrap().join("p.proj");
    std::fs::write(&path, xml).unwrap();
    let probe = ReadProbe {
        execute: Some(ExecutionProbe {
            targets: vec!["T0".to_owned()],
            properties: PROPERTIES.iter().map(|p| (*p).to_owned()).collect(),
            item_types: vec!["I".to_owned()],
        }),
        ..ReadProbe::default()
    };
    let (_, reads) = parse_fsproj_reads(
        xml,
        &path,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
        &probe,
    )
    .unwrap();
    if triggers_cycle(xml) {
        tally.cycles += 1;
        assert!(
            matches!(reads.execution, Some(ExecutionRead::Declined(_))),
            "we ran a target graph MSBuild never finishes, in\n{xml}"
        );
        return;
    }
    let Some(theirs) = oracle.run_targets(&path, &["T0"], &PROPERTIES, &["I"]) else {
        tally.rejected += 1;
        return;
    };
    let succeeded = theirs["succeeded"].as_bool() == Some(true);
    tally.failed += usize::from(!succeeded);
    let ExecutionRead::Ran {
        targets,
        properties,
        items,
    } = reads.execution.unwrap()
    else {
        return;
    };
    assert!(
        succeeded,
        "we ran a build MSBuild fails ({}), in\n{xml}",
        theirs["errors"]
    );
    tally.committed += 1;
    tally.committed_with_targets += usize::from(targets.len() > 1);
    let msbuild_targets: Vec<&str> = theirs["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert_eq!(targets, msbuild_targets, "targets run, in\n{xml}");
    for (name, ours) in &properties {
        let msbuild = theirs["properties"][name].as_str();
        assert_eq!(
            ours.as_deref().filter(|v| !v.is_empty()),
            msbuild.filter(|v| !v.is_empty()),
            "property {name}, in\n{xml}"
        );
    }
    let ours: Vec<(String, BTreeMap<String, String>)> = items["I"]
        .iter()
        .map(|item| (item.identity.clone(), item.metadata.clone()))
        .collect();
    let msbuild: Vec<(String, BTreeMap<String, String>)> = theirs["items"]["I"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["identity"].as_str().unwrap().to_owned(),
                item["metadata"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str().unwrap().to_owned()))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(ours, msbuild, "items of I, in\n{xml}");
}

fn sweep(seed: u64, cases: usize) -> Tally {
    let mut rng = SplitMix64(seed);
    let mut oracle = Oracle::spawn();
    let mut tally = Tally::default();
    for _ in 0..cases {
        let xml = project(&mut rng);
        check(&mut oracle, &xml, &mut tally);
    }
    tally
}

#[test]
fn committed_runs_match_msbuild() {
    sweep(0x3c5e_a1e0, 300);
}

/// A sweep in which every run declined would pass while testing nothing.
#[test]
fn many_runs_are_committed() {
    let tally = sweep(0x0e7e_c0de, 200);
    assert!(
        tally.rejected * 10 < tally.cases,
        "MSBuild rejected {} of {} generated projects",
        tally.rejected,
        tally.cases
    );
    assert!(
        tally.committed * 4 > tally.cases,
        "only {} of {} runs were committed",
        tally.committed,
        tally.cases
    );
    assert!(
        tally.committed_with_targets * 8 > tally.cases,
        "only {} of {} committed runs executed more than one target",
        tally.committed_with_targets,
        tally.cases
    );
}

#[test]
#[ignore = "a fresh-seed soak; BORZOI_MSBUILD_SOAK_SEED / BORZOI_MSBUILD_SOAK_CASES"]
fn soak() {
    let (seed, cases) = common::soak_parameters("fsproj_target_execution_generative_diff", 5000);
    sweep(seed, cases);
}
