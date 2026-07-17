use std::fs;
use std::path::Path;

use borzoi::project_assets::resolve_assemblies;
use tempfile::TempDir;

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"").unwrap();
}

/// End-to-end smoke test against the real `tools/fcs-dump` assets file.
///
/// The fixture uses TFM `net10.0` and lists `Microsoft.NETCore.App` as a
/// framework reference; we stub out a minimal `packs/` tree so the
/// resolver doesn't depend on whatever .NET install the host happens to
/// have.
#[test]
fn end_to_end_against_fcs_dump_fixture() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_assets/single_tfm.json");

    let tmp = TempDir::new().unwrap();
    let dotnet_root = tmp.path();
    let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0");
    touch(&pack.join("System.dll"));
    touch(&pack.join("System.Runtime.dll"));

    let result = resolve_assemblies(&fixture, dotnet_root).expect("resolve_assemblies");

    // No project references in the fixture.
    assert!(
        result.project_ref_tfms.is_empty(),
        "unexpected project refs: {:?}",
        result.project_ref_tfms
    );

    // FSharp.Compiler.Service is the headline package; assert its DLL is
    // present in the result.
    assert!(
        result.package_dlls.iter().any(|p| p
            .file_name()
            .is_some_and(|n| n == "FSharp.Compiler.Service.dll")),
        "FSharp.Compiler.Service.dll missing from {:#?}",
        result.package_dlls
    );

    // Framework DLLs came from our stubbed pack.
    let mut expected_framework = vec![pack.join("System.Runtime.dll"), pack.join("System.dll")];
    expected_framework.sort();
    assert_eq!(result.framework_dlls, expected_framework);
}
