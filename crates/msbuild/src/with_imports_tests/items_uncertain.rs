//! SDK-provenance for `items_uncertain`: a Compile-affecting condition in the
//! SDK's *own* targets/props is tolerated (it's the default-item machinery
//! every project carries), while the same construct in a user-authored import
//! (`Directory.Build.props`, an explicit `<Import>`) is respected — the
//! Compile-set-trustworthiness distinction that lets real SDK projects resolve.

use super::*;
use crate::CompileConditionReason;
use tempfile::TempDir;

#[test]
fn conditional_compile_inside_the_sdk_is_tolerated() {
    // The SDK's `Sdk.props` pulls in a sibling props that gates a `<Compile>`
    // on a property the walk can't decide. Since C.2b a *plain* undefined
    // name (e.g. `EnableDefaultItems`) reads exactly empty and the gate
    // decides cleanly, so we gate on `TargetFramework` — the consumer-contract
    // carve-out that stays inexact (the realistic multi-TFM default-item
    // shape). It lives under the SDK installation tree, so it must NOT make
    // the Compile set uncertain: the entry project's explicit list is what
    // compiles.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        // Sdk.props imports a sibling that adds a conditional default-item.
        r#"<Project>
  <Import Project="DefaultItems.props" />
</Project>"#,
        "<Project/>",
    );
    // The conditional-compile props lives alongside Sdk.props, i.e. under the
    // SDK tolerance root (the SDK root's parent).
    write_at(
        &root,
        "DefaultItems.props",
        r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="**/*.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="A.fs" />
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
    // The SDK's conditional default-item is tolerated.
    assert!(
        !result.items_uncertain,
        "SDK-internal conditional Compile must not flag items_uncertain; diags: {:?}",
        result.diagnostics
    );
    assert!(result.compile_condition_uncertainties.is_empty());
    // (The inexact `TargetFramework` read still flips the broad `is_partial`.)
    assert!(result.is_partial);
}

#[test]
fn compile_metadata_update_inside_the_sdk_is_tolerated() {
    // The .NET SDK's default-items targets use this exact shape to set Link
    // metadata for Compile items outside the project directory. It does not
    // add, remove, or reorder Compile items, so treating it as an unsupported
    // item operation makes the corpus runner distrust otherwise-known projects.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        "<Project/>",
        r#"<Project>
  <ItemGroup Condition="'$(SetLinkMetadataAutomatically)' != 'false'">
    <Compile Update="@(Compile)">
      <Link>%(Filename)%(Extension)</Link>
    </Compile>
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(tmp.path(), "A.fs", "");
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
        paths_of(&result.items),
        vec![canon(&tmp.path().join("A.fs"))]
    );
    assert!(
        !result.items_uncertain,
        "metadata-only Compile Update must not flag items_uncertain; diags: {:?}",
        result.diagnostics
    );
    assert!(result.compile_condition_uncertainties.is_empty());
    assert!(
        !result.diagnostics.iter().any(|diag| matches!(
            &diag.kind,
            DiagnosticKind::UnsupportedItemOperation { operation }
                if operation == "Update=@(Compile)"
        )),
        "metadata-only Compile Update should not be diagnosed as unsupported: {:?}",
        result.diagnostics
    );
}

