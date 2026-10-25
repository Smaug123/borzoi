//! End-to-end pin: compile-asset selection vs a **real `dotnet restore`**.
//!
//! `compile_assets_diff.rs` diffs us against NuGet's content model, but it
//! reaches that model through an oracle op *we* wrote — so it pins our
//! implementation of `LockFileUtils`' compile rules against our *reading* of
//! them (ref-over-lib precedence, the `<references>` filter, `_._`), not
//! against restore itself. If that reading were wrong, both sides would be
//! wrong together and agree.
//!
//! This closes the loop. Each fixture package is laid out in a private global
//! packages folder — the same on-disk shape [`list_package_files`] reads — and
//! a real `dotnet restore` is run against it, offline, with no feed at all
//! (restore consults the global packages folder first, so a committed package
//! there needs no source). The `compile` section restore writes into
//! `project.assets.json` is the ground truth, and it is exactly what the LSP
//! will consume.
//!
//! Every fixture must stay *compatible* with every project framework here:
//! a package with no compatible asset at all is NU1202, which fails the whole
//! restore. Hence the `netstandard2.0` floor in most layouts.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use borzoi_nuget::{
    NuGetFramework, NuGetVersion, PackageId, PackageIdentity, read_installed_package,
    select_installed_compile_assets,
};
use borzoi_oracle_harness::BoundedCommand;

/// A cold restore reads an SDK and writes a package graph; the bound is here to
/// stop a *stalled* one (a NuGet lock held by a concurrent run, say) hanging the
/// suite, not to police a slow one.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(900);

/// The project frameworks that restore offline in this repo's devshell — one
/// from each family the resolver serves. (A `net472` or `net8.0` project
/// normally drags in a reference-assembly or targeting-pack *package*; the
/// fixture projects turn those implicit references off, since none of this
/// needs to compile.)
const PROJECT_FRAMEWORKS: &[&str] = &["net10.0", "net8.0", "netstandard2.0", "net472"];

/// One fixture package: a file layout, and the nuspec body that goes with it.
struct Fixture {
    id: String,
    files: Vec<String>,
    nuspec_body: String,
}

/// The curated fixtures, as written below.
struct StaticFixture {
    id: &'static str,
    files: &'static [&'static str],
    nuspec_body: &'static str,
}

impl StaticFixture {
    fn to_fixture(&self) -> Fixture {
        Fixture {
            id: self.id.to_owned(),
            files: self.files.iter().map(|f| (*f).to_owned()).collect(),
            nuspec_body: self.nuspec_body.to_owned(),
        }
    }
}

