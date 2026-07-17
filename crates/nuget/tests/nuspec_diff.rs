//! Differential tests: nuspec dependency-group projection vs NuGet.Packaging.
//!
//! This slice pins the resolver-critical projection: group target framework,
//! dependency id, and dependency version range. Dependency include/exclude
//! asset lists are surfaced by the oracle but intentionally not compared yet:
//! the Rust side preserves raw strings, while NuGet.Packaging normalises them
//! into ordered asset-type lists. Exact asset-list interpretation belongs with
//! the resolver/asset-selection slices that consume it.

mod common;

use borzoi_nuget::{NuGetFramework, parse_nuspec};
use common::{FRAMEWORK_ZOO, Oracle, SplitMix64};

const NUSPECS: &[&str] = &[
    r#"
<package>
  <metadata>
    <id>NoDeps</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>Flat</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <dependency id="Alpha" version="1.0" />
      <dependency id="Beta.Core" version="[2.0, 3.0)" include="Compile" exclude="Build,Analyzers" />
      <dependency id="MissingVersion" />
      <dependency id="EmptyVersion" version="" />
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>SplitDependencies</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <dependency id="Ignored.Flat.First" version="1.0" />
    </dependencies>
    <dependencies>
      <group targetFramework="net6.0">
        <dependency id="First.Grouped" version="2.0" />
      </group>
    </dependencies>
    <dependencies>
      <dependency id="Ignored.Flat.Second" version="3.0" />
      <group targetFramework="net8.0">
        <dependency id="Second.Grouped" version="4.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>Grouped</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <group targetFramework=".NETFramework4.7.2">
        <dependency id="Newtonsoft.Json" version="[13.0.1, )" />
      </group>
      <group targetFramework="netstandard2.0">
        <dependency id="System.Memory" version="4.5.5" />
        <dependency id="System.Runtime.CompilerServices.Unsafe" version="[6.0, 7.0]" />
      </group>
      <group targetFramework="net8.0" />
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>HeterogeneousLegacyGroups</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <group targetFramework="netstandard1.2">
        <dependency id="NetStandard" version="1.0" />
      </group>
      <group targetFramework="win8">
        <dependency id="Windows" version="1.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2010/07/nuspec.xsd">
  <metadata>
    <id>MixedForms</id>
    <version>1.0.0-beta.1</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <dependency id="Unscoped" version="(1.0, 2.0]" />
      <group>
        <dependency id="AnyGroup" version="[3.0]" />
      </group>
      <group targetFramework=".NETStandard,Version=v2.1">
        <dependency id="LongForm" version="[4.0, )" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>EmptyTargetFramework</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <group targetFramework="">
        <dependency id="AnyDependency" version="4.0" />
      </group>
      <group targetFramework=" ">
        <dependency id="UnsupportedDependency" version="5.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <extensions>
    <metadata>
      <dependencies>
        <dependency id="Ignored.NestedMetadata" version="1.0" />
      </dependencies>
    </metadata>
  </extensions>
  <metadata>
    <id>TopLevelMetadata</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <dependency id="Real.TopLevelMetadata" version="2.0" />
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd" xmlns:x="urn:ignored">
  <metadata>
    <id>FlatElementNames</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <Dependency id="Ignored.Case" version="1.0" />
      <x:dependency id="Ignored.Namespace" version="2.0" />
      <dependency id="Real.Flat" version="3.0" />
    </dependencies>
  </metadata>
</package>
"#,
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd" xmlns:x="urn:ignored">
  <metadata>
    <id>GroupedElementNames</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
      <Group targetFramework="net6.0">
        <dependency id="Ignored.GroupCase" version="1.0" />
      </Group>
      <x:group targetFramework="net7.0">
        <dependency id="Ignored.GroupNamespace" version="2.0" />
      </x:group>
      <group targetFramework="net8.0">
        <Dependency id="Ignored.DependencyCase" version="3.0" />
        <x:dependency id="Ignored.DependencyNamespace" version="4.0" />
        <dependency id="Real.Grouped" version="5.0" />
      </group>
    </dependencies>
  </metadata>
</package>
"#,
];

struct NuspecCase {
    name: String,
    input: String,
    has_duplicate_equivalent_groups: bool,
}

