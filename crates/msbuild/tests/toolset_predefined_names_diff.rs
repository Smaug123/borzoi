//! `TOOLSET_PREDEFINED_NAMES` against MSBuild, in both directions.
//!
//! The SDK's default idiom, `<Name Condition="'$(Name)' == ''">fallback</Name>`,
//! is decided by the walker's self-default exemption, which believes an
//! undefined read rather than declining it (`self_default_absence_is_a_fact`).
//! That belief is wrong for exactly one class of name: one MSBuild's toolset
//! defines before any project text runs. The walker holds no value for such a
//! name whenever it has not seeded it (no canonical SDK resolved, the walk
//! passed content it could not see, or the SDK is one whose
//! `VisualStudioVersion` it does not know), while the real build holds the
//! toolset's, so the fallback is a wrong commit: `VisualStudioVersion`'s `10.0`
//! in `Microsoft.Common.CurrentVersion.targets` against MSBuild 18's `18.0`.
//!
//! The list of such names is not something to enumerate by reading MSBuild's
//! source, so this test asks MSBuild. Every self-default write in the pinned
//! SDK's chain is harvested, and MSBuild evaluates an empty project for those
//! names: any it defines is predefined, and must be on the list. Each name on
//! the list must in turn be one MSBuild defines, or the walker declines reads
//! it could have committed.

mod common;

use std::collections::BTreeSet;

use borzoi_msbuild::test_support::TOOLSET_PREDEFINED_NAMES;
use tempfile::TempDir;

/// Every name the SDK chain writes under its own `'$(Name)' == ''` gate.
fn self_default_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in common::sdk_chain::msbuild_files(&common::sdk_chain::sdk_dir()) {
        let Ok(text) = borzoi_msbuild::observed::host_fs::read_to_string(&file) else {
            continue;
        };
        let Ok(doc) = roxmltree::Document::parse(&text) else {
            continue;
        };
        for node in doc.descendants().filter(|n| n.is_element()) {
            let in_property_group = node
                .parent_element()
                .is_some_and(|p| p.tag_name().name() == "PropertyGroup");
            let Some(condition) = node.attribute("Condition") else {
                continue;
            };
            let name = node.tag_name().name();
            let condition: String = condition.chars().filter(|c| !c.is_whitespace()).collect();
            let gate = format!("'$({name})'==''");
            if in_property_group && condition.eq_ignore_ascii_case(&gate) {
                names.insert(name.to_string());
            }
        }
    }
    names
}

#[test]
fn toolset_predefined_names_are_exactly_the_ones_msbuild_predefines() {
    let harvested = self_default_names();
    // Non-vacuity: the 10.0.4xx chain has several hundred.
    assert!(
        harvested.len() > 100,
        "harvested only {} self-default names from the SDK chain",
        harvested.len()
    );

    let mut asked: BTreeSet<String> = harvested.clone();
    asked.extend(TOOLSET_PREDEFINED_NAMES.iter().map(|n| (*n).to_string()));
    let asked: Vec<String> = asked.into_iter().collect();

    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("Empty.proj");
    let body = "<Project />";
    std::fs::write(&path, body).expect("write project");
    let mut oracle = common::Oracle::spawn();
    let theirs = oracle
        .project(body, &asked, Some(&path), &[])
        .expect("MSBuild evaluates an empty project");
    let defined = |name: &str| theirs.get(name).is_some_and(|v| !v.is_empty());

    let listed: BTreeSet<String> = TOOLSET_PREDEFINED_NAMES
        .iter()
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let missing: Vec<&String> = harvested
        .iter()
        .filter(|n| defined(n) && !listed.contains(&n.to_ascii_lowercase()))
        .collect();
    assert!(
        missing.is_empty(),
        "the SDK self-defaults these names, and MSBuild defines them before \
         evaluation starts, so their absence in the walk is never a fact: add \
         them to TOOLSET_PREDEFINED_NAMES: {missing:?}"
    );

    let spurious: Vec<&str> = TOOLSET_PREDEFINED_NAMES
        .iter()
        .copied()
        .filter(|n| !defined(n))
        .collect();
    assert!(
        spurious.is_empty(),
        "MSBuild leaves these undefined in an empty project, so listing them \
         declines reads the walker could commit: {spurious:?}"
    );
}

