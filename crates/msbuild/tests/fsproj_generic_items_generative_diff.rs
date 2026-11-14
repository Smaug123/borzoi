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
//!   value, an absent one as "";
//! - on such a list, every identity whose escaped spelling the walk claims is
//!   spelt as MSBuild holds it (`EvaluatedIncludeEscaped`: `a%2Ex`, not `a.x`).
//!
//! A tainted list makes no claim. `most_lists_are_clean` keeps the claim from
//! going vacuous.

mod common;

use std::collections::{BTreeMap, HashMap};

use borzoi_msbuild::observed::host_fs;
use borzoi_msbuild::test_support::{ReadProbe, parse_fsproj_reads};
use common::{Oracle, SplitMix64};

/// `PackageReference` and `FrameworkReference` are captured by the dependency
/// capture, not the generic tables; `PackageReference` collapses `Update` onto
/// prior `Include`s. `PackageVersion` and `GlobalPackageReference` are captured
/// there too, each with its own certainty. `ProjectReference` is captured by
/// the reference capture.
const ITEM_TYPES: [&str; 7] = [
    "Helper",
    "Known",
    "PackageReference",
    "FrameworkReference",
    "PackageVersion",
    "GlobalPackageReference",
    "ProjectReference",
];
/// What an `Update` names.
const UPDATES: [&str; 4] = ["a", "A", "b", "@(Helper)"];
const METADATA: [&str; 3] = ["Version", "Pack", "Flavor"];
const INCLUDES: [&str; 14] = [
    "a",
    "a;b",
    "$(Name)",
    "x%3By",
    // Spelt escaped where MSBuild needs no escape: the identity decodes,
    // the spelling MSBuild keeps does not.
    "a%2Ex",
    "%34%31",
    "%61;b",
    "@(Helper)",
    "$(Undefined)c",
    "  b  ",
    "@(Helper->'%(Flavor)')",
    "@(ProjectReference)",
    // A type this crate keeps in a bucket of its own.
    "@(PackageReference)",
    // Path-shaped: MSBuild's Unix path fixup may respell it.
    "\\tmp\\foo",
];
/// What an `Exclude` names: MSBuild matches it by normalised path.
const EXCLUDES: [&str; 6] = ["a", "./a", "A", "b;x%3By", "\\tmp\\foo", "$(Name)"];
/// Values for a metadata attribute.
const VALUES: [&str; 17] = [
    "1.0",
    "$(Name)",
    "pre$(Name)post",
    "",
    "%(Identity)",
    "@(Helper)",
    "x%3By",
    "$(Gated)",
    "  padded  ",
    " $(Name) ",
    "a&amp;b",
    // Path-shaped, so MSBuild's Unix path fixup may rewrite it: `/tmp` exists
    // wherever the oracle runs.
    "\\tmp\\value",
    "x\\y",
    // The fixup decides on the escaped value, where this is no `$(`.
    "%24(foo)\\x",
    // What XML attribute-value normalisation and entity decoding touch.
    "a\tb",
    "line\nbreak",
    "&lt;![CDATA[c]]&gt;",
];
/// Bodies for a metadata child element: the attribute values, and what only
/// element content can hold.
const BODIES: [&str; 8] = [
    "<nested>hello</nested>",
    "<!-- c -->x",
    "<![CDATA[ c ]]>",
    "a<!-- c -->b",
    "\n      spread\n    ",
    "a<?thing b?>c",
    "a\r\nb",
    "a&amp;b&#9;c",
];
const CONDITIONS: [&str; 6] = [
    "'$(Name)' == 'n'",
    "'$(Name)' == 'other'",
    "'$(Undefined)' == ''",
    "'%(Identity)' == 'a'",
    "'$(Gated)' != ''",
    // Undecidable for the walk, and false in MSBuild: an item it skips
    // without knowing.
    "'$(Gated)' == ''",
];

/// The file a generated project may import.
const IMPORTED: &str = "<Project>\n  <ItemGroup>\n    <Helper Include=\"imported\" Flavor=\"f\" />\n    <Known Remove=\"a\" />\n  </ItemGroup>\n</Project>\n";

