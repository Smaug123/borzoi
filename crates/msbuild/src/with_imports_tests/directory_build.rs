//! Implicit `Directory.Build.props`/`.targets` splicing: ordering relative
//! to the project body, opt-out gating, path overrides, and `*Path`
//! property seeding.

use super::*;
use tempfile::TempDir;

#[test]
fn a_percent_in_a_seeded_directory_build_path_is_literal() {
    // `DirectoryBuildPropsPath` is a path *we* discovered on disk, not project
    // XML, so a `%XX` in it is literal — MSBuild keeps such values escaped and
    // its single unescape pass hands them back unchanged (the same rule as the
    // `well_known` seeds; pinned against `dotnet msbuild` for a project really
    // living under `…/a%20b/`). Reading the seed back must therefore commit,
    // not degrade: a stray decline here would silently drop Compile items for
    // everyone whose checkout path happens to contain a percent-hex pair.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("a%20b");
    std::fs::create_dir_all(&root).unwrap();
    let project_path = write_at(
        &root,
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(DirectoryBuildPropsPath)" />
  </ItemGroup>
</Project>"#,
    );
    write_at(&root, "Directory.Build.props", "<Project />");
    let result = parse_file(&project_path);
    assert!(
        !result.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedPropertyExpression { .. }
        )),
        "a percent in an evaluator-discovered path is literal: {:?}",
        result.diagnostics
    );
    assert_eq!(
        paths_of(&result.items),
        vec![canon(&root).join("Directory.Build.props")]
    );
}

#[test]
fn implicit_directory_build_props_seeds_before_project_body() {
    // Directory.Build.props is walked before the project body, so
    // properties it defines are visible to the body. We verify by
    // having the body *use* a value the props set.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="$(SeededName).fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <SeededName>FromProps</SeededName>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let dir = canon(tmp.path());
    assert_eq!(paths_of(&result.items), vec![dir.join("FromProps.fs")]);
    // The follow succeeded, so no ImplicitImportPresent diagnostic
    // should remain. (The pure parser would emit one; the with-imports
    // path explicitly suppresses it.)
    let any_implicit_present = result
        .diagnostics
        .iter()
        .any(|d| matches!(d.kind, DiagnosticKind::ImplicitImportPresent { .. }));
    assert!(
        !any_implicit_present,
        "ImplicitImportPresent should be suppressed by parse_fsproj_with_imports; got: {:?}",
        result.diagnostics
    );
}

#[test]
fn directory_build_props_can_import_parent_with_get_path_of_file_above() {
    // The F# repo uses nested Directory.Build.props wrappers that chain upward
    // with MSBuild::GetPathOfFileAbove. The import is user-authored, so failing
    // to follow it makes the Compile set uncertain and hides the real cause
    // behind unrelated SDK diagnostics in corpus reports.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromRoot.fs" />
  </ItemGroup>
</Project>"#,
    );
    let src = tmp.path().join("src");
    write_at(
        &src,
        "Directory.Build.props",
        r#"<Project>
  <Import Project="$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildThisFileDirectory)../'))" />
</Project>"#,
    );
    let project_path = write_at(
        &src,
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let root = canon(tmp.path());
    let project_dir = root.join("src");
    assert_eq!(
        paths_of(&result.items),
        vec![project_dir.join("FromRoot.fs"), project_dir.join("Main.fs")]
    );
    assert!(
        !result.items_uncertain,
        "GetPathOfFileAbove import should be followed cleanly; diags: {:?}",
        result.diagnostics
    );
}

#[test]
fn implicit_directory_build_targets_runs_after_project_body() {
    // Project sets X=FromBody; Directory.Build.targets overrides
    // X=FromTargets. Since targets is walked AFTER the body, the
    // final value is FromTargets.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <X>FromBody</X>
  </PropertyGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup>
    <X>FromTargets</X>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        result.properties.get("X").map(String::as_str),
        Some("FromTargets")
    );
}

#[test]
fn caller_extra_disables_implicit_directory_build_props() {
    // `ImportDirectoryBuildProps=false` supplied by the caller must
    // suppress the implicit Directory.Build.props splice. Without
    // this gate the walker would silently merge a file MSBuild
    // itself would skip, producing items the oracle never emits.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), "false".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    let canon_root = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![canon_root.join("Main.fs")],
        "Directory.Build.props must be skipped when opt-out is set",
    );
}

#[test]
fn project_property_disables_implicit_directory_build_targets() {
    // `ImportDirectoryBuildTargets=false` written *inside the
    // project body* must suppress the targets splice. MSBuild
    // evaluates the body before deciding whether to import
    // Directory.Build.targets, so the project itself can opt out.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let canon_root = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![canon_root.join("Main.fs")],
        "Directory.Build.targets must be skipped when project sets opt-out",
    );
}

