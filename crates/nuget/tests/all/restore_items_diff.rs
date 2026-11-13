//! `borzoi_nuget::dgspec::write_restore_graph` against `WriteRestoreGraphTask`'s
//! own code (`MSBuildRestoreUtility.GetDependencySpec`, then
//! `DependencyGraphSpec.Save`), run by the oracle's `restoreGraphItems` op.
//!
//! The contract is certain-implies-exact at the strongest grain there is:
//! whenever we write a spec, NuGet saves the same file, byte for byte, under
//! every environment in `dgspec_diff`'s list. A decline claims nothing.
//!
//! The cases start from entry lists shaped as the SDK's restore targets
//! produce them (taken from `dotnet msbuild -getTargetResult:_GenerateRestoreGraph`
//! on SDK 10.0.401), each of which must commit, and mutate them: values from
//! per-name pools of spellings, metadata dropped, duplicated, re-cased or
//! foreign, items added, dropped, duplicated and reordered. Every mutation
//! kind must have produced an agreement, so none of them is graded vacuously.
//! Because a random sweep reaches any one name and value rarely, a second
//! sweep sets each name of each kind to every value in its pool, one at a
//! time.
//!
//! Besides `dgspec_diff`'s environments, the oracles run under process
//! environments that move the culture, since the writer sorts downloads by
//! the culture's collation ([`CULTURES`]).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

use borzoi_nuget::dgspec::{
    NuGetIdentity, RestoreGraphItem, RestoreItemsDecline, write_restore_graph,
};
use serde_json::{Value, json};

use super::common::{Oracle, SplitMix64, gen_range_string};
use super::dgspec_diff::ENVIRONMENTS;

// ---------------------------------------------------------------------------
// Grading.
// ---------------------------------------------------------------------------

/// One oracle per environment, with the environment as our function sees it.
struct Oracles(Vec<(String, BTreeMap<OsString, OsString>, Oracle)>);

/// Process environments that move the culture the writer sorts downloads by.
/// They are applied to the oracle's process, since ICU reads the locale from
/// the C library's environment, which a variable the oracle sets on itself
/// never reaches.
const CULTURES: &[&[(&str, Option<&str>)]] = &[
    &[
        ("LANG", Some("cs_CZ.UTF-8")),
        ("LC_ALL", None),
        ("LC_MESSAGES", None),
    ],
    &[("LC_ALL", Some("sv_SE.UTF-8"))],
    &[
        ("LANG", Some("en_US.UTF-8")),
        ("LC_ALL", None),
        ("LC_MESSAGES", None),
    ],
    &[("LANG", Some("C")), ("LC_ALL", None), ("LC_MESSAGES", None)],
    &[("LANG", None), ("LC_ALL", None), ("LC_MESSAGES", None)],
    &[("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", Some("1"))],
];

