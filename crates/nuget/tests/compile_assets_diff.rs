//! Differential tests: compile-asset selection vs the real NuGet content model
//! (`ManagedCodeConventions` + `ContentItemCollection` + `LockFileUtils`'
//! compile rules) via `tools/nuget-oracle`'s `selectCompileAssets`.
//!
//! Two properties, and the second is the load-bearing one:
//!
//! - **Agreement.** Whenever we produce a selection, it is NuGet's, exactly.
//! - **Order-invariance.** Whenever we produce a selection, NuGet's answer does
//!   not depend on the order the package's files were listed in. This is what
//!   licenses us to answer at all: NuGet's group selection reads a hash
//!   dictionary, so on a tie *its own* answer moves with file order. We decline
//!   on ties; this asserts we decline on **every** tie, by re-asking the oracle
//!   with the file list reversed and requiring the same answer back.
//!
//! Fixed seeds, so a failure reproduces exactly; `soak.rs` owns fresh-seed
//! exploration.

mod common;

use borzoi_nuget::{
    AssetSelectionDecline, NuGetFramework, PackageNuspec, parse_nuspec, select_compile_assets,
};
use common::{Oracle, SplitMix64};

/// The project frameworks we resolve for: the `.NETFramework` /`.NETCoreApp` /
/// `.NETStandard` envelope, spread across the generations that actually differ
/// in what they can consume.
const PROJECT_FRAMEWORKS: &[&str] = &[
    "net8.0",
    "net10.0",
    "net6.0",
    "net5.0",
    "netcoreapp3.1",
    "netcoreapp2.1",
    "net472",
    "net48",
    "net45",
    "net40",
    "netstandard2.0",
    "netstandard2.1",
    "netstandard1.3",
    "netstandard1.0",
    "netcoreapp1.0",
    "net9.0-windows",
];

/// Folder names that could appear as the `{tfm}` segment of an asset path:
/// real TFMs across every generation, the two `any` spellings, the pre-TFM
/// `net`, legacy families, and things that are not frameworks at all.
const FOLDER_TFMS: &[&str] = &[
    "net8.0",
    "net6.0",
    "net5.0",
    "net10.0",
    "NET8.0",
    "netstandard2.0",
    "netstandard2.1",
    "netstandard1.0",
    "netstandard1.6",
    "netcoreapp2.0",
    "netcoreapp3.1",
    "net45",
    "net461",
    "net472",
    "net48",
    "net20",
    "net",
    "any",
    "Any",
    "net8.0-windows",
    "net6.0-android",
    "portable-net45+win8",
    "uap10.0",
    "monoandroid90",
    "xamarinios10",
    "dotnet5.6",
    "dotnet5.0",
    "dotnet",
    "sl5",
    "wp8",
    "win8",
    "tizen40",
    "netcore50",
    "native",
    "not-a-tfm",
    "contract",
    "contracts",
    "_._",
    "sub",
    "netstandard2.0.3",
    "NETSTANDARD2.0",
];

/// File names within an asset folder: assemblies in every accepted extension
/// and case, the empty-folder placeholder, non-assemblies, and a nested path.
const FILE_NAMES: &[&str] = &[
    "Alpha.dll",
    "Beta.dll",
    "alpha.DLL",
    // Non-ASCII names, whose case relation NuGet's `OrdinalIgnoreCase` and
    // Rust's folding disagree about. The reference filter is the only place
    // that compares them; everywhere else NuGet's comparisons turn out to be
    // ASCII-only (`lıb/` and `.dlſ` match nothing — checked against the real
    // content model), which is what licenses the ASCII tests in `assets.rs`.
    "Ärger.dll",
    "ärger.dll",
    "Kelvin\u{212a}.dll",
    // `σ` and `ς` uppercase to the same `Σ`, so NuGet folds them while their
    // *lowercase* forms differ — the pair that proves the comparison must fold
    // upwards.
    "σigma.dll",
    "ςigma.dll",
    // The OPC packaging apparatus, which restore strips before the content model
    // runs. Inside a `ref/` folder it would otherwise form an assembly-less
    // group and suppress `lib/` entirely.
    "marker.psmdcp",
    "Gamma.exe",
    "Delta.winmd",
    "Alpha.xml",
    "readme.txt",
    "_._",
    "sub/Nested.dll",
    "Alpha.dll.config",
    "resources.resources.dll",
];