/// Conditions on an item definition or its group: no `%(…)`, which a
/// definition cannot batch on.
const DEFINITION_CONDITIONS: [&str; 5] = [
    "'$(Name)' == 'n'",
    "'$(Name)' == 'other'",
    "'$(Undefined)' == ''",
    "'$(Gated)' != ''",
    // Undecidable for the walk, and false in MSBuild.
    "'$(Gated)' == ''",
];

/// A condition MSBuild accepts and this evaluator cannot decide, so that
/// `Gated` is untrusted.
const UNDECIDABLE: &str = "$([System.String]::IsNullOrWhiteSpace(''))";

/// One generated project, and whether it writes the untrusted `Gated`.
fn project(rng: &mut SplitMix64) -> (String, bool) {
    let mut out = String::from("<Project>\n  <PropertyGroup>\n    <Name>n</Name>\n");
    // `Gated` is written under a condition outside the modelled grammar, so
    // its value is untrusted.
    let gated = rng.below(2) == 0;
    if gated {
        out.push_str(&format!(
            "    <Gated Condition=\"{UNDECIDABLE}\">g</Gated>\n"
        ));
    }
    out.push_str("  </PropertyGroup>\n");
    // A file of items, imported plainly or under a condition this evaluator
    // cannot decide (so the walk turns opaque).
    match rng.below(6) {
        0 => out.push_str("  <Import Project=\"items.props\" />\n"),
        1 => out.push_str(&format!(
            "  <Import Project=\"items.props\" Condition=\"{UNDECIDABLE}\" />\n"
        )),
        _ => {}
    }
    // A source whose metadata this crate models in its own bucket.
    if rng.below(4) == 0 {
        out.push_str(
            "  <ItemGroup>\n    <ProjectReference Include=\"q.fsproj\" Flavor=\"x\" />\n  </ItemGroup>\n",
        );
    }
    // Item definitions: up to two groups, each optionally conditioned, each
    // defining one type with metadata as attributes or children, which may be
    // conditioned too. Pass 2 evaluates them all before any item, wherever
    // they sit.
    for _ in 0..rng.below(3) {
        let item_type = rng.pick(&ITEM_TYPES);
        let group_condition = if rng.below(4) == 0 {
            format!(" Condition=\"{}\"", rng.pick(&DEFINITION_CONDITIONS))
        } else {
            String::new()
        };
        let definition_condition = if rng.below(4) == 0 {
            format!(" Condition=\"{}\"", rng.pick(&DEFINITION_CONDITIONS))
        } else {
            String::new()
        };
        out.push_str(&format!("  <ItemDefinitionGroup{group_condition}>\n"));
        item(rng, &mut out, item_type, "", &definition_condition);
        out.push_str("  </ItemDefinitionGroup>\n");
    }
    out.push_str("  <ItemGroup>\n");
    if rng.below(2) == 0 {
        item(
            rng,
            &mut out,
            "PackageReference",
            "Include=\"a\" Version=\"1.0\"",
            "",
        );
    }
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
        let operation = if rng.below(5) == 0 {
            format!("Update=\"{}\"", rng.pick(&UPDATES))
        } else {
            let exclude = if rng.below(6) == 0 {
                format!(" Exclude=\"{}\"", rng.pick(&EXCLUDES))
            } else {
                String::new()
            };
            format!("Include=\"{}\"{exclude}", rng.pick(&INCLUDES))
        };
        item(rng, &mut out, item_type, &operation, &condition);
    }
    // A versioned reference and an `Update` after everything else, the shape
    // whose collapse a reader of the item table sees.
    if rng.below(2) == 0 {
        let operation = format!("Update=\"{}\"", rng.pick(&UPDATES));
        item(rng, &mut out, "PackageReference", &operation, "");
    }
    out.push_str("  </ItemGroup>\n</Project>\n");
    (out, gated)
}

