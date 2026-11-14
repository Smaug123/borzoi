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
/// Padding is significant; a backslash is one MSBuild's Unix path fixup may
/// respell in an item; a literal tab survives in MSBuild where XML attribute
/// normalisation would turn it into a space. `@` and `(I)` assemble an item
/// reference as `$(A)$(B)`, which MSBuild expands as such.
/// `@(I)` and `@(J)` read item lists (an `Include` copies them), and a
/// transform is an item expression the executor declines.
const VALUES: [&str; 21] = [
    "x",
    "y",
    "$(A)z",
    "$(B)",
    "",
    "$(Gated)",
    " x ",
    "a\\b",
    "x\ty",
    "@",
    "(I)",
    "$(A)$(B)",
    "@(I)",
    "@(J)",
    "@(I->'%(M)')",
    // Literal text in MSBuild, which does not expand `@(0)`; and a read of a
    // property no project can write.
    "@(0)",
    "$(-ItemList-J)",
    // A list run into text: with `J` holding a lone `%`, MSBuild reads `A`.
    "@(J)41",
    // Property functions: one the walk models, a static one, and one MSBuild
    // throws on.
    "$(A.ToUpper())",
    "$([System.String]::Concat('p', $(A)))",
    "$(A.Substring(9))",
];
const CONDITIONS: [&str; 17] = [
    "'$(A)' == 'x'",
    "'$(A)' != 'x'",
    "'$(B)' == ''",
    "'$(C)' == 'y'",
    "'$(Gated)' == 'g'",
    "'@(I)' == ''",
    "@(J) != ''",
    // Property functions, which the walk's expression evaluator decides.
    "'$(A.ToUpper())' == 'X'",
    "$(A.EndsWith('x'))",
    // An entity, and line breaks outside and inside a quoted string.
    "'$(A)' != '&lt;'",
    "'$(A)' == 'x'\n   and '$(B)' == ''",
    // False in MSBuild, which keeps the line break; true once an XML reader
    // normalises it to a space.
    "'x\ny' == 'x y'",
    // The same with its quotes spelt as entities.
    "&apos;x\ny&apos; == &apos;x y&apos;",
    // A property function's quoted argument breaking a line: with `A` as
    // " x ", false in MSBuild and true once normalised; and its quotes spelt
    // as entities.
    "$(A.Contains(`x\n`))",
    "$(A.Contains(&quot;x\n&quot;))",
    // MSBuild passes a function the item reference as text.
    "$([System.String]::IsNullOrEmpty('@(J)'))",
    "Exists('nowhere')",
];
/// A condition MSBuild accepts and this evaluator cannot decide, so that
/// `Gated` is untrusted.
const UNDECIDABLE: &str = "$([System.String]::IsNullOrWhiteSpace(''))";

/// A value: mostly one of the first seven, which the executor runs, and now
/// and then any, many of which it declines.
fn value(rng: &mut SplitMix64) -> &'static str {
    if rng.below(5) == 0 {
        rng.pick(&VALUES)
    } else {
        rng.pick(&VALUES[..7])
    }
}

/// The structural `Label` attribute, now and then: it is not metadata.
fn label(rng: &mut SplitMix64) -> &'static str {
    if rng.below(8) == 0 {
        " Label=\"l\""
    } else {
        ""
    }
}

