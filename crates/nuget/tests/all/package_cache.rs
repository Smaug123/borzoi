use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use borzoi_nuget::{
    NuGetVersion, PackageCacheError, PackageId, PackageIdentity, PackagePaths, PackageReadError,
    list_committed_package_versions, read_installed_package,
};

fn identity(id: &str, version: &str) -> PackageIdentity {
    PackageIdentity::new(
        PackageId::parse(id).expect("package id parses"),
        NuGetVersion::parse(version).expect("version parses"),
    )
}

fn nuspec_text() -> &'static str {
    r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>FSharp.Core</id>
    <version>10.1.204</version>
    <authors>FSharp</authors>
    <description>Test package</description>
    <dependencies>
      <group targetFramework="net8.0">
        <dependency id="System.Memory" version="[4.5.5, )" />
      </group>
    </dependencies>
  </metadata>
</package>
"#
}

fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(|unit| unit.to_le_bytes()));
    bytes
}

fn write_committed_version(root: &std::path::Path, id: &str, version: &str) -> PackagePaths {
    let identity = identity(id, version);
    let paths = PackagePaths::new(root, &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.metadata_path, "{}").expect("write marker");
    paths
}

#[test]
fn global_packages_paths_use_lowercase_id_and_normalized_version() {
    let root = PathBuf::from("/tmp/global-packages");
    let identity = identity("FSharp.Core", "10.1.204+metadata");
    let paths = PackagePaths::new(&root, &identity);

    assert_eq!(paths.package_dir, root.join("fsharp.core").join("10.1.204"));
    assert_eq!(
        paths.nuspec_path,
        root.join("fsharp.core")
            .join("10.1.204")
            .join("fsharp.core.nuspec")
    );
    assert_eq!(
        paths.metadata_path,
        root.join("fsharp.core")
            .join("10.1.204")
            .join(".nupkg.metadata")
    );
}

#[test]
fn list_committed_package_versions_returns_sorted_committed_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let id = PackageId::parse("FSharp.Core").expect("package id parses");
    let first = write_committed_version(temp.path(), "FSharp.Core", "1.0.0");
    let second = write_committed_version(temp.path(), "FSharp.Core", "2.0.0");
    let uncommitted = PackagePaths::new(temp.path(), &identity("FSharp.Core", "3.0.0"));
    fs::create_dir_all(&uncommitted.package_dir).expect("uncommitted package dir");
    fs::create_dir_all(temp.path().join("fsharp.core").join("not-a-version"))
        .expect("invalid version dir");
    fs::write(
        temp.path().join("fsharp.core").join("readme.txt"),
        "ignored",
    )
    .expect("stray file");

    let entries =
        list_committed_package_versions(temp.path(), &id).expect("list committed versions");

    let versions = entries
        .iter()
        .map(|entry| entry.identity.version.to_normalized_string())
        .collect::<Vec<_>>();
    assert_eq!(versions, ["1.0.0", "2.0.0"]);
    assert_eq!(entries[0].identity.id, id);
    assert_eq!(entries[0].paths, first);
    assert_eq!(entries[1].paths, second);
}

#[test]
fn list_committed_package_versions_requires_canonical_version_folder_spelling() {
    let temp = tempfile::tempdir().expect("tempdir");
    let id = PackageId::parse("FSharp.Core").expect("package id parses");
    let id_dir = temp.path().join("fsharp.core");
    fs::create_dir_all(&id_dir).expect("id dir");
    for folder in ["1", "1.0.0+metadata", "2.0.0-BETA"] {
        let package_dir = id_dir.join(folder);
        fs::create_dir_all(&package_dir).expect("noncanonical package dir");
        fs::write(package_dir.join(".nupkg.metadata"), "{}").expect("write marker");
    }
    write_committed_version(temp.path(), "FSharp.Core", "1.0.0");
    write_committed_version(temp.path(), "FSharp.Core", "1.0.0-beta");

    let entries =
        list_committed_package_versions(temp.path(), &id).expect("list committed versions");

    let versions = entries
        .iter()
        .map(|entry| entry.identity.version.to_normalized_string())
        .collect::<Vec<_>>();
    assert_eq!(versions, ["1.0.0-beta", "1.0.0"]);
}

#[test]
fn list_committed_package_versions_preserves_strict_version_identities() {
    let temp = tempfile::tempdir().expect("tempdir");
    let id = PackageId::parse("FSharp.Core").expect("package id parses");
    write_committed_version(temp.path(), "FSharp.Core", "1.0--0");
    write_committed_version(temp.path(), "FSharp.Core", "1.0-0");

    let entries =
        list_committed_package_versions(temp.path(), &id).expect("list committed versions");

    let versions = entries
        .iter()
        .map(|entry| entry.identity.version.to_normalized_string())
        .collect::<Vec<_>>();
    assert_eq!(versions, ["1.0.0--0", "1.0.0-0"]);
    assert_ne!(entries[0].identity, entries[1].identity);
}