/// One item element: `operation` (`Include=…` or `Update=…`), random metadata as
/// attributes and child elements, and `condition`.
fn item(rng: &mut SplitMix64, out: &mut String, item_type: &str, operation: &str, condition: &str) {
    let mut attributes = String::new();
    if rng.below(8) == 0 {
        attributes.push_str(" Label=\"group\"");
    }
    // An item definition (no operation) cannot reference items or batch on
    // metadata.
    let definition = operation.is_empty();
    let value = |rng: &mut SplitMix64| loop {
        let value = *rng.pick(&VALUES);
        if !definition || !(value.contains("%(") || value.contains("@(")) {
            break value;
        }
    };
    let conditions: &[&str] = if definition {
        &DEFINITION_CONDITIONS
    } else {
        &CONDITIONS
    };
    let mut children = String::new();
    for name in METADATA {
        // A `Version` attribute already in `operation` is not repeated.
        if operation.contains(&format!(" {name}=")) {
            continue;
        }
        match rng.below(4) {
            0 => attributes.push_str(&format!(" {name}=\"{}\"", value(rng))),
            1 => {
                let condition = if rng.below(2) == 0 {
                    format!(" Condition=\"{}\"", rng.pick(conditions))
                } else {
                    String::new()
                };
                let body = if rng.below(3) == 0 {
                    rng.pick(&BODIES)
                } else {
                    value(rng)
                };
                children.push_str(&format!("      <{name}{condition}>{body}</{name}>\n"));
            }
            _ => {}
        }
    }
    if children.is_empty() {
        out.push_str(&format!(
            "    <{item_type} {operation}{attributes}{condition} />\n"
        ));
    } else {
        out.push_str(&format!(
            "    <{item_type} {operation}{attributes}{condition}>\n{children}    </{item_type}>\n"
        ));
    }
}

/// What a sweep reached: how many projects MSBuild rejected (and so tested
/// nothing), how many lists made a claim, how many were checked, and how many
/// of those were in a project writing the untrusted `Gated`.
#[derive(Default)]
struct Tally {
    cases: usize,
    rejected: usize,
    lists: usize,
    clean: usize,
    clean_with_gated: usize,
    clean_by_type: BTreeMap<&'static str, usize>,
    /// Rows of clean lists; of those, rows whose escaped spelling the walk
    /// claims; and of those, rows whose spelling differs from the decoded
    /// identity.
    rows: usize,
    spelled: usize,
    spelled_apart: usize,
}

/// Evaluate `xml` both ways and hold every clean list to MSBuild's.
fn check(oracle: &mut Oracle, xml: &str, gated: bool, tally: &mut Tally) {
    tally.cases += 1;
    let dir = tempfile::tempdir().unwrap();
    let path = host_fs::canonicalize(dir.path()).unwrap().join("p.fsproj");
    std::fs::write(&path, xml).unwrap();
    std::fs::write(path.with_file_name("items.props"), IMPORTED).unwrap();
    let probe = ReadProbe {
        properties: Vec::new(),
        item_types: ITEM_TYPES.iter().map(|t| (*t).to_owned()).collect(),
        targets: Vec::new(),
        execute: None,
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
        // Every name the generator writes, the structural `Label` among them,
        // and every name our table holds: a name we hold that MSBuild does not
        // must read as "" there.
        let read = &reads.items[item_type];
        let mut names: Vec<String> = METADATA.iter().map(|n| (*n).to_owned()).collect();
        names.push("Label".to_owned());
        for row in read.items.iter().flatten() {
            names.extend(row.metadata.keys().cloned());
        }
        names.sort();
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let Some(theirs) = oracle.items_meta_spelled(xml, &path, item_type, &names) else {
            // MSBuild rejects the project: there is nothing to agree with.
            tally.rejected += 1;
            return;
        };
        tally.lists += 1;
        if !read.exact {
            continue;
        }
        tally.clean += 1;
        tally.clean_with_gated += usize::from(gated);
        *tally.clean_by_type.entry(item_type).or_default() += 1;
        let rows = read.items.as_deref().unwrap_or_default();
        let ours: Vec<&str> = rows.iter().map(|r| r.identity.as_str()).collect();
        let msbuild: Vec<&str> = theirs.iter().map(|item| item.identity.as_str()).collect();
        assert_eq!(
            ours, msbuild,
            "a clean {item_type} list differs from MSBuild's in\n{xml}"
        );
        for (row, item) in rows.iter().zip(&theirs) {
            tally.rows += 1;
            if let Some(escaped) = &row.escaped_identity {
                tally.spelled += 1;
                tally.spelled_apart += usize::from(*escaped != row.identity);
                assert_eq!(
                    escaped, &item.escaped,
                    "{item_type} item {:?} is spelt {escaped:?}, by MSBuild {:?}, in\n{xml}",
                    row.identity, item.escaped
                );
            }
            let values: BTreeMap<String, &str> = item
                .metadata
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str()))
                .collect();
            for (name, expected) in values {
                if row.uncertain_metadata.contains(&name) {
                    continue;
                }
                let Some(read) = row
                    .metadata
                    .get(&name)
                    .map(String::as_str)
                    .or(row.metadata_complete.then_some(""))
                else {
                    continue;
                };
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
        let (xml, gated) = project(&mut rng);
        check(&mut oracle, &xml, gated, &mut tally);
    }
    tally
}

