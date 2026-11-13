//! Generative differential: **certainty implies exactness** for the target
//! definitions the walk's probe reports (stage A₁.3b of
//! `docs/nuget-dgspec-boundary-plan.md`).
//!
//! MSBuild keeps one definition per target name: the last its evaluation
//! reaches, comparing names unescaped and case-insensitively. The probe
//! (`test_support::parse_fsproj_reads`) answers a target name with the
//! definition the walk kept, and calls it exact only when it may: the walk
//! reached MSBuild's import graph and no name is outside the ones it compares.
//!
//! This harness writes small projects over what decides that: targets before
//! and after imports, imports taken, skipped or undecidable, a nested import,
//! a `Directory.Build.*` file beside the project (which MSBuild never imports
//! without an SDK), and names that differ only by case, by a `%` escape, by
//! padding, or by non-ASCII case. Every exact read must name the element
//! MSBuild's own target table keeps (the oracle's `evaluated` op), at the same
//! file, line and column.

mod common;

use std::collections::HashMap;

use borzoi_msbuild::observed::host_fs;
use borzoi_msbuild::test_support::{ReadProbe, parse_fsproj_reads};
use common::{Oracle, SplitMix64};

/// Names the walk compares.
const PLAIN: [&str; 5] = ["A", "a", "B", "Build_Inner", "x_1"];
/// Names MSBuild compares otherwise than byte for byte: an escape, padding,
/// non-ASCII case.
const ODD: [&str; 5] = ["%41", " A ", "\u{c4}", "\u{e4}", "%62"];
/// Conditions on an import: absent, true, false, and one the walk cannot
/// decide (MSBuild takes it).
const CONDITIONS: [&str; 4] = [
    "",
    " Condition=\"'$(Name)' == 'n'\"",
    " Condition=\"'$(Name)' == 'other'\"",
    " Condition=\"$([System.String]::IsNullOrWhiteSpace(''))\"",
];

fn name(rng: &mut SplitMix64) -> &'static str {
    if rng.below(12) == 0 {
        rng.pick(&ODD)
    } else {
        rng.pick(&PLAIN)
    }
}

fn targets(rng: &mut SplitMix64, out: &mut String) {
    for _ in 0..rng.below(3) {
        out.push_str(&format!("  <Target Name=\"{}\" />\n", name(rng)));
    }
}

/// One generated tree: the project and the files beside it.
fn tree(rng: &mut SplitMix64) -> Vec<(&'static str, String)> {
    let mut project =
        String::from("<Project>\n  <PropertyGroup>\n    <Name>n</Name>\n  </PropertyGroup>\n");
    targets(rng, &mut project);
    project.push_str(&format!(
        "  <Import Project=\"one.targets\"{} />\n",
        rng.pick(&CONDITIONS)
    ));
    targets(rng, &mut project);
    project.push_str("</Project>\n");

    let mut one = String::from("<Project>\n");
    targets(rng, &mut one);
    if rng.below(2) == 0 {
        one.push_str(&format!(
            "  <Import Project=\"two.targets\"{} />\n",
            rng.pick(&CONDITIONS)
        ));
    }
    targets(rng, &mut one);
    one.push_str("</Project>\n");

    let mut two = String::from("<Project>\n");
    targets(rng, &mut two);
    two.push_str("</Project>\n");

    let mut files = vec![
        ("p.fsproj", project),
        ("one.targets", one),
        ("two.targets", two),
    ];
    for beside in ["Directory.Build.targets", "Directory.Build.props"] {
        if rng.below(8) == 0 {
            let mut text = String::from("<Project>\n");
            targets(rng, &mut text);
            text.push_str("</Project>\n");
            files.push((beside, text));
        }
    }
    files
}

#[derive(Default)]
struct Tally {
    cases: usize,
    rejected: usize,
    /// Reads of a name in [`PLAIN`], the only ones that can be exact.
    plain_reads: usize,
    exact: usize,
    exact_defined: usize,
}

fn check(oracle: &mut Oracle, files: &[(&str, String)], tally: &mut Tally) {
    tally.cases += 1;
    let dir = tempfile::tempdir().unwrap();
    let root = host_fs::canonicalize(dir.path()).unwrap();
    for (file, text) in files {
        std::fs::write(root.join(file), text).unwrap();
    }
    let path = root.join("p.fsproj");
    let queries: Vec<&str> = PLAIN.iter().chain(&ODD).copied().collect();
    let Some(theirs) = oracle.try_evaluated(&path, &[], &[], &[], &queries) else {
        tally.rejected += 1;
        return;
    };
    let probe = ReadProbe {
        targets: queries.iter().map(|q| (*q).to_owned()).collect(),
        ..ReadProbe::default()
    };
    let (_, ours) = parse_fsproj_reads(
        &files[0].1,
        &path,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
        &probe,
    )
    .unwrap();
    for query in queries {
        tally.plain_reads += usize::from(PLAIN.contains(&query));
        let read = &ours.targets[query];
        match common::check_target(read, &theirs["targets"][query]) {
            Ok(true) => {
                tally.exact += 1;
                tally.exact_defined += usize::from(read.definition.is_some());
            }
            Ok(false) => {}
            Err(why) => panic!("target {query:?}: {why}, in\n{files:#?}"),
        }
    }
}

fn sweep(seed: u64, cases: usize) -> Tally {
    let mut rng = SplitMix64(seed);
    let mut oracle = Oracle::spawn();
    let mut tally = Tally::default();
    for _ in 0..cases {
        let files = tree(&mut rng);
        check(&mut oracle, &files, &mut tally);
    }
    tally
}

#[test]
fn exact_target_reads_match_msbuild() {
    sweep(0x7a29_e7d5, 300);
}

/// A sweep in which every read declined, or MSBuild rejected every project,
/// would pass while testing nothing.
#[test]
fn many_target_reads_are_exact() {
    let tally = sweep(0x0da7_a5e7, 200);
    assert!(
        tally.rejected * 10 < tally.cases,
        "MSBuild rejected {} of {} generated projects",
        tally.rejected,
        tally.cases
    );
    assert!(
        tally.exact * 4 > tally.plain_reads,
        "only {} of {} reads of a plain target name were exact",
        tally.exact,
        tally.plain_reads
    );
    assert!(
        tally.exact_defined * 10 > tally.plain_reads,
        "only {} of {} reads of a plain target name found an exact definition",
        tally.exact_defined,
        tally.plain_reads
    );
}

#[test]
#[ignore = "a fresh-seed soak; BORZOI_MSBUILD_SOAK_SEED / BORZOI_MSBUILD_SOAK_CASES"]
fn soak() {
    let (seed, cases) = common::soak_parameters("fsproj_target_definitions_generative_diff", 5000);
    sweep(seed, cases);
}