fn condition(rng: &mut SplitMix64) -> String {
    if rng.below(3) == 0 {
        // Mostly one of the first five, which the executor decides; now and
        // then any but the last; `Exists` (which it declines) rarely.
        let pick = if rng.below(20) == 0 {
            CONDITIONS.len() - 1
        } else if rng.below(3) == 0 {
            rng.below(CONDITIONS.len() - 1)
        } else {
            rng.below(5)
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
    let any = rng.below(10) == 0;
    TARGETS
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != of && (any || (*i > of) == later) && rng.below(2) == 0)
        .map(|(_, t)| *t)
        .collect()
}

fn project(rng: &mut SplitMix64) -> String {
    // `InitialTargets` run before any target the build asks for.
    let initial = if rng.below(25) == 0 {
        " InitialTargets=\"T3\""
    } else {
        ""
    };
    let mut out = format!("<Project{initial}>\n  <PropertyGroup>\n");
    // `A=@` and `B=(I)` assemble an item reference as `$(A)$(B)`; a padded
    // `A` tells a line break in a quoted argument from a space.
    out.push_str(&format!(
        "    <A>{}</A>\n",
        rng.pick(&["x", "y", "x", "y", "@", " x "])
    ));
    match rng.below(8) {
        0..=3 => out.push_str("    <B>b</B>\n"),
        4 => out.push_str("    <B>(I)</B>\n"),
        5 => out.push_str("    <B>(I->DoesNotExist())</B>\n"),
        _ => {}
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
    // Another list, with metadata of its own, for a target to copy.
    if rng.below(2) == 0 {
        out.push_str("  <ItemGroup>\n    <J Include=\"j1;j2\" N=\"n\" />\n");
        // A padded identity, escaped so MSBuild keeps the padding.
        if rng.below(4) == 0 {
            out.push_str("    <J Include=\"%20j3%20\" />\n");
        }
        // A lone `%`, which beside other text can form a new escape.
        if rng.below(6) == 0 {
            out.push_str("    <J Include=\"%\" />\n");
        }
        out.push_str("  </ItemGroup>\n");
    }
    // A `UsingTask` may rebind a built-in task.
    if rng.below(15) == 0 {
        out.push_str(&format!(
            "  <UsingTask TaskName=\"{}\" AssemblyFile=\"missing.dll\" />\n",
            rng.pick(&["Message", "Outer+Message", "Microsoft.Build.Tasks.Message"])
        ));
    }
    // Defaults an item a target creates takes too.
    if rng.below(5) == 0 {
        out.push_str(
            "  <ItemDefinitionGroup>\n    <I><M>d</M><N>n</N></I>\n  </ItemDefinitionGroup>\n",
        );
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
                // `$(Hook)` names T1, an earlier target only for T2 and T3.
                let named = if index > 1 && rng.below(8) == 0 {
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
        // What a target returns: plain references, which the executor accepts,
        // and a transform or a property function that throws (failing the
        // build once the target has run), which it declines.
        // A condition MSBuild evaluates once the target has run.
        if rng.below(80) == 0 {
            attributes.push_str(" KeepDuplicateOutputs=\"bogus\"");
        }
        if rng.below(20) == 0 {
            attributes.push_str(&format!(
                " Returns=\"{}\"",
                rng.pick(&[
                    "@(I)",
                    "$(A);@(I)",
                    "@(I->'%(M)')",
                    "$([System.IO.File]::ReadAllText('nowhere'))",
                    // An item list run together with text fails the build.
                    "prefix@(I)",
                    "@(I)@(I)",
                    // With `A=@` and `B=(I->DoesNotExist())`, a transform that fails.
                    "$(A)$(B)",
                ])
            ));
        }
        out.push_str(&format!("  <Target Name=\"{name}\"{attributes}>\n"));
        for _ in 0..rng.below(3) {
            match rng.below(13) {
                0..=6 => {
                    out.push_str(&format!("    <PropertyGroup{}>\n", condition(rng)));
                    for _ in 0..1 + rng.below(2) {
                        let property = rng.pick(&PROPERTIES);
                        out.push_str(&format!(
                            "      <{property}{}{}>{}</{property}>\n",
                            condition(rng),
                            label(rng),
                            value(rng)
                        ));
                    }
                    out.push_str("    </PropertyGroup>\n");
                }
                7 | 8 => out.push_str(&format!(
                    "    <ItemGroup>\n      <I Include=\"{};k\" M=\"{}\"{}{} />\n    </ItemGroup>\n",
                    value(rng),
                    value(rng),
                    condition(rng),
                    label(rng)
                )),
                9 | 10 => out.push_str(&format!(
                    "    <Message Text=\"$(A)\" Importance=\"{}\" />\n",
                    rng.pick(&["low", "high", " Normal ", "invalid"])
                )),
                // Batching MSBuild cannot do fails the build whatever the condition.
                12 => out.push_str("    <Error Condition=\"false\" Text=\"%(M)\" />\n"),
                // A task the executor does not run, behind a condition that is
                // false (it does not run in MSBuild either) or true.
                _ => out.push_str(&format!(
                    "    <Error Condition=\"{}\" Text=\"no\" />\n",
                    rng.pick(&["false", "'$(A)' == 'nope'", "true"])
                )),
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

/// An empty request runs the project's default targets, which the executor
/// does not model.
#[test]
fn an_empty_request_declines() {
    let dir = tempfile::tempdir().unwrap();
    let path = host_fs::canonicalize(dir.path()).unwrap().join("p.proj");
    let xml =
        "<Project><Target Name=\"T0\"><PropertyGroup><A>x</A></PropertyGroup></Target></Project>";
    std::fs::write(&path, xml).unwrap();
    let probe = ReadProbe {
        execute: Some(ExecutionProbe::default()),
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
    assert!(matches!(reads.execution, Some(ExecutionRead::Declined(_))));
}

/// The `Returns` shapes on which MSBuild fails the build once the target has
/// run, each needing more than one rare draw of the generator: a bogus
/// `KeepDuplicateOutputs` beside `Returns`, a transform properties assemble
/// (including from what the target itself writes), and item lists run
/// together with text. Each run must be declined or be MSBuild's.
#[test]
fn returns_that_fail_the_build_decline() {
    const CASES: [&str; 5] = [
        r#"<Project><Target Name="T0" Returns="x" KeepDuplicateOutputs="bogus" /></Project>"#,
        r#"<Project><PropertyGroup><A>@</A><B>(I->DoesNotExist())</B></PropertyGroup>
           <ItemGroup><I Include="e" /></ItemGroup>
           <Target Name="T0" Returns="$(A)$(B)" /></Project>"#,
        r#"<Project><ItemGroup><I Include="e" /></ItemGroup>
           <Target Name="T0" Returns="$(A)$(B)">
             <PropertyGroup><A>@</A><B>(I->DoesNotExist())</B></PropertyGroup>
           </Target></Project>"#,
        r#"<Project><ItemGroup><I Include="e" /></ItemGroup>
           <Target Name="T0" Returns="prefix@(I)" /></Project>"#,
        r#"<Project><ItemGroup><I Include="e" /></ItemGroup>
           <Target Name="T0" Returns="@(I)@(I)" /></Project>"#,
    ];
    let mut oracle = Oracle::spawn();
    let mut tally = Tally::default();
    for xml in CASES {
        check(&mut oracle, xml, &mut tally);
    }
    assert_eq!(tally.failed, CASES.len(), "every case fails in MSBuild");
}

/// Item references whose effect the generator rarely shows: a copy takes its
/// type's item-definition defaults beneath the metadata it carries, and a
/// condition reads a list as MSBuild does. Each must run, as MSBuild runs it.
#[test]
fn item_references_keep_definitions_and_escapes() {
    const CASES: [&str; 2] = [
        r#"<Project>
             <ItemDefinitionGroup><I><M>d</M></I></ItemDefinitionGroup>
             <ItemGroup><J Include="j1;j2" N="n" /></ItemGroup>
             <Target Name="T0"><ItemGroup><I Include="@(J)" /></ItemGroup></Target>
           </Project>"#,
        r#"<Project>
             <ItemGroup><J Include="x%3By;z" /></ItemGroup>
             <Target Name="T0" Condition="'@(J)' != ''">
               <PropertyGroup><C>ran</C></PropertyGroup>
             </Target>
           </Project>"#,
    ];
    let mut oracle = Oracle::spawn();
    let mut tally = Tally::default();
    for xml in CASES {
        check(&mut oracle, xml, &mut tally);
    }
    assert_eq!(tally.committed, CASES.len(), "every case runs");
}

/// Identities whose escaped spelling decoding loses, read through a
/// property: MSBuild keeps padding escaped (`%20j%20`), and a lone `%` run into
/// `41` forms the escape for `A`. The executor's tables hold identities
/// decoded. Each run may decline; if it runs, it must be MSBuild's.
#[test]
fn identities_decoding_loses_the_spelling_of() {
    const CASES: [&str; 6] = [
        r#"<Project>
             <ItemGroup><J Include="%34%31" /></ItemGroup>
             <Target Name="T0"><PropertyGroup><B>@(J)</B><C>%$(B)</C></PropertyGroup></Target>
           </Project>"#,
        r#"<Project>
             <ItemGroup><J Include="%34%31" /></ItemGroup>
             <Target Name="T0"><PropertyGroup><C>%@(J)</C></PropertyGroup></Target>
           </Project>"#,
        r#"<Project>
             <ItemGroup><J Include="%34%31" /></ItemGroup>
             <Target Name="T0" Condition="'%@(J)' == 'A'">
               <PropertyGroup><C>ran</C></PropertyGroup>
             </Target>
           </Project>"#,
        r#"<Project>
             <ItemGroup><J Include="x%3By;z" /></ItemGroup>
             <Target Name="T0">
               <PropertyGroup><C>@(J)</C></PropertyGroup>
               <ItemGroup><I Include="$(C)" /></ItemGroup>
             </Target>
           </Project>"#,
        r#"<Project>
         <ItemGroup><J Include="%20j%20" /></ItemGroup>
         <Target Name="T0">
           <PropertyGroup><C>@(J)</C></PropertyGroup>
           <ItemGroup><I Include="$(C)" /></ItemGroup>
         </Target>
       </Project>"#,
        r#"<Project>
             <ItemGroup><J Include="%" /></ItemGroup>
             <Target Name="T0">
               <PropertyGroup><C>@(J)41</C></PropertyGroup>
               <ItemGroup><I Include="$(C)" /></ItemGroup>
             </Target>
           </Project>"#,
    ];
    let mut oracle = Oracle::spawn();
    for xml in CASES {
        check(&mut oracle, xml, &mut Tally::default());
    }
}

/// Expressions the XML reader and MSBuild read apart: a line break inside
/// any of MSBuild's quotes (`'`, `"`, `` ` ``), nested ones too, which XML
/// attribute normalisation turns into a space, in a condition and in a
/// target list; a quote spelt as an entity; and an item list inside a
/// property function's quoted argument, past parentheses the quote holds.
/// Each run may decline; if it runs, it must be MSBuild's.
#[test]
fn quoted_expressions_read_as_msbuild_reads_them() {
    const CASES: [&str; 7] = [
        "<Project>
           <PropertyGroup><A>x y</A></PropertyGroup>
           <Target Name=\"T0\"
                   Condition=\"$([System.String]::IsNullOrEmpty('$(A.TrimStart('x\ny'))'))\">
             <PropertyGroup><C>ran</C></PropertyGroup>
           </Target>
         </Project>",
        "<Project>
           <PropertyGroup><A>x y</A></PropertyGroup>
           <Target Name=\"T0\" Condition=\"$(A.Split(`\n`).Length) == 2\">
             <PropertyGroup><C>ran</C></PropertyGroup>
           </Target>
         </Project>",
        "<Project>
           <PropertyGroup><A>x y</A></PropertyGroup>
           <Target Name=\"T0\" DependsOnTargets=\"T$(A.Split('\n').Length)\" />
           <Target Name=\"T1\"><PropertyGroup><C>one</C></PropertyGroup></Target>
           <Target Name=\"T2\"><PropertyGroup><C>two</C></PropertyGroup></Target>
         </Project>",
        "<Project>
           <PropertyGroup><A>x y z</A></PropertyGroup>
           <Target Name=\"T0\" DependsOnTargets=\"T1\" />
           <Target Name=\"T1\"><PropertyGroup><C>one</C></PropertyGroup></Target>
           <Target Name=\"T2\" AfterTargets='T$(A.Split(\"\t\").Length)'>
             <PropertyGroup><C>two</C></PropertyGroup>
           </Target>
         </Project>",
        "<Project>
           <PropertyGroup><A>x y</A></PropertyGroup>
           <Target Name=\"T0\" Condition=\"$(A.Split(&quot;\n&quot;).Length) == 2\">
             <PropertyGroup><C>ran</C></PropertyGroup>
           </Target>
         </Project>",
        "<Project>
           <PropertyGroup><A>x y</A></PropertyGroup>
           <Target Name=\"T0\" Condition=\"$(A.Split(&apos;\n&apos;).Length) == 2\">
             <PropertyGroup><C>ran</C></PropertyGroup>
           </Target>
         </Project>",
        "<Project>
           <PropertyGroup><A>)) j </A></PropertyGroup>
           <ItemGroup><J Include=\"j\" /></ItemGroup>
           <Target Name=\"T0\" Condition=\"'$(A.Contains(`)) @(J) `))' == 'true'\">
             <PropertyGroup><C>ran</C></PropertyGroup>
           </Target>
         </Project>",
    ];
    let mut oracle = Oracle::spawn();
    for xml in CASES {
        check(&mut oracle, xml, &mut Tally::default());
    }
}