/// A plain SDK project, whose `VisualStudioVersion` MSBuild's toolset defines
/// and the SDK's `Microsoft.Common.CurrentVersion.targets` self-defaults to
/// `10.0`. The walk seeds it for a release SDK, so this is the seeded control
/// beside the unseeded cases below.
#[test]
fn visual_studio_version_on_a_real_sdk_project_is_exact_or_declined() {
    use crate::common::parse_fsproj_with_imports;
    use borzoi_msbuild::{resolve_sdk, workloads};
    use std::collections::HashMap;

    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("Demo.fsproj");
    let body = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
</Project>"#;
    std::fs::write(&path, body).expect("write project");

    let dotnet_root = common::dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = common::workload_env_from_process();
    let resolver = |name: &str, observer: &borzoi_msbuild::observed::Observer| {
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
            observer,
        )
    };
    let ours = parse_fsproj_with_imports(
        body,
        &path,
        &HashMap::new(),
        &common::oracle_environment(),
        Some(&resolver as &borzoi_msbuild::SdkResolver<'_>),
        None,
    )
    .expect("well-formed XML parses");

    let name = "VisualStudioVersion".to_string();
    let mut oracle = common::Oracle::spawn();
    let theirs = oracle
        .project(body, std::slice::from_ref(&name), Some(&path), &[])
        .expect("MSBuild evaluates a plain SDK project");
    // Absent (the walk never reached a write) and untrusted both make no claim.
    let ours_value = ours.properties.get(&name);
    assert!(
        ours_value.is_none()
            || ours.property_provenance_untrusted(&name)
            || ours_value == theirs.get(&name),
        "certain-implies-exact violated: we committed {ours_value:?}, MSBuild reads {:?}",
        theirs.get(&name)
    );
}

/// The same defect where nothing seeds the toolset: a project that resolves no
/// SDK. The walk then holds no value for any predefined name, while MSBuild
/// holds the toolset's for every one, so each read below must decline or be
/// exact. Every name is read directly and through a gate on its emptiness; the
/// overridable ones are also read after the SDK's self-default idiom (the
/// reserved ones refuse a write, MSB4004).
#[test]
fn predefined_names_without_an_sdk_are_exact_or_declined() {
    const OVERRIDABLE: &[&str] = &[
        "MSBuildExtensionsPath",
        "MSBuildExtensionsPath32",
        "MSBuildSDKsPath",
        "VisualStudioVersion",
    ];
    const RESERVED: &[&str] = &[
        "MSBuildToolsPath",
        "MSBuildBinPath",
        "MSBuildToolsVersion",
        "MSBuildRuntimeType",
    ];
    // The list under test and the names this test reads must not drift apart.
    let read: BTreeSet<String> = OVERRIDABLE
        .iter()
        .chain(RESERVED)
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let listed: BTreeSet<String> = TOOLSET_PREDEFINED_NAMES
        .iter()
        .map(|n| (*n).to_string())
        .collect();
    assert_eq!(read, listed);

    let mut cases: Vec<(String, String)> = Vec::new();
    for name in OVERRIDABLE.iter().chain(RESERVED) {
        cases.push((
            format!("{name}/direct"),
            format!("<Probe>$({name})</Probe>"),
        ));
        cases.push((
            format!("{name}/gate"),
            format!(
                "<Probe Condition=\"'$({name})' == ''\">empty</Probe>\
                 <Probe Condition=\"'$({name})' != ''\">nonempty</Probe>"
            ),
        ));
    }
    for name in OVERRIDABLE {
        cases.push((
            format!("{name}/self-default"),
            format!(
                "<{name} Condition=\"'$({name})' == ''\">/FALLBACK</{name}>\
                 <Probe>$({name})</Probe>"
            ),
        ));
    }

    let probe = "Probe".to_string();
    let mut oracle = common::Oracle::spawn();
    let mut violations = Vec::new();
    for (label, group) in &cases {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("NoSdk.proj");
        let body = format!("<Project><PropertyGroup>{group}</PropertyGroup></Project>");
        std::fs::write(&path, &body).expect("write project");
        let theirs = oracle
            .project(&body, std::slice::from_ref(&probe), Some(&path), &[])
            .expect("MSBuild evaluates a project with no SDK");
        let theirs = theirs.get(&probe);
        // The fixture's claim: MSBuild predefines the name, so no reader sees
        // it empty and no self-default fires.
        assert!(
            theirs.is_some_and(|v| !v.is_empty() && v != "empty" && v != "/FALLBACK"),
            "{label}: MSBuild reads {theirs:?}, so the name is not predefined here"
        );
        let ours = common::parse_fsproj_with_imports(
            &body,
            &path,
            &std::collections::HashMap::new(),
            &common::oracle_environment(),
            None,
            None,
        )
        .expect("well-formed XML parses");
        let ours_value = ours.properties.get(&probe);
        let committed = ours_value.is_some() && !ours.property_provenance_untrusted(&probe);
        if committed && ours_value != theirs {
            violations.push(format!(
                "{label}: we committed {ours_value:?}, MSBuild reads {theirs:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "certain-implies-exact violated:\n{}",
        violations.join("\n")
    );
}