#[test]
fn shared_dotnet_sdk_version_files_are_tolerated() {
    // Microsoft.NET.Sdk's `Sdk.props` enters shared files under the SDK version
    // directory (`Current/Microsoft.Common.props`, language targets, etc.), not
    // only files under `Sdks/Microsoft.NET.Sdk`. Import gates there are still
    // SDK machinery and must not make the entry project's Compile set
    // uncertain. (The gate reads `TargetFramework` — the carve-out that stays
    // inexact under C.2b — so the import genuinely stays undecidable and the
    // tolerance machinery is what's exercised.)
    let tmp = TempDir::new().unwrap();
    let version_dir = tmp.path().join("dotnet").join("sdk").join("1.2.300");
    let root = version_dir.join("Sdks").join("MySdk").join("Sdk");
    let props = write_at(
        &root,
        "Sdk.props",
        r#"<Project>
  <Import Project="../../../Current/Microsoft.Common.props" />
</Project>"#,
    );
    let targets = write_at(&root, "Sdk.targets", "<Project/>");
    write_at(
        &version_dir,
        "Current/Microsoft.Common.props",
        r#"<Project>
  <Import Project="$(GeneratedTargets)" Condition="'$(TargetFramework)' == 'net8.0'" />
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="A.fs" />
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
    assert!(
        !result.items_uncertain,
        "shared SDK-version files must be tolerated; diags: {:?}",
        result.diagnostics
    );
    // The broad parse is still partial because we can't decide the gate's
    // `TargetFramework` read, but that partiality is not Compile-set
    // uncertainty.
    assert!(result.is_partial);
}

#[test]
fn conditional_compile_in_a_base_sdk_imported_by_a_variant_is_tolerated() {
    // SDK variant case (`Microsoft.NET.Sdk.Web` → base `Microsoft.NET.Sdk`):
    // the entry SDK's `Sdk.props` does `<Import Sdk="Base">`, whose files live
    // under a *sibling* directory not covered by the entry SDK's own tolerance
    // root. The base SDK's conditional default-item must still be tolerated.
    let tmp = TempDir::new().unwrap();
    let sdks = tmp.path().join("sdks");
    // Real layout: …/Sdks/<name>/Sdk/Sdk.props, so each SDK's tolerance root
    // (its parent) is a distinct …/Sdks/<name> dir.
    let web_root = sdks.join("Web").join("Sdk");
    let base_root = sdks.join("Base").join("Sdk");
    write_at(
        &web_root,
        "Sdk.props",
        r#"<Project>
  <Import Sdk="Base" Project="Sdk.props" />
</Project>"#,
    );
    write_at(&web_root, "Sdk.targets", "<Project/>");
    // The base SDK adds a conditional default-item, in its OWN tree.
    write_at(
        &base_root,
        "Sdk.props",
        r#"<Project>
  <ItemGroup Condition="'$(EnableDefaultItems)' == 'true'">
    <Compile Include="**/*.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(&base_root, "Sdk.targets", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Web">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, move |name| match name {
        "Web" => Ok(SdkPaths {
            root: web_root.clone(),
            props: web_root.join("Sdk.props"),
            targets: web_root.join("Sdk.targets"),
        }),
        "Base" => Ok(SdkPaths {
            root: base_root.clone(),
            props: base_root.join("Sdk.props"),
            targets: base_root.join("Sdk.targets"),
        }),
        _ => Err(SdkResolveError::NotFound),
    });
    assert!(
        !result.items_uncertain,
        "a base SDK pulled in by a variant must be tolerated too; diags: {:?}",
        result.diagnostics
    );
    assert!(result.compile_condition_uncertainties.is_empty());
}

#[test]
fn define_constants_set_conditionally_inside_the_sdk_is_tolerated() {
    // SDK props that set `<DefineConstants>` under a condition we can't resolve
    // must not flag the preprocessor symbols — we already don't model the
    // framework defines the SDK adds, so our view is consistent, not corrupt.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <PropertyGroup Condition="'$(SomeUnresolved)' == 'x'">
    <DefineConstants>SDKDEF</DefineConstants>
  </PropertyGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <ItemGroup>
    <Compile Include="A.fs" />
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
    assert!(
        !result.define_constants_uncertain,
        "SDK define manipulation is tolerated; diags: {:?}",
        result.diagnostics
    );
}

#[test]
fn define_constants_gated_in_directory_build_props_is_uncertain() {
    // A *user* `Directory.Build.props` gating `<DefineConstants>` on an
    // unresolved property: the `#if` symbols are uncertain, so respected.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == 'net6.0'">
    <DefineConstants>NET6</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.define_constants_uncertain,
        "a user-gated define on an unresolved property must be respected"
    );
}

