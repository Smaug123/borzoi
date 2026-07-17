//! Compile-asset selection: the behaviours worth naming.
//!
//! Every expectation here was first *asked of the real NuGet content model*
//! (`tools/nuget-oracle`'s `selectCompileAssets`); `compile_assets_diff.rs`
//! keeps them honest at scale, and `compile_assets_restore.rs` pins the rules
//! against a genuine `dotnet restore`. These are the readable version.

use borzoi_nuget::{
    AssetSelectionDecline, CompileAssets, NuGetFramework, PackageNuspec, parse_nuspec,
    select_compile_assets,
};

fn framework(name: &str) -> NuGetFramework {
    NuGetFramework::parse(name).expect("framework parses")
}

fn bare_nuspec() -> PackageNuspec {
    parse_nuspec(
        r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>Pkg</id><version>1.0.0</version></metadata>
</package>
"#,
    )
    .expect("nuspec parses")
}

fn nuspec_with(body: &str) -> PackageNuspec {
    parse_nuspec(&format!(
        r#"
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>Pkg</id><version>1.0.0</version>{body}</metadata>
</package>
"#
    ))
    .expect("nuspec parses")
}

fn select(files: &[&str], target: &str) -> CompileAssets {
    select_with(files, &bare_nuspec(), target)
}

fn select_with(files: &[&str], nuspec: &PackageNuspec, target: &str) -> CompileAssets {
    let files = files.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
    select_compile_assets(&files, nuspec, &framework(target)).expect("selection succeeds")
}

fn decline(files: &[&str], target: &str) -> AssetSelectionDecline {
    let files = files.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
    select_compile_assets(&files, &bare_nuspec(), &framework(target))
        .expect_err("selection declines")
}

fn items(assets: &CompileAssets) -> Vec<&str> {
    assets.items.iter().map(String::as_str).collect()
}

#[test]
fn selects_the_nearest_compatible_lib_folder() {
    let assets = select(
        &[
            "lib/net472/old.dll",
            "lib/netstandard2.0/portable.dll",
            "lib/net6.0/modern.dll",
        ],
        "net8.0",
    );

    assert_eq!(items(&assets), ["lib/net6.0/modern.dll"]);
}

#[test]
fn ref_takes_precedence_over_lib() {
    let assets = select(&["ref/net8.0/api.dll", "lib/net8.0/impl.dll"], "net8.0");

    assert_eq!(items(&assets), ["ref/net8.0/api.dll"]);
}

#[test]
fn an_incompatible_ref_folder_does_not_suppress_lib() {
    let assets = select(&["ref/net472/api.dll", "lib/net8.0/impl.dll"], "net8.0");

    assert_eq!(items(&assets), ["lib/net8.0/impl.dll"]);
}

/// The rule that would be easiest to get wrong by simplifying: `ref` beats
/// `lib` at the level of *groups*, not of assemblies. A `ref/` folder that is
/// compatible but holds no assembly still wins — and contributes nothing — so
/// the package compiles against nothing at all rather than falling back to its
/// `lib/` implementation.
#[test]
fn a_compatible_but_assembly_less_ref_folder_suppresses_lib_entirely() {
    let assets = select(&["ref/net8.0/readme.txt", "lib/net8.0/impl.dll"], "net8.0");

    assert!(items(&assets).is_empty());
}

#[test]
fn placeholder_folders_are_selected_and_are_not_assemblies() {
    let assets = select(&["ref/net8.0/_._", "lib/net8.0/impl.dll"], "net8.0");

    assert_eq!(items(&assets), ["ref/net8.0/_._"]);
    assert_eq!(assets.assemblies().count(), 0);
}

#[test]
fn selects_assemblies_by_extension_only() {
    let assets = select(
        &[
            "lib/net8.0/a.dll",
            "lib/net8.0/b.exe",
            "lib/net8.0/c.winmd",
            "lib/net8.0/d.xml",
            "lib/net8.0/e.pdb",
        ],
        "net8.0",
    );

    assert_eq!(
        items(&assets),
        ["lib/net8.0/a.dll", "lib/net8.0/b.exe", "lib/net8.0/c.winmd"]
    );
}