#[test]
fn non_true_value_disables_implicit_directory_build_props() {
    // MSBuild's `Microsoft.Common.props` imports `Directory.Build.props`
    // under `'$(ImportDirectoryBuildProps)' == 'true'` — empty/unset
    // defaults to "true", and a value outside MSBuild's *boolean
    // vocabulary* suppresses the import. `"0"` is such a value: `==`
    // coerces both sides through the vocabulary, which does not admit
    // `0`/`1`, so the comparison falls through to a string compare and
    // is false. Probed end-to-end (dotnet 10.0.301, 2026-08-01,
    // `-p:ImportDirectoryBuildProps=0`): the file is not imported.
    // The opt-*in* spellings are pinned by
    // [`msbuild_boolean_gate_value_imports_directory_build_props`].
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), "0".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    let canon_root = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![canon_root.join("Main.fs")],
        "Directory.Build.props must be skipped for a gate value outside \
         MSBuild's boolean vocabulary",
    );
}

#[test]
fn msbuild_boolean_gate_value_imports_directory_build_props() {
    // `'$(ImportDirectoryBuildProps)' == 'true'` is an MSBuild `==`, which
    // coerces *both* sides through the boolean vocabulary before falling
    // back to a string compare. So every spelling of true opens the gate,
    // not just the literal word. Probed end-to-end against the real
    // evaluator (dotnet 10.0.301, 2026-08-01, `Microsoft.NET.Sdk` project,
    // `-p:ImportDirectoryBuildProps=<v>`, reading back `-getProperty` on a
    // property only `Directory.Build.props` sets): `true`/`yes`/`on`/
    // `!false` import it; `no`/`off`/`0` do not.
    //
    // Reading this gate with a bare `== "true"` string test skips a file
    // the real build imports, which loses every property and item that
    // file contributes — and the evaluator publishes the shortfall as
    // certain, since nothing about the gate is recorded as uncertain.
    for value in ["yes", "YES", "on", "!false"] {
        let tmp = TempDir::new().unwrap();
        write_at(
            tmp.path(),
            "Directory.Build.props",
            r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(
            tmp.path(),
            "Demo.fsproj",
            r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
        );
        let mut extras = HashMap::new();
        extras.insert("ImportDirectoryBuildProps".to_string(), value.to_string());
        let result = parse_file_with_extras(&project_path, extras);
        let canon_root = canon(tmp.path());
        assert_eq!(
            paths_of(&result.items),
            vec![
                canon_root.join("FromDirBuild.fs"),
                canon_root.join("Main.fs"),
            ],
            "gate value {value:?} is MSBuild-true, so Directory.Build.props \
             must be imported",
        );
    }
}

#[test]
fn empty_global_gate_property_skips_directory_build_props() {
    // An *empty global* `ImportDirectoryBuildProps` is read-only:
    // MSBuild's `Microsoft.Common.props` default-fill
    // (`<ImportDirectoryBuildProps Condition="'$(...)' == ''">true</...>`)
    // cannot write through a global, so the value stays "" and the
    // import gate `'$(...)' == 'true'` is false → the implicit import
    // is **skipped**. (The genuinely-optional case — the caller not
    // supplying the property at all → default-fill → import — is
    // covered by [`implicit_directory_build_props_seeds_before_project_body`].)
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <FromDirBuild>here</FromDirBuild>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), String::new());
    let result = parse_file_with_extras(&project_path, extras);
    assert!(
        !result.properties.contains_key("FromDirBuild"),
        "empty global gate value is sticky-empty → Directory.Build.props skipped; \
         properties: {:?}",
        result.properties,
    );
}

#[test]
fn true_global_gate_property_imports_directory_build_props() {
    // Guard: a non-empty global `ImportDirectoryBuildProps=true` still
    // imports — the sticky-global path agrees with the default-fill
    // path for the "true" value. Only the *empty* global case changed.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup>
    <FromDirBuild>here</FromDirBuild>
  </PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), "true".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    assert_eq!(
        result.properties.get("FromDirBuild").map(String::as_str),
        Some("here"),
        "non-empty global gate value \"true\" must still import Directory.Build.props",
    );
}

#[test]
fn empty_global_gate_property_skips_directory_build_targets() {
    // Same as the props gate but for `ImportDirectoryBuildTargets`,
    // checked *after* the body: an empty global stays "" (read-only),
    // so the targets gate is false → skip.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildTargets".to_string(), String::new());
    let result = parse_file_with_extras(&project_path, extras);
    let canon_root = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![canon_root.join("Main.fs")],
        "empty global gate value is sticky-empty → Directory.Build.targets skipped",
    );
}

