//! Property tests for compile-asset selection, pure Rust side (no oracle).
//!
//! NuGet fidelity is `compile_assets_diff.rs`'s and `compile_assets_restore.rs`'
//! job. These pin the oracle-independent invariants — the things that must hold
//! of *any* answer we give, whatever the compatibility tables say — so a
//! regression is caught even with the oracle unavailable.
//!
//! The load-bearing one is order-independence. NuGet's own selection is *not*
//! order-independent (it reads a hash dictionary, so a tie resolves by
//! insertion order), and the entire justification for answering at all is that
//! we decline precisely where that happens. Ours must therefore be a function
//! of the file *set*, not the file list.

use borzoi_nuget::{
    AssetSelectionDecline, CompileAssets, NuGetFramework, PackageNuspec, parse_nuspec,
    select_compile_assets,
};
use proptest::prelude::*;

const PROJECT_FRAMEWORKS: &[&str] = &[
    "net10.0",
    "net8.0",
    "net6.0",
    "netcoreapp3.1",
    "net472",
    "net45",
    "netstandard2.0",
    "netstandard1.6",
];

const FOLDER_TFMS: &[&str] = &[
    "net8.0",
    "net6.0",
    "netstandard2.0",
    "netstandard1.3",
    "netcoreapp3.1",
    "net472",
    "net45",
    "net",
    "NET8.0",
    "any",
    "Any",
    "monoandroid90",
    "not-a-tfm",
];

const FILE_NAMES: &[&str] = &[
    "Alpha.dll",
    "Beta.DLL",
    "Gamma.exe",
    "Delta.winmd",
    "Alpha.xml",
    "readme.txt",
    "_._",
    "sub/Nested.dll",
];

/// Roots that can bear a compile asset.
const COMPILE_ROOTS: &[&str] = &["lib", "ref", "LIB", "Ref"];

/// Roots that cannot: whatever they contain, the compile selection must not see
/// it.
const INERT_ROOTS: &[&str] = &[
    "build",
    "buildTransitive",
    "runtimes/win-x64/lib",
    "runtimes/win-x64/native",
    "contentFiles/any",
    "tools",
    "embed",
    "analyzers/dotnet/cs",
];

fn project_framework() -> impl Strategy<Value = NuGetFramework> {
    proptest::sample::select(PROJECT_FRAMEWORKS)
        .prop_map(|name| NuGetFramework::parse(name).expect("framework parses"))
}

/// A compile-bearing package file: `{lib|ref}/[{tfm}/]{name}`.
fn compile_file() -> impl Strategy<Value = String> {
    (
        proptest::sample::select(COMPILE_ROOTS),
        proptest::option::of(proptest::sample::select(FOLDER_TFMS)),
        proptest::sample::select(FILE_NAMES),
    )
        .prop_map(|(root, tfm, name)| match tfm {
            Some(tfm) => format!("{root}/{tfm}/{name}"),
            None => format!("{root}/{name}"),
        })
        // `lib/contract` is a decline of its own, and would make every property
        // below vacuous rather than false; the unit tests own that case.
        .prop_filter("not a lib/contract package", |path| {
            !path.starts_with("lib/contract")
        })
}

/// A file that cannot be a compile asset: package-root bookkeeping, or anything
/// under a root the compile patterns do not match.
fn inert_file() -> impl Strategy<Value = String> {
    prop_oneof![
        proptest::sample::select(&["Pkg.nuspec", ".nupkg.metadata", "Icon.png", "LICENSE"][..])
            .prop_map(str::to_owned),
        (
            proptest::sample::select(INERT_ROOTS),
            proptest::sample::select(FOLDER_TFMS),
            proptest::sample::select(FILE_NAMES),
        )
            .prop_map(|(root, tfm, name)| format!("{root}/{tfm}/{name}")),
    ]
}

fn package_files() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(compile_file(), 0..8)
}

fn bare_nuspec() -> PackageNuspec {
    parse_nuspec(
        r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
             <metadata><id>Pkg</id><version>1.0.0</version></metadata>
           </package>"#,
    )
    .expect("nuspec parses")
}

/// The outcome, collapsed to something comparable across runs.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Selected(Vec<String>),
    Declined(&'static str),
}

fn select(files: &[String], project: &NuGetFramework) -> Outcome {
    match select_compile_assets(files, &bare_nuspec(), project) {
        Ok(assets) => Outcome::Selected(assets.items),
        Err(decline) => Outcome::Declined(match decline {
            AssetSelectionDecline::UnsupportedProjectFramework { .. } => "unsupported-framework",
            AssetSelectionDecline::AmbiguousAssetGroup { .. } => "ambiguous-group",
            AssetSelectionDecline::LibContract { .. } => "lib-contract",
            AssetSelectionDecline::UndecidableReferenceName { .. } => "undecidable-reference",
            AssetSelectionDecline::NonUtf8PackageFile { .. } => "non-utf8",
            AssetSelectionDecline::Io { .. } => "io",
        }),
    }
}

