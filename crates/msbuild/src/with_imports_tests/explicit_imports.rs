//! Explicit `<Import>` handling: property/item contribution, path
//! substitution, importer-relative resolution, and project-references
//! contributed by imported files.

use super::*;
use tempfile::TempDir;

#[test]
fn explicit_import_contributes_property_visible_to_project_body() {
    // The hello-world scenario: an Import sets a property that the
    // project file then uses. If the walker fails to merge property
    // state from imported files into the project's substitution map,
    // `$(Suffix)` would substitute to "" and the item path would be
    // garbage — so this catches the most basic merging mistake.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="common.props" />
  <ItemGroup>
    <Compile Include="A.$(Suffix)" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "common.props",
        r#"<Project>
  <PropertyGroup>
    <Suffix>fs</Suffix>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("A.fs")]);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
}

#[test]
fn explicit_import_contributes_compile_items_in_walk_order() {
    // Items contributed by an Import appear in document order
    // interleaved with the project's own items, mirroring MSBuild's
    // single linear evaluation across all imported files.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Before.fs" />
  </ItemGroup>
  <Import Project="extra.props" />
  <ItemGroup>
    <Compile Include="After.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromImport.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![
            dir.join("Before.fs"),
            dir.join("FromImport.fs"),
            dir.join("After.fs"),
        ]
    );
    assert!(!result.is_partial);
}

#[test]
fn import_path_substitutes_property_references() {
    // `<Import Project="$(SharedDir)/lib.props" />` is a common idiom
    // — substitution must happen *before* the path is resolved.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <SharedDir>shared</SharedDir>
  </PropertyGroup>
  <Import Project="$(SharedDir)/lib.props" />
  <ItemGroup>
    <Compile Include="$(LibValue).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "shared/lib.props",
        r#"<Project>
  <PropertyGroup>
    <LibValue>fromlib</LibValue>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("fromlib.fs")]);
    assert!(!result.is_partial);
}