#[test]
fn empty_global_directory_build_props_path_skips() {
    // An *empty global* `DirectoryBuildPropsPath` is read-only:
    // MSBuild assigns the discovered path to it only when it is unset,
    // so a global "" stays "" and `Exists('')` is false → the props
    // import is skipped entirely. We must NOT fall back to the
    // nearest discovered `Directory.Build.props`.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup><Source>nearest</Source></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(tmp.path(), "Demo.fsproj", "<Project>\n</Project>");
    let mut extras = HashMap::new();
    extras.insert("DirectoryBuildPropsPath".to_string(), String::new());
    let result = parse_file_with_extras(&project_path, extras);
    assert!(
        !result.properties.contains_key("Source"),
        "empty global DirectoryBuildPropsPath is sticky-empty → no fallback to \
         discovered Directory.Build.props; properties: {:?}",
        result.properties,
    );
}

#[test]
fn empty_global_directory_build_targets_path_skips() {
    // As above for `DirectoryBuildTargetsPath`: empty global stays
    // "", `Exists('')` false → no fallback to the discovered
    // Directory.Build.targets.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup><Source>nearest</Source></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(tmp.path(), "Demo.fsproj", "<Project>\n</Project>");
    let mut extras = HashMap::new();
    extras.insert("DirectoryBuildTargetsPath".to_string(), String::new());
    let result = parse_file_with_extras(&project_path, extras);
    assert!(
        !result.properties.contains_key("Source"),
        "empty global DirectoryBuildTargetsPath is sticky-empty → no fallback to \
         discovered Directory.Build.targets; properties: {:?}",
        result.properties,
    );
}

#[cfg(unix)]
#[test]
fn nested_import_resolves_against_pre_canonical_directory() {
    // Symlink semantics: when an `<Import>` points at a file that's
    // a symlink, MSBuild reads the file via the *symlink* path, so
    // any nested `<Import Project="local.props" />` inside it
    // resolves against the symlink's parent — not the target's
    // parent. Pre-round-3 we passed `canon.parent()` as the base
    // directory for nested imports, which silently swapped the
    // wrong sibling file when the layout differed across the link.
    // The fix: keep `canon` strictly for walked-file identity,
    // and use `path.parent()` for resolution.
    let tmp = TempDir::new().unwrap();
    let link_dir = tmp.path().join("link_side");
    let target_dir = tmp.path().join("target_side");
    std::fs::create_dir(&link_dir).unwrap();
    std::fs::create_dir(&target_dir).unwrap();

    // The "real" common.props lives in target_side and pulls in
    // its sibling local.props (whichever sibling the symlink path
    // points at). Both link_side and target_side have their own
    // local.props; only the link-side one should win when MSBuild
    // reaches common.props through link_side.
    write_at(
        &target_dir,
        "common.props",
        r#"<Project>
  <Import Project="local.props" />
</Project>"#,
    );
    write_at(
        &target_dir,
        "local.props",
        r#"<Project>
  <PropertyGroup><Side>target</Side></PropertyGroup>
</Project>"#,
    );
    write_at(
        &link_dir,
        "local.props",
        r#"<Project>
  <PropertyGroup><Side>link</Side></PropertyGroup>
</Project>"#,
    );
    // link_side/common.props -> target_side/common.props
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
    assert_eq!(
        result.properties.get("Side").map(String::as_str),
        Some("link"),
        "nested import must resolve against the symlink path's parent (link_side), \
         not the canonicalised target_side; properties: {:?}",
        result.properties,
    );
}

#[test]
fn directory_build_targets_path_override_redirects_targets_import() {
    // MSBuild's `Microsoft.Common.targets` imports
    // `$(DirectoryBuildTargetsPath)` (when set and the file exists)
    // *instead* of walking up the tree to the nearest
    // Directory.Build.targets. The override is evaluated after the
    // body, so the project itself can set it to redirect the import.
    // We place a real Directory.Build.targets in the parent that
    // would normally be picked up, and an alternative file
    // elsewhere; the project then redirects to the alternative, and
    // we assert the property set in the alternative wins (and the
    // sibling's never runs).
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup><Source>nearest</Source></PropertyGroup>
</Project>"#,
    );
    let alt = write_at(
        tmp.path(),
        "alt/Custom.targets",
        r#"<Project>
  <PropertyGroup><Source>override</Source></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <DirectoryBuildTargetsPath>{}</DirectoryBuildTargetsPath>
  </PropertyGroup>
</Project>"#,
            alt.display(),
        ),
    );
    let result = parse_file(&project_path);
    assert_eq!(
        result.properties.get("Source").map(String::as_str),
        Some("override"),
        "DirectoryBuildTargetsPath override should redirect to alt/Custom.targets, \
         not the sibling Directory.Build.targets; properties: {:?}",
        result.properties,
    );
}

