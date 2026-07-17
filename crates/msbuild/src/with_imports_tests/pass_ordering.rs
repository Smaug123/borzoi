//! MSBuild pass ordering across files: every `<PropertyGroup>` in the whole
//! import graph (body, explicit imports, `Directory.Build.*`) finalises
//! before ANY `<ItemGroup>` evaluates. So an item may consume a property
//! that is only written *after* its own document position — including by a
//! file (`Directory.Build.targets`, a trailing `<Import>`) that is walked
//! after the item's file. Expectations validated against
//! `dotnet msbuild -getItem` (see `tests/fsproj_packageref_diff.rs` for the
//! in-CI differential pins).

use super::*;
use tempfile::TempDir;

#[test]
fn body_item_sees_property_from_trailing_explicit_import() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(GenDir)Gen.fs" />
  </ItemGroup>
  <Import Project="trailing.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "trailing.props",
        r#"<Project>
  <PropertyGroup>
    <GenDir>gen/</GenDir>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("gen/Gen.fs")]);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn body_item_sees_property_from_directory_build_targets() {
    // Directory.Build.targets is imported after the project body, but its
    // property writes still belong to the property pass — the body's items
    // evaluate afterwards and see them.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(FromTargets).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup>
    <FromTargets>Late</FromTargets>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("Late.fs")]);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn imported_item_sees_later_body_property_and_keeps_this_file_binding() {
    // Two things at once, both pinned against real MSBuild:
    //   * the imported file's item is gated on a property the entry body
    //     defines *after* the import site — pass ordering includes it;
    //   * at item-evaluation time `$(MSBuildThisFileDirectory)` still binds
    //     to the *defining* file's directory, not the entry project's.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="sub/inner.props" />
  <PropertyGroup>
    <Late>yes</Late>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "sub/inner.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(MSBuildThisFileDirectory)Extra.fs" Condition="'$(Late)' == 'yes'" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("sub/Extra.fs")]);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn glob_include_expands_property_defined_later_in_document() {
    // Glob expansion happens in the item pass too, so a wildcard include
    // whose pattern embeds a property gets the FINAL property value — the
    // resolver must see the expanded pattern, not an empty fragment.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(SrcDir)/**/*.fs" />
  </ItemGroup>
  <PropertyGroup>
    <SrcDir>src</SrcDir>
  </PropertyGroup>
</Project>"#,
    );
    let canon_project = canon(&project_path);
    let requests = std::cell::RefCell::new(Vec::new());
    let resolved = vec![canon(tmp.path()).join("src/A.fs")];
    let resolver = |req: &GlobRequest<'_>| {
        requests.borrow_mut().push(req.include.to_string());
        resolved.clone()
    };
    let result = parse_fsproj_with_imports(
        &std::fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        None,
        Some(&resolver),
    )
    .expect("well-formed XML parses");
    assert_eq!(requests.into_inner(), vec!["src/**/*.fs".to_string()]);
    assert_eq!(paths_of(&result.items), resolved);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
}

#[test]
fn import_path_leaning_on_unpinned_property_is_dropped_and_flagged() {
    // `P` was assembled from $(TargetFramework), which is carved out of
    // undefined-read exactness: a real build that supplies TargetFramework
    // imports a different file, so best-effort following of "shared.props"
    // would be an over-resolve. The import is dropped (same treatment as a
    // directly-undefined path) and both item sets degrade structurally.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <P>$(TargetFramework)shared.props</P>
  </PropertyGroup>
  <Import Project="$(P)" />
  <ItemGroup>
    <Compile Include="Body.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "shared.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromShared.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("Body.fs")],
        "the ambiguous import must not be followed"
    );
    assert!(result.items_uncertain, "diags: {:?}", result.diagnostics);
    assert!(
        result.package_references_uncertain,
        "a dropped import could have carried dependency items"
    );
}

#[test]
fn get_path_of_file_above_with_unpinned_argument_is_dropped_and_flagged() {
    // The search-start argument leaned on $(TargetFramework), which is
    // carved out of undefined-read exactness: a real build could search
    // from a different directory and import a different file, so the
    // resolved path inherits the unpinned state and the import is dropped
    // and flagged like any other ambiguous path.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "sub/Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <P>$(TargetFramework)</P>
  </PropertyGroup>
  <Import Project="$([MSBuild]::GetPathOfFileAbove('shared.props', '$(P)'))" />
  <ItemGroup>
    <Compile Include="Body.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "shared.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromShared.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("sub/Body.fs")],
        "the ambiguous import must not be followed; diags: {:?}",
        result.diagnostics
    );
    assert!(result.items_uncertain);
    assert!(result.package_references_uncertain);
}

#[test]
fn body_item_condition_sees_final_value_written_by_directory_build_targets() {
    // The body sets Flag=true; Directory.Build.targets (walked after the
    // body) resets it to false. The body item's condition evaluates in the
    // item pass against the FINAL table, so the item is excluded — cleanly,
    // with no divergence to report.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <Flag>true</Flag>
  </PropertyGroup>
  <ItemGroup Condition="'$(Flag)' == 'true'">
    <Compile Include="Gated.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup>
    <Flag>false</Flag>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items.is_empty(),
        "final Flag=false must exclude the item, got {:?}",
        result.items
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}