fn nuspec_cases() -> Vec<NuspecCase> {
    let mut cases = NUSPECS
        .iter()
        .enumerate()
        .map(|(i, input)| NuspecCase {
            name: format!("corner-{i}"),
            input: (*input).to_owned(),
            has_duplicate_equivalent_groups: false,
        })
        .collect::<Vec<_>>();
    cases.extend(generated_duplicate_group_nuspecs());
    cases.extend(generated_heterogeneous_group_nuspecs());
    cases
}

fn generated_duplicate_group_nuspecs() -> Vec<NuspecCase> {
    const EQUIVALENT_TARGETS: &[(&str, &[Option<&str>])] = &[
        (
            "net8",
            &[
                Some("net8.0"),
                Some(".NETCoreApp,Version=v8.0"),
                Some("NET8.0"),
            ],
        ),
        (
            "net472",
            &[
                Some("net472"),
                Some(".NETFramework,Version=v4.7.2"),
                Some("NET472"),
            ],
        ),
        (
            "netstandard20",
            &[
                Some("netstandard2.0"),
                Some(".NETStandard,Version=v2.0"),
                Some("NETSTANDARD2.0"),
            ],
        ),
        ("any", &[None, Some("")]),
    ];

    let mut cases = Vec::new();
    for (family, targets) in EQUIVALENT_TARGETS {
        for (left_index, left) in targets.iter().enumerate() {
            for (right_index, right) in targets.iter().enumerate() {
                let name = format!("duplicate-{family}-{left_index}-{right_index}");
                let left = dependency_group_xml(*left, "First.Dependency", "1.0");
                let right = dependency_group_xml(*right, "Second.Dependency", "2.0");
                cases.push(NuspecCase {
                    name: name.clone(),
                    input: format!(
                        r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{name}</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
{left}
{right}
    </dependencies>
  </metadata>
</package>
"#
                    ),
                    has_duplicate_equivalent_groups: true,
                });
            }
        }
    }
    cases
}