#[test]
fn import_condition_exists_true_follows_import() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extra.props" Condition="Exists('extra.props')" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromImport.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(tmp.path()).join("FromImport.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn import_condition_exists_false_skips_import_cleanly() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extra.props" Condition="Exists('extra.props')" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(tmp.path()).join("A.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn import_condition_exists_trims_argument_before_probe() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <ImportPath> extra.props </ImportPath>
  </PropertyGroup>
  <Import Project="extra.props" Condition="Exists(' $(ImportPath) ')" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromImport.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(tmp.path()).join("FromImport.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn exists_in_imported_file_resolves_relative_to_imported_file() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="../shared/outer.props" />
</Project>"#,
    );
    write_at(
        &tmp.path().join("shared"),
        "outer.props",
        r#"<Project>
  <Import Project="inner.props" Condition="Exists('inner.props')" />
</Project>"#,
    );
    write_at(
        &tmp.path().join("shared"),
        "inner.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromInner.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(&tmp.path().join("project")).join("FromInner.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn item_condition_exists_in_imported_file_resolves_relative_to_project_dir() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="../shared/extra.props" />
</Project>"#,
    );
    write_at(&tmp.path().join("project"), "marker.txt", "");
    write_at(
        &tmp.path().join("shared"),
        "extra.props",
        r#"<Project>
  <ItemGroup Condition="Exists('marker.txt')">
    <Compile Include="FromImport.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(&tmp.path().join("project")).join("FromImport.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn item_condition_exists_in_imported_file_ignores_imported_file_sibling() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="../shared/extra.props" />
</Project>"#,
    );
    write_at(&tmp.path().join("shared"), "marker.txt", "");
    write_at(
        &tmp.path().join("shared"),
        "extra.props",
        r#"<Project>
  <ItemGroup Condition="Exists('marker.txt')">
    <Compile Include="FromImport.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(result.items.is_empty(), "{:?}", result.items);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn item_metadata_condition_exists_in_imported_file_resolves_relative_to_project_dir() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="../shared/extra.props" />
</Project>"#,
    );
    write_at(&tmp.path().join("project"), "marker.txt", "");
    write_at(
        &tmp.path().join("shared"),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs">
      <CompileOrder Condition="Exists('marker.txt')">CompileFirst</CompileOrder>
    </Compile>
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(&tmp.path().join("project"));
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("B.fs"), dir.join("A.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn item_metadata_condition_exists_in_imported_file_ignores_imported_file_sibling() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="../shared/extra.props" />
</Project>"#,
    );
    write_at(&tmp.path().join("shared"), "marker.txt", "");
    write_at(
        &tmp.path().join("shared"),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs">
      <CompileOrder Condition="Exists('marker.txt')">CompileFirst</CompileOrder>
    </Compile>
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(&tmp.path().join("project"));
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("A.fs"), dir.join("B.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn property_condition_exists_in_imported_file_resolves_relative_to_project_dir() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <FromImportedProperty>Fallback</FromImportedProperty>
  </PropertyGroup>
  <Import Project="../shared/extra.props" />
  <ItemGroup>
    <Compile Include="$(FromImportedProperty).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(&tmp.path().join("project"), "marker.txt", "");
    write_at(
        &tmp.path().join("shared"),
        "extra.props",
        r#"<Project>
  <PropertyGroup>
    <FromImportedProperty Condition="Exists('marker.txt')">Included</FromImportedProperty>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(&tmp.path().join("project")).join("Included.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn property_condition_exists_in_imported_file_ignores_imported_file_sibling() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        &tmp.path().join("project"),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <FromImportedProperty>Fallback</FromImportedProperty>
  </PropertyGroup>
  <Import Project="../shared/extra.props" />
  <ItemGroup>
    <Compile Include="$(FromImportedProperty).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(&tmp.path().join("shared"), "marker.txt", "");
    write_at(
        &tmp.path().join("shared"),
        "extra.props",
        r#"<Project>
  <PropertyGroup>
    <FromImportedProperty Condition="Exists('marker.txt')">Included</FromImportedProperty>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(&tmp.path().join("project")).join("Fallback.fs")]
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
fn import_relative_path_resolves_against_importer_not_project() {
    // sub/a.props has `<Import Project="b.props" />`. We must resolve
    // `b.props` against sub/, not the project directory — otherwise a
    // top-level b.props (if any) would shadow the sibling one. This
    // test puts a *different-valued* b.props at the project root to
    // catch the wrong resolution; the importer-relative one wins.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="sub/a.props" />
  <ItemGroup>
    <Compile Include="$(Whose).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "sub/a.props",
        r#"<Project>
  <Import Project="b.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "sub/b.props",
        r#"<Project>
  <PropertyGroup>
    <Whose>sibling</Whose>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "b.props",
        r#"<Project>
  <PropertyGroup>
    <Whose>root</Whose>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("sibling.fs")]);
}

#[test]
fn project_reference_in_imported_file_lands_in_bucket() {
    // ProjectReference items contributed by an explicit `<Import>`
    // must land in the same `project_references` bucket as those
    // declared in the project body — MSBuild's single linear
    // evaluation across all files.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../body/Body.csproj" />
  </ItemGroup>
  <Import Project="extra.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../imported/Imported.csproj" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.project_references),
        vec![
            dir.join("../body/Body.csproj"),
            dir.join("../imported/Imported.csproj"),
        ],
    );
    assert!(result.items.is_empty());
    assert!(!result.is_partial);
}

// --- Wildcard imports ---
//
// MSBuild expands wildcards in `<Import Project=...>` and silently skips
// the import when nothing matches (this is how the SDK's unconditional
// `Microsoft.VisualStudioVersion.v*.Common.props` import behaves on a
// machine with no VS-era files). Matches are imported in sorted order.

#[test]
fn wildcard_import_with_no_matches_is_silently_skipped() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extensions/v*.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("A.fs")]);
    assert!(
        result.diagnostics.is_empty(),
        "a zero-match wildcard import is not a failure; got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
    assert!(!result.is_partial);
}

#[test]
fn wildcard_import_with_missing_directory_is_silently_skipped() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="no-such-dir/*.props" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
    assert!(!result.is_partial);
}