#[test]
fn assets_in_a_subfolder_of_the_framework_folder_are_not_compile_assets() {
    let assets = select(
        &["lib/net8.0/sub/nested.dll", "lib/net8.0/top.dll"],
        "net8.0",
    );

    assert_eq!(items(&assets), ["lib/net8.0/top.dll"]);
}

#[test]
fn files_in_the_package_root_are_not_assets() {
    let assets = select(&["Pkg.nuspec", "stray.dll", "lib/net8.0/a.dll"], "net8.0");

    assert_eq!(items(&assets), ["lib/net8.0/a.dll"]);
}

/// Folder names are matched case-insensitively, so two spellings of one TFM are
/// one group — and both their assemblies are selected.
#[test]
fn framework_folders_differing_only_in_case_are_one_group() {
    let assets = select(&["lib/NET8.0/a.dll", "lib/net8.0/b.dll"], "net8.0");

    assert_eq!(items(&assets), ["lib/NET8.0/a.dll", "lib/net8.0/b.dll"]);
}

/// The pre-TFM layout: a package that predates framework folders puts its
/// assemblies straight in `lib/`, and they are treated as `.NETFramework`
/// v0.0 — usable by a .NET Framework project, and by nothing else.
#[test]
fn assemblies_directly_in_lib_are_net_framework_assets() {
    let assets = select(&["lib/legacy.dll"], "net472");
    assert_eq!(items(&assets), ["lib/legacy.dll"]);

    let assets = select(&["lib/legacy.dll"], "net8.0");
    assert!(items(&assets).is_empty());
}

/// `any` is a replacement token in NuGet's pattern table, and the table is keyed
/// *ordinally*: `lib/any/` is the .NETPlatform 5.0 framework (compatible with
/// nothing modern), while `lib/Any/` misses the table, parses as NuGet's Any
/// framework, and is compatible with everything.
#[test]
fn the_any_folder_means_different_things_in_different_cases() {
    assert!(items(&select(&["lib/any/a.dll"], "net8.0")).is_empty());
    assert_eq!(
        items(&select(&["lib/Any/a.dll"], "net8.0")),
        ["lib/Any/a.dll"]
    );
}

/// `lib/any/` is `.NETPlatform` at the *empty* version, not at 5.0 — so a
/// package holding both `lib/any/` and `lib/dotnet5.0/` has two groups, and the
/// nearer one wins outright. (With only one of them present the distinction is
/// invisible: both are compatible with exactly the same projects. This case is
/// the one that tells them apart, and the corpus differential is what found it.)
#[test]
fn the_any_folder_is_dotnet_at_the_empty_version() {
    let assets = select(&["lib/any/legacy.dll", "lib/dotnet5.0/modern.dll"], "net48");

    assert_eq!(items(&assets), ["lib/dotnet5.0/modern.dll"]);
}

/// An unrecognised folder name becomes a framework named after itself, which is
/// compatible with nothing — it does not poison the rest of the package.
#[test]
fn unparseable_framework_folders_are_ignored() {
    let assets = select(&["lib/not-a-tfm/a.dll", "lib/net8.0/b.dll"], "net8.0");

    assert_eq!(items(&assets), ["lib/net8.0/b.dll"]);
}

/// NuGet's group selection reads a hash dictionary, so when two compatible
/// groups tie it returns whichever the package's file order happened to insert
/// first: for these files the real `ContentItemCollection` selects `lib/net/a.dll`
/// or `lib/b.dll` depending on nothing but enumeration order. There is no right
/// answer to reproduce, so we decline.
#[test]
fn declines_when_two_asset_groups_tie() {
    let decline = decline(&["lib/net/a.dll", "lib/b.dll"], "net472");

    assert!(
        matches!(decline, AssetSelectionDecline::AmbiguousAssetGroup { .. }),
        "expected an ambiguity decline, got {decline:?}"
    );
}

#[test]
fn declines_on_lib_contract_packages() {
    let decline = decline(&["lib/contract/Pkg.dll", "lib/net8.0/a.dll"], "net8.0");

    assert!(
        matches!(decline, AssetSelectionDecline::LibContract { .. }),
        "expected a lib/contract decline, got {decline:?}"
    );
}

