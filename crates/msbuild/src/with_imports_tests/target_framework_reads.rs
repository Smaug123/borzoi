//! An undefined `$(TargetFramework)` read made before the project body defines
//! the property — the Web SDK's `Sdk.props` gates `Sdk.Server.props` on
//! `GetTargetPlatformIdentifier($(TargetFramework))` there. The read is exactly
//! `""` in MSBuild, and it is trusted as such exactly when the evaluation is a
//! build of a `TargetFramework` the project writes itself (see
//! `walk_with_imports`). In a cross-targeting project, or one whose own
//! `TargetFramework` the walk cannot pin, the carve-out keeps the read inexact.

use super::*;
use tempfile::TempDir;

/// The import gate the SDK's props evaluate before the body runs.
const GATES: &[&str] = &[
    "'$([MSBuild]::GetTargetPlatformIdentifier($(TargetFramework)))' != 'browser'",
    "'$(TargetFramework)' != 'net8.0'",
];

/// One body shape, and whether the gate's read must be trusted under it.
struct Body {
    name: &'static str,
    property_group: &'static str,
    globals: &'static [(&'static str, &'static str)],
    certain: bool,
}

const BODIES: &[Body] = &[
    Body {
        name: "single TFM",
        property_group: "<TargetFramework>net10.0</TargetFramework>",
        globals: &[],
        certain: true,
    },
    // `TargetFramework` wins over `TargetFrameworks`: MSBuild's
    // `IsCrossTargetingBuild` is false, so this evaluation is still the build.
    Body {
        name: "both singular and plural",
        property_group: "<TargetFrameworks>net8.0;net10.0</TargetFrameworks>\
                         <TargetFramework>net10.0</TargetFramework>",
        globals: &[],
        certain: true,
    },
    Body {
        name: "cross-targeting",
        property_group: "<TargetFrameworks>net8.0;net10.0</TargetFrameworks>",
        globals: &[],
        certain: false,
    },
    // Written, then cleared: the evaluation ends as the dispatch build.
    Body {
        name: "written then cleared",
        property_group: "<TargetFrameworks>net8.0;net10.0</TargetFrameworks>\
                         <TargetFramework>net10.0</TargetFramework>\
                         <TargetFramework></TargetFramework>",
        globals: &[],
        certain: false,
    },
    // Written only under a gate the walk cannot decide: the real build may
    // leave `TargetFramework` unset and cross-target.
    Body {
        name: "written under an undecided gate",
        property_group: "<TargetFrameworks>net8.0;net10.0</TargetFrameworks>\
                         <TargetFramework Condition=\"'$([MSBuild]::Nope())' == ''\">net10.0</TargetFramework>",
        globals: &[],
        certain: false,
    },
    Body {
        name: "no TFM at all",
        property_group: "",
        globals: &[],
        certain: false,
    },
    // An inner build: the global is defined at the gate, so nothing is read
    // undefined in the first place.
    Body {
        name: "TargetFramework global",
        property_group: "<TargetFrameworks>net8.0;net10.0</TargetFrameworks>",
        globals: &[("TargetFramework", "net10.0")],
        certain: true,
    },
];

#[test]
fn a_pre_body_target_framework_read_is_trusted_exactly_when_the_project_builds_its_own_tfm() {
    for gate in GATES {
        for body in BODIES {
            let tmp = TempDir::new().unwrap();
            let (root, props, targets) = write_synthetic_sdk(
                tmp.path(),
                "MySdk",
                &format!(
                    r#"<Project><Import Project="server.props" Condition="{gate}" /></Project>"#
                ),
                "<Project/>",
            );
            write_at(
                &root,
                "server.props",
                r#"<Project><ItemGroup><FrameworkReference Include="Microsoft.AspNetCore.App" /></ItemGroup></Project>"#,
            );
            let source = format!(
                r#"<Project Sdk="MySdk"><PropertyGroup>{}</PropertyGroup></Project>"#,
                body.property_group
            );
            let project_path = write_at(tmp.path(), "Demo.fsproj", &source);
            let resolver = |name: &str| {
                if name == "MySdk" {
                    Ok(SdkResolution::from(SdkPaths {
                        root: root.clone(),
                        props: props.clone(),
                        targets: targets.clone(),
                    }))
                } else {
                    Err(SdkResolveError::NotFound)
                }
            };
            let globals: HashMap<String, String> = body
                .globals
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            let result = parse_fsproj_with_imports(
                &source,
                &canon(&project_path),
                &globals,
                &HashMap::new(),
                Some(&resolver),
                None,
            )
            .expect("well-formed XML parses");
            let context = format!(
                "gate {gate} / body {:?}; causes: {:#?}",
                body.name, result.package_reference_uncertainties
            );
            assert_eq!(
                !result.package_references_uncertain, body.certain,
                "{context}"
            );
            if body.certain {
                let names: Vec<&str> = result
                    .framework_references
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect();
                // Every TFM the bodies name passes both gates.
                assert_eq!(names, ["Microsoft.AspNetCore.App"], "{context}");
            }
        }
    }
}