/// The folder an asset hangs off. Only `lib` and `ref` bear compile assets, but
/// the rest must be *inert*, which is worth generating.
const ROOTS: &[&str] = &[
    "lib",
    "ref",
    "lib",
    "ref",
    "LIB",
    "Ref",
    "runtimes/win-x64/lib",
    "build",
    "embed",
    "tools",
    "contentFiles/any",
];

/// Files in the package root: never assets, always present in a real package.
const ROOT_FILES: &[&str] = &[
    "Pkg.nuspec",
    ".nupkg.metadata",
    "[Content_Types].xml",
    "_rels/.rels",
    "Icon.png",
];

fn framework(name: &str) -> NuGetFramework {
    NuGetFramework::parse(name).expect("framework parses")
}

/// A generated package layout: its files, and the nuspec that goes with them.
struct Package {
    files: Vec<String>,
    nuspec_xml: String,
    nuspec: PackageNuspec,
}

/// TFM folders a package plausibly *multi-targets*. Drawing from these keeps
/// the corpus dense in the case that actually exercises the hard part — several
/// mutually-compatible candidate folders, one of which NuGet's reducer must
/// pick — rather than in packages that match nothing.
const PLAUSIBLE_TFMS: &[&str] = &[
    "net8.0",
    "net6.0",
    "net5.0",
    "net10.0",
    "netstandard2.0",
    "netstandard2.1",
    "netstandard1.6",
    "netstandard1.0",
    "netcoreapp3.1",
    "netcoreapp2.0",
    "net45",
    "net461",
    "net472",
    "net48",
    "net20",
    "net8.0-windows",
];

fn gen_package(rng: &mut SplitMix64) -> Package {
    let mut files: Vec<String> = Vec::new();

    if rng.below(4) > 0 {
        files.push((*rng.pick(ROOT_FILES)).to_owned());
    }

    // Most packages are a fan of framework folders under `lib` (and sometimes a
    // parallel `ref` fan); the rest are adversarial soup over the full folder
    // and file-name zoo.
    if rng.below(10) < 6 {
        // What a real framework folder holds: an assembly or two, usually with
        // its doc file, and occasionally the empty-folder placeholder instead.
        const CONTENTS: &[&str] = &[
            "Pkg.dll",
            "Pkg.dll",
            "Pkg.dll",
            "Alpha.dll",
            "Beta.dll",
            "Gamma.exe",
            "Pkg.xml",
            "_._",
            "readme.txt",
        ];

        let fan = 1 + rng.below(4);
        let with_ref = rng.below(4) == 0;
        for _ in 0..fan {
            let tfm = rng.pick(PLAUSIBLE_TFMS);
            for _ in 0..1 + rng.below(2) {
                files.push(format!("lib/{tfm}/{}", rng.pick(CONTENTS)));
            }
            if with_ref {
                files.push(format!("ref/{tfm}/{}", rng.pick(CONTENTS)));
            }
        }
        // Inert noise: assets under roots that bear no compile assets at all.
        if rng.below(3) == 0 {
            let root = rng.pick(ROOTS);
            let tfm = rng.pick(PLAUSIBLE_TFMS);
            files.push(format!("{root}/{tfm}/{}", rng.pick(FILE_NAMES)));
        }
    } else {
        let count = 1 + rng.below(6);
        for _ in 0..count {
            let root = rng.pick(ROOTS);
            let name = rng.pick(FILE_NAMES);
            // A quarter of assets skip the framework folder entirely: the
            // pre-TFM `lib/Alpha.dll` shape.
            if rng.below(4) == 0 {
                files.push(format!("{root}/{name}"));
            } else {
                let tfm = rng.pick(FOLDER_TFMS);
                files.push(format!("{root}/{tfm}/{name}"));
            }
        }
    }

    files.sort();
    files.dedup();

    let references = gen_references(rng);
    let nuspec_xml = format!(
        "<?xml version=\"1.0\"?>\
         <package xmlns=\"http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd\">\
         <metadata><id>Pkg</id><version>1.0.0</version><authors>a</authors>\
         <description>d</description>{references}</metadata></package>"
    );
    let nuspec = parse_nuspec(&nuspec_xml).expect("generated nuspec parses");

    Package {
        files,
        nuspec_xml,
        nuspec,
    }
}