const FIXTURES: &[StaticFixture] = &[
    StaticFixture {
        id: "Simple",
        files: &["lib/netstandard2.0/Simple.dll"],
        nuspec_body: "",
    },
    StaticFixture {
        id: "MultiTarget",
        files: &[
            "lib/netstandard2.0/MultiTarget.dll",
            "lib/net6.0/MultiTarget.dll",
            "lib/net8.0/MultiTarget.dll",
        ],
        nuspec_body: "",
    },
    // ref wins over lib where it is compatible, and only there.
    StaticFixture {
        id: "RefAndLib",
        files: &[
            "ref/net8.0/RefAndLib.dll",
            "lib/net8.0/RefAndLib.dll",
            "lib/netstandard2.0/RefAndLib.dll",
        ],
        nuspec_body: "",
    },
    // The rule this whole test exists for: a compatible `ref/` group holding no
    // assembly still wins, so a net8.0+ project compiles against *nothing* —
    // while net472/netstandard2.0, for which the ref group is not compatible,
    // fall through to lib.
    StaticFixture {
        id: "EmptyRefGroup",
        files: &[
            "ref/net8.0/readme.txt",
            "lib/net8.0/EmptyRefGroup.dll",
            "lib/netstandard2.0/EmptyRefGroup.dll",
        ],
        nuspec_body: "",
    },
    StaticFixture {
        id: "PlaceholderRef",
        files: &[
            "ref/net8.0/_._",
            "lib/net8.0/PlaceholderRef.dll",
            "lib/netstandard2.0/PlaceholderRef.dll",
        ],
        nuspec_body: "",
    },
    StaticFixture {
        id: "PlaceholderLib",
        files: &["lib/netstandard2.0/_._"],
        nuspec_body: "",
    },
    StaticFixture {
        id: "NonAssemblies",
        files: &[
            "lib/netstandard2.0/NonAssemblies.dll",
            "lib/netstandard2.0/NonAssemblies.xml",
            "lib/netstandard2.0/readme.txt",
            "lib/netstandard2.0/sub/Nested.dll",
        ],
        nuspec_body: "",
    },
    StaticFixture {
        id: "Extensions",
        files: &[
            "lib/netstandard2.0/Extensions.dll",
            "lib/netstandard2.0/Tool.exe",
            "lib/netstandard2.0/Component.winmd",
        ],
        nuspec_body: "",
    },
    // The pre-TFM shape: assemblies straight in `lib/`, which are .NETFramework
    // v0.0 — a net472 project prefers them over the netstandard folder.
    StaticFixture {
        id: "PreTfmLib",
        files: &["lib/PreTfmLib.dll", "lib/netstandard2.0/PreTfmLib.dll"],
        nuspec_body: "",
    },
    StaticFixture {
        id: "NetFrameworkFolder",
        files: &[
            "lib/net472/NetFrameworkFolder.dll",
            "lib/netstandard2.0/NetFrameworkFolder.dll",
        ],
        nuspec_body: "",
    },
    StaticFixture {
        id: "ReferencesFlat",
        files: &[
            "lib/netstandard2.0/ReferencesFlat.dll",
            "lib/netstandard2.0/Extra.dll",
        ],
        nuspec_body: r#"<references><reference file="ReferencesFlat.dll" /></references>"#,
    },
    StaticFixture {
        id: "ReferencesGrouped",
        files: &[
            "lib/net8.0/Alpha.dll",
            "lib/net8.0/Beta.dll",
            "lib/netstandard2.0/Alpha.dll",
            "lib/netstandard2.0/Beta.dll",
        ],
        nuspec_body: r#"<references>
             <group targetFramework="net8.0"><reference file="Beta.dll" /></group>
             <group targetFramework="netstandard2.0"><reference file="Alpha.dll" /></group>
           </references>"#,
    },
    // The allow-list is scoped to `lib/`: the ref assemblies survive it.
    StaticFixture {
        id: "ReferencesVersusRef",
        files: &[
            "ref/net8.0/Alpha.dll",
            "ref/net8.0/Beta.dll",
            "lib/netstandard2.0/Alpha.dll",
            "lib/netstandard2.0/Beta.dll",
        ],
        nuspec_body: r#"<references><reference file="Alpha.dll" /></references>"#,
    },
    // The OPC packaging apparatus a `.nupkg` carries. Restore strips it from the
    // file list before the content model runs, and it matters *because* of the
    // empty-ref-group rule above: left in, the `.psmdcp` would make
    // `ref/net8.0/` a compatible-but-assembly-less group and cost the package
    // every compile asset it has.
    StaticFixture {
        id: "OpcParts",
        files: &[
            "ref/net8.0/marker.psmdcp",
            "_rels/.rels",
            "[Content_Types].xml",
            "lib/net8.0/OpcParts.dll",
            "lib/netstandard2.0/OpcParts.dll",
        ],
        nuspec_body: "",
    },
    // Everything a package ships that is not a compile asset.
    StaticFixture {
        id: "InertAssets",
        files: &[
            "lib/netstandard2.0/InertAssets.dll",
            "build/net8.0/InertAssets.props",
            "buildTransitive/net8.0/InertAssets.targets",
            "contentFiles/any/any/Template.txt",
            "runtimes/win-x64/lib/net8.0/InertAssets.dll",
            "tools/net8.0/any/Tool.dll",
        ],
        nuspec_body: "",
    },
];

fn framework(name: &str) -> NuGetFramework {
    NuGetFramework::parse(name).expect("framework parses")
}