proptest! {
    /// The answer is a function of the file *set*. This is the invariant that
    /// licenses the whole module: NuGet's own answer is not, and we decline
    /// exactly where it is not.
    #[test]
    fn selection_does_not_depend_on_file_order(
        files in package_files(),
        project in project_framework(),
        rotation in 0usize..8,
    ) {
        let forward = select(&files, &project);

        let mut reversed = files.clone();
        reversed.reverse();
        prop_assert_eq!(&forward, &select(&reversed, &project));

        let mut rotated = files.clone();
        if !rotated.is_empty() {
            let by = rotation % rotated.len();
            rotated.rotate_left(by);
        }
        prop_assert_eq!(&forward, &select(&rotated, &project));
    }

    /// Nothing is invented: every selected path is one of the package's files,
    /// and is an assembly (or the placeholder that stands in for one).
    #[test]
    fn selected_items_are_assemblies_the_package_actually_ships(
        files in package_files(),
        project in project_framework(),
    ) {
        let Outcome::Selected(items) = select(&files, &project) else {
            return Ok(());
        };

        for item in &items {
            prop_assert!(files.contains(item), "{item} is not a package file");

            let name = item.rsplit('/').next().expect("item has a name");
            let is_assembly = name == "_._"
                || [".dll", ".exe", ".winmd"]
                    .iter()
                    .any(|extension| name.to_ascii_lowercase().ends_with(extension));
            prop_assert!(is_assembly, "{item} is not an assembly");
        }
    }

    /// A selection comes from exactly one asset group, so every item shares one
    /// root (`ref` beats `lib` wholesale — they are never mixed) and one
    /// framework folder. Case is not part of that: `lib/NET8.0` and `lib/net8.0`
    /// are one group, so the folder *names* may differ where the frameworks do
    /// not.
    #[test]
    fn selected_items_come_from_a_single_asset_group(
        files in package_files(),
        project in project_framework(),
    ) {
        let Outcome::Selected(items) = select(&files, &project) else {
            return Ok(());
        };

        let roots = items
            .iter()
            .map(|item| item.split('/').next().expect("item has a root").to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        prop_assert!(roots.len() <= 1, "items span several roots: {items:?}");

        // The folder each item sits in, as a framework: `lib/net8.0/A.dll` is
        // net8.0, and the pre-TFM `lib/A.dll` is .NETFramework v0.0.
        let frameworks = items
            .iter()
            .map(|item| {
                let segments = item.split('/').collect::<Vec<_>>();
                match segments.len() {
                    2 => NuGetFramework::parse_folder("net").expect("net parses"),
                    _ => NuGetFramework::parse_folder(segments[1])
                        .unwrap_or_else(|_| NuGetFramework::parse_folder("net").expect("parses")),
                }
            })
            .collect::<Vec<_>>();
        for framework in &frameworks {
            prop_assert!(
                framework == &frameworks[0],
                "items span several frameworks: {items:?}"
            );
        }
    }

    /// Compile selection reads `lib/` and `ref/` and nothing else: adding
    /// build assets, content files, RID-specific runtime assemblies, analyzers,
    /// or package-root bookkeeping cannot change the answer.
    #[test]
    fn inert_files_never_change_the_selection(
        files in package_files(),
        inert in proptest::collection::vec(inert_file(), 0..5),
        project in project_framework(),
    ) {
        let before = select(&files, &project);

        let mut with_inert = files.clone();
        with_inert.extend(inert);
        with_inert.sort();

        prop_assert_eq!(before, select(&with_inert, &project));
    }

    /// `assemblies()` is `items` minus the placeholders — and a placeholder is
    /// never a path a compiler could be handed.
    #[test]
    fn assemblies_drops_exactly_the_placeholders(
        files in package_files(),
        project in project_framework(),
    ) {
        let Ok(assets) = select_compile_assets(&files, &bare_nuspec(), &project) else {
            return Ok(());
        };

        let dropped = assets.items.len() - assets.assemblies().count();
        let placeholders = assets
            .items
            .iter()
            .filter(|item| item.ends_with("/_._"))
            .count();
        prop_assert_eq!(dropped, placeholders);
        prop_assert!(assets.assemblies().all(|item| !item.ends_with("_._")));

        // And the paths handed out resolve under the package directory.
        let package_dir = std::path::Path::new("/packages/pkg/1.0.0");
        let resolved: Vec<_> = assets.assembly_paths(package_dir);
        prop_assert_eq!(resolved.len(), assets.assemblies().count());
        prop_assert!(resolved.iter().all(|path| path.starts_with(package_dir)));
    }
}

/// A regression net for the `CompileAssets` accessors themselves, which the
/// properties above lean on.
#[test]
fn compile_assets_accessors_agree() {
    let assets = CompileAssets {
        items: vec!["lib/net8.0/_._".to_owned(), "lib/net8.0/A.dll".to_owned()],
    };

    assert_eq!(
        assets.assemblies().collect::<Vec<_>>(),
        ["lib/net8.0/A.dll"]
    );
    assert_eq!(
        assets.assembly_paths(std::path::Path::new("/pkg")),
        [std::path::Path::new("/pkg/lib/net8.0/A.dll")]
    );
}