#[test]
fn wildcard_import_walks_matches_in_sorted_order() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="ext/*.props" />
  <ItemGroup>
    <Compile Include="A.$(Suffix)" />
  </ItemGroup>
</Project>"#,
    );
    // Written out of order and with mixed case: MSBuild consumes
    // wildcard matches ordinal-ignore-case (verified against
    // `dotnet msbuild`: `a.props` imports before `B.props`, where byte
    // order would put `B` first), so the walk must visit a.props then
    // B.props — the later write wins and contributed items appear in
    // that order.
    write_at(
        tmp.path(),
        "ext/B.props",
        r#"<Project>
  <PropertyGroup>
    <Suffix>from-b</Suffix>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="FromB.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "ext/a.props",
        r#"<Project>
  <PropertyGroup>
    <Suffix>from-a</Suffix>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="FromA.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![
            dir.join("FromA.fs"),
            dir.join("FromB.fs"),
            dir.join("A.from-b"),
        ],
    );
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
}

#[test]
fn wildcard_import_respects_import_condition() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="ext/*.props" Condition="'$(Gate)' == 'true'" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "ext/a.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromA.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(result.items.is_empty());
}

#[test]
fn wildcard_in_directory_component_is_conservative() {
    // A wildcard *directory* component is real MSBuild syntax we don't
    // model; the import must surface as a structural skip (the dropped
    // file could have carried anything), not silently vanish.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="*/common.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "ext/common.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromExt.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "an unmodelled wildcard shape must stay conservative; diags: {:?}",
        result.diagnostics
    );
    assert!(
        result.is_partial,
        "the skip must be visible on the diagnostic surface"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedConstruct { .. }))
    );
}

#[test]
fn wildcard_import_over_a_file_is_silently_skipped() {
    // `file.txt/*.props`: the "directory" is a file, so the glob yields
    // nothing — MSBuild-silent, like any zero-match wildcard.
    let tmp = TempDir::new().unwrap();
    write_at(tmp.path(), "file.txt", "not a directory");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="file.txt/*.props" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics
    );
    assert!(!result.items_uncertain);
}

#[test]
#[cfg(unix)]
fn wildcard_import_over_unreadable_directory_is_conservative() {
    use std::os::unix::fs::PermissionsExt;
    // An unreadable directory is NOT "zero matches" — files may well be
    // there. The dropped imports could have carried anything, so the
    // walk must surface a structural skip rather than silently
    // continuing.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "ext/a.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromA.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="ext/*.props" />
</Project>"#,
    );
    let ext = tmp.path().join("ext");
    std::fs::set_permissions(&ext, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = parse_file(&project_path);
    std::fs::set_permissions(&ext, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        result.items_uncertain,
        "an unreadable wildcard directory must stay conservative; diags: {:?}",
        result.diagnostics
    );
    assert!(
        result.is_partial,
        "the skip must be visible on the diagnostic surface"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { .. }))
    );
}

// --- semicolon-separated `Project` lists ------------------------------------
//
// Pinned against dotnet msbuild 10.0.300 with stub projects (2026-07-09):
// the `Project` attribute is a semicolon-separated list; segments are
// whitespace-trimmed, empty segments are skipped (`";a.props"` imports
// a.props; `";"` alone is a silent no-op), files import left to right,
// and a missing non-wildcard segment fails the evaluation (MSB4019).
// The SDK relies on the empty-segment rule: `Sdk.props` appends to the
// possibly-empty `$(CustomAfterDirectoryBuildProps)` accumulator, so the
// imported value routinely starts with `;`.

#[test]
fn import_list_imports_each_file_in_order() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project=" a.props ; b.props " />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    write_at(
        tmp.path(),
        "b.props",
        r#"<Project><PropertyGroup><Order>$(Order)b</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[ab]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn import_list_skips_empty_segments() {
    // The accumulator idiom: the expanded value starts with `;` because
    // the prior accumulator value was (exactly) empty.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <Hooks>$(Hooks);a.props</Hooks>
  </PropertyGroup>
  <Import Project="$(Hooks)" Condition="'$(Hooks)' != ''" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[a]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn import_list_of_only_empty_segments_is_a_silent_no_op() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project=";" />
  <PropertyGroup>
    <R>ok</R>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("ok"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn import_list_with_missing_segment_degrades() {
    // MSBuild fails the whole evaluation (MSB4019); we import what we
    // can and record the failure conservatively.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props;missing.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><FromA>yes</FromA></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        result.properties.get("FromA").map(String::as_str),
        Some("yes")
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::ImportFailed { .. })),
        "{:?}",
        result.diagnostics
    );
    assert!(result.is_partial);
}

