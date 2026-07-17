//! Handshake and baseline metadata-emit smoke tests.
//!
//! Split out of the former single-file `csharp_sidecar.rs`.

use super::support::*;

#[test]
fn handshake_and_shutdown() {
    let dotnet = find_dotnet();
    let handle = build_and_start(&dotnet);
    let init = handle.initialize_result();
    assert_eq!(init.protocol_version, PROTOCOL_VERSION);
    assert!(
        !init.runtime_version.is_empty(),
        "runtimeVersion should be populated, got {:?}",
        init.runtime_version
    );
    // Phase 3 always reports a Roslyn version: the Microsoft.CodeAnalysis
    // assemblies are loaded by the sidecar even before any MSBuild work.
    let roslyn = init
        .roslyn_version
        .as_deref()
        .expect("phase 3 reports a Roslyn version");
    assert!(
        roslyn.starts_with('5') || roslyn.starts_with('4'),
        "Roslyn version should look like 4.x or 5.x, got {roslyn:?}"
    );

    handle.shutdown().expect("shutdown clean");
}

#[test]
fn build_metadata_empty_fixture_emits_a_loadable_dll() {
    let dotnet = find_dotnet();
    // Restore packages for the fixture so MSBuildWorkspace finds an asset
    // file. We pay this once per CI run; results are cached in ~/.nuget.
    let fixture = workspace_root().join("tools/csharp-sidecar/test-fixtures/empty");
    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Empty.csproj");
    let result = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect("phase 4 buildMetadata returns a metadata DLL");

    // We deliberately do not assert `!from_cache`: prior `cargo test` runs
    // (or another test in this binary that exercises the same fixture)
    // populate the content-addressed cache, and a hit here is correct
    // behaviour. The dedicated idempotency test covers the "second call
    // hits" property directly via its own temp fixture.
    assert!(
        result.transitive_project_refs.is_empty(),
        "phase 4 does not chase transitive project refs"
    );
    assert!(
        result.metadata_dll_path.is_absolute(),
        "metadataDllPath should be absolute, got {:?}",
        result.metadata_dll_path,
    );
    assert!(
        result.metadata_dll_path.exists(),
        "metadata DLL should exist at {:?}",
        result.metadata_dll_path,
    );
    // The atomic-publish contract: file lives under the workspace's
    // obj/borzoi/csharp-sidecar/<prefix>/ directory and its
    // filename is `<content_hash hex>.dll`.
    let expected_root = workspace_root().join("obj/borzoi/csharp-sidecar");
    assert!(
        result.metadata_dll_path.starts_with(&expected_root),
        "expected DLL inside {expected_root:?}, got {:?}",
        result.metadata_dll_path,
    );
    assert_eq!(
        result
            .metadata_dll_path
            .extension()
            .and_then(|s| s.to_str()),
        Some("dll"),
        "expected a .dll, got {:?}",
        result.metadata_dll_path,
    );
    // Filename stem is the lowercase-hex of the content hash, full match.
    let stem = result
        .metadata_dll_path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("emitted DLL has a stem");
    assert_eq!(
        stem,
        borzoi::csharp_sidecar::content_hash_hex(&result.content_hash),
        "DLL filename stem must be the lowercase-hex content hash",
    );
    // Round-trip: the assembly reader should see the user's public class.
    // A successful parse here is also the test of atomic publish — a
    // half-written `.tmp` that escaped the rename would fail to parse — so
    // we don't separately scan for stray temps. (The cache directory is
    // workspace-wide and shared across tests, so a direct stray-temp scan
    // would still race against any other test publishing into the same
    // prefix; the parse oracle sidesteps that.)
    let bytes = std::fs::read(&result.metadata_dll_path).expect("read DLL");
    let view = Ecma335Assembly::parse(&bytes).expect("parse DLL");
    let entities = view.enumerate_type_defs().expect("enumerate type defs");
    let fqns: Vec<String> = entities
        .iter()
        .map(|e| {
            if e.namespace.is_empty() {
                e.name.clone()
            } else {
                format!("{}.{}", e.namespace.join("."), e.name)
            }
        })
        .collect();
    assert!(
        fqns.iter().any(|f| f == "EmptyFixture.HelloWorld"),
        "expected EmptyFixture.HelloWorld among emitted types, got {fqns:?}",
    );

    handle.shutdown().expect("shutdown clean");
}
