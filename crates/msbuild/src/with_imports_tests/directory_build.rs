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
    // MSBuild's `Microsoft.Common.props` only imports
    // `Directory.Build.props` when the gate property's value is
    // (case-insensitively) "true" — empty/unset defaults to "true",
    // but anything else suppresses the import. Pre-round-3 the
    // walker treated "anything except 'false'" as opt-in, which
    // would erroneously import for values like "0", "yes", or
    // "no". This test pins the tightened semantics by using a
    // value MSBuild treats as non-opt-in.
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
    // "0" is *not* "true" and MSBuild therefore skips the import,
    // but the old `is_false` helper saw it as non-false → import.
    extras.insert("ImportDirectoryBuildProps".to_string(), "0".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    let canon_root = canon(tmp.path());
    assert_eq!(
        paths_of(&result.items),
        vec![canon_root.join("Main.fs")],
        "Directory.Build.props must be skipped for any non-true gate value",
    );
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
