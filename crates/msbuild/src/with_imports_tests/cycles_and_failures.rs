//! Import failure and abort paths — missing/malformed files, an unsupported
//! `Sdk` attribute on `<Import>`, the depth-limit guard — plus the
//! *non*-failure cyclic cases: MSBuild cuts an import cycle with the same
//! duplicate-import skip it applies to any re-import (warning MSB4011, or
//! MSB4210 for the entry project) and the evaluation succeeds, so ours must
//! too. Pinned against dotnet msbuild 10.0.301 with stub projects
//! (2026-07-12).

use super::*;
use tempfile::TempDir;

#[test]
fn cycle_between_two_files_is_cut_by_the_duplicate_import_skip() {
    // a.props and b.props import each other. MSBuild registers each
    // import before walking the file, so b's import of a hits the
    // seen-set: both bodies run exactly once, in import order, and the
    // evaluation is clean (probed: `[ab]`, exit 0).
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="a.props" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project>
  <PropertyGroup><Order>$(Order)a</Order></PropertyGroup>
  <Import Project="b.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "b.props",
        r#"<Project>
  <PropertyGroup><Order>$(Order)b</Order></PropertyGroup>
  <Import Project="a.props" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[ab]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn import_of_the_entry_project_is_skipped() {
    // Entry-project cycle: project imports a.props which imports the
    // entry project back. MSBuild skips it (warning MSB4210, "attempting
    // to import itself, directly or indirectly") and succeeds — probed:
    // `[xa]`. The seen-set's seed (the entry project's path) is what
    // catches this; without it the walker would run the entry body twice.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup><Order>$(Order)x</Order></PropertyGroup>
  <Import Project="a.props" />
  <PropertyGroup>
    <R>[$(Order)]</R>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "a.props",
        r#"<Project>
  <PropertyGroup><Order>$(Order)a</Order></PropertyGroup>
  <Import Project="Demo.fsproj" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(result.properties.get("R").map(String::as_str), Some("[xa]"));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(!result.is_partial);
}

#[test]
fn missing_imported_file_reports_not_found() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="does-not-exist.props" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    let any_not_found = result.diagnostics.iter().any(|d| {
        matches!(
            d.kind,
            DiagnosticKind::ImportFailed {
                reason: ImportFailReason::NotFound,
                ..
            }
        )
    });
    assert!(
        any_not_found,
        "expected a NotFound diagnostic, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn malformed_imported_xml_reports_malformed_xml() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="broken.props" />
</Project>"#,
    );
    write_at(
        tmp.path(),
        "broken.props",
        // Unbalanced XML — element opened, not closed.
        "<Project><PropertyGroup>",
    );
    let result = parse_file(&project_path);
    let any_malformed = result.diagnostics.iter().any(|d| {
        matches!(
            d.kind,
            DiagnosticKind::ImportFailed {
                reason: ImportFailReason::MalformedXml { .. },
                ..
            }
        )
    });
    assert!(
        any_malformed,
        "expected a MalformedXml diagnostic, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn import_with_sdk_attribute_reports_unsupported_construct() {
    // Sdk-aware imports need MSBuild's SDK resolver to locate the
    // SDK on disk; that's a phase-7b concern. The walker must refuse
    // to follow such imports and surface them as unsupported rather
    // than try (and fail) to resolve `Sdk.props` as a relative path.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="Sdk.props" Sdk="Microsoft.NET.Sdk" />
</Project>"#,
    );
    let result = parse_file(&project_path);
    let any_unsupported = result.diagnostics.iter().any(|d| {
        matches!(d.kind, DiagnosticKind::UnsupportedConstruct { ref element } if element.contains("Sdk"))
    });
    assert!(
        any_unsupported,
        "expected an UnsupportedConstruct with 'Sdk', got: {:?}",
        result.diagnostics
    );
    assert!(result.is_partial);
}

#[test]
fn depth_limit_aborts_pathological_chain() {
    // Generate a chain of 200 imports — well above the MAX_IMPORT_DEPTH
    // of 64. Without the depth guard this would either stack-overflow
    // or exhaust resources reading 200 separate files; with it, the
    // walk emits a `DepthLimit` diagnostic and returns cleanly.
    let tmp = TempDir::new().unwrap();
    let depth = 200;
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="link0.props" />
</Project>"#,
    );
    for i in 0..depth - 1 {
        write_at(
            tmp.path(),
            &format!("link{i}.props"),
            &format!(
                r#"<Project>
  <Import Project="link{}.props" />
</Project>"#,
                i + 1
            ),
        );
    }
    // Final link is a leaf — chain terminates if the walker actually
    // reaches it (it shouldn't).
    write_at(
        tmp.path(),
        &format!("link{}.props", depth - 1),
        "<Project />",
    );
    let result = parse_file(&project_path);
    let any_depth = result.diagnostics.iter().any(|d| {
        matches!(
            d.kind,
            DiagnosticKind::ImportFailed {
                reason: ImportFailReason::DepthLimit { .. },
                ..
            }
        )
    });
    assert!(
        any_depth,
        "expected a DepthLimit diagnostic for a {depth}-deep chain, got: {:?}",
        result.diagnostics
    );
    assert!(result.is_partial);
}