#[test]
fn compile_asset_selection_matches_a_real_dotnet_restore() {
    let fixtures = FIXTURES
        .iter()
        .map(StaticFixture::to_fixture)
        .collect::<Vec<_>>();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let packages = workspace.path().join("packages");

    for fixture in &fixtures {
        install_fixture(&packages, fixture);
    }

    let mut mismatches: Vec<String> = Vec::new();
    for project_framework in PROJECT_FRAMEWORKS {
        let assets = restore(workspace.path(), &packages, &fixtures, project_framework);
        assert_golden(project_framework, &assets);

        for fixture in &fixtures {
            let ours = ours(&packages, fixture, project_framework);
            let theirs = assets
                .get(&format!("{}/1.0.0", fixture.id))
                .unwrap_or_else(|| panic!("restore wrote no target library for {}", fixture.id));

            if &ours != theirs {
                mismatches.push(format!(
                    "[{project_framework}] {}\n  ours   ={ours:?}\n  restore={theirs:?}",
                    fixture.id
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} package(s) whose compile assets differ from a real `dotnet restore`:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// What `dotnet restore` itself says, asserted directly.
///
/// The diff above would still pass if our code and our reading of NuGet were
/// wrong *in the same way* and restore quietly disagreed with both — the diff
/// only compares the two of us. These are claims about restore's output alone,
/// so the rules the module leans on stay pinned even if our side changes: ref
/// beats lib, a compatible-but-empty ref group compiles against nothing, a `_._`
/// placeholder is a real compile entry, and the `<references>` allow-list bites
/// `lib/` while sparing `ref/`.
fn assert_golden(project_framework: &str, assets: &BTreeMap<String, Vec<String>>) {
    let golden: &[(&str, &[&str])] = match project_framework {
        "net10.0" | "net8.0" => &[
            ("RefAndLib/1.0.0", &["ref/net8.0/RefAndLib.dll"]),
            ("EmptyRefGroup/1.0.0", &[]),
            ("PlaceholderRef/1.0.0", &["ref/net8.0/_._"]),
            // The `.psmdcp` did *not* form a ref group: restore reached lib.
            ("OpcParts/1.0.0", &["lib/net8.0/OpcParts.dll"]),
            (
                "ReferencesVersusRef/1.0.0",
                &["ref/net8.0/Alpha.dll", "ref/net8.0/Beta.dll"],
            ),
            ("ReferencesGrouped/1.0.0", &["lib/net8.0/Beta.dll"]),
        ],
        "netstandard2.0" => &[
            ("RefAndLib/1.0.0", &["lib/netstandard2.0/RefAndLib.dll"]),
            (
                "EmptyRefGroup/1.0.0",
                &["lib/netstandard2.0/EmptyRefGroup.dll"],
            ),
            ("ReferencesGrouped/1.0.0", &["lib/netstandard2.0/Alpha.dll"]),
            (
                "ReferencesFlat/1.0.0",
                &["lib/netstandard2.0/ReferencesFlat.dll"],
            ),
        ],
        "net472" => &[
            ("PreTfmLib/1.0.0", &["lib/PreTfmLib.dll"]),
            (
                "NetFrameworkFolder/1.0.0",
                &["lib/net472/NetFrameworkFolder.dll"],
            ),
        ],
        other => panic!("no golden expectations for {other}"),
    };

    for (library, expected) in golden {
        let actual = assets
            .get(*library)
            .unwrap_or_else(|| panic!("restore wrote no target library for {library}"));
        assert_eq!(
            actual, expected,
            "[{project_framework}] real `dotnet restore` no longer selects what this test \
             was written against for {library}"
        );
    }
}

/// Our answer, through the entry point the LSP will call: the package is read
/// out of the very global packages folder restore just consumed — its own
/// `.nupkg.metadata` commit marker, its own nuspec, its own files — so the IO
/// path, the nuspec projection, and the selection are all exercised against
/// restore's ground truth, not just the pure core.
fn ours(packages: &Path, fixture: &Fixture, project_framework: &str) -> Vec<String> {
    let identity = PackageIdentity::new(
        PackageId::parse(&fixture.id).expect("fixture id parses"),
        NuGetVersion::parse("1.0.0").expect("version parses"),
    );
    let package = read_installed_package(packages, identity)
        .unwrap_or_else(|error| panic!("reading installed {}: {error}", fixture.id));

    select_installed_compile_assets(&package, &framework(project_framework))
        .unwrap_or_else(|decline| {
            panic!("{} declined for {project_framework}: {decline}", fixture.id)
        })
        .items
}

fn nuspec_xml(fixture: &Fixture) -> String {
    let id = &fixture.id;
    let body = &fixture.nuspec_body;
    format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id><version>1.0.0</version><authors>a</authors><description>d</description>
    {body}
  </metadata>
</package>
"#
    )
}

/// Lay a fixture out as a committed package in a global packages folder:
/// `{id-lower}/{version}/`, the nuspec, the files, and the `.nupkg.metadata`
/// marker NuGet writes last to say the extraction completed.
fn install_fixture(packages: &Path, fixture: &Fixture) {
    let package_dir = packages.join(fixture.id.to_ascii_lowercase()).join("1.0.0");
    std::fs::create_dir_all(&package_dir).expect("create package dir");

    std::fs::write(
        package_dir.join(format!("{}.nuspec", fixture.id.to_ascii_lowercase())),
        nuspec_xml(fixture),
    )
    .expect("write nuspec");

    for file in &fixture.files {
        let path = package_dir.join(file.replace('/', std::path::MAIN_SEPARATOR_STR));
        std::fs::create_dir_all(path.parent().expect("asset has a parent"))
            .expect("create asset dir");
        std::fs::write(&path, b"").expect("write asset");
    }

    // Written last, exactly as NuGet does: it is the commit marker.
    std::fs::write(
        package_dir.join(".nupkg.metadata"),
        br#"{"version":2,"contentHash":"","source":null}"#,
    )
    .expect("write .nupkg.metadata");
}

/// Restore a project referencing every fixture, and return
/// `package id/version -> sorted compile asset paths` from `project.assets.json`.
fn restore(
    workspace: &Path,
    packages: &Path,
    fixtures: &[Fixture],
    project_framework: &str,
) -> BTreeMap<String, Vec<String>> {
    let project_dir = workspace.join(format!("proj-{project_framework}"));
    std::fs::create_dir_all(&project_dir).expect("create project dir");

    let references = fixtures
        .iter()
        .map(|fixture| {
            format!(
                r#"<PackageReference Include="{}" Version="1.0.0" />"#,
                fixture.id
            )
        })
        .collect::<Vec<_>>()
        .join("\n    ");

    // The implicit framework references are off because nothing here is
    // compiled: a net472 or net8.0 project would otherwise restore a
    // reference-assembly / targeting-pack package that this offline devshell
    // does not have.
    std::fs::write(
        project_dir.join("Fixtures.csproj"),
        format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>{project_framework}</TargetFramework>
    <DisableImplicitFrameworkReferences>true</DisableImplicitFrameworkReferences>
    <AutomaticallyUseReferenceAssemblyPackages>false</AutomaticallyUseReferenceAssemblyPackages>
  </PropertyGroup>
  <ItemGroup>
    {references}
  </ItemGroup>
</Project>
"#
        ),
    )
    .expect("write fixture project");

    // No feed: every package is already committed in the global packages folder,
    // which restore consults first. Clearing the sources keeps the test honest
    // (and offline) about that.
    std::fs::write(
        project_dir.join("NuGet.config"),
        r#"<?xml version="1.0" encoding="utf-8"?>
<configuration><packageSources><clear /></packageSources></configuration>
"#,
    )
    .expect("write NuGet.config");

    let mut command = Command::new("dotnet");
    command
        .arg("restore")
        .arg(project_dir.join("Fixtures.csproj"))
        .arg("--nologo")
        .env("NUGET_PACKAGES", packages);
    BoundedCommand::new(command)
        .timeout(RESTORE_TIMEOUT)
        .run_ok(format!("dotnet restore ({project_framework})"));

    let assets_path = project_dir.join("obj").join("project.assets.json");
    let assets: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&assets_path).expect("restore wrote project.assets.json"),
    )
    .expect("project.assets.json is JSON");

    let targets = assets["targets"]
        .as_object()
        .expect("assets file has targets");
    assert_eq!(targets.len(), 1, "expected exactly one restore target");
    let libraries = targets
        .values()
        .next()
        .expect("one target")
        .as_object()
        .expect("target is an object");

    libraries
        .iter()
        .map(|(library, entry)| {
            let mut compile = entry
                .get("compile")
                .and_then(serde_json::Value::as_object)
                .map(|compile| compile.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            compile.sort();
            (library.clone(), compile)
        })
        .collect()
}

/// Keep the fixture list honest: an id that is not a legal package id, or a
/// file list that would not round-trip through the cache layout, would make the
/// diff above vacuous rather than failing.
#[test]
fn fixture_ids_are_legal_package_ids() {
    for fixture in FIXTURES {
        PackageId::parse(fixture.id)
            .unwrap_or_else(|error| panic!("fixture id {}: {error}", fixture.id));
        assert!(
            !fixture.files.is_empty(),
            "fixture {} has no files",
            fixture.id
        );
    }
}

// ============================================================================
// The generative real-restore differential
// ============================================================================

/// Generated package layouts against a real `dotnet restore`.
///
/// This exists because of what the curated fixtures above could *not* catch on
/// their own. `compile_assets_diff.rs` diffs the content model, but both sides
/// of that diff take the package's file list as given — so a bug in what the
/// file list *is* is invisible to it. That is exactly what the `.psmdcp` bug
/// was: restore strips NuGet's OPC parts before the content model runs, we kept
/// them, and a stray marker in a `ref/` folder silently cost a package every
/// compile asset it had. Only an oracle that starts from *files on disk* can see
/// that step, and a hand-written fixture list only sees the shapes someone
/// thought of.
///
/// So the layouts are generated, and the whole pipeline — directory walk, OPC
/// filter, nuspec read, content model, reference filter — is compared against
/// restore's own `compile` section.
///
/// Two constraints the generator respects, both from reality rather than taste:
///
/// - **Every package must stay compatible with every project framework.** A
///   package with no compatible asset at all is NU1202, which fails the entire
///   restore. A `lib/netstandard1.0/` floor is necessary but *not sufficient* —
///   a nearer `lib/` folder holding no runtime asset shadows it, leaving the
///   package with none — so every `lib/` folder gets at least one assembly (or
///   the `_._` placeholder, which counts). `ref/` folders are under no such
///   obligation, which is the point: an assembly-less one is what suppresses
///   `lib/`, and an OPC marker in one is the bug this test exists to catch.
/// - **No case-variant or non-ASCII names.** These fixtures are real files on a
///   real filesystem, and macOS's is case-insensitive and Unicode-normalising —
///   `lib/NET8.0/` and `lib/net8.0/` would collide, and a name written NFC would
///   read back NFD. Those subtleties are string-level, and belong to (and are
///   covered by) the in-memory content-model differential.
#[test]
fn generated_package_layouts_match_a_real_dotnet_restore() {
    const TFMS: &[&str] = &[
        "net8.0",
        "net6.0",
        "netstandard2.0",
        "netstandard2.1",
        "net472",
        "netstandard1.0",
    ];
    /// What every `lib/` folder gets at least one of, so that whichever one ends
    /// up nearest still leaves the package with a runtime asset.
    const RUNTIME_ASSETS: &[&str] = &[
        "Alpha.dll",
        "Beta.dll",
        "Alpha.dll",
        "Tool.exe",
        "Component.winmd",
        "_._",
    ];
    /// What may sit alongside it, including the OPC part restore strips.
    const EXTRAS: &[&str] = &[
        "Alpha.xml",
        "readme.txt",
        "marker.psmdcp",
        "sub/Nested.dll",
        "Beta.dll",
    ];
    /// A `ref/` folder is under no obligation to hold an assembly.
    const REF_CONTENTS: &[&str] = &[
        "Alpha.dll",
        "Beta.dll",
        "_._",
        "readme.txt",
        "marker.psmdcp",
    ];
    /// The decoy alongside the always-allowed `Alpha.dll`: a sibling the filter
    /// must drop, a case-variant it must keep, and a name the package does not
    /// ship at all.
    const REFERENCED: &[&str] = &["Beta.dll", "ALPHA.DLL", "Missing.dll", "Tool.exe"];

    let mut rng = SplitMix64(0x5eed_7e57_0007);
    let mut fixtures: Vec<Fixture> = Vec::new();

    for index in 0..40 {
        // A `<references>` allow-list constrains the layout, because it strips
        // *runtime* assemblies as well as compile ones: a package whose
        // allow-list names nothing it actually ships is left with no assets at
        // all, which is NU1202. So when there is one, every `lib/` folder ships
        // the assembly it names.
        let allow_list = rng.below(3) == 0;

        // The floor that keeps every package NU1202-compatible with all four
        // project frameworks.
        let mut files = vec!["lib/netstandard1.0/Alpha.dll".to_owned()];

        for _ in 0..1 + rng.below(3) {
            let tfm = rng.pick(TFMS);
            let mandatory = if allow_list {
                "Alpha.dll"
            } else {
                rng.pick(RUNTIME_ASSETS)
            };
            files.push(format!("lib/{tfm}/{mandatory}"));
            for _ in 0..rng.below(2) {
                files.push(format!("lib/{tfm}/{}", rng.pick(EXTRAS)));
            }
            if rng.below(3) == 0 {
                files.push(format!("ref/{tfm}/{}", rng.pick(REF_CONTENTS)));
            }
        }
        // The OPC apparatus, and inert assets under roots that bear none.
        if rng.below(2) == 0 {
            files.push("_rels/.rels".to_owned());
            files.push("[Content_Types].xml".to_owned());
        }
        if rng.below(3) == 0 {
            files.push(format!("build/{}/Pkg.props", rng.pick(TFMS)));
            files.push(format!("runtimes/win-x64/lib/{}/Alpha.dll", rng.pick(TFMS)));
        }
        files.sort();
        files.dedup();

        // `Alpha.dll` is always allowed (see above); the decoy alongside it is
        // what the filter is actually observed doing something about.
        let nuspec_body = if !allow_list {
            String::new()
        } else if rng.below(2) == 0 {
            format!(
                r#"<references><reference file="Alpha.dll" /><reference file="{}" /></references>"#,
                rng.pick(REFERENCED)
            )
        } else {
            format!(
                r#"<references><group targetFramework="{}"><reference file="Alpha.dll" /><reference file="{}" /></group></references>"#,
                rng.pick(TFMS),
                rng.pick(REFERENCED)
            )
        };

        fixtures.push(Fixture {
            id: format!("Gen{index}"),
            files,
            nuspec_body,
        });
    }

    let workspace = tempfile::tempdir().expect("temp workspace");
    let packages = workspace.path().join("packages");
    for fixture in &fixtures {
        install_fixture(&packages, fixture);
    }

    let mut mismatches: Vec<String> = Vec::new();
    let mut compared = 0usize;
    let mut declined = 0usize;
    for project_framework in PROJECT_FRAMEWORKS {
        let assets = restore(workspace.path(), &packages, &fixtures, project_framework);

        for fixture in &fixtures {
            let Some(ours) = ours_or_decline(&packages, fixture, project_framework) else {
                declined += 1;
                continue;
            };
            compared += 1;

            let theirs = assets
                .get(&format!("{}/1.0.0", fixture.id))
                .unwrap_or_else(|| panic!("restore wrote no target library for {}", fixture.id));
            if &ours != theirs {
                mismatches.push(format!(
                    "[{project_framework}] {} files={:?} nuspec={}\n  ours   ={ours:?}\n  restore={theirs:?}",
                    fixture.id, fixture.files, fixture.nuspec_body
                ));
            }
        }
    }

    eprintln!("compared={compared} declined={declined}");
    assert!(
        compared > 140,
        "generator degenerated: only {compared} comparable cases"
    );
    assert!(
        mismatches.is_empty(),
        "{} generated package(s) whose compile assets differ from a real `dotnet restore`; \
         first:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Our answer, or `None` where we decline (a decline is never *wrong*, so it is
/// simply not a comparison; the count is reported so a generator that declines
/// everything cannot pass vacuously).
fn ours_or_decline(
    packages: &Path,
    fixture: &Fixture,
    project_framework: &str,
) -> Option<Vec<String>> {
    let identity = PackageIdentity::new(
        PackageId::parse(&fixture.id).expect("fixture id parses"),
        NuGetVersion::parse("1.0.0").expect("version parses"),
    );
    let package = read_installed_package(packages, identity)
        .unwrap_or_else(|error| panic!("reading installed {}: {error}", fixture.id));

    select_installed_compile_assets(&package, &framework(project_framework))
        .ok()
        .map(|assets| assets.items)
}

/// SplitMix64, as in the oracle harness: deterministic, so a failure reproduces.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}
