//! Generative differential: **certainty implies exactness** for the generic
//! item tables, metadata included (stage A₁.3b of
//! `docs/nuget-dgspec-boundary-plan.md`).
//!
//! The restore targets batch over item lists the evaluation leaves: the SDK's
//! `Known*` tables and their metadata, among others. `borzoi-msbuild` keeps
//! every item type it does not model elsewhere in a generic table, with every
//! metadatum its declaration gives it, and marks what it cannot vouch for: a
//! list whose declarations it could not fully evaluate is tainted, and a
//! metadatum it could not evaluate, or one an item definition may default, is
//! uncertain. The walk's probe (`test_support::parse_fsproj_reads`) reads
//! those tables back.
//!
//! This harness generates small projects over the constructs those marks
//! guard (literal, `$(…)` and escaped identities, `@(…)` copies, `Remove`,
//! metadata as attributes and as child elements, conditions on either,
//! `%(…)` and `@(…)` in metadata, item definitions) and holds every clean
//! read to MSBuild's evaluation, through the oracle's `itemsMeta` op:
//! - a list neither tainted nor untracked has MSBuild's identities, in order;
//! - on such a list, every metadatum not marked uncertain reads as MSBuild's
//!   value, an absent one as "".
//!
//! A tainted list makes no claim. `most_lists_are_clean` keeps the claim from
//! going vacuous.

mod common;

use std::collections::{BTreeMap, HashMap};

use borzoi_msbuild::test_support::{ReadProbe, parse_fsproj_reads};
use common::{Oracle, SplitMix64};

const ITEM_TYPES: [&str; 2] = ["Helper", "Known"];
const METADATA: [&str; 3] = ["Version", "Pack", "Flavor"];
const INCLUDES: [&str; 8] = [
    "a",
    "a;b",
    "$(Name)",
    "x%3By",
    "@(Helper)",
    "$(Undefined)c",
    "  b  ",
    "@(Helper->'%(Flavor)')",
];
const VALUES: [&str; 8] = [
    "1.0",
    "$(Name)",
    "pre$(Name)post",
    "",
    "%(Identity)",
    "@(Helper)",
    "x%3By",
    "$(Gated)",
];
const CONDITIONS: [&str; 5] = [
    "'$(Name)' == 'n'",
    "'$(Name)' == 'other'",
    "'$(Undefined)' == ''",
    "'%(Identity)' == 'a'",
    "'$(Gated)' != ''",
];

/// One generated project.
fn project(rng: &mut SplitMix64) -> String {
    let mut out = String::from("<Project>\n  <PropertyGroup>\n    <Name>n</Name>\n");
    // `Gated` is written under a condition outside the modelled grammar, so
    // its value is untrusted.
    if rng.below(2) == 0 {
        out.push_str("    <Gated Condition=\"'@(Nothing)' == ''\">g</Gated>\n");
    }
    out.push_str("  </PropertyGroup>\n");
    if rng.below(3) == 0 {
        let item_type = rng.pick(&ITEM_TYPES);
        let name = rng.pick(&METADATA);
        out.push_str(&format!(
            "  <ItemDefinitionGroup>\n    <{item_type}><{name}>def</{name}></{item_type}>\n  </ItemDefinitionGroup>\n"
        ));
    }
    out.push_str("  <ItemGroup>\n");
    for _ in 0..1 + rng.below(4) {
        let item_type = rng.pick(&ITEM_TYPES);
        let condition = if rng.below(5) == 0 {
            format!(" Condition=\"{}\"", rng.pick(&CONDITIONS))
        } else {
            String::new()
        };
        if rng.below(6) == 0 {
            out.push_str(&format!("    <{item_type} Remove=\"a\"{condition} />\n"));
            continue;
        }
        let include = rng.pick(&INCLUDES);
        let mut attributes = String::new();
        let mut children = String::new();
        for name in METADATA {
            match rng.below(4) {
                0 => attributes.push_str(&format!(" {name}=\"{}\"", rng.pick(&VALUES))),
                1 => {
                    let condition = if rng.below(2) == 0 {
                        format!(" Condition=\"{}\"", rng.pick(&CONDITIONS))
                    } else {
                        String::new()
                    };
                    children.push_str(&format!(
                        "      <{name}{condition}>{}</{name}>\n",
                        rng.pick(&VALUES)
                    ));
                }
                _ => {}
            }
        }
        if children.is_empty() {
            out.push_str(&format!(
                "    <{item_type} Include=\"{include}\"{attributes}{condition} />\n"
            ));
        } else {
            out.push_str(&format!(
                "    <{item_type} Include=\"{include}\"{attributes}{condition}>\n{children}    </{item_type}>\n"
            ));
        }
    }
    out.push_str("  </ItemGroup>\n</Project>\n");
    out
}

/// How many lists made a claim, and how many were checked.
#[derive(Default)]
struct Tally {
    lists: usize,
    clean: usize,
}

/// Evaluate `xml` both ways and hold every clean list to MSBuild's.
fn check(oracle: &mut Oracle, xml: &str, tally: &mut Tally) {
    let dir = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(dir.path()).unwrap().join("p.fsproj");
    std::fs::write(&path, xml).unwrap();
    let probe = ReadProbe {
        properties: Vec::new(),
        item_types: ITEM_TYPES.iter().map(|t| (*t).to_owned()).collect(),
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
    for item_type in ITEM_TYPES {
        let Some(theirs) = oracle.items_meta(xml, &path, item_type, &METADATA) else {
            // MSBuild rejects the project: there is nothing to agree with.
            return;
        };
        tally.lists += 1;
        let read = &reads.items[item_type];
        if read.tainted || read.untracked {
            continue;
        }
        tally.clean += 1;
        let rows = read.items.as_deref().unwrap_or_default();
        let ours: Vec<&str> = rows.iter().map(|r| r.identity.as_str()).collect();
        let msbuild: Vec<&str> = theirs.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ours, msbuild,
            "a clean {item_type} list differs from MSBuild's in\n{xml}"
        );
        for (row, (_, values)) in rows.iter().zip(&theirs) {
            let values: BTreeMap<String, &str> = values
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str()))
                .collect();
            for (name, expected) in values {
                if row.uncertain_metadata.contains(&name) {
                    continue;
                }
                let read = row.metadata.get(&name).map_or("", String::as_str);
                assert_eq!(
                    read, expected,
                    "{item_type} item {:?} reads %({name}) as {read:?}, MSBuild as {expected:?}, in\n{xml}",
                    row.identity
                );
            }
        }
    }
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
fn clean_generic_lists_match_msbuild() {
    sweep(0x6e5e_71c5, 400);
}

/// A sweep in which every list declined would pass while testing nothing.
#[test]
fn most_lists_are_clean() {
    let tally = sweep(0x0a11_c1ea, 200);
    assert!(
        tally.clean * 2 > tally.lists,
        "only {} of {} generated lists were clean",
        tally.clean,
        tally.lists
    );
}

#[test]
#[ignore = "a fresh-seed soak; BORZOI_MSBUILD_SOAK_SEED / BORZOI_MSBUILD_SOAK_CASES"]
fn soak() {
    let (seed, cases) = common::soak_parameters("fsproj_generic_items_generative_diff", 5000);
    sweep(seed, cases);
}