impl Oracles {
    /// One oracle per `dgspec_diff` environment and per culture. Each comes
    /// with the environment its process sees, which is what our function is
    /// given: the test's own, with the changes applied.
    fn spawn() -> Oracles {
        let inherited = || std::env::vars_os().collect::<BTreeMap<OsString, OsString>>();
        let mut oracles = Vec::new();
        for environment in ENVIRONMENTS {
            let mut map = inherited();
            map.extend(
                environment
                    .iter()
                    .map(|(k, v)| (OsString::from(k), OsString::from(v))),
            );
            oracles.push((
                format!("{environment:?}"),
                map,
                Oracle::spawn_with_environment(environment),
            ));
        }
        for culture in CULTURES {
            let mut map = inherited();
            for (name, value) in *culture {
                match value {
                    Some(value) => map.insert(OsString::from(name), OsString::from(value)),
                    None => map.remove(OsStr::new(name)),
                };
            }
            let process: Vec<(String, Option<String>)> = culture
                .iter()
                .map(|(k, v)| (k.to_string(), v.map(str::to_owned)))
                .collect();
            oracles.push((
                format!("process {culture:?}"),
                map,
                Oracle::spawn_with_process_environment(&process),
            ));
        }
        Oracles(oracles)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    /// We wrote a file, and it is NuGet's, byte for byte.
    Agree,
    /// We declined entries NuGet writes a file for.
    DeclinedWritten,
    /// We declined entries NuGet fails on.
    DeclinedFailed,
}

fn request(items: &[RestoreGraphItem], recursive: bool) -> Value {
    json!({
        "op": "restoreGraphItems",
        "restoreRecursive": recursive,
        "items": items.iter().map(|item| json!({
            "identity": item.identity,
            "metadata": item.metadata.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect::<serde_json::Map<_, _>>(),
        })).collect::<Vec<_>>(),
    })
}

/// Grade one entry list under every environment. `Err` is a violation.
fn grade(
    oracles: &mut Oracles,
    items: &[RestoreGraphItem],
    recursive: bool,
) -> Result<(Outcome, Option<String>), String> {
    let request = request(items, recursive);
    let mut by_default = None;
    for (label, environment, oracle) in &mut oracles.0 {
        let response = oracle.request(&request);
        let identity = NuGetIdentity::new(
            response["nugetCommands"]["informationalVersion"]
                .as_str()
                .expect("the op reports the NuGet that ran"),
        );
        let written = response["ok"].as_bool().expect("the op reports ok");
        let ours = write_restore_graph(items, recursive, &identity, environment);
        let outcome = match &ours {
            Ok(file) => {
                if !written {
                    return Err(format!(
                        "we wrote a spec where NuGet fails under {label} ({}: {})",
                        response["exceptionType"], response["message"]
                    ));
                }
                let theirs = response["text"].as_str().expect("a written spec has text");
                if file.text != theirs {
                    return Err(format!(
                        "we wrote a different file under {label}:\n{}",
                        first_line_difference(&file.text, theirs)
                    ));
                }
                (Outcome::Agree, None)
            }
            Err(decline) if written => (Outcome::DeclinedWritten, Some(shape(decline))),
            Err(decline) => (Outcome::DeclinedFailed, Some(shape(decline))),
        };
        by_default.get_or_insert(outcome);
    }
    Ok(by_default.expect("at least one environment"))
}

/// A decline without the item it arose at or the value, for the census.
fn shape(decline: &RestoreItemsDecline) -> String {
    match decline {
        RestoreItemsDecline::Value { name, detail, .. } => format!("{name}: {detail}"),
        RestoreItemsDecline::Vocabulary { detail, .. } => detail.clone(),
        RestoreItemsDecline::HostDependent { detail } => {
            format!("host: {}", detail.split('"').next().unwrap_or_default())
        }
        other => other.to_string(),
    }
}

fn first_line_difference(ours: &str, theirs: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (ours.lines().collect(), theirs.lines().collect());
    let i = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let context = |lines: &[&str]| lines[i.saturating_sub(3)..(i + 3).min(lines.len())].join("\n");
    format!(
        "first difference at line {}\n--- ours\n{}\n--- NuGet\n{}",
        i + 1,
        context(&a),
        context(&b)
    )
}

#[track_caller]
fn assert_sound(
    oracles: &mut Oracles,
    label: &str,
    items: &[RestoreGraphItem],
    recursive: bool,
) -> (Outcome, Option<String>) {
    match grade(oracles, items, recursive) {
        Ok(outcome) => outcome,
        Err(violation) => panic!("{label}: {violation}\nitems: {:#?}", items),
    }
}

// ---------------------------------------------------------------------------
// Entry lists shaped as the SDK writes them.
// ---------------------------------------------------------------------------

struct Builder {
    next: u32,
    items: Vec<RestoreGraphItem>,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            next: 0,
            items: Vec::new(),
        }
    }

    fn push(&mut self, ty: &str, project: &str, metadata: &[(&str, &str)]) {
        self.next += 1;
        let mut all = vec![
            ("Type".to_owned(), ty.to_owned()),
            ("ProjectUniqueName".to_owned(), project.to_owned()),
            ("MSBuildSourceProjectFile".to_owned(), project.to_owned()),
            (
                "MSBuildSourceTargetName".to_owned(),
                "_GenerateRestoreGraphProjectEntry".to_owned(),
            ),
        ];
        all.extend(metadata.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self.items.push(RestoreGraphItem {
            identity: format!("00000000-0000-0000-0000-{:012}", self.next),
            metadata: all,
        });
    }

    /// One project's entries, as `_GenerateRestoreGraph` produces them for an
    /// SDK-style F# project on SDK 10.0.401 under `restore.rs`'s globals.
    fn project(&mut self, dir: &str, name: &str, frameworks: &[&str], options: &ProjectOptions) {
        let path = format!("{dir}/{name}.fsproj");
        let p = path.as_str();
        let cross = if frameworks.len() > 1 { "true" } else { "" };
        let cpvm = if options.central { "true" } else { "" };
        self.push("RestoreSpec", p, &[]);
        let output = format!("{dir}/obj/");
        self.push(
            "ProjectSpec",
            p,
            &[
                ("Version", "1.0.0"),
                ("ProjectPath", p),
                ("ProjectName", name),
                ("Sources", "/scratch/empty-source;/sdk/library-packs;/sdk/sdk/10.0.401/FSharp/library-packs"),
                ("FallbackFolders", ""),
                ("PackagesPath", "/packages"),
                ("ProjectStyle", "PackageReference"),
                ("OutputPath", &output),
                ("RuntimeIdentifiers", ";"),
                ("RuntimeSupports", ""),
                ("CrossTargeting", cross),
                ("RestoreLegacyPackagesDirectory", ""),
                ("ValidateRuntimeAssets", "false"),
                ("SkipContentFileWrite", ""),
                ("ConfigFilePaths", "/home/u/.nuget/NuGet/NuGet.Config"),
                ("TreatWarningsAsErrors", options.warnings_as_errors),
                ("WarningsAsErrors", "3239;"),
                ("WarningsNotAsErrors", ""),
                ("NoWarn", ""),
                ("RestorePackagesWithLockFile", ""),
                ("NuGetLockFilePath", ""),
                ("RestoreLockedMode", ""),
                ("_CentralPackageVersionsEnabled", cpvm),
                ("CentralPackageFloatingVersionsEnabled", ""),
                ("CentralPackageVersionOverrideEnabled", ""),
                ("CentralPackageTransitivePinningEnabled", if options.central { "true" } else { "" }),
                ("NuGetAudit", "true"),
                ("NuGetAuditLevel", "low"),
                ("NuGetAuditMode", "direct"),
                ("SdkAnalysisLevel", "10.0.400"),
                ("UsingMicrosoftNETSdk", "true"),
                ("NETCoreSdkVersion", "10.0.401"),
                ("RestoreUseLegacyDependencyResolver", ""),
                ("RestoreDoNotWriteDependencyGraphSpec", ""),
            ],
        );
        for framework in frameworks {
            let (moniker, identifier, version, platform, pruning) = match *framework {
                "net8.0" => (
                    ".NETCoreApp,Version=v8.0",
                    ".NETCoreApp",
                    "v8.0",
                    "",
                    "false",
                ),
                "net10.0" => (
                    ".NETCoreApp,Version=v10.0",
                    ".NETCoreApp",
                    "v10.0",
                    "",
                    "true",
                ),
                "netstandard2.0" => (
                    ".NETStandard,Version=v2.0",
                    ".NETStandard",
                    "v2.0",
                    "Windows,Version=7.0",
                    "false",
                ),
                other => panic!("no fixture for {other}"),
            };
            let tfs = *framework;
            for (id, range, extra) in &options.packages {
                let mut metadata = vec![
                    ("Id", *id),
                    ("TargetFrameworks", tfs),
                    ("GeneratePathProperty", "true"),
                ];
                if !options.central || !range.is_empty() {
                    metadata.push(("VersionRange", *range));
                }
                metadata.extend(extra.iter().copied());
                self.push("Dependency", p, &metadata);
            }
            if !options.central {
                self.push(
                    "Dependency",
                    p,
                    &[
                        ("Id", "FSharp.Core"),
                        ("TargetFrameworks", tfs),
                        ("ExcludeAssets", "contentFiles"),
                        ("GeneratePathProperty", "true"),
                        ("VersionRange", "10.1.401"),
                    ],
                );
            }
            if *framework == "netstandard2.0" {
                self.push(
                    "Dependency",
                    p,
                    &[
                        ("Id", "NETStandard.Library"),
                        ("TargetFrameworks", tfs),
                        ("IsImplicitlyDefined", "true"),
                        ("PrivateAssets", "All"),
                        ("GeneratePathProperty", "true"),
                        ("VersionRange", "2.0.3"),
                    ],
                );
            } else {
                self.push(
                    "FrameworkReference",
                    p,
                    &[
                        ("Id", "Microsoft.NETCore.App"),
                        ("TargetFrameworks", tfs),
                        ("PrivateAssets", "All"),
                    ],
                );
            }
            if *framework == "net8.0" {
                for id in ["Microsoft.NETCore.App.Ref", "Microsoft.AspNetCore.App.Ref"] {
                    self.push(
                        "DownloadDependency",
                        p,
                        &[
                            ("Id", id),
                            ("TargetFrameworks", tfs),
                            ("VersionRange", "[8.0.31]"),
                        ],
                    );
                }
            }
            for (id, version) in [
                ("System.Text.Json", "10.0.0"),
                ("System.Collections", "4.3.0"),
                ("Microsoft.Win32.Primitives", "4.3.0"),
            ] {
                self.push(
                    "PrunePackageReference",
                    p,
                    &[
                        ("Id", id),
                        ("TargetFrameworks", tfs),
                        ("VersionRange", version),
                    ],
                );
            }
            if options.central {
                for (id, range) in &options.central_versions {
                    self.push(
                        "CentralPackageVersion",
                        p,
                        &[
                            ("Id", id),
                            ("TargetFrameworks", tfs),
                            ("VersionRange", range),
                        ],
                    );
                }
            }
            for (reference, unique) in &options.references {
                self.push(
                    "ProjectReference",
                    p,
                    &[
                        ("ProjectPath", reference),
                        ("ProjectReferenceUniqueName", unique),
                        ("TargetFrameworks", tfs),
                    ],
                );
            }
            let graph = if *framework == "netstandard2.0" {
                "/sdk/sdk/10.0.401/RuntimeIdentifierGraph.json"
            } else {
                "/sdk/sdk/10.0.401/PortableRuntimeIdentifierGraph.json"
            };
            let platform_version = if platform.is_empty() { "" } else { "7.0" };
            self.push(
                "TargetFrameworkInformation",
                p,
                &[
                    ("PackageTargetFallback", ""),
                    (
                        "AssetTargetFallback",
                        ";net461;net462;net47;net471;net472;net48;net481",
                    ),
                    ("TargetFramework", framework),
                    ("TargetFrameworkIdentifier", identifier),
                    ("TargetFrameworkVersion", version),
                    ("TargetFrameworkMoniker", moniker),
                    ("TargetFrameworkProfile", ""),
                    ("TargetPlatformMoniker", platform),
                    (
                        "TargetPlatformIdentifier",
                        if platform.is_empty() { "" } else { "Windows" },
                    ),
                    ("TargetPlatformVersion", platform_version),
                    ("TargetPlatformMinVersion", platform_version),
                    ("CLRSupport", ""),
                    ("RuntimeIdentifierGraphPath", graph),
                    ("WindowsTargetPlatformMinVersion", ""),
                    ("RestoreEnablePackagePruning", ""),
                    ("RestorePackagePruningDefault", pruning),
                    (
                        "NuGetAuditMode",
                        if *framework == "net10.0" {
                            "all"
                        } else {
                            "direct"
                        },
                    ),
                ],
            );
        }
    }
}