#[test]
fn conditional_compile_in_a_promoted_explicit_form_sdk_is_tolerated() {
    // Explicit-form SDK: the project's first element is `<Import Sdk=… Project=
    // "Sdk.props"/>`, promoted to the root splice via `find_explicit_sdk_promotion`
    // (a path that doesn't go through `resolve_project_sdk`). Its conditional
    // default-item must still be tolerated.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) = write_synthetic_sdk(
        tmp.path(),
        "MySdk",
        r#"<Project>
  <ItemGroup Condition="'$(EnableDefaultItems)' == 'true'">
    <Compile Include="**/*.fs" />
  </ItemGroup>
</Project>"#,
        "<Project/>",
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Sdk="MySdk" Project="Sdk.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
  <Import Sdk="MySdk" Project="Sdk.targets" />
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
    assert!(
        !result.items_uncertain,
        "a promoted explicit-form SDK must be tolerated; diags: {:?}",
        result.diagnostics
    );
}

#[test]
fn user_import_sdk_with_unresolved_project_path_makes_items_uncertain() {
    // A user-authored `<Import Sdk=… Project="$(Undefined)"/>` we can't resolve
    // is dropped; like any user import it could have hidden Compile items.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <Import Sdk="MySdk" Project="$(Undefined)" />
  <ItemGroup>
    <Compile Include="A.fs" />
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
    assert!(
        result.items_uncertain,
        "a user SDK import with an unresolved Project path could hide Compile items"
    );
}

#[test]
fn user_import_sdk_with_unsafe_project_path_makes_items_uncertain() {
    // `<Import Sdk=… Project="../escape.props"/>` is rejected (path escape); in
    // a user file that dropped import could have hidden Compile items.
    let tmp = TempDir::new().unwrap();
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="MySdk">
  <Import Sdk="MySdk" Project="../escape.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
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
    assert!(
        result.items_uncertain,
        "a rejected (unsafe-path) user SDK import could hide Compile items"
    );
}

#[test]
fn user_import_gated_on_unsupported_condition_makes_items_uncertain() {
    // A user `<Import>` skipped by a condition we can't model
    // could have carried Compile items.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extra.props" Condition="'@(_Unmodelled)' == 'x'" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "a user import skipped by an unsupported condition could hide Compile items"
    );
}

#[test]
fn user_import_gated_on_inexact_undefined_property_makes_items_uncertain() {
    // The gate reads a property whose value the walk can't pin. Since C.2b a
    // *plain* undefined name reads exactly empty and the gate decides cleanly
    // (see the companion test below), so the undecidable-import-gate machinery
    // this test pins needs an *inexact* read: `TargetFramework` is the
    // consumer-contract carve-out that never passes the exactness guard. We
    // can't tell whether the import (and any Compile items it carries)
    // applies.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extra.props" Condition="'$(TargetFramework)' == 'net8.0'" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "a user import gated on an inexact undefined property could hide Compile items"
    );
}

#[test]
fn user_import_gated_on_exact_undefined_property_is_not_items_uncertain() {
    // The flip side (C.2b): `IncludeExtra` is never written anywhere the walk
    // can see, the walk is not opaque, and the name is neither a toolset
    // property nor a carve-out — so the read is exactly empty and the gate
    // evaluates exactly False, matching `dotnet build` (MSBuild expands an
    // undefined `$(Name)` to ""). The import is a clean exclusion: nothing
    // uncertain, no diagnostic, not partial.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extra.props" Condition="'$(IncludeExtra)' == 'true'" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        !result.items_uncertain,
        "an exactly-False import gate is a clean exclusion; diags: {:?}",
        result.diagnostics
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "exact undefined read must not diagnose: {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
}

#[test]
fn user_import_gated_on_known_false_condition_is_not_items_uncertain() {
    // A condition we can fully evaluate to false is a clean exclusion — the
    // import genuinely doesn't apply, so nothing is uncertain.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="extra.props" Condition="'x' == 'y'" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        !result.items_uncertain,
        "a cleanly-false import condition is a correct exclusion"
    );
}

