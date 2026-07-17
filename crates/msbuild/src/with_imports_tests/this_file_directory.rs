//! `$(MSBuildThisFileDirectory)` rebinding and restoration across import
//! boundaries, including the symlinked-tempdir case.

use super::*;
use tempfile::TempDir;

#[test]
fn msbuild_this_file_directory_rebinds_inside_import() {
    // sub/a.props references $(MSBuildThisFileDirectory) — which
    // should resolve to the sub/ directory (with trailing slash), not
    // the project's directory. We use the rebound value to build an
    // include path; the test passes iff that path lands in `sub/`.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="sub/a.props" />
  <ItemGroup>
    <Compile Include="$(WhereAmI)/here.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "sub/a.props",
        r#"<Project>
  <PropertyGroup>
    <WhereAmI>$(MSBuildThisFileDirectory)</WhereAmI>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(paths_of(&result.items).len(), 1);
    // The include path was constructed as `<canon sub dir>//here.fs`
    // (trailing slash from MSBuildThisFileDirectory + `/` separator).
    // After project_dir.join(...), the result lives under sub/. We
    // assert by stripping the canonicalised tmp prefix.
    let canon_tmp = canon(tmp.path());
    let stripped = result.items[0]
        .include
        .strip_prefix(&canon_tmp)
        .expect("include path lives under tempdir");
    let stripped_str = stripped.to_string_lossy().replace('\\', "/");
    assert!(
        stripped_str.contains("sub/") && stripped_str.ends_with("here.fs"),
        "expected path under sub/, got {stripped_str}"
    );
}

#[test]
fn msbuild_this_file_restores_after_import_returns() {
    // After walking sub/a.props, the project body's *own*
    // MSBuildThisFile should be back to the project file. We
    // exercise this by reading MSBuildThisFile after the import via a
    // property write — the recorded value should be the project's
    // filename (`Demo.fsproj`), not the imported file's name.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="sub/a.props" />
  <PropertyGroup>
    <AfterImport>$(MSBuildThisFile)</AfterImport>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "sub/a.props",
        r#"<Project>
  <PropertyGroup>
    <InsideImport>$(MSBuildThisFile)</InsideImport>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        result.properties.get("AfterImport").map(String::as_str),
        Some("Demo.fsproj"),
        "MSBuildThisFile should be restored to project filename after import returns; got {:?}",
        result.properties.get("AfterImport"),
    );
    assert_eq!(
        result.properties.get("InsideImport").map(String::as_str),
        Some("a.props"),
        "MSBuildThisFile should rebind to imported file's name while inside; got {:?}",
        result.properties.get("InsideImport"),
    );
}

#[cfg(unix)]
#[test]
fn this_file_directory_rebinds_to_pre_canonical_path_through_symlink() {
    // Sibling of [`nested_import_resolves_against_pre_canonical_directory`]
    // for `$(MSBuildThisFileDirectory)`. When an import is reached
    // through a symlink, MSBuild rebinds `MSBuildThisFileDirectory`
    // to the *symlink* parent — not the canonical target's parent —
    // so `$(MSBuildThisFileDirectory)Generated.fs` inside the
    // imported file should resolve under link_side/. Using the
    // canonicalised path here would surface as a `Compile Include`
    // pointing at the target-side path, silently disagreeing with
    // MSBuild for any layout where the two sides differ.
    let tmp = TempDir::new().unwrap();
    let link_dir = tmp.path().join("link_side");
    let target_dir = tmp.path().join("target_side");
    std::fs::create_dir(&link_dir).unwrap();
    std::fs::create_dir(&target_dir).unwrap();

    write_at(
        &target_dir,
        "common.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(MSBuildThisFileDirectory)Generated.fs" />
  </ItemGroup>
</Project>"#,
    );
    std::os::unix::fs::symlink(
        target_dir.join("common.props"),
        link_dir.join("common.props"),
    )
    .unwrap();

    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="link_side/common.props" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    let canon_link = canon(&link_dir);
    assert_eq!(
        paths_of(&result.items),
        vec![canon_link.join("Generated.fs")],
        "MSBuildThisFileDirectory must rebind to the symlink-side parent, \
         not the canonicalised target's parent",
    );
}