#[test]
fn declines_on_project_frameworks_outside_the_envelope() {
    let decline = decline(&["lib/net8.0/a.dll"], "uap10.0");

    assert!(
        matches!(
            decline,
            AssetSelectionDecline::UnsupportedProjectFramework { .. }
        ),
        "expected an unsupported-framework decline, got {decline:?}"
    );
}

// ============================================================================
// The nuspec <references> allow-list
// ============================================================================

#[test]
fn a_flat_reference_list_filters_lib_assemblies() {
    let nuspec = nuspec_with(r#"<references><reference file="a.dll" /></references>"#);
    let assets = select_with(&["lib/net8.0/a.dll", "lib/net8.0/b.dll"], &nuspec, "net8.0");

    assert_eq!(items(&assets), ["lib/net8.0/a.dll"]);
}

#[test]
fn reference_groups_are_selected_by_target_framework() {
    let nuspec = nuspec_with(
        r#"<references>
             <group targetFramework="net8.0"><reference file="modern.dll" /></group>
             <group targetFramework="net472"><reference file="legacy.dll" /></group>
           </references>"#,
    );
    let assets = select_with(
        &["lib/net8.0/modern.dll", "lib/net8.0/legacy.dll"],
        &nuspec,
        "net8.0",
    );

    assert_eq!(items(&assets), ["lib/net8.0/modern.dll"]);
}

/// The filter is scoped to `lib/`: a `ref/` assembly is always referenced.
#[test]
fn the_reference_filter_does_not_touch_ref_assemblies() {
    let nuspec = nuspec_with(r#"<references><reference file="a.dll" /></references>"#);
    let assets = select_with(&["ref/net8.0/a.dll", "ref/net8.0/b.dll"], &nuspec, "net8.0");

    assert_eq!(items(&assets), ["ref/net8.0/a.dll", "ref/net8.0/b.dll"]);
}

#[test]
fn a_reference_group_that_matches_no_framework_filters_nothing() {
    let nuspec = nuspec_with(
        r#"<references>
             <group targetFramework="net472"><reference file="legacy.dll" /></group>
           </references>"#,
    );
    let assets = select_with(&["lib/net8.0/a.dll"], &nuspec, "net8.0");

    assert_eq!(items(&assets), ["lib/net8.0/a.dll"]);
}

#[test]
fn reference_file_names_are_matched_case_insensitively() {
    let nuspec = nuspec_with(r#"<references><reference file="A.DLL" /></references>"#);
    let assets = select_with(&["lib/net8.0/a.dll", "lib/net8.0/b.dll"], &nuspec, "net8.0");

    assert_eq!(items(&assets), ["lib/net8.0/a.dll"]);
}

/// NuGet matches reference names with `OrdinalIgnoreCase`, whose case relation
/// Rust does not have — `eq_ignore_ascii_case` misses `Ä`/`ä` (which NuGet
/// folds), and `to_lowercase` folds `ı`/`i` and Kelvin `K`/`k` (which NuGet does
/// not). So we answer where the two provably agree, and decline otherwise: an
/// ordinally-equal name matches, a name that differs even under Rust's *fuller*
/// folding cannot match, and a non-ASCII case difference is undecidable.
#[test]
fn non_ascii_reference_names_are_matched_only_where_nuget_and_rust_agree() {
    // Ordinally equal: unambiguous, whatever the case relation.
    let nuspec = nuspec_with(r#"<references><reference file="ä.dll" /></references>"#);
    let assets = select_with(&["lib/net8.0/ä.dll", "lib/net8.0/b.dll"], &nuspec, "net8.0");
    assert_eq!(items(&assets), ["lib/net8.0/ä.dll"]);

    // Different under Rust's full folding, so different under NuGet's simple
    // one too: filtered out, no decline needed.
    let nuspec = nuspec_with(r#"<references><reference file="ä.dll" /></references>"#);
    let assets = select_with(&["lib/net8.0/ö.dll"], &nuspec, "net8.0");
    assert!(items(&assets).is_empty());

    // Differs only by non-ASCII case: NuGet keeps it, and we cannot show that
    // our case relation is NuGet's, so we decline rather than guess.
    let nuspec = nuspec_with(r#"<references><reference file="Ä.dll" /></references>"#);
    let files = ["lib/net8.0/ä.dll".to_owned()];
    let decline = select_compile_assets(&files, &nuspec, &framework("net8.0"))
        .expect_err("undecidable reference name declines");
    assert!(
        matches!(
            decline,
            AssetSelectionDecline::UndecidableReferenceName { .. }
        ),
        "expected an undecidable-reference decline, got {decline:?}"
    );
}

/// `.NET`'s `OrdinalIgnoreCase` folds by *uppercasing*, and case folding is not
/// symmetric: `σ` and `ς` both uppercase to `Σ`, so NuGet calls them equal even
/// though their lowercase forms differ. Comparing lowercased strings would drop
/// an asset restore keeps.
#[test]
fn a_greek_final_sigma_is_undecidable_rather_than_a_non_match() {
    let nuspec = nuspec_with(r#"<references><reference file="σ.dll" /></references>"#);
    let files = ["lib/net8.0/ς.dll".to_owned()];

    let decline = select_compile_assets(&files, &nuspec, &framework("net8.0"))
        .expect_err("undecidable reference name declines");
    assert!(
        matches!(
            decline,
            AssetSelectionDecline::UndecidableReferenceName { .. }
        ),
        "expected an undecidable-reference decline, got {decline:?}"
    );
}

/// Conversely, NuGet's mapping is the *simple* one, so it does **not** fold the
/// Kelvin sign into an ASCII `k` — where Rust's `to_uppercase` agrees, we can
/// answer, and the asset is filtered out with no decline at all.
#[test]
fn the_kelvin_sign_is_not_an_ascii_k() {
    let nuspec = nuspec_with(r#"<references><reference file="KelvinK.dll" /></references>"#);
    let assets = select_with(&["lib/net8.0/Kelvin\u{212a}.dll"], &nuspec, "net8.0");

    assert!(items(&assets).is_empty());
}

// ============================================================================
// The OPC packaging apparatus
// ============================================================================

/// Restore strips the OPC parts a `.nupkg` carries before the content model sees
/// them. It matters because of the empty-ref-group rule: left in, a `.psmdcp`
/// would make `ref/net8.0/` a compatible-but-assembly-less group and cost the
/// package every compile asset it has. (Pinned against a real `dotnet restore`
/// in `compile_assets_restore.rs`.)
#[test]
fn opc_parts_are_not_assets() {
    let assets = select(
        &[
            "ref/net8.0/marker.psmdcp",
            "_rels/.rels",
            "[Content_Types].xml",
            "lib/net8.0/impl.dll",
        ],
        "net8.0",
    );

    assert_eq!(items(&assets), ["lib/net8.0/impl.dll"]);
}

/// The allow-list only filters `lib/`, so a non-ASCII name under `ref/` is never
/// compared and never forces a decline.
#[test]
fn non_ascii_ref_assets_are_never_compared_against_the_allow_list() {
    let nuspec = nuspec_with(r#"<references><reference file="Ä.dll" /></references>"#);
    let assets = select_with(&["ref/net8.0/ä.dll"], &nuspec, "net8.0");

    assert_eq!(items(&assets), ["ref/net8.0/ä.dll"]);
}

// ============================================================================
// Reading a package off disk
// ============================================================================

#[test]
fn lists_package_files_as_relative_slash_separated_paths() {
    let root = tempdir("list-package-files");
    std::fs::create_dir_all(root.join("lib").join("net8.0")).unwrap();
    std::fs::create_dir_all(root.join("ref").join("net8.0")).unwrap();
    std::fs::write(root.join("pkg.nuspec"), "").unwrap();
    std::fs::write(root.join("lib").join("net8.0").join("a.dll"), "").unwrap();
    std::fs::write(root.join("ref").join("net8.0").join("a.dll"), "").unwrap();

    let files = borzoi_nuget::list_package_files(&root).expect("lists files");

    assert_eq!(
        files,
        ["lib/net8.0/a.dll", "pkg.nuspec", "ref/net8.0/a.dll"]
    );

    let assets = select_compile_assets(&files, &bare_nuspec(), &framework("net8.0"))
        .expect("selection succeeds");
    assert_eq!(items(&assets), ["ref/net8.0/a.dll"]);
    assert_eq!(
        assets.assembly_paths(&root),
        [root.join("ref").join("net8.0").join("a.dll")]
    );

    std::fs::remove_dir_all(&root).unwrap();
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "borzoi-nuget-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}