#[test]
fn clean_generic_lists_match_msbuild() {
    sweep(0x6e5e_71c5, 400);
}

/// A sweep in which every list declined, or MSBuild rejected every project,
/// would pass while testing nothing. The floor is per type: the dependency
/// types share one certainty, which any central-package item in a project
/// clears, so how clean a sweep is overall turns on its mix of types.
#[test]
fn most_lists_are_clean() {
    let tally = sweep(0x0a11_c1ea, 200);
    assert!(
        tally.rejected * 10 < tally.cases,
        "MSBuild rejected {} of {} generated projects",
        tally.rejected,
        tally.cases
    );
    for item_type in ITEM_TYPES {
        let clean = tally
            .clean_by_type
            .get(item_type)
            .copied()
            .unwrap_or_default();
        assert!(
            clean * 10 > tally.cases,
            "only {clean} of {} cases had an exact {item_type} list",
            tally.cases
        );
    }
    assert!(
        tally.clean_with_gated > 0,
        "no clean list was checked in a project writing the untrusted property"
    );
    assert!(
        tally.spelled * 2 > tally.rows && tally.spelled_apart > 0,
        "the escaped spelling went unclaimed: {} of {} clean rows, {} apart",
        tally.spelled,
        tally.rows,
        tally.spelled_apart
    );
}

#[test]
#[ignore = "a fresh-seed soak; BORZOI_MSBUILD_SOAK_SEED / BORZOI_MSBUILD_SOAK_CASES"]
fn soak() {
    let (seed, cases) = common::soak_parameters("fsproj_generic_items_generative_diff", 5000);
    sweep(seed, cases);
}

/// The shape a copy's definitions turn on, fixed: `Known` defines a name the
/// copied `Helper` items give no value (MSBuild applies `Known`'s default),
/// and one `Helper` defines too (MSBuild keeps `Helper`'s). Every list must
/// reach the comparison.
#[test]
fn a_copy_takes_the_destination_definitions_beneath_the_source() {
    const XML: &str = r#"<Project>
  <ItemDefinitionGroup>
    <Known><Pack>kdef</Pack><Flavor>kflav</Flavor></Known>
    <Helper><Flavor>hdef</Flavor></Helper>
  </ItemDefinitionGroup>
  <ItemGroup>
    <Helper Include="h1" />
    <Helper Include="h2" Flavor="own" />
    <Known Include="@(Helper)" Version="v" />
    <Known Include="direct" />
  </ItemGroup>
</Project>
"#;
    let mut oracle = Oracle::spawn();
    let mut tally = Tally::default();
    check(&mut oracle, XML, false, &mut tally);
    assert_eq!(tally.rejected, 0);
    assert_eq!(
        tally.clean, tally.lists,
        "every list in the fixed case must be exact"
    );
}