#[test]
fn directory_build_props_path_override_via_caller_globals() {
    // The props override has to come from outside the project body
    // (the body hasn't been walked yet when MSBuild checks it). A
    // caller-supplied global is the realistic path here — e.g., a
    // build harness pinning Directory.Build.props for an out-of-tree
    // configuration. The nearest Directory.Build.props on disk
    // becomes irrelevant; the override file's writes are what
    // surface.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup><Source>nearest</Source></PropertyGroup>
</Project>"#,
    );
    let alt = write_at(
        tmp.path(),
        "alt/Custom.props",
        r#"<Project>
  <PropertyGroup><Source>override</Source></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert(
        "DirectoryBuildPropsPath".to_string(),
        alt.to_string_lossy().into_owned(),
    );
    let result = parse_file_with_extras(&project_path, extras);
    assert_eq!(
        result.properties.get("Source").map(String::as_str),
        Some("override"),
        "DirectoryBuildPropsPath override should redirect to alt/Custom.props, \
         not the sibling Directory.Build.props; properties: {:?}",
        result.properties,
    );
}

#[test]
fn directory_build_targets_path_override_to_missing_file_skips_silently() {
    // MSBuild's Import on the override path carries
    // `Condition="... and Exists('$(DirectoryBuildTargetsPath)')"`,
    // so a typo / stale path silently *skips* the targets import
    // rather than emitting a diagnostic or falling back to the
    // nearest sibling. Falling back would silently load a file the
    // user clearly redirected away from; emitting a diagnostic
    // would surface non-bugs from harnesses that pre-emptively set
    // the property to a path that may or may not exist.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup><Source>nearest</Source></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildTargetsPath>/nonexistent/path/Custom.targets</DirectoryBuildTargetsPath>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(
        !result.properties.contains_key("Source"),
        "override pointing at missing file must skip silently, \
         not fall back to nearest; properties: {:?}",
        result.properties,
    );
    let import_failed: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| matches!(d.kind, DiagnosticKind::ImportFailed { .. }))
        .collect();
    assert!(
        import_failed.is_empty(),
        "missing-file override must not produce an ImportFailed diagnostic, got: {:?}",
        import_failed,
    );
}

#[test]
fn no_implicit_files_walks_only_project_body() {
    // With no surrounding Directory.* files, parse_fsproj_with_imports
    // behaves like parse_fsproj on a project with no Imports. We
    // place the project at a deep path so it has no chance of finding
    // any of the well-known files on the host machine.
    let tmp = TempDir::new().unwrap();
    let deep = tmp.path().join("a/b/c");
    std::fs::create_dir_all(&deep).unwrap();
    let project_path = write_at(
        &deep,
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="Only.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let canon_deep = canon(&deep);
    assert_eq!(paths_of(&result.items), vec![canon_deep.join("Only.fs")]);
    // Whether is_partial is set depends on whether any Directory.*
    // files exist on the host filesystem *above* the tempdir (e.g.,
    // a developer's $HOME might have one). We don't assert on it.
}

#[test]
fn implicit_directory_build_props_path_is_seeded_for_substitution() {
    // MSBuild's `Microsoft.Common.props` sets `$(DirectoryBuildPropsPath)`
    // to the implicitly-discovered file *before* importing it, so
    // references inside the imported file (or later in the project)
    // expand to the actual path. Without seeding it, `$(DirectoryBuildPropsPath)`
    // would silently expand to "" — masking real import-path bugs and
    // breaking files that key off their own location.
    let tmp = TempDir::new().unwrap();
    let dirbuild = write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup><PropsPathSeenByImport>$(DirectoryBuildPropsPath)</PropsPathSeenByImport></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <PropsPathSeenByBody>$(DirectoryBuildPropsPath)</PropsPathSeenByBody>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let expected = canon(&dirbuild).to_string_lossy().replace('\\', "/");
    assert_eq!(
        result
            .properties
            .get("PropsPathSeenByImport")
            .map(String::as_str),
        Some(expected.as_str()),
        "imported file should see DirectoryBuildPropsPath seeded to its own path; properties: {:?}",
        result.properties,
    );
    assert_eq!(
        result
            .properties
            .get("PropsPathSeenByBody")
            .map(String::as_str),
        Some(expected.as_str()),
        "project body should see DirectoryBuildPropsPath still seeded after the implicit import; properties: {:?}",
        result.properties,
    );
}

#[test]
fn implicit_directory_build_targets_path_is_seeded_for_substitution() {
    // The targets variant: the implicit targets import happens *after*
    // the project body, so the body cannot capture
    // `$(DirectoryBuildTargetsPath)` directly — but the targets file
    // itself can, and that's the substitution path most likely to
    // matter (Microsoft.NET.Sdk.targets-style files that key off their
    // own location).
    let tmp = TempDir::new().unwrap();
    let dirbuild = write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <PropertyGroup><TargetsPathSeenByImport>$(DirectoryBuildTargetsPath)</TargetsPathSeenByImport></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
</Project>"#,
    );
    let result = parse_file(&project_path);
    let expected = canon(&dirbuild).to_string_lossy().replace('\\', "/");
    assert_eq!(
        result
            .properties
            .get("TargetsPathSeenByImport")
            .map(String::as_str),
        Some(expected.as_str()),
        "imported targets file should see DirectoryBuildTargetsPath seeded; properties: {:?}",
        result.properties,
    );
}

