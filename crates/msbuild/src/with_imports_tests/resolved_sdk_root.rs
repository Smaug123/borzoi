//! [`ParsedProject::resolved_sdk_root`] — the import-directory root of the
//! first SDK the *returned* evaluation resolved. The headline guarantee is
//! that the two-pass walk reports the **selected** pass's root, never one seen
//! only in a pass that was discarded.

use super::*;
use tempfile::TempDir;

#[test]
fn entry_sdk_records_its_resolved_root() {
    // The simplest case: an entry project with its own `<Project Sdk=...>`.
    // Only one pass runs (the entry has an SDK), so the recorded root is just
    // that SDK's.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="a.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "MySdk" {
            Ok(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert_eq!(
        result.resolved_sdk_root.as_deref(),
        Some(root.as_path()),
        "the entry project's own SDK root must be recorded"
    );
}

#[test]
fn no_sdk_records_no_root() {
    // A bare `<Project>` resolves no SDK at all.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="a.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |_name| Err(SdkResolveError::NotFound));
    assert_eq!(
        result.resolved_sdk_root, None,
        "a project that resolves no SDK records no root"
    );
}

#[test]
fn sdkless_entry_nested_body_sdk_is_not_recorded() {
    // An SDK-less *entry* whose framework SDK arrives via a body
    // `<Import Project="nested.targets"/>` where `nested.targets` is itself
    // `<Project Sdk="BodySdk">`. The SDK *is* resolved and spliced (its body
    // contributes `nested-body.fs`), but it is **not** recorded as
    // `resolved_sdk_root`: identifying which of a body's imports establishes
    // the framework SDK is subtle and order-dependent (and was a repeated
    // source of mis-recording), so we deliberately leave it `None` and let the
    // consumer fall back to its own default-root probe.
    let tmp = TempDir::new().unwrap();
    let (root_b, props_b, targets_b) =
        write_synthetic_sdk(tmp.path(), "BodySdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="BodySdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="nested.targets" />
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "BodySdk" {
            Ok(SdkPaths {
                root: root_b.clone(),
                props: props_b.clone(),
                targets: targets_b.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });

    // The nested SDK really was resolved and spliced (sanity): its body's item
    // is present, so this is a resolved-but-deliberately-not-recorded case, not
    // a resolution failure.
    let dir = canon(tmp.path());
    assert!(
        paths_of(&result.items).contains(&dir.join("nested-body.fs")),
        "expected the nested SDK to resolve and splice its body",
    );
    assert_eq!(
        result.resolved_sdk_root, None,
        "a body-reached nested SDK is resolved but deliberately not recorded; \
         got {:?}",
        result.resolved_sdk_root,
    );
}

#[test]
fn directory_build_props_import_sdk_helper_is_not_recorded() {
    // An SDK-less entry whose body reaches no project SDK: only an implicit
    // `Directory.Build.props` resolves an SDK, via `<Import Sdk=...>`. That
    // helper is infrastructure, not the project's framework SDK, so it must
    // *not* be recorded — `resolved_sdk_root` stays `None` and the consumer
    // falls back to its default dotnet root rather than the helper's install.
    let tmp = TempDir::new().unwrap();
    let (helper_root, helper_props, helper_targets) =
        write_synthetic_sdk(tmp.path(), "HelperSdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <Import Sdk="HelperSdk" Project="Sdk.props" />
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "HelperSdk" {
            Ok(SdkPaths {
                root: helper_root.clone(),
                props: helper_props.clone(),
                targets: helper_targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert_eq!(
        result.resolved_sdk_root, None,
        "a helper SDK imported by Directory.Build.props must not be recorded \
         as the project's SDK root; got {:?}",
        result.resolved_sdk_root,
    );
}

#[test]
fn directory_build_props_root_sdk_helper_is_not_recorded() {
    // Same principle via the root-`Sdk` form: a `Directory.Build.props` that
    // is itself `<Project Sdk="HelperSdk">` resolves HelperSdk when spliced
    // (an *imported* file's root SDK, reached outside the entry body), but it
    // is still infrastructure, not the project's framework SDK.
    let tmp = TempDir::new().unwrap();
    let (helper_root, helper_props, helper_targets) =
        write_synthetic_sdk(tmp.path(), "HelperSdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project Sdk="HelperSdk">
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "HelperSdk" {
            Ok(SdkPaths {
                root: helper_root.clone(),
                props: helper_props.clone(),
                targets: helper_targets.clone(),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert_eq!(
        result.resolved_sdk_root, None,
        "a Directory.Build.props that carries its own `Sdk` must not be \
         recorded as the project's SDK root; got {:?}",
        result.resolved_sdk_root,
    );
}
