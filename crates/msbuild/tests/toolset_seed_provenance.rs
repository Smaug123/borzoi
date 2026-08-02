//! The toolset seeds' provenance rule, which is **two** rules that look like
//! one omission.
//!
//! `State::seed_toolset_properties` fills MSBuild's toolset properties once an
//! SDK resolves to the canonical dotnet layout. It handles two groups
//! differently, and the difference is deliberate even though the second reads
//! like a missing call:
//!
//! - **Reserved** (`MSBuildToolsPath`, `MSBuildBinPath`, `MSBuildToolsVersion`,
//!   `MSBuildRuntimeType`) — seeded *over* whatever the walk stored, and their
//!   provenance is scrubbed. Real MSBuild rejects a project write to these
//!   outright, so any value or taint the walk accumulated describes something
//!   no real evaluation ever sees.
//! - **Overridable** (`MSBuildSDKsPath`, `MSBuildExtensionsPath32`,
//!   `MSBuildExtensionsPath`) — seeded only into an empty slot, and their
//!   provenance is **left alone**. Real MSBuild honours a project write here,
//!   so a write the walker *refused* may really have set the property in the
//!   real build. The seed is then a fallback rather than the answer, and the
//!   mark saying so is correct.
//!
//! Ground truth (dotnet 10.0.301, probed 2026-08-02, plain `<Project>` writing
//! each name and reading it back with `-getProperty`):
//!
//! ```text
//! <MSBuildToolsPath>/SPOOFED</…>         → error MSB4004: … reserved, and cannot be modified
//! <MSBuildSDKsPath>/SPOOFED</…>          → "/SPOOFED"
//! <MSBuildExtensionsPath32>/SPOOFED32</…> → "/SPOOFED32"
//! ```
//!
//! Without this test the asymmetry is prose, and the natural "tidy-up" —
//! giving the overridable seeds the same `apply_property_provenance(Clear)`
//! call the reserved ones get — silently converts a correct decline into a
//! wrong commit: the walker would publish its own fallback path as certain
//! while the real build used the project's value.

mod common;

use std::collections::HashMap;

use borzoi_msbuild::{ParsedProject, parse_fsproj_with_imports, resolve_sdk, workloads};
use tempfile::TempDir;

/// Parse `body` with the real SDK resolver, so `seed_toolset_properties` runs.
fn parse_with_real_sdk(body: &str) -> ParsedProject {
    let tmp = TempDir::new().unwrap();
    let project_path = tmp.path().join("Demo.fsproj");
    std::fs::write(&project_path, body).expect("write project");
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
    parse_fsproj_with_imports(
        body,
        &project_path,
        &HashMap::new(),
        &common::oracle_environment(),
        Some(&resolver as &borzoi_msbuild::SdkResolver<'_>),
        None,
    )
    .expect("well-formed XML parses")
}

/// A document whose body writes `name` under a condition the walker cannot
/// decide, *before* any SDK resolves — so the refused write lands before
/// `seed_toolset_properties` and the seed meets a name that is unset but
/// marked.
///
/// The entry deliberately has no `Sdk` attribute: with one, the SDK resolves
/// (and seeds) before the body is ever walked, and the ordering this test is
/// about cannot arise.
///
/// `'$(X.Substring(0,1))' == 'a'` is ordinary MSBuild that this crate does not
/// model, and the oracle says it is **true** — so the real build performs the
/// write.
fn refused_write_before_the_sdk(name: &str) -> String {
    format!(
        r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <{name} Condition="'$(X.Substring(0,1))' == 'a'">/SPOOFED</{name}>
  </PropertyGroup>
  <Import Sdk="Microsoft.NET.Sdk" Project="Sdk.props" />
</Project>"#
    )
}

#[test]
fn an_overridable_toolset_seed_keeps_a_refused_writes_uncertainty() {
    // MSBuild would have taken `/SPOOFED`. We could not decide the condition,
    // so we hold our own computed path — which is only a guess. The seed must
    // not launder it into a certainty.
    for name in ["MSBuildSDKsPath", "MSBuildExtensionsPath32"] {
        let parsed = parse_with_real_sdk(&refused_write_before_the_sdk(name));
        assert!(
            parsed.property_provenance_untrusted(name),
            "{name} takes a project write in the real build, so a refused write \
             to it must leave the toolset seed untrusted",
        );
    }
}

#[test]
fn a_reserved_toolset_seed_discards_a_refused_writes_uncertainty() {
    // The mirror. MSBuild answers a write here with MSB4004 and keeps its own
    // value, so whether *we* could decide the condition changes nothing about
    // the real build's value — and carrying the mark would be a decline bought
    // for no risk at all.
    let parsed = parse_with_real_sdk(&refused_write_before_the_sdk("MSBuildToolsPath"));
    assert!(
        !parsed.property_provenance_untrusted("MSBuildToolsPath"),
        "MSBuildToolsPath is reserved (MSB4004), so a refused project write to \
         it says nothing about the real build's value and must not taint the \
         toolset seed",
    );
}