#[test]
fn explicit_directory_build_props_path_override_is_not_rewritten() {
    // MSBuild preserves a user-supplied property's value verbatim — it
    // never rewrites it to the resolved/canonicalised import path. Our
    // seeding logic must only fire on the fallback branch, or callers
    // that round-trip the override (read it back, write it elsewhere)
    // would see a value they never set.
    let tmp = TempDir::new().unwrap();
    let alt = write_at(
        tmp.path(),
        "alt/Custom.props",
        r#"<Project>
  <PropertyGroup><AltSeen>true</AltSeen></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <CapturedOverride>$(DirectoryBuildPropsPath)</CapturedOverride>
  </PropertyGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    let override_value = alt.to_string_lossy().into_owned();
    extras.insert(
        "DirectoryBuildPropsPath".to_string(),
        override_value.clone(),
    );
    let result = parse_file_with_extras(&project_path, extras);
    assert_eq!(
        result.properties.get("AltSeen").map(String::as_str),
        Some("true"),
        "alt props file should have been imported (sanity check); properties: {:?}",
        result.properties,
    );
    assert_eq!(
        result
            .properties
            .get("CapturedOverride")
            .map(String::as_str),
        Some(override_value.as_str()),
        "explicit override value must be preserved verbatim, not rewritten to a resolved/normalised form; properties: {:?}",
        result.properties,
    );
}

#[test]
fn directory_build_targets_path_override_with_backslashes_resolves_on_unix() {
    // MSBuild accepts both `\` and `/` separators on either platform.
    // Explicit `<Import Project="...">` resolution already normalises
    // `\` to `/`; the override resolver must follow suit, otherwise
    // `alt\Custom.targets` probes a literal-backslash filename on Unix
    // and the import silently skips.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "alt/Custom.targets",
        r#"<Project>
  <PropertyGroup><Source>override</Source></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        // The override value uses backslashes deliberately — that's
        // the MSBuild-style spelling we expect to handle.
        r#"<Project>
  <PropertyGroup>
    <DirectoryBuildTargetsPath>alt\Custom.targets</DirectoryBuildTargetsPath>
  </PropertyGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert_eq!(
        result.properties.get("Source").map(String::as_str),
        Some("override"),
        "backslash-separated override path must resolve cross-platform; properties: {:?}",
        result.properties,
    );
}

#[test]
fn gated_out_implicit_props_import_does_not_seed_path_property() {
    // MSBuild assigns `DirectoryBuildPropsPath` inside the same gated
    // block that performs the import, so opting out via
    // `ImportDirectoryBuildProps=false` must leave the path property
    // unset. Without this, an opted-out project body would still see
    // `$(DirectoryBuildPropsPath)` resolve to the discovered file,
    // and conditions/includes keyed on it would diverge from MSBuild.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project>
  <PropertyGroup><FromDirBuild>seen</FromDirBuild></PropertyGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <PropsPathSeenByBody>$(DirectoryBuildPropsPath)</PropsPathSeenByBody>
  </PropertyGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), "false".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    assert!(
        !result.properties.contains_key("FromDirBuild"),
        "sanity: ImportDirectoryBuildProps=false must skip the import; properties: {:?}",
        result.properties,
    );
    assert_eq!(
        result
            .properties
            .get("PropsPathSeenByBody")
            .map(String::as_str),
        Some(""),
        "gated-out import must not seed DirectoryBuildPropsPath; properties: {:?}",
        result.properties,
    );
}

