//! Splicing for nested SDK roots declared by imported files, and their
//! interaction with deferred `Directory.Build.props`.

use super::*;
use tempfile::TempDir;

// -------------------------------------------------------------------------
// Phase 7b-v1c: splicing for nested SDK roots inside imported files
// -------------------------------------------------------------------------

#[test]
fn nested_sdk_props_runs_before_imported_file_body() {
    // An imported file that declares `<Project Sdk="X">` should get
    // the same splice as a root-Sdk entry project: `Sdk.props` runs
    // before the imported file's body, so a property the SDK sets is
    // visible to a `$(...)` reference inside the imported body.
    // Without nested-SDK splicing the body's `$(SdkSeed)` would be
    // undefined and we'd produce a garbage item path.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <SdkSeed>fromprops</SdkSeed>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="$(SdkSeed).fs" />
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
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("fromprops.fs")]);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "$(SdkSeed) should resolve from nested Sdk.props, no UndefinedProperty expected: {:?}",
        result.diagnostics,
    );
}

#[test]
fn nested_sdk_targets_runs_after_imported_file_body() {
    // The other half of the splice: `Sdk.targets` of a nested SDK
    // root runs *after* the imported file's body. Items it
    // contributes therefore appear later in document order than
    // items the imported body itself declares. Pins the
    // [body, targets] ordering — without targets-splicing we'd lose
    // `last.fs` entirely; with mis-ordered splicing we'd see it
    // before the body's items.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <ItemGroup>
    <Compile Include="last.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
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
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("nested-body.fs"), dir.join("last.fs")],
    );
}

