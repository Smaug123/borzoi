//! Every XML reader in this crate is held to bounded recursion: no element
//! path, repeated thousands of levels deep, may overflow the stack.
//!
//! A `.nuspec` in the global packages folder and a user `NuGet.Config` are
//! input the user need not control, and a stack overflow aborts the LSP
//! outright. `roxmltree` parses iteratively; the hazard is a walk of ours that
//! recurses per element. [`borzoi_oracle_harness::xml_pump`] repeats every short
//! path of the inputs below, so such a walk is found without this file naming
//! it. The inputs hold every element each reader looks at, inside the domain
//! B₁ admits wherever the reader has one, so the admission walks go as deep as
//! they can before declining.
//!
//! A failure aborts the test binary; the last `pump:` line on stderr names the
//! path that overflowed.

use borzoi_oracle_harness::xml_pump::{on_pump_stack, pumps};

use super::resolve::config_is_admitted;
use super::{NuGetIdentity, nuspec_is_admitted, verified_nuget_identities};
use crate::{NuGetVersion, PackageId, PackageIdentity, parse_nuspec};

/// Every element the lenient reader and the admission walk read.
const EVERY_NUSPEC_CONSTRUCT: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata minClientVersion="2.12">
    <id>A</id>
    <version>1.0.0</version>
    <authors>x</authors>
    <description>d<!-- c --></description>
    <repository type="git" url="https://example.invalid/" />
    <license type="expression">MIT</license>
    <packageTypes><packageType name="Dependency" /></packageTypes>
    <dependencies>
      <group targetFramework="net8.0">
        <dependency id="B" version="[1.0.0, )" include="All" exclude="Build" />
      </group>
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
    <contentFiles><files include="any/any/x.txt" buildAction="None" copyToOutput="false" /></contentFiles>
  </metadata>
  <files><file src="a.dll" target="lib/net8.0" /></files>
</package>
"#;

/// Every element `config_is_admitted` reads, and one it declines.
const EVERY_CONFIG_CONSTRUCT: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="local" value="/packages" protocolVersion="3" />
  </packageSources>
  <disabledPackageSources><add key="other" value="true" /></disabledPackageSources>
  <config><add key="globalPackagesFolder" value="/g" /></config>
</configuration>
"#;

const LEVELS: usize = 10_000;
const BYTE_BUDGET: usize = 512 << 10;

#[test]
fn no_pumped_path_overflows_a_nuspec_reader() {
    let mut paths = Vec::new();
    for pump in pumps(EVERY_NUSPEC_CONSTRUCT, 3, LEVELS, BYTE_BUDGET) {
        let label = format!("nuspec {} x{}", pump.path, pump.repeats);
        on_pump_stack(&label, move || {
            let _ = parse_nuspec(&pump.text);
            let identity = PackageIdentity::new(
                PackageId::parse("A").expect("a package id"),
                NuGetVersion::parse("1.0.0").expect("a version"),
            );
            let nuget = NuGetIdentity::new(
                verified_nuget_identities()
                    .next()
                    .expect("the verified list is not empty"),
            );
            let _ = nuspec_is_admitted(pump.text.as_bytes(), &identity, &nuget);
        });
        paths.push(pump.path);
    }
    assert!(
        paths.iter().any(|p| p == "metadata/dependencies/group"),
        "{paths:?}"
    );
}

#[test]
fn no_pumped_path_overflows_the_config_reader() {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let mut paths = Vec::new();
    for pump in pumps(EVERY_CONFIG_CONSTRUCT, 3, LEVELS, BYTE_BUDGET) {
        let file = tmp.path().join("NuGet.Config");
        std::fs::write(&file, &pump.text).expect("write the pumped config");
        let label = format!("config {} x{}", pump.path, pump.repeats);
        on_pump_stack(&label, move || {
            let _ = config_is_admitted(&file);
        });
        paths.push(pump.path);
    }
    assert!(
        paths
            .iter()
            .any(|p| p == "configuration/packageSources/add"),
        "{paths:?}"
    );
}
