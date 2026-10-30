//! The oracle's `restore` op runs the devshell SDK's own NuGet.
//!
//! Every resolver verdict `resolver_diff` establishes is a claim about one NuGet
//! build, and the one that matters is the build `dotnet restore` would run: a
//! NuGet release can change resolution while reading the same dependency-graph
//! spec (`docs/nuget-dgspec-boundary-plan.md`, *Per-SDK drift*). The SDKs
//! bundle unpublished release-candidate builds, so no package reference can
//! stand in for them; the oracle loads NuGet from the SDK directory instead
//! (see `tools/nuget-oracle/Program.fs`). This pins that it does.
//!
//! The expected identity is read here, from the file, without the oracle's
//! help: the path from `dotnet` itself ([`common::sdk_dir`]), and the
//! informational version from the `ProductVersion` entry of the DLL's Win32
//! version resource, which the .NET build stamps with the same string as the
//! `AssemblyInformationalVersion` attribute the oracle reports.

use std::path::Path;

use serde_json::json;

use crate::common::{Oracle, sdk_dir};

/// The smallest restore that runs the resolver: one package, required directly.
fn restore_request(engine: &str) -> serde_json::Value {
    let nuspec = r#"<?xml version="1.0"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>A</id>
    <version>1.0.0</version>
    <authors>a</authors>
    <description>d</description>
  </metadata>
</package>
"#;
    json!({
        "op": "restore",
        "engine": engine,
        "framework": "net10.0",
        "packages": [{ "id": "A", "version": "1.0.0", "nuspec": nuspec }],
        "direct": [{ "id": "A", "range": "1.0.0" }],
    })
}

/// The value of the `ProductVersion` string in a PE file's version resource.
///
/// A `StringFileInfo` entry is `{wLength, wValueLength, wType}` (three `u16`s),
/// the key as NUL-terminated UTF-16LE, padding to a 32-bit boundary, then the
/// value as NUL-terminated UTF-16LE. Resource data is 32-bit aligned in the
/// file, so the padding can be computed from the file offset. Panics unless the
/// key occurs exactly once.
fn product_version(dll: &Path) -> String {
    let bytes = std::fs::read(dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let key: Vec<u8> = "ProductVersion\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let hits: Vec<usize> = bytes
        .windows(key.len())
        .enumerate()
        .filter(|(_, window)| *window == key.as_slice())
        .map(|(offset, _)| offset)
        .collect();
    let [hit] = hits.as_slice() else {
        panic!(
            "expected one ProductVersion entry in {dll:?}, found {}",
            hits.len()
        );
    };
    let value_start = (hit + key.len()).next_multiple_of(4);
    let value: Vec<u16> = bytes[value_start..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16(&value).expect("ProductVersion is UTF-16")
}

#[test]
fn restore_runs_the_devshell_sdks_nuget_commands() {
    let expected_path = sdk_dir().join("NuGet.Commands.dll");
    let expected_version = product_version(&expected_path);
    let mut oracle = Oracle::spawn();

    for engine in ["legacy", "default"] {
        let response = oracle.request(&restore_request(engine));
        assert_eq!(response["resolved"], json!(true), "{engine}: {response}");
        let identity = &response["nugetCommands"];
        let path = identity["path"]
            .as_str()
            .unwrap_or_else(|| panic!("{engine}: no nugetCommands.path in {response}"));
        assert_eq!(
            std::fs::canonicalize(path).expect("canonicalise the reported path"),
            std::fs::canonicalize(&expected_path).expect("canonicalise the SDK's path"),
            "{engine}: the oracle ran a NuGet.Commands other than the SDK's",
        );
        assert_eq!(
            identity["informationalVersion"].as_str(),
            Some(expected_version.as_str()),
            "{engine}: reported identity differs from {expected_path:?}'s",
        );
    }
}