/// A package reference: id, version range (empty for a centrally managed
/// one), extra metadata.
type Package = (
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
);

#[derive(Default)]
struct ProjectOptions {
    central: bool,
    warnings_as_errors: &'static str,
    packages: Vec<Package>,
    central_versions: Vec<(&'static str, &'static str)>,
    references: Vec<(&'static str, &'static str)>,
}

/// The entry lists every one of which must commit.
fn bases() -> Vec<(&'static str, Vec<RestoreGraphItem>)> {
    let mut out = Vec::new();

    let mut b = Builder::new();
    b.project("/r/App", "App", &["net8.0"], &ProjectOptions::default());
    out.push(("single net8.0", b.items));

    let mut b = Builder::new();
    b.project(
        "/r/Lib",
        "Lib",
        &["net8.0", "netstandard2.0"],
        &ProjectOptions {
            packages: vec![("Newtonsoft.Json", "13.0.1", vec![])],
            ..ProjectOptions::default()
        },
    );
    out.push(("cross-targeting net8.0;netstandard2.0", b.items));

    let mut b = Builder::new();
    b.project(
        "/r/App",
        "App",
        &["net10.0", "net8.0"],
        &ProjectOptions {
            central: true,
            warnings_as_errors: "true",
            packages: vec![
                ("System.Text.Json", "", vec![]),
                ("Newtonsoft.Json", "", vec![("PrivateAssets", "all")]),
                ("FSharp.Core", "", vec![]),
            ],
            central_versions: vec![
                ("System.Text.Json", "8.0.5"),
                ("Newtonsoft.Json", "[13.0.1, 14.0.0)"),
                ("FSharp.Core", "10.1.401"),
            ],
            references: vec![("/r/Lib/Lib.fsproj", "/r/Lib/Lib.fsproj")],
        },
    );
    b.project(
        "/r/Lib",
        "Lib",
        &["net8.0", "netstandard2.0"],
        &ProjectOptions::default(),
    );
    out.push((
        "central, pinned, warnings as errors, with a reference",
        b.items,
    ));

    let mut b = Builder::new();
    b.project("/r/Tool", "Tool", &["net10.0"], &ProjectOptions::default());
    out.push(("net10.0, pruning on", b.items));
    out
}