#[test]
fn list_committed_package_versions_missing_id_dir_is_empty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let id = PackageId::parse("FSharp.Core").expect("package id parses");

    let entries =
        list_committed_package_versions(temp.path(), &id).expect("list committed versions");

    assert!(entries.is_empty());
}

#[test]
fn list_committed_package_versions_errors_when_id_path_is_not_a_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let id = PackageId::parse("FSharp.Core").expect("package id parses");
    fs::write(temp.path().join("fsharp.core"), "not a directory").expect("id file");

    let err =
        list_committed_package_versions(temp.path(), &id).expect_err("id path is not a directory");

    assert!(
        matches!(err, PackageCacheError::Io { path, .. } if path == temp.path().join("fsharp.core"))
    );
}

#[cfg(unix)]
#[test]
fn list_committed_package_versions_errors_when_commit_marker_stat_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let id = PackageId::parse("FSharp.Core").expect("package id parses");
    let identity = identity("FSharp.Core", "1.0.0");
    let paths = PackagePaths::new(temp.path(), &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.metadata_path, "{}").expect("write marker");
    let original_mode = fs::metadata(&paths.package_dir)
        .expect("package dir metadata")
        .permissions()
        .mode();
    fs::set_permissions(&paths.package_dir, fs::Permissions::from_mode(0o000))
        .expect("seal package dir");

    let result = list_committed_package_versions(temp.path(), &id);

    fs::set_permissions(
        &paths.package_dir,
        fs::Permissions::from_mode(original_mode),
    )
    .expect("restore package dir permissions");
    let err = result.expect_err("commit marker stat failure is propagated");
    let PackageCacheError::Io { path, source } = err;
    assert_eq!(path, paths.metadata_path);
    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn read_installed_package_requires_commit_marker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let identity = identity("FSharp.Core", "10.1.204");
    let paths = PackagePaths::new(temp.path(), &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.nuspec_path, nuspec_text()).expect("write nuspec");

    let err = read_installed_package(temp.path(), identity.clone()).expect_err("not committed");
    assert!(
        matches!(err, PackageReadError::NotInstalled { metadata_path } if metadata_path == paths.metadata_path)
    );

    fs::write(&paths.metadata_path, "{}").expect("write marker");
    let package = read_installed_package(temp.path(), identity).expect("installed package");

    assert_eq!(package.paths, paths);
    assert_eq!(package.nuspec.dependency_groups.len(), 1);
    assert_eq!(
        package.nuspec.dependency_groups[0]
            .target_framework
            .short_folder_name()
            .as_deref(),
        Some("net8.0")
    );
    assert_eq!(
        package.nuspec.dependency_groups[0].dependencies[0]
            .version_range
            .as_ref()
            .expect("range")
            .to_normalized_string(),
        "[4.5.5, )"
    );
}

#[test]
fn read_installed_package_accepts_utf16_nuspec() {
    let temp = tempfile::tempdir().expect("tempdir");
    let identity = identity("FSharp.Core", "10.1.204");
    let paths = PackagePaths::new(temp.path(), &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.metadata_path, "{}").expect("write marker");
    let nuspec = format!(
        r#"<?xml version="1.0" encoding="utf-16"?>{}"#,
        nuspec_text()
    );
    fs::write(&paths.nuspec_path, utf16le_with_bom(&nuspec)).expect("write nuspec");

    let package = read_installed_package(temp.path(), identity).expect("installed package");

    assert_eq!(package.nuspec.dependency_groups.len(), 1);
    assert_eq!(
        package.nuspec.dependency_groups[0].dependencies[0].id,
        PackageId::parse("System.Memory").unwrap()
    );
}

#[test]
fn read_installed_package_requires_nuspec_after_commit_marker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let identity = identity("FSharp.Core", "10.1.204");
    let paths = PackagePaths::new(temp.path(), &identity);
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.metadata_path, "{}").expect("write marker");

    let err = read_installed_package(temp.path(), identity).expect_err("missing nuspec");
    assert!(
        matches!(err, PackageReadError::MissingNuspec { nuspec_path } if nuspec_path == paths.nuspec_path)
    );
}

#[test]
fn read_installed_package_finds_cased_root_nuspec() {
    let temp = tempfile::tempdir().expect("tempdir");
    let identity = identity("FSharp.Core", "10.1.204");
    let paths = PackagePaths::new(temp.path(), &identity);
    let cased_nuspec_path = paths.package_dir.join("FSharp.Core.nuspec");
    fs::create_dir_all(&paths.package_dir).expect("package dir");
    fs::write(&paths.metadata_path, "{}").expect("write marker");
    fs::write(&cased_nuspec_path, nuspec_text()).expect("write nuspec");

    let package = read_installed_package(temp.path(), identity).expect("installed package");

    assert_eq!(package.paths.nuspec_path, cased_nuspec_path);
    assert_eq!(package.nuspec.dependency_groups.len(), 1);
}