// --- duplicate imports -------------------------------------------------------
//
// MSBuild registers every performed import in a per-evaluation seen-set
// (`Evaluator._importsSeen`, `StringComparer.OrdinalIgnoreCase`) *before*
// evaluating the imported file's contents, and skips any later import that
// resolves to a seen path with warning MSB4011 (MSB4210 when the target is
// the entry project itself) — the evaluation succeeds and is *not* an error.
// The key is the lexically-normalised path: `.`/`..` collapse and the
// case-insensitive compare are string-level, with **no symlink resolution**,
// so a symlink alias of an already-imported file genuinely imports again.
// All pinned against dotnet msbuild 10.0.301 with stub projects (2026-07-12).

#[test]
fn import_list_with_duplicate_segment_imports_once() {
    // The review-motivating case: `Project="a.props;a.props"` must not
    // evaluate a.props twice — an accumulator property would diverge
    // from MSBuild ("aa" vs "a") while the result claimed to be exact.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props;a.props" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[a]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn repeated_import_elements_import_the_file_once() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props" />
  <Import Project="a.props" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[a]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn wildcard_segment_overlapping_a_literal_segment_imports_once() {
    // The wildcard's expansion of a.props hits the seen-set entry the
    // literal segment just registered.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props;*.props" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[a]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn differently_spelt_duplicate_imports_are_skipped() {
    // The seen-set key is the *lexically normalised* path: a `..`-bearing
    // respelling collapses to the seen entry, and the compare is
    // case-insensitive (`A.PROPS` dedups against `a.props` — MSBuild's
    // OrdinalIgnoreCase, which fires whether or not the filesystem is
    // case-insensitive: the check is string-level, before any file IO).
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props;sub/../a.props;A.PROPS" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[a]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[cfg(unix)]
#[test]
fn symlink_alias_of_an_imported_file_imports_again() {
    // The boundary of the dedup key's domain: MSBuild's seen-set compares
    // normalised *strings* and never resolves symlinks, so an alias of an
    // already-imported file is a distinct import and the file's body runs
    // twice (probed: `[aa]`, no warning). Deduping on the canonicalised
    // path would wrongly skip this and diverge while claiming exactness.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props;link.props" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    std::os::unix::fs::symlink(tmp.path().join("a.props"), tmp.path().join("link.props")).unwrap();
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[aa]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn non_ascii_near_duplicate_import_declines() {
    // `σ.props` then `Σ.props`: .NET's OrdinalIgnoreCase equates the pair
    // (probed on dotnet 10), but its non-ASCII casing table deviates from
    // Unicode's (`ı`≠`I`, `ſ`≠`S`), so we cannot reproduce its verdict in
    // general. A near-duplicate under the wider Unicode fold that is not an
    // ASCII-fold duplicate therefore *declines* — no silent skip (wrong if
    // MSBuild imports it), no second walk (wrong if MSBuild dedups it).
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        "<Project>\n  <Import Project=\"\u{3c3}.props;\u{3a3}.props\" />\n  <PropertyGroup>\n    <R>[$(Order)]</R>\n  </PropertyGroup>\n</Project>",
    );
    write_at(
        tmp.path(),
        "\u{3c3}.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    // The first spelling was walked once; the ambiguous respelling was
    // neither walked nor silently skipped.
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[a]"));
    assert!(
        result.is_partial,
        "an unresolvable near-duplicate must degrade: {:?}",
        result.diagnostics
    );
    assert!(
        result.items_uncertain,
        "the declined import could have carried items: {:?}",
        result.diagnostics
    );
}

#[test]
fn non_ascii_case_pair_that_dotnet_distinguishes_imports_both() {
    // `İ.props` (dotted capital I) and `i.props`: Unicode folds İ to
    // itself (its full uppercase is itself; its lowercase is `i` +
    // combining dot, not plain `i`), and .NET's ordinal table also keeps
    // the pair distinct (probed) — so both sides import both files and we
    // can commit that with certainty. Guards the fuzzy tier against
    // over-widening into declines for pairs that are genuinely distinct.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        "<Project>\n  <Import Project=\"\u{130}.props;i.props\" />\n  <PropertyGroup>\n    <R>[$(Order)]</R>\n  </PropertyGroup>\n</Project>",
    );
    write_at(
        tmp.path(),
        "\u{130}.props",
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#,
    );
    write_at(
        tmp.path(),
        "i.props",
        r#"<Project><PropertyGroup><Order>$(Order)b</Order></PropertyGroup></Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[ab]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn declined_mixed_wildcard_import_makes_later_undefined_reads_inexact() {
    // A `Project` with a *live* `*` alongside an escaped `%2a` (a literal
    // star) cannot be expressed as a glob, so the import is declined
    // (`UnsupportedGlob`). MSBuild, though, may import a real file named
    // `*.props` here, and that file could define anything — so the decline
    // must latch walk opacity (C.2b). The proof: a *later* import gated on a
    // plain undefined property, which would otherwise read exactly False and
    // be a clean exclusion (see
    // `user_import_gated_on_exact_undefined_property_is_not_items_uncertain`),
    // must now read as inexact and mark the item set uncertain.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="*.props%2a" />
  <Import Project="extra.props" Condition="'$(IncludeExtra)' == 'true'" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UnsupportedGlob { .. })),
        "the mixed live/escaped wildcard import is declined: {:?}",
        result.diagnostics
    );
    assert!(
        result.items_uncertain,
        "the declined import left the walk opaque, so the later undefined \
         import gate can no longer be proven exactly False: {:?}",
        result.diagnostics
    );
}