#[test]
fn custom_sdk_layout_does_not_tolerate_sibling_user_files() {
    // A custom resolver may return `root` as the dir directly holding
    // `Sdk.{props,targets}` (allowed by `SdkPaths`). Broadening tolerance to
    // `root.parent()` there would swallow a user `Directory.Build.props` sitting
    // beside the SDK dir — so for non-canonical layouts we tolerate `root` only.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".sdk"); // self-contained, NOT `…/Sdks/<name>/Sdk`
    write_at(&root, "Sdk.props", "<Project/>");
    write_at(&root, "Sdk.targets", "<Project/>");
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Custom">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    // A user Directory.Build.props beside the SDK dir (under the SDK root's
    // parent) gates a Compile on `TargetFramework` — the carve-out that stays
    // inexact under C.2b, so the gate is genuinely undecidable and only the
    // tolerance-root scoping decides whether it's respected. It must be.
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="Shared.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "Custom" {
            Ok(SdkPaths {
                root: root.clone(),
                props: root.join("Sdk.props"),
                targets: root.join("Sdk.targets"),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(
        result.items_uncertain,
        "a user Directory.Build.props beside a custom SDK root must be respected"
    );
}

#[test]
fn sdk_import_escaping_its_root_with_dotdot_does_not_tolerate_the_user_file() {
    // An SDK file that `<Import>`s `../Shared.props` reaches a *user* file
    // outside the SDK root. Its un-normalised reach path
    // (`<root>/../Shared.props`) still `starts_with` the SDK root component-wise,
    // so the raw-path Compile-tolerance check must lexically normalise the reach
    // path first — otherwise the user file's conditional Compile is wrongly
    // swallowed as SDK machinery.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".sdk"); // flat custom layout: tolerance root = `.sdk` only
    write_at(
        &root,
        "Sdk.props",
        r#"<Project>
  <Import Project="../Shared.props" />
</Project>"#,
    );
    write_at(&root, "Sdk.targets", "<Project/>");
    // The user file sits beside the SDK root (its normalised reach path is NOT
    // under `.sdk`) and gates a Compile on the `TargetFramework` carve-out —
    // the read that stays inexact under C.2b, so the import genuinely stays
    // undecidable and only the tolerance-root scoping decides whether it's
    // respected.
    write_at(
        tmp.path(),
        "Shared.props",
        r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="Shared.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Custom">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "Custom" {
            Ok(SdkPaths {
                root: root.clone(),
                props: root.join("Sdk.props"),
                targets: root.join("Sdk.targets"),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(
        result.items_uncertain,
        "a user file reached by an SDK `../` import escapes the SDK root and must be respected; diags: {:?}",
        result.diagnostics
    );
}

#[test]
#[cfg(unix)]
fn sdk_directory_symlink_escaping_the_root_does_not_tolerate_the_user_file() {
    // The raw-path (reach) tolerance arm exists for a *leaf-file* symlink-merge
    // layout, NOT for a *directory* symlink inside the SDK tree that points out
    // of it. Here the SDK root holds a `link` directory symlinked to a sibling
    // user directory; an SDK import of `link/Shared.props` reaches a user file
    // whose canonical path (and whose canonicalised parent directory) is outside
    // the SDK root — so its conditional Compile must be respected, not swallowed.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".sdk"); // flat custom layout: tolerance root = `.sdk`
    write_at(
        &root,
        "Sdk.props",
        r#"<Project>
  <Import Project="link/Shared.props" />
</Project>"#,
    );
    write_at(&root, "Sdk.targets", "<Project/>");
    // A user directory outside the SDK root, holding the conditional-Compile
    // props, reached only through the in-tree directory symlink `.sdk/link`.
    // The gate reads the `TargetFramework` carve-out (inexact under C.2b) so
    // the import stays genuinely undecidable.
    let outside = tmp.path().join("outside");
    write_at(
        &outside,
        "Shared.props",
        r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="Shared.fs" />
  </ItemGroup>
</Project>"#,
    );
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project Sdk="Custom">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file_with_sdk(&project_path, |name| {
        if name == "Custom" {
            Ok(SdkPaths {
                root: root.clone(),
                props: root.join("Sdk.props"),
                targets: root.join("Sdk.targets"),
            })
        } else {
            Err(SdkResolveError::NotFound)
        }
    });
    assert!(
        result.items_uncertain,
        "a user file reached through an SDK-tree directory symlink that escapes the \
         root must be respected, not tolerated as SDK machinery; diags: {:?}",
        result.diagnostics
    );
}

#[test]
fn conditional_compile_in_directory_build_props_undefined_gate_is_exactly_excluded() {
    // A *user* `Directory.Build.props` gates a `<Compile>` on
    // `'$(IncludeShared)' == 'true'` where `IncludeShared` is never set
    // anywhere the walk can see. Since C.2b that read is exactly empty
    // (MSBuild expands an undefined `$(Name)` to ""), so the gate evaluates
    // exactly False — the same exclusion `dotnet build` performs. The item is
    // exactly excluded: no uncertainty, no carve-out, no diagnostic.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup Condition="'$(IncludeShared)' == 'true'">
    <Compile Include="Shared.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        paths_of(&result.items),
        vec![canon(tmp.path()).join("A.fs")],
        "the gated Compile is exactly excluded; diags: {:?}",
        result.diagnostics
    );
    assert!(
        !result.items_uncertain,
        "an exactly-False user gate is a clean exclusion; diags: {:?}",
        result.diagnostics
    );
    assert!(result.compile_condition_uncertainties.is_empty());
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UndefinedProperty { .. })),
        "exact undefined read must not diagnose: {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
}

#[test]
fn conditional_compile_in_explicit_user_import_is_respected() {
    // The user-provenance counterpart of
    // `conditional_compile_inside_the_sdk_is_tolerated`: the same undecidable
    // `TargetFramework` gate (the carve-out that stays inexact under C.2b) on
    // a `<Compile>`, but in an explicitly-imported *user* props file — not
    // under the SDK tree, so it must be respected: the Compile set is
    // uncertain and the carve-out is recorded.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="Shared.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Shared.props",
        r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="Shared.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "a conditional Compile in an explicitly-imported user props must be respected"
    );
    assert_eq!(result.compile_condition_uncertainties.len(), 1);
    assert_eq!(
        result.compile_condition_uncertainties[0].reason,
        CompileConditionReason::UndefinedProperties(vec!["TargetFramework".to_string()])
    );
}

#[test]
fn user_import_with_unresolved_property_path_makes_items_uncertain() {
    // A user `<Import>` whose path reads a property we can't pin is dropped
    // unresolved; it could have carried Compile items, so the Compile set is
    // uncertain. (An SDK chain doing the same is tolerated.) Since C.2b a
    // *plain* undefined name substitutes exactly to "" — the import would
    // then be attempted at the literal expanded path and fail as
    // `ImportFailed` instead — so the unresolved-path machinery this test
    // pins needs an *inexact* read: `TargetFramework`, the carve-out that
    // never passes the exactness guard.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="$(TargetFramework)/shared.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "a user import with an unresolved path could hide Compile items"
    );
    assert!(
        result
            .compile_item_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                CompileItemUncertaintyCauseKind::Structural(
                    StructuralCompileItemUncertainty::ImportProjectUnresolved { project }
                ) if project == "$(TargetFramework)/shared.props"
            )),
        "expected unresolved import path to be recorded as the causal Compile uncertainty: {:?}",
        result.compile_item_uncertainties
    );
}

#[test]
fn unfollowable_import_in_a_user_file_makes_items_uncertain() {
    // A user `<Import>` we can't follow (missing file) could have carried
    // Compile items, so the Compile set is uncertain.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="Missing.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "a failed user import is Compile-risk"
    );
    assert!(
        result
            .compile_item_uncertainties
            .iter()
            .any(|cause| matches!(
                &cause.kind,
                CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::ImportFailed { .. })
            )),
        "expected failed import diagnostic to be recorded as a Compile uncertainty cause: {:?}",
        result.compile_item_uncertainties
    );
}
