//! `TreatAsLocalProperty` interaction with global gate properties across
//! import boundaries.

use super::*;
use tempfile::TempDir;

#[test]
fn imported_root_treat_as_local_property_unprotects_global() {
    // MSBuild's `TreatAsLocalProperty` lets a project root opt
    // specific global properties out of read-only treatment so the
    // body can reassign them. The attribute applies to whichever
    // `<Project>` element carries it — including an imported file's
    // root — and only for the scope of that file. Without honouring
    // this on imports, a Directory.Build.props that legitimately
    // overrides a caller-supplied global (a common pattern for
    // pinning `Configuration` or `RestoreSources` in repo defaults)
    // would have its write silently dropped and the global would
    // leak through unchanged.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project TreatAsLocalProperty="Foo">
  <PropertyGroup>
    <Foo>local</Foo>
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
    extras.insert("Foo".to_string(), "global".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    assert_eq!(
        result.properties.get("Foo").map(String::as_str),
        Some("local"),
        "Directory.Build.props's TreatAsLocalProperty=\"Foo\" must let the write win; \
         properties: {:?}",
        result.properties,
    );
}

#[test]
fn imported_root_treat_as_local_property_does_not_leak_outside_file() {
    // Scope check: `TreatAsLocalProperty` on an imported file only
    // affects that file's body. After the import returns, the entry
    // project's writes to the same name must still be discarded as
    // protected — otherwise an opt-out in some upstream
    // Directory.Build.props would silently unprotect a global for
    // the entire walk, which is a quieter divergence but the same
    // class of bug.
    let tmp = TempDir::new().unwrap();
    write_at(
        tmp.path(),
        "Directory.Build.props",
        r#"<Project TreatAsLocalProperty="Foo">
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup>
    <Foo>entry-write</Foo>
  </PropertyGroup>
</Project>"#,
    );
    let mut extras = HashMap::new();
    extras.insert("Foo".to_string(), "global".to_string());
    let result = parse_file_with_extras(&project_path, extras);
    // The entry project did NOT list Foo on its own root, so the
    // protection should be back in force by the time we walk the
    // body. The write must be discarded.
    assert!(
        !result.properties.contains_key("Foo"),
        "entry-project write to Foo should remain discarded; properties: {:?}",
        result.properties,
    );
}

#[test]
fn imported_treat_as_local_unprotects_empty_gate_global_and_imports() {
    // The interaction between two features: an *empty global*
    // `ImportDirectoryBuildProps` is normally sticky-empty (read-only,
    // so MSBuild's default-fill cannot write `true` through it →
    // Directory.Build.props is skipped). But `TreatAsLocalProperty`
    // makes the named global locally writable for the scope of the
    // file that declares it — at which point the default-fill *can*
    // write through, flipping the empty value to `true` and importing
    // Directory.Build.props after all.
    //
    // We model `TreatAsLocalProperty` by removing the name from the
    // protected set for that file's scope, so the sticky-global gate
    // must honour that same scoping: a name unprotected by an imported
    // root is no longer a read-only global there. The only gate inside
    // such an unprotect window is the deferred nested-`Sdk.props` fire
    // of Directory.Build.props, so we drive it: the entry project has
    // no SDK of its own, but its body imports a file that declares
    // `Sdk="MySdk"` *and* `TreatAsLocalProperty="ImportDirectoryBuildProps"`.
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
    let (root, props, targets) =
        write_synthetic_sdk(tmp.path(), "MySdk", "<Project/>", "<Project/>");
    // The body-imported file carries both the SDK (so the deferred
    // fire reaches its `Sdk.props`) and the `TreatAsLocalProperty`
    // opt-out (so the empty global is locally writable in its scope).
    write_at(
        tmp.path(),
        "inner.props",
        r#"<Project Sdk="MySdk" TreatAsLocalProperty="ImportDirectoryBuildProps">
</Project>"#,
    );
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <Import Project="inner.props" />
  <ItemGroup>
    <Compile Include="Main.fs" />
  </ItemGroup>
</Project>"#,
    );
    let canon_project = canon(&project_path);
    let mut extras = HashMap::new();
    extras.insert("ImportDirectoryBuildProps".to_string(), String::new());
    let resolver = |name: &str| {
        if name == "MySdk" {
            Ok(SdkResolution::Single(SdkPaths {
                root: root.clone(),
                props: props.clone(),
                targets: targets.clone(),
            }))
        } else {
            Err(SdkResolveError::NotFound)
        }
    };
    let result = parse_fsproj_with_imports(
        &std::fs::read_to_string(&project_path).unwrap(),
        &canon_project,
        &extras,
        &HashMap::new(),
        Some(&resolver),
        None,
    )
    .expect("well-formed XML parses");
    assert_eq!(
        result.properties.get("FromDirBuild").map(String::as_str),
        Some("here"),
        "TreatAsLocalProperty on the imported root unprotects the empty global, so the \
         default-fill writes `true` and Directory.Build.props is imported; properties: {:?}",
        result.properties,
    );
}

/// [`ParsedProject::locally_overridable_properties`] reports what the document
/// *asked for*, across every root the walk reads — the entry's own attribute
/// and any imported file's.
///
/// A caller that supplies a global needs this to know whether the evaluation it
/// got back was really conducted under that global. It cannot ask the evaluated
/// property table instead: an override whose value we refuse leaves no entry
/// there, so absence conflates "the document never wrote it" with "the document
/// wrote something we could not evaluate" — the last case in the table below.
#[test]
fn locally_overridable_properties_reports_every_roots_opt_out() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        // (label, entry attribute, imported attribute, expected union)
        ("neither", "", "", &[]),
        ("entry only", r#" TreatAsLocalProperty="Foo""#, "", &["foo"]),
        (
            "import only",
            "",
            r#" TreatAsLocalProperty="Bar""#,
            &["bar"],
        ),
        (
            "both, case- and list-normalised",
            r#" TreatAsLocalProperty="Foo;BAZ""#,
            r#" TreatAsLocalProperty="bar""#,
            &["bar", "baz", "foo"],
        ),
    ];
    for (label, entry_attr, import_attr, expected) in cases {
        let tmp = TempDir::new().unwrap();
        write_at(
            tmp.path(),
            "Directory.Build.props",
            &format!("<Project{import_attr}><PropertyGroup/></Project>"),
        );
        let project_path = write_at(
            tmp.path(),
            "Demo.fsproj",
            &format!("<Project{entry_attr}><PropertyGroup/></Project>"),
        );
        let result = parse_file_with_extras(&project_path, HashMap::new());
        let got: Vec<&str> = result
            .locally_overridable_properties
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(got, *expected, "{label}");
    }
}

/// The opt-out is recorded whether or not *this* evaluation supplied a matching
/// global, because it answers a question about the document rather than about
/// one caller's inputs — and the caller that needs it (the LSP's inner-build
/// seeding) asks before deciding whether to supply the global at all.
#[test]
fn an_opt_out_is_recorded_without_a_matching_global() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project TreatAsLocalProperty="Foo"><PropertyGroup/></Project>"#,
    );
    let result = parse_file_with_extras(&project_path, HashMap::new());
    assert!(
        result.locally_overridable_properties.contains("foo"),
        "no `Foo` global was supplied, but the document still asked: {:?}",
        result.locally_overridable_properties,
    );
}