#[test]
fn empty_import_project_value_is_an_error_not_a_silent_noop() {
    // MSBuild errors (MSB4035/MSB4020) on an empty or whitespace-only *whole*
    // `Project` value — probed: `dotnet msbuild -getProperty` on
    // `<Import Project="$(Empty)"/>` exits 1. It is not the silent no-op a
    // separator-bearing list is, so we degrade it structurally (the item set
    // is uncertain) rather than report a clean, fully-evaluated project.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Empty.fsproj",
        r#"<Project>
  <Import Project="" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "an empty import path is an MSBuild error, not a clean no-op: {:?}",
        result.diagnostics
    );
    // `is_partial` derives solely from `diagnostics`, so the degrade must
    // push one: without it the result would claim full fidelity (and the
    // LSP would surface nothing) for a project MSBuild refuses to
    // evaluate at all.
    assert!(
        result.is_partial,
        "an MSBuild-fatal import shape must not report a non-partial \
         result: {:?}",
        result.diagnostics
    );

    // The same via a *cleanly-evaluated* empty expansion: an exact
    // undefined read (C.2b) substitutes "" without any diagnostic of its
    // own, so this variant only degrades if the empty-path branch pushes
    // one itself.
    let missing = write_at(
        tmp.path(),
        "Missing.fsproj",
        r#"<Project>
  <Import Project="$(Missing)" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&missing);
    assert!(
        result.items_uncertain && result.is_partial,
        "an exact-undefined expansion to an empty import path is still an \
         MSBuild error: {:?}",
        result.diagnostics
    );

    // The contrast the split is built around: a separator-bearing list whose
    // entries are all empty (`";"`, the SDK's possibly-empty
    // `$(CustomAfterDirectoryBuildProps)` accumulator) *is* a silent no-op.
    let noop = write_at(
        tmp.path(),
        "Noop.fsproj",
        r#"<Project>
  <Import Project=";" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&noop);
    assert!(
        !result.items_uncertain,
        "a separator-only import list imports nothing without erroring: {:?}",
        result.diagnostics
    );
}