#[test]
fn nested_sdk_items_carry_entry_import_site_span() {
    // Items contributed by a nested SDK's `Sdk.props` must collapse
    // to the *entry* project's `<Import>` element span — that's the
    // standing import-site span contract for any item coming from an
    // imported file or any file transitively reached from it. Without
    // it, the LSP would point users at a phantom span in a file they
    // can't edit.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-sdk-props.fs" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    write_at(tmp.path(), "nested.targets", r#"<Project Sdk="MySdk" />"#);
    let project_source = r#"<Project>
  <Import Project="nested.targets" />
</Project>"#;
    let project_path = write_at(tmp.path(), "Demo.fsproj", project_source);
    let result = parse_with_sdk(&project_path, project_source, |name| {
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
    let import_start = project_source.find("<Import").unwrap();
    let import_end = project_source[import_start..].find("/>").unwrap() + import_start + 2;
    let item = result
        .items
        .iter()
        .find(|i| {
            i.include
                .file_name()
                .is_some_and(|n| n == "from-sdk-props.fs")
        })
        .expect("from-sdk-props.fs should appear in items");
    assert_eq!(
        item.span,
        import_start..import_end,
        "nested SDK items must collapse to the entry project's <Import> site",
    );
}

#[test]
fn nested_sdk_resolver_error_surfaces_as_sdk_not_found() {
    // The new path routes nested SDK resolution through the same
    // `resolve_project_sdk` the entry project uses, so resolver-side
    // errors map to the same diagnostic kinds. Previously we just
    // diagnosed *any* SDK attribute on an imported root as
    // `UnsupportedConstruct`, losing the distinction between "we
    // don't know how to resolve SDKs" and "the resolver looked and
    // it isn't there". Witness that an `Err(NotFound)` from the
    // resolver now reaches the caller as `SdkNotFound` with origin
    // `Imported` (so LSP consumers can suppress it if they choose).
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="DoesNotExist" />"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="nested.targets" />
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |_name| Err(SdkResolveError::NotFound));
    let sdk_not_found: Vec<_> = result
        .diagnostics
        .iter()
        .filter(
            |d| matches!(&d.kind, DiagnosticKind::SdkNotFound { name } if name == "DoesNotExist"),
        )
        .collect();
    assert_eq!(
        sdk_not_found.len(),
        1,
        "expected exactly one SdkNotFound for the nested SDK, got: {:?}",
        result.diagnostics,
    );
    assert_eq!(
        sdk_not_found[0].origin,
        DiagnosticOrigin::Imported,
        "diagnostic from a nested file must carry Imported origin",
    );
    assert!(result.is_partial);
}

#[test]
fn nested_sdk_does_not_double_splice_directory_build_props() {
    // Regression guard: the Directory.Build.{props,targets} splice is
    // an entry-project-only concern (MSBuild walks ancestor dirs once
    // from the entry project's location). When `walk_external_file`
    // gained nested-SDK splicing it must NOT have also gained
    // Directory.Build splicing — that would double-import the file
    // and double-count any items it contributes.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(tmp.path(), "nested.targets", r#"<Project Sdk="MySdk" />"#);
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="nested.targets" />
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
    let dirbuild_count = result
        .items
        .iter()
        .filter(|i| {
            i.include
                .file_name()
                .is_some_and(|n| n == "from-dirbuild.fs")
        })
        .count();
    assert_eq!(
        dirbuild_count,
        1,
        "Directory.Build.props should walk exactly once (entry-project splice only), got: {:?}",
        paths_of(&result.items),
    );
}

#[test]
fn entry_no_sdk_nested_sdk_props_visible_to_directory_build_props() {
    // THE fix. MSBuild imports `Directory.Build.props` exactly once,
    // right after the *first* `Sdk.props` to run. When the entry
    // project has no resolvable SDK but a nested imported file does,
    // that first `Sdk.props` is the nested one (mid-body), so
    // `Directory.Build.props` must be spliced *after* it — a
    // `Directory.Build.props` that conditions on (or substitutes) a
    // property the nested SDK sets should see it. Before the deferred
    // splice we always ran `Directory.Build.props` before the body, so
    // `$(NestedFlag)` was undefined there and we produced `.fs`.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <NestedFlag>yes</NestedFlag>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <Saw>$(NestedFlag)</Saw>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Saw).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
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
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    assert!(
        paths.contains(&dir.join("yes.fs")),
        "Directory.Build.props must run after the nested Sdk.props so $(NestedFlag) is visible; got: {paths:?}",
    );
    assert!(
        !paths.contains(&dir.join(".fs")),
        "an empty $(Saw) (`.fs`) means Directory.Build.props ran before the nested Sdk.props; got: {paths:?}",
    );
}

#[test]
fn entry_no_sdk_no_nested_sdk_keeps_directory_build_props_before_body() {
    // Pass-1 path preserved: with no entry SDK and no nested SDK firing,
    // `Directory.Build.props` still splices before the entry body, so
    // its items precede the body's. (No deferral when nothing nested
    // fires.)
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
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
    // Resolver present but irrelevant: there is no SDK anywhere.
    let result = parse_file_with_sdk(&project_path, |_name| Err(SdkResolveError::NotFound));
    let dir = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![dir.join("from-dirbuild.fs"), dir.join("body.fs")],
        "Directory.Build.props items must precede the body when no nested SDK fires",
    );
}

#[test]
fn nested_sdk_deferred_directory_build_props_imported_once() {
    // The deferred splice must still fire exactly once — `take()` on the
    // pending splice guarantees MSBuild's single import even though the
    // first nested `Sdk.props` is what triggers it. Sentinel item from
    // `Directory.Build.props` must appear exactly once.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup>
    <NestedFlag>yes</NestedFlag>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
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
    let dirbuild_count = result
        .items
        .iter()
        .filter(|i| {
            i.include
                .file_name()
                .is_some_and(|n| n == "from-dirbuild.fs")
        })
        .count();
    assert_eq!(
        dirbuild_count,
        1,
        "deferred Directory.Build.props must still import exactly once, got: {:?}",
        paths_of(&result.items),
    );
}