/// A `<references>` allow-list, present on a minority of packages (as in life).
fn gen_references(rng: &mut SplitMix64) -> String {
    const REFERENCED: &[&str] = &[
        "Alpha.dll",
        "Beta.dll",
        "ALPHA.DLL",
        "Missing.dll",
        "_._",
        "Ärger.dll",
        "ärger.dll",
        "ÄRGER.DLL",
        "σigma.dll",
        "ςigma.dll",
        "Σigma.dll",
        // An *ASCII* name against the Kelvin-sign asset above: Rust's folding
        // equates them (U+212A lowercases to `k`), NuGet's does not. Naively
        // reaching for `to_lowercase` here would keep an asset restore drops —
        // an over-resolution — so this must land on the undecidable arm.
        "KelvinK.dll",
    ];

    match rng.below(10) {
        0..=6 => String::new(),
        7 => {
            let file = rng.pick(REFERENCED);
            format!("<references><reference file=\"{file}\" /></references>")
        }
        8 => {
            let (a, b) = (rng.pick(REFERENCED), rng.pick(REFERENCED));
            format!("<references><reference file=\"{a}\" /><reference file=\"{b}\" /></references>")
        }
        _ => {
            let tfm = rng.pick(&["net8.0", "netstandard2.0", "net472", ""]);
            let file = rng.pick(REFERENCED);
            format!(
                "<references><group targetFramework=\"{tfm}\">\
                 <reference file=\"{file}\" /></group></references>"
            )
        }
    }
}

fn oracle_items(
    oracle: &mut Oracle,
    framework: &str,
    files: &[String],
    nuspec: &str,
) -> Vec<String> {
    let response = oracle.request(&serde_json::json!({
        "op": "selectCompileAssets",
        "framework": framework,
        "files": files,
        "nuspec": nuspec,
    }));
    assert!(
        response["ok"].as_bool().unwrap_or(false),
        "oracle rejected framework={framework:?} files={files:?}"
    );
    let mut items = response["items"]
        .as_array()
        .expect("oracle items array")
        .iter()
        .map(|item| item.as_str().expect("item is a string").to_owned())
        .collect::<Vec<_>>();
    items.sort();
    items
}

