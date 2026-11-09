//! Every XML reader in this crate is held to bounded recursion: no element
//! path, repeated thousands of levels deep, may overflow the stack.
//!
//! A `.nuspec` in the global packages folder and a user `NuGet.Config` are
//! input the user need not control, and a stack overflow aborts the LSP
//! outright. `roxmltree` parses iteratively; the hazard is a walk of ours that
//! recurses per element. [`borzoi_oracle_harness::xml_pump`] repeats every short
//! path of the inputs below, so such a walk is found without this file naming
//! it.
//!
//! An admission walk stops at the first thing outside its domain, so a pump of
//! an input it declines tests only the walk up to that point. Each reader with a
//! domain is therefore pumped from inputs it admits (asserted, so a domain
//! change cannot silently hollow the sweep out), covering each shape the domain
//! holds; what it declines but the lenient reader reads gets inputs of its own.
//!
//! A failure aborts the test binary; the last `pump:` line on stderr names the
//! path that overflowed.

use std::path::Path;

use borzoi_oracle_harness::xml_pump::{on_pump_stack, pumps};

use super::resolve::config_is_admitted;
use super::{NuGetIdentity, NuspecDecline, nuspec_is_admitted, verified_nuget_identities};
use crate::{NuGetVersion, PackageId, PackageIdentity, parse_nuspec};

/// Nuspecs B₁ admits: grouped dependencies and references in one, flat in the
/// other (`pack` writes one form or the other), each with a descriptive element
/// holding a subtree, which the admission walk may hold anything in.
const ADMITTED_NUSPECS: &[&str] = &[
    r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata minClientVersion="2.12">
    <id>A</id>
    <version>1.0.0</version>
    <authors>x</authors>
    <description>d<!-- c --></description>
    <releaseNotes><p><b>notes</b></p></releaseNotes>
    <repository type="git" url="https://example.invalid/" />
    <packageTypes><packageType name="Dependency" /></packageTypes>
    <dependencies>
      <group targetFramework="net8.0">
        <dependency id="B" version="[1.0.0, )" include="All" exclude="Build" />
      </group>
    </dependencies>
    <references>
      <group targetFramework="net8.0"><reference file="a.dll" /></group>
    </references>
    <contentFiles><files include="any/any/x.txt" buildAction="None" copyToOutput="false" /></contentFiles>
  </metadata>
</package>
"#,
    r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>A</id>
    <version>1.0.0</version>
    <tags><t>x</t></tags>
    <dependencies><dependency id="C" version="2.0.0" /></dependencies>
    <references><reference file="b.dll" /></references>
  </metadata>
</package>
"#,
];

/// What the lenient reader reads and B₁ declines: groups and flat entries side
/// by side, the elements restore reads and B₁ does not model, and `<files>`.
const DECLINED_NUSPEC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>A</id>
    <version>1.0.0</version>
    <dependencies>
      <group targetFramework="net8.0"><dependency id="B" version="1.0.0" /></group>
      <dependency id="C" version="2.0.0" />
    </dependencies>
    <references>
      <group targetFramework="net8.0"><reference file="a.dll" /></group>
      <reference file="b.dll" />
    </references>
    <frameworkAssemblies>
      <frameworkAssembly assemblyName="System.Xml" targetFramework="net472" />
    </frameworkAssemblies>
    <frameworkReferences>
      <group targetFramework="net8.0"><frameworkReference name="Microsoft.AspNetCore.App" /></group>
    </frameworkReferences>
  </metadata>
  <files><file src="a.dll" target="lib/net8.0" /></files>
</package>
"#;

/// A `NuGet.Config` `config_is_admitted` admits: every section and entry it
/// reads.
const ADMITTED_CONFIG: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="local" value="/packages" protocolVersion="3" />
  </packageSources>
  <disabledPackageSources><add key="other" value="true" /></disabledPackageSources>
</configuration>
"#;

const LEVELS: usize = 10_000;
const BYTE_BUDGET: usize = 512 << 10;

fn identity() -> PackageIdentity {
    PackageIdentity::new(
        PackageId::parse("A").expect("a package id"),
        NuGetVersion::parse("1.0.0").expect("a version"),
    )
}

fn nuget() -> NuGetIdentity {
    NuGetIdentity::new(
        verified_nuget_identities()
            .next()
            .expect("the verified list is not empty"),
    )
}

/// Pump `seed` through both nuspec readers, returning each pump's path and
/// the admission walk's verdict on it.
fn pump_nuspec(seed: &str) -> Vec<(String, Result<(), NuspecDecline>)> {
    let mut out = Vec::new();
    for pump in pumps(seed, 3, LEVELS, BYTE_BUDGET) {
        let label = format!("nuspec {} x{}", pump.path, pump.repeats);
        let verdict = on_pump_stack(&label, move || {
            let _ = parse_nuspec(&pump.text);
            nuspec_is_admitted(pump.text.as_bytes(), &identity(), &nuget())
        });
        out.push((pump.path, verdict));
    }
    out
}

#[test]
fn no_pumped_path_overflows_a_nuspec_reader() {
    let mut pumped = Vec::new();
    for seed in ADMITTED_NUSPECS {
        assert_eq!(
            nuspec_is_admitted(seed.as_bytes(), &identity(), &nuget()),
            Ok(()),
            "a seed must be admitted, or its pumps stop at the decline:\n{seed}"
        );
        pumped.extend(pump_nuspec(seed));
    }
    assert!(
        nuspec_is_admitted(DECLINED_NUSPEC.as_bytes(), &identity(), &nuget()).is_err(),
        "the declined seed is declined"
    );
    assert!(
        parse_nuspec(DECLINED_NUSPEC).is_ok(),
        "the lenient reader reads the declined seed"
    );
    pumped.extend(pump_nuspec(DECLINED_NUSPEC));

    // The admission walk reached inside `<metadata>` on a pumped document, and
    // admitted the pumps that nest only a descriptive element's subtree.
    let reached_metadata = |path: &str| {
        pumped.iter().any(|(p, verdict)| {
            p == path
                && matches!(verdict, Err(NuspecDecline::Vocabulary { path, .. })
                    if path.starts_with("metadata/"))
        })
    };
    assert!(
        reached_metadata("dependencies/group/dependency"),
        "{pumped:?}"
    );
    assert!(reached_metadata("references/group/reference"), "{pumped:?}");
    assert!(
        pumped
            .iter()
            .any(|(p, verdict)| p == "releaseNotes/p/b" && verdict.is_ok()),
        "{pumped:?}"
    );
}

#[test]
fn no_pumped_path_overflows_the_config_reader() {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let seed_file = tmp.path().join("seed.config");
    std::fs::write(&seed_file, ADMITTED_CONFIG).expect("write the seed config");
    assert!(
        config_is_admitted(&seed_file),
        "the seed must be admitted, or its pumps stop at the decline"
    );
    let mut paths = Vec::new();
    for pump in pumps(ADMITTED_CONFIG, 3, LEVELS, BYTE_BUDGET) {
        let file = tmp.path().join("NuGet.Config");
        std::fs::write(&file, &pump.text).expect("write the pumped config");
        let label = format!("config {} x{}", pump.path, pump.repeats);
        let admitted = on_pump_stack(&label, move || config_is_admitted(Path::new(&file)));
        paths.push((pump.path, admitted));
    }
    assert!(
        paths
            .iter()
            .any(|(p, _)| p == "configuration/packageSources/add"),
        "{paths:?}"
    );
}