/// A `Directory.Build.*` gate written under a condition the walker cannot
/// decide leaves the import — and therefore the item set — unknown.
///
/// `'$(X.Substring(0,1))' == 'a'` is ordinary MSBuild that we do not model.
/// The oracle says it evaluates **true** (dotnet 10.0.301, 2026-08-01), so the
/// real build writes the gate and acts on it; we skip the write and act on the
/// default. Whichever way each side lands, the walker cannot claim the item set
/// is exact — checklist entry 1, both directions.
const UNDECIDABLE_TRUE: &str = "'$(X.Substring(0,1))' == 'a'";

#[test]
fn an_undecided_gate_write_leaves_the_item_set_uncertain() {
    // MSBuild writes `false` and *skips* Directory.Build.targets. We never
    // write the gate, read it as absent, default it to true, and import — so we
    // publish a Compile item the real build does not have.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <ImportDirectoryBuildTargets Condition="{UNDECIDABLE_TRUE}">false</ImportDirectoryBuildTargets>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
        ),
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "the gate write could not be decided, so the item set must not be \
         published as exact; items: {:?}",
        paths_of(&result.items)
    );
}

#[test]
fn an_undecided_gate_write_that_would_re_enable_the_import_also_flags() {
    // The mirror direction, and the more damaging one: a decided write turns
    // the gate off, then an undecidable write turns it back on. MSBuild takes
    // the second write and imports; we skip it, keep `false`, and **drop** the
    // Compile item the real build has. A missing item is invisible to any
    // consumer that trusts `items_uncertain`.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>
    <ImportDirectoryBuildTargets Condition="{UNDECIDABLE_TRUE}">true</ImportDirectoryBuildTargets>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
        ),
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "an undecidable write that could re-enable the import must flag; \
         items: {:?}",
        paths_of(&result.items)
    );
}

#[test]
fn an_undecided_path_redirect_leaves_the_item_set_uncertain() {
    // The gate's sibling: `DirectoryBuildTargetsPath` chooses *which* file is
    // imported, so an undecidable write to it is the same defect one property
    // over — we import one file's items where the real build imports another's.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
    );
    write_at(
        tmp.path(),
        "Other.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromOther.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <DirectoryBuildTargetsPath Condition="{UNDECIDABLE_TRUE}">$(MSBuildThisFileDirectory)Other.targets</DirectoryBuildTargetsPath>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
        ),
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "an undecidable redirect must flag; items: {:?}",
        paths_of(&result.items)
    );
}

