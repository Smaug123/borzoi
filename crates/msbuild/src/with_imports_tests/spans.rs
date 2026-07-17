//! Diagnostic and item span attribution for content contributed by
//! imported files, plus the base directory used for include-path
//! resolution.

use super::*;
use tempfile::TempDir;

#[test]
fn diagnostic_in_imported_file_uses_import_site_span() {
    // Public contract: `Diagnostic::span` is a byte offset into the
    // source the caller handed in. When a diagnostic originates
    // inside an imported file, returning `node.range()` from that
    // file's buffer would point at bytes the caller can't index. We
    // collapse all imported-file spans to the top-level `<Import>`
    // element in the entry project — that's the closest legal
    // entry-source byte range for the issue. (The vehicle reads
    // `$(TargetFramework)` — the carve-out that stays inexact under
    // C.2b — because a plain undefined name now reads exactly empty
    // and would raise no diagnostic at all.)
    let tmp = TempDir::new().unwrap();
    let source = r#"<Project>
  <Import Project="extra.props" />
</Project>"#;
    let project_path = write_at(tmp.path(), "Demo.fsproj", source);
    write_at(
        tmp.path(),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(TargetFramework).fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse(&project_path, source);
    let undef: Vec<&_> = result
        .diagnostics
        .iter()
        .filter(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. }))
        .collect();
    assert_eq!(
        undef.len(),
        1,
        "expected exactly one UndefinedProperty, got: {:?}",
        result.diagnostics
    );
    let import_start = source.find("<Import").expect("Import in entry source");
    let import_end = source
        .find("/>")
        .map(|i| i + 2)
        .expect("import end in entry source");
    let span = undef[0].span.clone();
    assert_eq!(
        span,
        import_start..import_end,
        "imported-file diagnostic must point at the entry project's <Import> element"
    );
    // And the span must be within the entry source's bounds — the
    // contract this test exists to enforce.
    assert!(span.end <= source.len());
}

#[test]
fn item_from_imported_file_uses_import_site_span() {
    // Same contract for `ResolvedItem::span`. A `<Compile>` declared
    // inside an imported props file must carry a span that's valid
    // in the entry project's buffer — not the imported buffer.
    let tmp = TempDir::new().unwrap();
    let source = r#"<Project>
  <Import Project="extra.props" />
</Project>"#;
    let project_path = write_at(tmp.path(), "Demo.fsproj", source);
    write_at(
        tmp.path(),
        "extra.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromImport.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse(&project_path, source);
    assert_eq!(result.items.len(), 1);
    let import_start = source.find("<Import").expect("Import in entry source");
    let import_end = source
        .find("/>")
        .map(|i| i + 2)
        .expect("import end in entry source");
    let span = result.items[0].span.clone();
    assert_eq!(
        span,
        import_start..import_end,
        "imported Compile item must carry the entry-project <Import> span"
    );
    assert!(span.end <= source.len());
}

#[test]
fn imported_file_compile_include_resolves_against_entry_project_dir() {
    // MSBuild rule: an unqualified `<Compile Include="Generated.fs" />`
    // appearing *inside an imported props file* resolves against
    // `$(MSBuildProjectDirectory)` (the entry project), NOT the
    // importing file's directory. (The `$(MSBuildThisFileDirectory)`
    // prefix is the documented opt-in for "use my own folder".) A
    // previous version of the walker rebound the base directory to
    // the imported file, so we'd have produced `imp/Generated.fs`
    // here instead of the entry-project-relative `Generated.fs` — a
    // silent divergence from MSBuild that would mis-order Compile
    // items in any real project using a shared `Common.props`.
    let tmp = TempDir::new().unwrap();
    let imp = tmp.path().join("imp");
    std::fs::create_dir(&imp).unwrap();
    write_at(
        &imp,
        "Common.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Generated.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="imp/Common.props" />
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let canon_root = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![canon_root.join("Generated.fs"), canon_root.join("Main.fs")],
        "imported Compile must resolve against entry project dir, not imp/",
    );
}

#[test]
fn imported_file_compile_with_this_file_directory_uses_import_folder() {
    // The MSBuildThisFileDirectory opt-in: an imported file *can*
    // refer to its own folder, but only by prefixing the Include with
    // `$(MSBuildThisFileDirectory)`. That property is rebound to the
    // imported file's directory while we walk it (see
    // `State::enter_this_file`), so the result here must point inside
    // `imp/`.
    let tmp = TempDir::new().unwrap();
    let imp = tmp.path().join("imp");
    std::fs::create_dir(&imp).unwrap();
    write_at(
        &imp,
        "Common.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(MSBuildThisFileDirectory)Generated.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="imp/Common.props" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    let canon_imp = canon(&imp);
    assert_eq!(
        paths_of(&result.items),
        vec![canon_imp.join("Generated.fs")]
    );
}

#[test]
fn root_sdk_attribute_emits_unsupported_construct() {
    // Phase 7a doesn't resolve SDKs. A `<Project Sdk="...">` is
    // shorthand for `<Import Project="Sdk.props" Sdk="..." />` before
    // the body and `<Import Project="Sdk.targets" Sdk="..." />` after
    // — both of which we already flag as UnsupportedConstruct when
    // they appear explicitly. Without flagging the root form,
    // SDK-style projects that rely on default Compile globs would
    // parse as `is_partial == false` with an empty item list,
    // silently disagreeing with MSBuild.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="Only.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let has_sdk_diag = result.diagnostics.iter().any(|d| {
        matches!(
            &d.kind,
            DiagnosticKind::UnsupportedConstruct { element }
                if element == "Project Sdk=\"Microsoft.NET.Sdk\""
        )
    });
    assert!(
        has_sdk_diag,
        "expected UnsupportedConstruct for root Sdk attribute, got: {:?}",
        result.diagnostics
    );
    assert!(result.is_partial);
}