fn generated_heterogeneous_group_nuspecs() -> Vec<NuspecCase> {
    let targets = FRAMEWORK_ZOO
        .iter()
        .copied()
        .filter(|target| NuGetFramework::parse(target).is_ok())
        .collect::<Vec<_>>();
    let mut rng = SplitMix64(0x5eed_0006);
    let mut cases = Vec::new();

    for case_index in 0..120 {
        let mut picked = Vec::new();
        let group_count = 2 + rng.below(5);
        while picked.len() < group_count {
            let target = *rng.pick(&targets);
            if !picked.contains(&target) {
                picked.push(target);
            }
        }

        let groups = picked
            .iter()
            .enumerate()
            .map(|(group_index, target)| {
                dependency_group_xml(
                    Some(target),
                    &format!("Generated.Dependency.{case_index}.{group_index}"),
                    "1.0",
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let name = format!("heterogeneous-{case_index}");
        cases.push(NuspecCase {
            name: name.clone(),
            input: format!(
                r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{name}</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
    <dependencies>
{groups}
    </dependencies>
  </metadata>
</package>
"#
            ),
            has_duplicate_equivalent_groups: false,
        });
    }

    cases
}

fn dependency_group_xml(
    target_framework: Option<&str>,
    dependency_id: &str,
    version: &str,
) -> String {
    let target_framework = target_framework
        .map(|tfm| format!(r#" targetFramework="{tfm}""#))
        .unwrap_or_default();
    format!(
        r#"      <group{target_framework}>
        <dependency id="{dependency_id}" version="{version}" />
      </group>"#
    )
}

fn oracle_bool(v: &serde_json::Value, field: &str) -> bool {
    v.get(field)
        .and_then(|x| x.as_bool())
        .unwrap_or_else(|| panic!("oracle response missing bool field {field}: {v}"))
}

fn oracle_str<'a>(v: &'a serde_json::Value, field: &str) -> &'a str {
    v.get(field)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("oracle response missing string field {field}: {v}"))
}

fn oracle_i64(v: &serde_json::Value, field: &str) -> i64 {
    v.get(field)
        .and_then(|x| x.as_i64())
        .unwrap_or_else(|| panic!("oracle response missing i64 field {field}: {v}"))
}

#[test]
fn nuspec_dependency_projection_agrees_with_oracle() {
    let mut oracle = Oracle::spawn();
    let mut mismatches = Vec::new();
    let cases = nuspec_cases();
    let duplicate_equivalent_group_cases = cases
        .iter()
        .filter(|case| case.has_duplicate_equivalent_groups)
        .count();
    assert!(
        duplicate_equivalent_group_cases >= 30,
        "nuspec oracle sweep generated too few duplicate equivalent group cases: {duplicate_equivalent_group_cases}"
    );

    for case in &cases {
        let ours = parse_nuspec(&case.input).expect("our nuspec parser accepts fixture");
        let resp = oracle.request(&serde_json::json!({
            "op": "readNuspec",
            "input": case.input,
        }));
        assert!(oracle_bool(&resp, "ok"), "oracle rejected fixture: {resp}");

        let oracle_groups = resp["groups"]
            .as_array()
            .unwrap_or_else(|| panic!("groups should be array: {resp}"));
        if ours.dependency_groups.len() != oracle_groups.len() {
            mismatches.push(format!(
                "{}: group count ours={} oracle={}",
                case.name,
                ours.dependency_groups.len(),
                oracle_groups.len()
            ));
            continue;
        }

        for (group_index, (our_group, oracle_group)) in ours
            .dependency_groups
            .iter()
            .zip(oracle_groups.iter())
            .enumerate()
        {
            let our_target = our_group
                .target_framework
                .short_folder_name()
                .unwrap_or_default();
            let oracle_target = oracle_str(oracle_group, "targetFramework");
            if our_target != oracle_target {
                mismatches.push(format!(
                    "{}: group {group_index} target ours={our_target:?} oracle={oracle_target:?}",
                    case.name
                ));
            }

            let oracle_deps = oracle_group["dependencies"]
                .as_array()
                .unwrap_or_else(|| panic!("dependencies should be array: {oracle_group}"));
            if our_group.dependencies.len() != oracle_deps.len() {
                mismatches.push(format!(
                    "{}: group {group_index} dependency count ours={} oracle={}",
                    case.name,
                    our_group.dependencies.len(),
                    oracle_deps.len()
                ));
                continue;
            }

            for (dep_index, (our_dep, oracle_dep)) in our_group
                .dependencies
                .iter()
                .zip(oracle_deps.iter())
                .enumerate()
            {
                let oracle_id = oracle_str(oracle_dep, "id");
                if our_dep.id.as_str() != oracle_id {
                    mismatches.push(format!(
                        "{}: group {group_index} dep {dep_index} id ours={:?} oracle={oracle_id:?}",
                        case.name,
                        our_dep.id.as_str()
                    ));
                }

                let oracle_has_range = oracle_bool(oracle_dep, "hasVersionRange");
                if our_dep.version_range.is_some() != oracle_has_range {
                    mismatches.push(format!(
                        "{}: group {group_index} dep {dep_index} range presence ours={} oracle={oracle_has_range}",
                        case.name,
                        our_dep.version_range.is_some()
                    ));
                    continue;
                }

                if let Some(range) = &our_dep.version_range {
                    let ours_range = range.to_normalized_string();
                    let oracle_range = oracle_str(oracle_dep, "versionRange");
                    if ours_range != oracle_range {
                        mismatches.push(format!(
                            "{}: group {group_index} dep {dep_index} range ours={ours_range:?} oracle={oracle_range:?}",
                            case.name
                        ));
                    }
                }
            }
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "{} nuspec projection divergence(s):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

#[test]
fn nuspec_dependency_group_selection_agrees_with_oracle() {
    const PROJECT_FRAMEWORKS: &[&str] = &[
        "net8.0",
        "net6.0",
        "netcoreapp3.1",
        "netcore50",
        "net472",
        "netstandard2.0",
        "uap10.0",
        "win8",
        "wpa81",
        "net6.0-android",
        "net8.0-tizen",
        "portable-net45+win8",
        "any",
        "agnostic",
        "unsupported",
    ];

    let mut oracle = Oracle::spawn();
    let mut mismatches = Vec::new();

    for case in nuspec_cases() {
        let ours = parse_nuspec(&case.input).expect("our nuspec parser accepts fixture");
        for project in PROJECT_FRAMEWORKS {
            let project_framework =
                NuGetFramework::parse(project).expect("project framework fixture parses");
            let resp = oracle.request(&serde_json::json!({
                "op": "selectDependencyGroup",
                "project": project,
                "input": case.input,
            }));
            assert!(oracle_bool(&resp, "ok"), "oracle rejected fixture: {resp}");

            let oracle_index = oracle_i64(&resp, "nearest");
            let our_index = ours
                .select_dependency_group_index(&project_framework)
                .map(|index| index as i64)
                .unwrap_or(-1);

            if our_index != oracle_index {
                mismatches.push(format!(
                    "{} project={project}: selected group ours={our_index} oracle={oracle_index}",
                    case.name
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "{} nuspec dependency-group selection divergence(s):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

/// The `<references>` allow-list, which compile-asset selection consumes
/// (`crates/nuget/src/assets.rs`). Its parsing has the same shape as the
/// dependency groups' — grouped entries win outright over the pre-2.5 flat
/// list — with its own corners: a `<reference>` with no `file`, an empty one,
/// an empty `<references>` element, and a `targetFramework` NuGet reads with
/// `Parse` rather than `ParseFolder`.
const REFERENCE_NUSPECS: &[&str] = &[
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references><reference file="A.dll" /></references>
  </metadata></package>"#,
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references><reference file="A.dll" /><reference file="B.dll" /></references>
  </metadata></package>"#,
    // No `file`, and an empty one: both dropped.
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references><reference /><reference file="" /><reference file="A.dll" /></references>
  </metadata></package>"#,
    // An empty <references> yields no group at all.
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version><references /></metadata></package>"#,
    // Groups win outright: the flat siblings are ignored entirely.
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references>
      <reference file="Ignored.dll" />
      <group targetFramework="net8.0"><reference file="Modern.dll" /></group>
      <group targetFramework="net472"><reference file="Legacy.dll" /></group>
    </references>
  </metadata></package>"#,
    // A group with no targetFramework is the Any group; an empty group has no files.
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references>
      <group><reference file="Any.dll" /></group>
      <group targetFramework="netstandard2.0" />
    </references>
  </metadata></package>"#,
    // Long-form and empty target frameworks; NuGet parses these with `Parse`.
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references>
      <group targetFramework=".NETCoreApp,Version=v8.0"><reference file="A.dll" /></group>
      <group targetFramework=""><reference file="B.dll" /></group>
    </references>
  </metadata></package>"#,
    // Split <references> sections, as with <dependencies>.
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references><reference file="A.dll" /></references>
    <references><reference file="B.dll" /></references>
  </metadata></package>"#,
    r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>P</id><version>1.0.0</version>
    <references><reference file="A.dll" /></references>
    <references><group targetFramework="net8.0"><reference file="B.dll" /></group></references>
  </metadata></package>"#,
];

#[test]
fn nuspec_reference_projection_agrees_with_oracle() {
    let mut oracle = Oracle::spawn();
    let mut mismatches = Vec::new();

    // The reference corners, plus every dependency fixture — which must project
    // to *no* reference groups on both sides.
    let cases = REFERENCE_NUSPECS
        .iter()
        .enumerate()
        .map(|(i, input)| (format!("reference-{i}"), (*input).to_owned()))
        .chain(
            nuspec_cases()
                .into_iter()
                .map(|case| (case.name, case.input)),
        )
        .collect::<Vec<_>>();

    for (name, input) in &cases {
        let ours = parse_nuspec(input).expect("our nuspec parser accepts fixture");
        let resp = oracle.request(&serde_json::json!({
            "op": "readNuspec",
            "input": input,
        }));
        assert!(oracle_bool(&resp, "ok"), "oracle rejected fixture: {resp}");

        let oracle_groups = resp["references"]
            .as_array()
            .unwrap_or_else(|| panic!("references should be array: {resp}"));
        if ours.reference_groups.len() != oracle_groups.len() {
            mismatches.push(format!(
                "{name}: reference group count ours={} oracle={}",
                ours.reference_groups.len(),
                oracle_groups.len()
            ));
            continue;
        }

        for (index, (our_group, oracle_group)) in ours
            .reference_groups
            .iter()
            .zip(oracle_groups.iter())
            .enumerate()
        {
            let our_target = our_group
                .target_framework
                .short_folder_name()
                .unwrap_or_default();
            let oracle_target = oracle_str(oracle_group, "targetFramework");
            if our_target != oracle_target {
                mismatches.push(format!(
                    "{name}: reference group {index} target ours={our_target:?} oracle={oracle_target:?}"
                ));
            }

            let oracle_files = oracle_group["files"]
                .as_array()
                .unwrap_or_else(|| panic!("files should be array: {oracle_group}"))
                .iter()
                .map(|file| file.as_str().expect("file is a string").to_owned())
                .collect::<Vec<_>>();
            if our_group.files != oracle_files {
                mismatches.push(format!(
                    "{name}: reference group {index} files ours={:?} oracle={oracle_files:?}",
                    our_group.files
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "{} nuspec reference projection divergence(s):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}