#[test]
fn nested_sdk_deferred_dbp_falls_back_when_path_gated_on_dbp_property() {
    // Pathological dangle case. The body's import of the nested-SDK file
    // is gated on a property that *only* `Directory.Build.props` sets.
    // In pass 1 (before-body splice) the gate is satisfied, the nested
    // SDK fires, and we detect divergence. But in pass 2 (deferred) the
    // gate is unsatisfied — the import never fires, so the pending
    // splice dangles. The orchestrator must fall back to pass 1 rather
    // than drop `Directory.Build.props` entirely. We witness the
    // fallback by the nested body item being present: had pass 2 been
    // returned, the gated import would not have fired and
    // `nested-body.fs` would be absent.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <FromDbp>1</FromDbp>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="nested.targets" Condition="'$(FromDbp)' == '1'" />
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
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    assert!(
        paths.contains(&dir.join("nested-body.fs")),
        "fallback to pass 1: the gated import fires once Directory.Build.props sets $(FromDbp); got: {paths:?}",
    );
    assert!(
        paths.contains(&dir.join("from-dirbuild.fs")),
        "Directory.Build.props must not be dropped on the dangle fallback; got: {paths:?}",
    );
}

#[test]
fn entry_with_sdk_and_nested_sdk_splices_directory_build_props_once_after_entry() {
    // Common-path regression guard. When the entry project *has* an SDK,
    // its `Sdk.props` is the first to run, so `Directory.Build.props`
    // already splices in the right place (after the entry Sdk.props,
    // before the body). The presence of a nested SDK deeper in the body
    // must not trigger any repositioning or double-import.
    let tmp = TempDir::new().unwrap();
    let (entry_root, entry_props, entry_targets) = write_synthetic_sdk(
        tmp.path(),
        "EntrySdk",
        r#"<Project>
  <ItemGroup>
    <Compile Include="entry-props.fs" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let (nested_root, nested_props, nested_targets) =
        write_synthetic_sdk(tmp.path(), "NestedSdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="NestedSdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="EntrySdk">
  <ItemGroup>
    <Compile Include="body.fs" />
  </ItemGroup>
  <Import Project="nested.targets" />
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, move |name| match name {
        "EntrySdk" => Ok(SdkPaths {
            root: entry_root.clone(),
            props: entry_props.clone(),
            targets: entry_targets.clone(),
        }),
        "NestedSdk" => Ok(SdkPaths {
            root: nested_root.clone(),
            props: nested_props.clone(),
            targets: nested_targets.clone(),
        }),
        _ => Err(SdkResolveError::NotFound),
    });
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    let idx = |name: &str| {
        paths
            .iter()
            .position(|p| p == &dir.join(name))
            .unwrap_or_else(|| panic!("{name} missing from {paths:?}"))
    };
    assert_eq!(
        paths
            .iter()
            .filter(|p| *p == &dir.join("from-dirbuild.fs"))
            .count(),
        1,
        "Directory.Build.props must import exactly once; got: {paths:?}",
    );
    assert!(
        idx("entry-props.fs") < idx("from-dirbuild.fs"),
        "entry Sdk.props must precede Directory.Build.props; got: {paths:?}",
    );
    assert!(
        idx("from-dirbuild.fs") < idx("body.fs"),
        "Directory.Build.props must precede the entry body; got: {paths:?}",
    );
}

#[test]
fn nested_sdk_deferred_dbp_gate_reevaluated_after_body_property() {
    // MSBuild evaluates the `ImportDirectoryBuildProps` gate at the point
    // it imports `Directory.Build.props` — for the entry-no-SDK shape that
    // is the first nested `Sdk.props`, *after* the entry body has run. A
    // body `<PropertyGroup>` that sets `ImportDirectoryBuildProps=false`
    // before the nested SDK import must therefore suppress the deferred
    // import. The deferred splice must re-read the gate against live state
    // at the fire point, not reuse the before-body value.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <ImportDirectoryBuildProps>false</ImportDirectoryBuildProps>
  </PropertyGroup>
  <Import Project="nested.targets" />
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
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    assert!(
        paths.contains(&dir.join("nested-body.fs")),
        "the nested SDK import still fires; got: {paths:?}",
    );
    assert!(
        !paths.contains(&dir.join("from-dirbuild.fs")),
        "ImportDirectoryBuildProps=false set before the nested SDK must \
         suppress the deferred Directory.Build.props; got: {paths:?}",
    );
}

#[test]
fn nested_sdk_deferred_dbp_path_override_reevaluated_after_body_property() {
    // The `DirectoryBuildPropsPath` redirect is likewise re-resolved at
    // the deferred fire point. A body `<PropertyGroup>` that redirects it
    // before the nested SDK import must make the deferred splice import
    // the redirect target, not the nearest-ancestor fallback captured
    // before the body ran.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    // Nearest-ancestor fallback (would be imported if we used the
    // stale before-body resolution).
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-fallback.fs" />
  </ItemGroup>
</Project>"#,
    );
    // The redirect target the body points at.
    let alt = tmp.path().join("alt");
    std::fs::create_dir_all(&alt).unwrap();
    write_at(
        &alt,
        "Custom.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-custom.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildPropsPath>$(MSBuildThisFileDirectory)alt/Custom.props</DirectoryBuildPropsPath>
  </PropertyGroup>
  <Import Project="nested.targets" />
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
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    assert!(
        paths.contains(&dir.join("from-custom.fs")),
        "the body's DirectoryBuildPropsPath redirect must be honoured at the deferred fire point; got: {paths:?}",
    );
    assert!(
        !paths.contains(&dir.join("from-fallback.fs")),
        "the stale before-body fallback must not be imported once the body redirects the path; got: {paths:?}",
    );
}

#[test]
fn entry_no_sdk_body_directory_build_props_path_override_imported_at_nested_sdk() {
    // Regression for the orchestrator short-circuit: when the entry has
    // no SDK and there is *no* on-disk `Directory.Build.props`, pass 1's
    // eager before-body resolution finds nothing to import (the body
    // `DirectoryBuildPropsPath` override has not run yet, and there is no
    // nearest-ancestor fallback). The deferred second pass must still run
    // so the body-set override is honoured at the nested `Sdk.props`
    // point — exactly where MSBuild performs the import. Gating the
    // second pass on "pass 1 imported a props file" would drop the
    // override entirely.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    // The redirect target the body points at. Note: there is deliberately
    // *no* `Directory.Build.props` on disk, so pass 1 imports nothing.
    let alt = tmp.path().join("alt");
    std::fs::create_dir_all(&alt).unwrap();
    write_at(
        &alt,
        "Custom.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="from-custom.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildPropsPath>$(MSBuildThisFileDirectory)alt/Custom.props</DirectoryBuildPropsPath>
  </PropertyGroup>
  <Import Project="nested.targets" />
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
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    assert!(
        paths.contains(&dir.join("from-custom.fs")),
        "the body's DirectoryBuildPropsPath override must be imported at the nested Sdk.props point even with no on-disk Directory.Build.props; got: {paths:?}",
    );
    assert!(
        paths.contains(&dir.join("nested-body.fs")),
        "the nested SDK body items must still be present; got: {paths:?}",
    );
}

#[test]
fn directory_build_props_cannot_suppress_nested_sdk_detection() {
    // Pass 1's eager before-body `Directory.Build.props` splice must not
    // be trusted to decide whether a nested `Sdk.props` fires. Here
    // `Directory.Build.props` sets `SkipNested=true` and the body's only
    // nested-SDK import is gated on `'$(SkipNested)' != 'true'`. Under
    // pass 1's (wrong) eager order that property is already set, so the
    // import is suppressed and a pass-1 detection would conclude "no
    // nested SDK fired" — wrongly returning the before-body result. Under
    // MSBuild's real order (modelled by pass 2) the condition is
    // evaluated *before* `Directory.Build.props` runs, so `SkipNested` is
    // unset, the import fires, and the nested `Sdk.props` runs. The
    // orchestrator must therefore always try pass 2 for an entry with no
    // SDK and decide by whether the deferred splice dangles, never by
    // pass 1's contaminated detection.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <SkipNested>true</SkipNested>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="from-dirbuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "nested.targets",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="nested-body.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="nested.targets" Condition="'$(SkipNested)' != 'true'" />
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
    let dir = canon(tmp.path());
    let paths = paths_of(&result.items);
    assert!(
        paths.contains(&dir.join("nested-body.fs")),
        "the nested import must fire under the faithful order: its condition is evaluated before Directory.Build.props sets SkipNested; got: {paths:?}",
    );
    assert!(
        paths.contains(&dir.join("from-dirbuild.fs")),
        "Directory.Build.props must still contribute, fired at the nested Sdk.props; got: {paths:?}",
    );
}