#[test]
fn compile_asset_selection_matches_the_nuget_content_model() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x5eed_a55e_7000);
    let mut mismatches: Vec<String> = Vec::new();
    let mut declines: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut compared = 0usize;
    let mut selected_something = 0usize;

    for _ in 0..4000 {
        let package = gen_package(&mut rng);
        let project = rng.pick(PROJECT_FRAMEWORKS);

        let ours = match select_compile_assets(&package.files, &package.nuspec, &framework(project))
        {
            Ok(assets) => assets,
            Err(decline) => {
                *declines.entry(decline_kind(&decline)).or_default() += 1;
                continue;
            }
        };
        compared += 1;
        if !ours.items.is_empty() {
            selected_something += 1;
        }

        let theirs = oracle_items(&mut oracle, project, &package.files, &package.nuspec_xml);
        if ours.items != theirs {
            mismatches.push(format!(
                "project={project} files={:?} nuspec={}\n  ours={:?}\n  nuget={theirs:?}",
                package.files, package.nuspec_xml, ours.items
            ));
            continue;
        }

        // Order-invariance: having answered, we are asserting NuGet has one
        // answer to give. Ask it again with the files reversed.
        let mut reversed = package.files.clone();
        reversed.reverse();
        let reordered = oracle_items(&mut oracle, project, &reversed, &package.nuspec_xml);
        if reordered != theirs {
            mismatches.push(format!(
                "NuGet's own selection moved with file order, so we should have declined:\n  \
                 project={project} files={:?}\n  forward={theirs:?}\n  reversed={reordered:?}",
                package.files
            ));
        }
    }

    eprintln!("compared={compared} selected={selected_something} declines={declines:?}");
    assert!(
        compared > 2500,
        "generator degenerated: only {compared} comparable cases (declines: {declines:?})"
    );
    assert!(
        selected_something > 1000,
        "generator degenerated: only {selected_something} cases selected any asset"
    );

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(15).cloned().collect::<Vec<_>>();
        panic!(
            "{} divergence(s) from the NuGet content model (declines: {declines:?}); first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}

/// The ambiguity decline, proved *necessary* rather than merely safe.
///
/// For each layout here, the real `ContentItemCollection` is asked twice — once
/// with the files in one order, once reversed — and returns **different compile
/// assets**. There is no answer to reproduce, so declining is the only correct
/// behaviour, and this asserts both halves of that: NuGet really does flip, and
/// we really do decline.
///
/// Every case is the same collision underneath. A folder named `net` parses to
/// `.NETFramework,Version=v0.0`, and so does the *defaulted* framework of the
/// pre-TFM `lib/{assembly}` pattern — so the two groups hold the same framework
/// while remaining distinct groups, and nothing can order them.
#[test]
fn declines_exactly_where_nugets_own_selection_is_order_dependent() {
    const TIED_LAYOUTS: &[(&str, &[&str])] = &[
        ("net472", &["lib/net/a.dll", "lib/b.dll"]),
        ("net45", &["lib/net/a.dll", "lib/b.dll"]),
        ("net40", &["lib/net/a.dll", "lib/b.dll"]),
        // The folder name's case is not what does it.
        ("net472", &["lib/NET/a.dll", "lib/b.dll"]),
        // Nor is the spelling: `net00` is .NETFramework v0.0 too.
        ("net472", &["lib/net00/a.dll", "lib/b.dll"]),
        ("net472", &["lib/net/a.dll", "lib/net/b.dll", "lib/c.dll"]),
        ("net472", &["lib/net/_._", "lib/b.dll"]),
    ];

    let mut oracle = Oracle::spawn();
    let nuspec_xml = gen_bare_nuspec();
    let nuspec = parse_nuspec(&nuspec_xml).expect("nuspec parses");

    for (project, layout) in TIED_LAYOUTS {
        let forward = layout.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
        let reversed = forward.iter().rev().cloned().collect::<Vec<_>>();

        let ours = select_compile_assets(&forward, &nuspec, &framework(project));
        assert!(
            matches!(ours, Err(AssetSelectionDecline::AmbiguousAssetGroup { .. })),
            "{layout:?} for {project}: expected an ambiguity decline, got {ours:?}"
        );

        let a = oracle_items(&mut oracle, project, &forward, &nuspec_xml);
        let b = oracle_items(&mut oracle, project, &reversed, &nuspec_xml);
        assert_ne!(
            a, b,
            "{layout:?} for {project}: NuGet's selection did *not* move with file order, so \
             this is not really a tie and the decline is unjustified — reclassify the case"
        );
    }
}

/// The declines must be *rare shapes*, not a way of dodging the differential.
/// Realistic package layouts — the ones a project actually restores — have to
/// resolve, so this corpus asserts a zero decline rate as well as agreement.
#[test]
fn realistic_package_layouts_never_decline() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x5eed_a55e_7001);
    let mut mismatches: Vec<String> = Vec::new();

    // The shapes real packages ship: framework-folder libs, ref/lib pairs,
    // placeholder folders, and the multi-targeted fan.
    const LAYOUTS: &[&[&str]] = &[
        &["lib/netstandard2.0/Pkg.dll"],
        &["lib/netstandard2.0/Pkg.dll", "lib/netstandard2.0/Pkg.xml"],
        &[
            "lib/net6.0/Pkg.dll",
            "lib/net8.0/Pkg.dll",
            "lib/netstandard2.0/Pkg.dll",
        ],
        &["ref/net8.0/Pkg.dll", "lib/net8.0/Pkg.dll"],
        &[
            "ref/netstandard2.0/Pkg.dll",
            "lib/net472/Pkg.dll",
            "lib/netstandard2.0/Pkg.dll",
        ],
        &["lib/net472/Pkg.dll", "lib/net48/Pkg.dll"],
        &["lib/netstandard2.0/_._"],
        &["ref/net8.0/_._", "lib/net8.0/Pkg.dll"],
        &["lib/net8.0/Pkg.dll", "runtimes/win-x64/lib/net8.0/Pkg.dll"],
        &[
            "lib/net8.0/Pkg.dll",
            "build/net8.0/Pkg.props",
            "contentFiles/any/any/x.txt",
        ],
        &[
            "lib/net8.0/A.dll",
            "lib/net8.0/B.dll",
            "lib/netstandard2.0/A.dll",
        ],
        &[
            "lib/netstandard1.0/Pkg.dll",
            "lib/netstandard1.6/Pkg.dll",
            "lib/net45/Pkg.dll",
        ],
        &["lib/net8.0/Pkg.dll", "lib/net8.0-windows/Pkg.dll"],
        &["tools/net8.0/any/tool.dll"],
        &[
            "lib/netstandard2.0/Pkg.dll",
            "lib/netstandard2.1/Pkg.dll",
            "lib/net6.0/Pkg.dll",
        ],
    ];

    let mut checked = 0usize;
    for layout in LAYOUTS {
        for project in PROJECT_FRAMEWORKS {
            let mut files = layout.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
            files.push("Pkg.nuspec".to_owned());
            files.sort();

            let package = Package {
                nuspec_xml: gen_bare_nuspec(),
                nuspec: parse_nuspec(&gen_bare_nuspec()).expect("nuspec parses"),
                files,
            };
            let _ = &mut rng;

            let ours = select_compile_assets(&package.files, &package.nuspec, &framework(project))
                .unwrap_or_else(|decline| {
                    panic!("realistic layout {layout:?} declined for {project}: {decline}")
                });
            checked += 1;

            let theirs = oracle_items(&mut oracle, project, &package.files, &package.nuspec_xml);
            if ours.items != theirs {
                mismatches.push(format!(
                    "project={project} layout={layout:?}\n  ours={:?}\n  nuget={theirs:?}",
                    ours.items
                ));
            }
        }
    }

    assert_eq!(checked, LAYOUTS.len() * PROJECT_FRAMEWORKS.len());
    assert!(
        mismatches.is_empty(),
        "{} divergence(s) on realistic layouts:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

fn gen_bare_nuspec() -> String {
    "<?xml version=\"1.0\"?>\
     <package xmlns=\"http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd\">\
     <metadata><id>Pkg</id><version>1.0.0</version><authors>a</authors>\
     <description>d</description></metadata></package>"
        .to_owned()
}

fn decline_kind(decline: &AssetSelectionDecline) -> &'static str {
    match decline {
        AssetSelectionDecline::UnsupportedProjectFramework { .. } => "unsupported-framework",
        AssetSelectionDecline::AmbiguousAssetGroup { .. } => "ambiguous-group",
        AssetSelectionDecline::LibContract { .. } => "lib-contract",
        AssetSelectionDecline::UndecidableReferenceName { .. } => "undecidable-reference",
        AssetSelectionDecline::NonUtf8PackageFile { .. } => "non-utf8",
        AssetSelectionDecline::Io { .. } => "io",
    }
}
