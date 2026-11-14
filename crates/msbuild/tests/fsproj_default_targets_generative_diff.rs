//! Generative differential: **certainty implies exactness** for
//! `MSBuildProjectDefaultTargets`, which MSBuild sets from the first project
//! or import root, in depth-first import order, whose `DefaultTargets`
//! expands to something (stage A₁.3c of
//! `docs/nuget-dgspec-boundary-plan.md`: the restore targets' hooks read
//! `$(RebuildDependsOn)`, which rests on it).
//!
//! The harness writes an entry project and two imported files, each root
//! optionally carrying a `DefaultTargets` over literal, padded, escaped,
//! list, empty and property-expanded values, the properties defined before
//! or after the root, and an import the walk cannot decide. Every value the
//! walk commits must be MSBuild's (the oracle's `evaluated` op), and a name it
//! reads as unset must be unset there.

mod common;

use std::collections::HashMap;

use borzoi_msbuild::observed::host_fs;
use borzoi_msbuild::test_support::{PropertyRead, ReadProbe, parse_fsproj_reads};
use common::{Oracle, SplitMix64};

const NAME: &str = "MSBuildProjectDefaultTargets";
/// MSBuild stores the unescaped text as the escaped value, so `%2541` reads
/// back as `A`.
const VALUES: [&str; 9] = [
    "A",
    " A ; B ",
    "%41",
    "%2541",
    "A%3BB",
    "",
    "$(P)",
    "$(Undefined)",
    "$(P)x",
];
/// A condition MSBuild accepts and this evaluator cannot decide.
const UNDECIDABLE: &str = "$([System.String]::IsNullOrWhiteSpace(''))";

fn default_targets(rng: &mut SplitMix64) -> String {
    if rng.below(3) == 0 {
        String::new()
    } else {
        format!(" DefaultTargets=\"{}\"", rng.pick(&VALUES))
    }
}

/// The entry project and the two files it may import.
fn tree(rng: &mut SplitMix64) -> [(&'static str, String); 3] {
    let mut entry = format!("<Project{}>\n", default_targets(rng));
    if rng.below(2) == 0 {
        entry.push_str("  <PropertyGroup><P>p</P></PropertyGroup>\n");
    }
    for file in ["a.props", "b.targets"] {
        match rng.below(4) {
            0 => {}
            1 => entry.push_str(&format!(
                "  <Import Project=\"{file}\" Condition=\"{UNDECIDABLE}\" />\n"
            )),
            _ => entry.push_str(&format!("  <Import Project=\"{file}\" />\n")),
        }
    }
    entry.push_str("  <Target Name=\"A\" />\n</Project>\n");
    let imported = |rng: &mut SplitMix64| {
        let mut text = format!("<Project{}>\n", default_targets(rng));
        if rng.below(3) == 0 {
            text.push_str("  <PropertyGroup><P>q</P></PropertyGroup>\n");
        }
        text.push_str("</Project>\n");
        text
    };
    let a = imported(rng);
    let b = imported(rng);
    [("p.proj", entry), ("a.props", a), ("b.targets", b)]
}

#[derive(Default)]
struct Tally {
    cases: usize,
    rejected: usize,
    committed_set: usize,
    committed_unset: usize,
}

fn check(oracle: &mut Oracle, files: &[(&str, String); 3], tally: &mut Tally) {
    tally.cases += 1;
    let dir = tempfile::tempdir().unwrap();
    let root = host_fs::canonicalize(dir.path()).unwrap();
    for (file, text) in files {
        std::fs::write(root.join(file), text).unwrap();
    }
    let path = root.join("p.proj");
    let Some(theirs) = oracle.try_evaluated(&path, &[], &[NAME], &[], &[]) else {
        tally.rejected += 1;
        return;
    };
    let msbuild = theirs["properties"][NAME].as_str().unwrap_or_default();
    let probe = ReadProbe {
        properties: vec![NAME.to_owned()],
        ..ReadProbe::default()
    };
    let (_, reads) = parse_fsproj_reads(
        &files[0].1,
        &path,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
        &probe,
    )
    .unwrap();
    match &reads.properties[NAME] {
        PropertyRead::Value(ours) => {
            tally.committed_set += 1;
            assert_eq!(ours, msbuild, "{NAME}, in\n{files:#?}");
        }
        PropertyRead::Unset => {
            tally.committed_unset += 1;
            assert_eq!(msbuild, "", "{NAME} reads unset, in\n{files:#?}");
        }
        _ => {}
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
fn committed_default_targets_match_msbuild() {
    sweep(0x0def_7a26, 300);
}

/// A sweep in which every read declined would pass while testing nothing.
#[test]
fn many_default_targets_are_committed() {
    let tally = sweep(0x5e7_ded, 200);
    assert!(
        tally.rejected * 10 < tally.cases,
        "MSBuild rejected {} of {} generated projects",
        tally.rejected,
        tally.cases
    );
    assert!(
        tally.committed_set * 3 > tally.cases,
        "only {} of {} reads committed to a value",
        tally.committed_set,
        tally.cases
    );
}

#[test]
#[ignore = "a fresh-seed soak; BORZOI_MSBUILD_SOAK_SEED / BORZOI_MSBUILD_SOAK_CASES"]
fn soak() {
    let (seed, cases) = common::soak_parameters("fsproj_default_targets_generative_diff", 5000);
    sweep(seed, cases);
}