#[test]
fn shaped_entry_lists_commit_and_agree() {
    let mut oracles = Oracles::spawn();
    for (label, items) in bases() {
        for recursive in [true, false] {
            let (outcome, decline) = assert_sound(&mut oracles, label, &items, recursive);
            assert_eq!(
                outcome,
                Outcome::Agree,
                "{label} (recursive: {recursive}) must commit; declined with {decline:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Mutations.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mutation {
    SetValue,
    PadValue,
    DropMetadata,
    RecaseName,
    ForeignMetadata,
    RecaseType,
    DropItem,
    DuplicateItem,
    SwapItems,
    AddItem,
    Retarget,
}

const MUTATIONS: [Mutation; 11] = [
    Mutation::SetValue,
    Mutation::PadValue,
    Mutation::DropMetadata,
    Mutation::RecaseName,
    Mutation::ForeignMetadata,
    Mutation::RecaseType,
    Mutation::DropItem,
    Mutation::DuplicateItem,
    Mutation::SwapItems,
    Mutation::AddItem,
    Mutation::Retarget,
];

const BOOLEANS: &[&str] = &[
    "true", "True", "TRUE", "false", "False", "", "yes", "1", "tru",
];
const FLAGS: &[&str] = &[
    "all",
    "All",
    "none",
    "None",
    "compile;runtime",
    "Compile, Runtime",
    "build",
    "buildtransitive",
    "contentFiles; analyzers",
    "native;bogus",
    "",
    ";",
    "runtime;compile;build;native;contentfiles;analyzers;buildtransitive",
    "1",
    "All, None",
    "-1",
    "bogus",
];
const IDS: &[&str] = &[
    "FSharp.Core",
    "fsharp.core",
    "FSHARP.CORE",
    "Newtonsoft.Json",
    "System.Text.Json",
    "A",
    "a-b_c",
    "Bad Id",
    "char",
    "horse",
    "Ch",
    "a.b",
    "a1",
    "Microsoft.AspNetCore.App.Ref",
    "",
];
const PATHS: &[&str] = &[
    "/r/App/App.fsproj",
    "/r/Lib/Lib.fsproj",
    "/r/lib/lib.fsproj",
    "/r/Tool/Tool.fsproj",
    "/r/Missing/M.fsproj",
    "",
];
const ALIASES: &[&str] = &[
    "net8.0",
    "NET8.0",
    "net10.0",
    "netstandard2.0",
    "net8.0;netstandard2.0",
    "netstandard2.0;net8.0",
    "net8.0; NET8.0",
    "net9.0",
    "",
    "net10.0;net8.0",
];
const MONIKERS: &[&str] = &[
    ".NETCoreApp,Version=v8.0",
    ".NETCoreApp,Version=v10.0",
    ".NETStandard,Version=v2.0",
    ".NETFramework,Version=v4.7.2",
    ".NETCoreApp,Version=v8",
    "netcoreapp,Version=v8.0",
    ".NETCoreApp, Version=v8.0",
    ".NETCoreApp,Version=v3.1",
    ".NETFramework,Version=v4.0,Profile=Client",
    "garbage",
    "",
];
const PLATFORMS: &[&str] = &[
    "",
    "Windows,Version=7.0",
    "UAP,Version=10.0",
    "Android,Version=33.0",
    "windows",
];
const FALLBACKS: &[&str] = &[
    ";net461;net462;net47;net471;net472;net48;net481",
    "net461",
    "net462;net461",
    "portable-net45+win8",
    "garbage",
    "",
    "net461;net461",
];
const CODES: &[&str] = &[
    "NU1605",
    "nu1605;NU1701",
    "NU1605,NU1603",
    "NU9999",
    "CS0168;NU1605",
    "1605",
    "3239;",
    "NU1605;NU1605",
    "",
];
const SOURCES: &[&str] = &[
    "/s/a;/s/b",
    "/s/b;/s/a",
    "http:/x.org/v3",
    "https://api.nuget.org/v3/index.json",
    "HTTP:/x.org",
    "file:/tmp/x",
    "/s/a;/s/a",
    "/s/a; ;/s/b",
    "",
];
const VERSIONS: &[&str] = &[
    "1.0-preview.1",
    "1.0.0-beta.2+x",
    "1.0.0",
    "1.0",
    "2.0.0-beta",
    "1.0.0+meta",
    "01.002.3",
    "garbage",
    "",
    "1.0.0.0",
];
const LEVELS: &[&str] = &[
    "10.0.400",
    "10.0",
    "10.0.100.0",
    "garbage",
    "10.0.100-preview.1",
    "",
    "9.0.100+x",
    "1.0-preview.1",
    "01.0-beta.2",
    "1.0-Beta",
    "1.0.0.0+m",
];
const AUDIT: &[&str] = &[
    "direct", "all", "ALL", "low", "moderate", "", "true", "false",
];
const STYLES: &[&str] = &[
    "PackageReference",
    "packagereference",
    "PackagesConfig",
    "Unknown",
    "ProjectJson",
    "",
    "1",
];

/// The spellings that matter to `name`: the pool every value for it is drawn
/// from. Version ranges add a few generated ones to a fixed list.
fn pool_for(rng: &mut SplitMix64, name: &str) -> Vec<String> {
    let pool = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    match name {
        "CrossTargeting"
        | "SkipContentFileWrite"
        | "TreatWarningsAsErrors"
        | "RestoreLockedMode"
        | "_CentralPackageVersionsEnabled"
        | "CentralPackageTransitivePinningEnabled"
        | "CentralPackageFloatingVersionsEnabled"
        | "CentralPackageVersionOverrideEnabled"
        | "RestoreLegacyPackagesDirectory"
        | "ValidateRuntimeAssets"
        | "UsingMicrosoftNETSdk"
        | "RestoreUseLegacyDependencyResolver"
        | "RestoreDoNotWriteDependencyGraphSpec"
        | "RestoreEnableAnalyzerAssets"
        | "IsImplicitlyDefined"
        | "GeneratePathProperty"
        | "RestoreEnablePackagePruning"
        | "RestorePackagePruningDefault"
        | "NuGetAudit" => pool(BOOLEANS),
        "IncludeAssets" | "ExcludeAssets" | "PrivateAssets" => pool(FLAGS),
        "Id" => pool(IDS),
        "ProjectPath" | "ProjectUniqueName" | "ProjectReferenceUniqueName" => pool(PATHS),
        "TargetFramework" | "TargetFrameworks" => pool(ALIASES),
        "TargetFrameworkMoniker" => pool(MONIKERS),
        "TargetPlatformMoniker" => pool(PLATFORMS),
        "AssetTargetFallback" => pool(FALLBACKS),
        "PackageTargetFallback" => {
            let mut values = pool(FALLBACKS);
            values.extend(["".to_owned(), "".to_owned()]);
            values
        }
        "NoWarn" | "WarningsAsErrors" | "WarningsNotAsErrors" => pool(CODES),
        "Sources" => pool(SOURCES),
        "FallbackFolders" | "ConfigFilePaths" => {
            pool(&["/f/a", "/f/b;/f/a", "/f/a;/f/a", "", " /f/c "])
        }
        "Version" | "NETCoreSdkVersion" => pool(VERSIONS),
        "SdkAnalysisLevel" => pool(LEVELS),
        "NuGetAuditMode" | "NuGetAuditLevel" => pool(AUDIT),
        "ProjectStyle" => pool(STYLES),
        "VersionRange" | "VersionOverride" => {
            let mut values = pool(&[
                "[8.0.31]",
                "[8.0.31];[9.0.0]",
                "8.0.31",
                "[8.0.31, 9.0.0)",
                "10.0.0",
                "",
                "[1.0.0];[1.0.0]",
                "1.0.0-Beta",
                "1.0.0+meta",
                "1.0",
                "1.*",
            ]);
            values.extend((0..4).map(|_| gen_range_string(rng)));
            values
        }
        "RuntimeIdentifiers" | "RuntimeSupports" => pool(&[";", "", "linux-x64", ";linux-x64"]),
        "Aliases" => pool(&["", "Alias", "a,b"]),
        "ProjectName" => pool(&["App", "Lib", "", "Ünï"]),
        "OutputPath" | "PackagesPath" | "RuntimeIdentifierGraphPath" => {
            pool(&["/o/", "/o/x", "", "/o/\"q\"", "/o/\\b"])
        }
        _ => pool(&["", "x", "true", "1.0.0"]),
    }
}

/// A value for `name`, drawn from its pool.
fn value_for(rng: &mut SplitMix64, name: &str) -> String {
    let pool = pool_for(rng, name);
    pool[rng.below(pool.len())].clone()
}

const KIND_NAMES: &[&str] = &[
    "RestoreSpec",
    "ProjectSpec",
    "TargetFrameworkInformation",
    "ProjectReference",
    "Dependency",
    "DownloadDependency",
    "FrameworkReference",
    "PrunePackageReference",
    "CentralPackageVersion",
    "NuGetAuditSuppress",
];

/// Every metadata name the restore targets and tasks write, across kinds.
const NAMES: &[&str] = &[
    "Version",
    "ProjectPath",
    "ProjectName",
    "Sources",
    "FallbackFolders",
    "PackagesPath",
    "ProjectStyle",
    "OutputPath",
    "RuntimeIdentifiers",
    "RuntimeSupports",
    "CrossTargeting",
    "RestoreLegacyPackagesDirectory",
    "ValidateRuntimeAssets",
    "SkipContentFileWrite",
    "ConfigFilePaths",
    "TreatWarningsAsErrors",
    "WarningsAsErrors",
    "WarningsNotAsErrors",
    "NoWarn",
    "RestorePackagesWithLockFile",
    "NuGetLockFilePath",
    "RestoreLockedMode",
    "_CentralPackageVersionsEnabled",
    "CentralPackageFloatingVersionsEnabled",
    "CentralPackageVersionOverrideEnabled",
    "CentralPackageTransitivePinningEnabled",
    "NuGetAudit",
    "NuGetAuditLevel",
    "NuGetAuditMode",
    "SdkAnalysisLevel",
    "UsingMicrosoftNETSdk",
    "NETCoreSdkVersion",
    "RestoreUseLegacyDependencyResolver",
    "RestoreDoNotWriteDependencyGraphSpec",
    "RestoreEnableAnalyzerAssets",
    "TargetFramework",
    "TargetFrameworkMoniker",
    "TargetPlatformMoniker",
    "TargetPlatformMinVersion",
    "PackageTargetFallback",
    "AssetTargetFallback",
    "RuntimeIdentifierGraphPath",
    "RestoreEnablePackagePruning",
    "RestorePackagePruningDefault",
    "CLRSupport",
    "WindowsTargetPlatformMinVersion",
    "TargetFrameworkIdentifier",
    "TargetFrameworkVersion",
    "TargetFrameworkProfile",
    "TargetPlatformIdentifier",
    "TargetPlatformVersion",
    "ProjectReferenceUniqueName",
    "TargetFrameworks",
    "IncludeAssets",
    "ExcludeAssets",
    "PrivateAssets",
    "Id",
    "VersionRange",
    "VersionOverride",
    "IsImplicitlyDefined",
    "GeneratePathProperty",
    "Aliases",
    "ProjectUniqueName",
    "Bogus",
];

fn recase(rng: &mut SplitMix64, s: &str) -> String {
    s.chars()
        .map(|c| match rng.below(3) {
            0 => c.to_ascii_uppercase(),
            1 => c.to_ascii_lowercase(),
            _ => c,
        })
        .collect()
}

fn mutate(
    rng: &mut SplitMix64,
    items: &mut Vec<RestoreGraphItem>,
    mutation: Mutation,
    next_id: &mut u32,
) {
    if items.is_empty() {
        return;
    }
    let i = rng.below(items.len());
    let fresh = |next_id: &mut u32| {
        *next_id += 1;
        format!("ffffffff-0000-0000-0000-{:012}", *next_id)
    };
    match mutation {
        Mutation::SetValue => {
            let item = &mut items[i];
            if item.metadata.is_empty() {
                return;
            }
            let j = rng.below(item.metadata.len());
            let name = item.metadata[j].0.clone();
            if name.eq_ignore_ascii_case("Type") {
                return;
            }
            item.metadata[j].1 = value_for(rng, &name);
        }
        Mutation::PadValue => {
            let item = &mut items[i];
            let j = rng.below(item.metadata.len());
            let pad = *rng.pick(&[" ", "\t", "  ", "\u{a0}", "\u{3000}"]);
            let value = &mut item.metadata[j].1;
            *value = match rng.below(3) {
                0 => format!("{pad}{value}"),
                1 => format!("{value}{pad}"),
                _ => format!("{pad}{value}{pad}"),
            };
        }
        Mutation::DropMetadata => {
            let item = &mut items[i];
            let j = rng.below(item.metadata.len());
            if !item.metadata[j].0.eq_ignore_ascii_case("Type") {
                item.metadata.remove(j);
            }
        }
        Mutation::RecaseName => {
            let item = &mut items[i];
            let j = rng.below(item.metadata.len());
            item.metadata[j].0 = recase(rng, &item.metadata[j].0.clone());
        }
        Mutation::ForeignMetadata => {
            let name = rng.pick(NAMES).to_string();
            let value = value_for(rng, &name);
            let item = &mut items[i];
            if !item
                .metadata
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case(&name))
            {
                item.metadata.push((name, value));
            }
        }
        Mutation::RecaseType => {
            let item = &mut items[i];
            if let Some(entry) = item
                .metadata
                .iter_mut()
                .find(|(n, _)| n.eq_ignore_ascii_case("Type"))
            {
                entry.1 = recase(rng, &entry.1.clone());
            }
        }
        Mutation::DropItem => {
            items.remove(i);
        }
        Mutation::DuplicateItem => {
            let mut copy = items[i].clone();
            copy.identity = fresh(next_id);
            let at = rng.below(items.len() + 1);
            items.insert(at, copy);
        }
        Mutation::SwapItems => {
            let j = rng.below(items.len());
            items.swap(i, j);
        }
        Mutation::AddItem => {
            let project = items[i]
                .metadata
                .iter()
                .find(|(n, _)| n == "ProjectUniqueName")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let ty = rng.pick(KIND_NAMES).to_string();
            let mut metadata = vec![
                ("Type".to_owned(), ty),
                ("ProjectUniqueName".to_owned(), project),
            ];
            for _ in 0..rng.below(5) {
                let name = rng.pick(&[
                    "Id",
                    "VersionRange",
                    "TargetFrameworks",
                    "ProjectPath",
                    "ProjectReferenceUniqueName",
                    "PrivateAssets",
                    "IncludeAssets",
                    "IsImplicitlyDefined",
                    "VersionOverride",
                    "NoWarn",
                ]);
                if !metadata.iter().any(|(n, _)| n == name) {
                    metadata.push((name.to_string(), value_for(rng, name)));
                }
            }
            let at = rng.below(items.len() + 1);
            items.insert(
                at,
                RestoreGraphItem {
                    identity: fresh(next_id),
                    metadata,
                },
            );
        }
        Mutation::Retarget => {
            let value = value_for(rng, "TargetFrameworks");
            let item = &mut items[i];
            match item
                .metadata
                .iter_mut()
                .find(|(n, _)| n == "TargetFrameworks")
            {
                Some(entry) => entry.1 = value,
                None => item.metadata.push(("TargetFrameworks".to_owned(), value)),
            }
        }
    }
}

fn cases() -> usize {
    std::env::var("BORZOI_RESTORE_ITEMS_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1500)
}

#[test]
fn mutated_entry_lists_are_written_as_nuget_writes_them() {
    let mut oracles = Oracles::spawn();
    let bases = bases();
    let mut rng = SplitMix64(0x000a_15ea_0001);
    let mut census: BTreeMap<Mutation, BTreeMap<Outcome, usize>> = BTreeMap::new();
    let mut declines: BTreeMap<String, usize> = BTreeMap::new();
    let mut next_id = 0;
    for case in 0..cases() {
        let (label, base) = rng.pick(&bases);
        let mut items = base.clone();
        let mut applied = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let mutation = *rng.pick(&MUTATIONS);
            mutate(&mut rng, &mut items, mutation, &mut next_id);
            applied.push(mutation);
        }
        let recursive = rng.below(4) != 0;
        let (outcome, decline) = match grade(&mut oracles, &items, recursive) {
            Ok(outcome) => outcome,
            Err(violation) => panic!(
                "case {case} ({label}, {applied:?}, recursive: {recursive}): {violation}\nitems: {items:#?}"
            ),
        };
        for mutation in applied {
            *census
                .entry(mutation)
                .or_default()
                .entry(outcome)
                .or_default() += 1;
        }
        if let Some(decline) = decline {
            *declines
                .entry(format!("{outcome:?} {decline}"))
                .or_default() += 1;
        }
    }
    eprintln!("census: {census:#?}");
    eprintln!("decline shapes: {declines:#?}");
    for mutation in MUTATIONS {
        let agreed = census
            .get(&mutation)
            .and_then(|outcomes| outcomes.get(&Outcome::Agree))
            .copied()
            .unwrap_or(0);
        assert!(
            agreed > 0,
            "no case under {mutation:?} agreed, so it graded nothing"
        );
    }
}

/// Every single mutation of one value: each metadata name of the first item of
/// each kind in each base, set to every value in its pool (and a few foreign
/// names besides). The random sweep reaches a given name and value only once
/// in thousands of cases; this reaches each of them.
#[test]
fn every_single_value_is_written_as_nuget_writes_it() {
    let mut oracles = Oracles::spawn();
    let mut rng = SplitMix64(0x000a_15ea_0002);
    let mut agreed = 0;
    let mut graded = 0;
    for (label, base) in bases() {
        let mut seen_kinds: Vec<String> = Vec::new();
        for (i, item) in base.iter().enumerate() {
            let kind = item
                .metadata
                .iter()
                .find(|(n, _)| n == "Type")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            if seen_kinds.contains(&kind) {
                continue;
            }
            seen_kinds.push(kind.clone());
            let mut names: Vec<String> = item
                .metadata
                .iter()
                .map(|(n, _)| n.clone())
                .filter(|n| n != "Type")
                .collect();
            for foreign in [
                "Bogus",
                "Id",
                "TargetFrameworks",
                "VersionOverride",
                "NoWarn",
            ] {
                if !names.iter().any(|n| n == foreign) {
                    names.push(foreign.to_owned());
                }
            }
            for name in names {
                for value in pool_for(&mut rng, &name) {
                    let mut items = base.clone();
                    match items[i].metadata.iter_mut().find(|(n, _)| *n == name) {
                        Some(entry) => entry.1 = value.clone(),
                        None => items[i].metadata.push((name.clone(), value.clone())),
                    }
                    let (outcome, _) = assert_sound(
                        &mut oracles,
                        &format!("{label}: {kind}.{name} = {value:?}"),
                        &items,
                        true,
                    );
                    graded += 1;
                    if outcome == Outcome::Agree {
                        agreed += 1;
                    }
                }
            }
        }
    }
    eprintln!("{agreed} of {graded} single-value mutations agreed");
    assert!(
        agreed * 2 > graded,
        "fewer than half the single-value mutations committed"
    );
}

/// Downloads sort by the culture's collation, which differs between cultures
/// even for ASCII names: under Czech, `ch` is one letter, sorting after `h`.
/// Each pair is graded under every culture in [`CULTURES`].
#[test]
fn culture_ordered_downloads_are_sound() {
    let mut oracles = Oracles::spawn();
    for (a, b) in [
        ("char", "horse"),
        ("Char", "Horse"),
        ("a1", "a.b"),
        ("Microsoft.NETCore.App.Ref", "Microsoft.AspNetCore.App.Ref"),
        ("ab", "AC"),
    ] {
        let mut builder = Builder::new();
        builder.project("/r/App", "App", &["net8.0"], &ProjectOptions::default());
        let mut items = builder.items;
        let mut ids = [a, b].into_iter();
        for item in &mut items {
            if item
                .metadata
                .iter()
                .any(|(n, v)| n == "Type" && v == "DownloadDependency")
                && let Some(entry) = item.metadata.iter_mut().find(|(n, _)| n == "Id")
            {
                entry.1 = ids.next().expect("two downloads").to_owned();
            }
        }
        assert_sound(
            &mut oracles,
            &format!("downloads {a} and {b}"),
            &items,
            true,
        );
    }
}