/// A way of writing a gate property, as `(description, build the XML)`. The
/// three that exist are the three places a `Condition` can decide whether the
/// write happens: on the element, on its `<PropertyGroup>`, and on the
/// `<When>` arm containing it.
type Placement = (&'static str, fn(&str, &str) -> String);

#[test]
fn every_declared_gate_name_is_swept_at_its_own_consumption_point() {
    // Placement × name, over the walker's declared directly-read set. The two
    // halves of that set are consumed at *different* points in the walk, and
    // the sweep has to say so, because the correct expectation differs:
    //
    //  * the targets pair is consumed after the body, so a body write decides
    //    the import — an undecidable one must leave the item set uncertain;
    //  * the props pair is consumed *before* the body, so a body write is
    //    inert on both sides and must **not** cost a decline. Probed (dotnet
    //    10.0.301, 2026-08-01): a body `<ImportDirectoryBuildProps>false</…>`
    //    leaves `Directory.Build.props`'s `FromProps` reading `set` — MSBuild
    //    imported it regardless.
    //
    // The names come from the splice constants the production code reads, so a
    // pair added there is swept here rather than going untested.
    let (targets_gate, targets_path) = crate::evaluator::DIRECTORY_BUILD_TARGETS_SPLICE;
    let (props_gate, props_path) = crate::evaluator::DIRECTORY_BUILD_PROPS_SPLICE;
    let consumed_after_body = [targets_gate, targets_path];
    let consumed_before_body = [props_gate, props_path];

    fn parse_with_body(body: &str) -> crate::ParsedProject {
        let tmp = TempDir::new().unwrap();
        write_at(
            tmp.path(),
            "Directory.Build.targets",
            r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(
            tmp.path(),
            "Demo.fsproj",
            &format!(
                r#"<Project>
  <PropertyGroup>
    <X>abc</X>
  </PropertyGroup>
{body}
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
            ),
        );
        parse_file(&project_path)
    }

    // The three places a condition can decide whether the write happens —
    // checklist entries 1, 2 and 3. Each is evaluated by different code, and
    // the first cut of this change covered only the innermost.
    let placements: &[Placement] = &[
        ("on the write", |name, condition| {
            format!(
                "  <PropertyGroup>\n    <{name} Condition=\"{condition}\">true</{name}>\n  \
                 </PropertyGroup>"
            )
        }),
        ("on the enclosing group", |name, condition| {
            format!(
                "  <PropertyGroup Condition=\"{condition}\">\n    <{name}>true</{name}>\n  \
                 </PropertyGroup>"
            )
        }),
        ("on a Choose arm", |name, condition| {
            format!(
                "  <Choose>\n    <When Condition=\"{condition}\">\n      <PropertyGroup>\n        \
                 <{name}>true</{name}>\n      </PropertyGroup>\n    </When>\n  </Choose>"
            )
        }),
    ];

    for (placement, build) in placements {
        for name in consumed_after_body {
            assert!(
                parse_with_body(&build(name, UNDECIDABLE_TRUE)).items_uncertain,
                "{name} written under an undecidable condition {placement} must \
                 leave the item set uncertain",
            );
            // `'$(X)' == 'abc'` is inside the modelled grammar over a property
            // the document defines, so the walker knows exactly whether the
            // write happened and owes no decline. Without this arm, marking
            // everything uncertain would pass the sweep perfectly.
            assert!(
                !parse_with_body(&build(name, "'$(X)' == 'abc'")).items_uncertain,
                "{name} written under a cleanly-decided condition {placement} \
                 must not cost a decline",
            );
        }
        for name in consumed_before_body {
            assert!(
                !parse_with_body(&build(name, UNDECIDABLE_TRUE)).items_uncertain,
                "{name} is consumed before the body, so a body write {placement} \
                 cannot change the import on either side and must not cost a \
                 decline",
            );
        }
    }
}

#[test]
fn a_body_read_before_the_gated_import_stays_exact() {
    // The mirror of the item claim, and the reason the trust question is asked
    // at the *splice* rather than at the write. MSBuild evaluates the body
    // before importing `Directory.Build.targets`, so a body read of a name that
    // file defines is exactly empty **whatever the gate decides** — probed
    // (dotnet 10.0.301, 2026-08-01): with the file present and defining
    // `FromDirBuild`, a body `<Reads>[$(FromDirBuild)]</Reads>` reads `[]`
    // while the final `FromDirBuild` reads `set`.
    //
    // So an undecidable gate write must not degrade that read. Latching
    // opacity at the write did exactly that, which is a spurious decline: the
    // gate cannot retroactively change what the body already saw.
    fn undefined_reads_for(condition: &str) -> Vec<String> {
        let tmp = TempDir::new().unwrap();
        write_at(
            tmp.path(),
            "Directory.Build.targets",
            r#"<Project>
  <PropertyGroup>
    <FromDirBuild>set</FromDirBuild>
  </PropertyGroup>
</Project>"#,
        );
        let project_path = write_at(
            tmp.path(),
            "Demo.fsproj",
            &format!(
                r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <ImportDirectoryBuildTargets Condition="{condition}">false</ImportDirectoryBuildTargets>
    <Reads>$(FromDirBuild)</Reads>
  </PropertyGroup>
</Project>"#
            ),
        );
        parse_file(&project_path)
            .diagnostics
            .iter()
            .filter_map(|d| match &d.kind {
                DiagnosticKind::UndefinedProperty { name } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    for condition in [UNDECIDABLE_TRUE, "'$(X)' == 'abc'"] {
        assert!(
            !undefined_reads_for(condition)
                .iter()
                .any(|n| n == "FromDirBuild"),
            "a body read before the gated import is exactly empty on both \
             sides, so it owes no degrade (condition {condition:?})",
        );
    }
}

#[test]
fn an_undecided_group_condition_over_a_gate_write_also_flags() {
    // Checklist entries 2 and 3: the gate write itself is unconditional, but
    // whether it *happens* is decided by the enclosing `<PropertyGroup>` — or
    // by which `<Choose>` arm runs. Those conditions are evaluated before the
    // walker ever reaches the property child, so a context scoped to the child
    // alone cannot see them, and a skipped branch never reaches the child at
    // all.
    fn items_uncertain_for(body: &str) -> bool {
        let tmp = TempDir::new().unwrap();
        write_at(
            tmp.path(),
            "Directory.Build.targets",
            r#"<Project>
  <ItemGroup>
    <Compile Include="FromDirBuild.fs" />
  </ItemGroup>
</Project>"#,
        );
        let project_path = write_at(
            tmp.path(),
            "Demo.fsproj",
            &format!(
                r#"<Project>
  <PropertyGroup>
    <X>abc</X>
  </PropertyGroup>
{body}
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
            ),
        );
        parse_file(&project_path).items_uncertain
    }

    assert!(
        items_uncertain_for(&format!(
            r#"  <PropertyGroup Condition="{UNDECIDABLE_TRUE}">
    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>
  </PropertyGroup>"#
        )),
        "an undecidable <PropertyGroup> condition over a gate write must flag",
    );
    assert!(
        items_uncertain_for(&format!(
            r#"  <Choose>
    <When Condition="{UNDECIDABLE_TRUE}">
      <PropertyGroup>
        <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>
      </PropertyGroup>
    </When>
  </Choose>"#
        )),
        "an undecidable <When> arm containing a gate write must flag",
    );
    // Control: a cleanly-decided group condition owes no decline, in both the
    // branch-taken and branch-skipped directions.
    assert!(
        !items_uncertain_for(
            r#"  <PropertyGroup Condition="'$(X)' == 'abc'">
    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>
  </PropertyGroup>"#
        ),
        "a cleanly-decided group condition must not cost a decline",
    );
    assert!(
        !items_uncertain_for(
            r#"  <PropertyGroup Condition="'$(X)' == 'nope'">
    <ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets>
  </PropertyGroup>"#
        ),
        "a cleanly-skipped group condition must not cost a decline",
    );
}

#[test]
fn an_undecided_gate_costs_nothing_when_there_is_no_file_to_import() {
    // The gate only matters if something could be imported. MSBuild's own
    // import is `Condition="… and exists('$(DirectoryBuildTargetsPath)')"`, so
    // with no `Directory.Build.targets` on disk and no redirect, both sides
    // skip whatever the gate says — and an undecidable write to it therefore
    // owes no decline.
    //
    // This matters because `items_uncertain` is not a small cost: the LSP
    // stops folding the project. A trust check that fires where the decision
    // cannot change the outcome spends that for nothing.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <ImportDirectoryBuildTargets Condition="{UNDECIDABLE_TRUE}">false</ImportDirectoryBuildTargets>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
        ),
    );
    let result = parse_file(&project_path);
    assert!(
        !result.items_uncertain,
        "no Directory.Build.targets exists, so the gate cannot change the item \
         set on either side and must not cost a decline",
    );
    assert_eq!(
        paths_of(&result.items),
        vec![canon(tmp.path()).join("Main.fs")],
    );
}

#[test]
fn an_undecided_path_redirect_flags_even_when_nothing_resolves_here() {
    // The counterpart of
    // [`an_undecided_gate_costs_nothing_when_there_is_no_file_to_import`], and
    // the boundary between them is the whole subtlety.
    //
    // There is no `Directory.Build.targets` next to the project, so *our*
    // resolution finds nothing. That proves nothing: the undecidable write
    // redirects `DirectoryBuildTargetsPath` at a file that does exist, and
    // MSBuild — which evaluates the condition — imports it. "Nothing resolved
    // here" is only evidence when the path itself is exact.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Other.targets",
        r#"<Project>
  <ItemGroup>
    <Compile Include="FromOther.fs" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <DirectoryBuildTargetsPath Condition="{UNDECIDABLE_TRUE}">$(MSBuildThisFileDirectory)Other.targets</DirectoryBuildTargetsPath>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
        ),
    );
    let result = parse_file(&project_path);
    assert!(
        result.items_uncertain,
        "the redirect target exists and MSBuild imports it, so the item set \
         must not be published as exact; items: {:?}",
        paths_of(&result.items),
    );
}

#[test]
fn an_undecided_gate_withdraws_every_item_facet_not_just_compile() {
    // A `Directory.Build.*` file carries `<ProjectReference>` and
    // `<PackageReference>` as readily as `<Compile>`, so an undecided splice
    // has to withdraw all of them. Setting only `items_uncertain` would leave a
    // consumer trusting a phantom project-graph edge or a missing dependency —
    // the "one missing entry per round" shape the trust checklist exists to
    // prevent.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.targets",
        r#"<Project>
  <ItemGroup>
    <ProjectReference Include="Other.fsproj" />
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        &format!(
            r#"<Project>
  <PropertyGroup>
    <X>abc</X>
    <ImportDirectoryBuildTargets Condition="{UNDECIDABLE_TRUE}">false</ImportDirectoryBuildTargets>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#
        ),
    );
    let result = parse_file(&project_path);
    assert!(result.items_uncertain, "Compile facet");
    assert!(
        result.project_references_uncertain,
        "the governed file declares a <ProjectReference>, so the graph edges \
         are unknown too; refs: {:?}",
        result.project_references,
    );
    assert!(
        result.package_references_uncertain,
        "…and its <PackageReference> list with them; packages: {:?}",
        result.package_references,
    );
}
