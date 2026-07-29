//! Differential: the walker's [`OutputDirVerdict`] vs real MSBuild's evaluated
//! `OutDir`, over whole `.fsproj` documents.
//!
//! The contract is the crate's usual one, specialised to what the verdict
//! *licenses a consumer to do* — which is the only thing worth checking, since
//! the consumer looks for a DLL in the directory this names:
//!
//! - [`OutputDirVerdict::Declared`] ⟹ MSBuild's `OutDir` is that string,
//!   exactly. There is no configuration caveat: the verdict declines a value
//!   whose directory depends on the configuration, so what it commits to is
//!   the directory the build writes whichever one was chosen.
//! - [`OutputDirVerdict::Default`] and [`OutputDirVerdict::Unknown`] ⟹ no
//!   claim. MSBuild may say anything.
//!
//! Only the committing arm is checked, because only it is a claim. `Default`
//! is the absence of a redirect *this walker recognises*, and MSBuild has more
//! ways to move the output than the props chain can be read for —
//! `BaseOutputPath`, `UseArtifactsOutput`, a redirect assembled in a targets
//! file. Asserting the standard layout there would be asserting the walker is
//! exhaustive about a set it cannot enumerate. Its consumer treats both
//! non-committing arms alike (scan the `bin` tree, as before the verdict
//! existed), so a misfiled project costs the coverage it always cost and
//! never a wrong directory.
//!
//! Two normalisations, both deliberate and both semantics-free *for a
//! directory*, which is what the verdict names:
//!
//! - **Separators.** MSBuild emits the SDK's own `bin\Debug` with a backslash
//!   while echoing a project-written `/` verbatim.
//! - **A trailing separator.** The common targets run `EnsureTrailingSlash` on
//!   `OutDir`, so `<OutDir>artifacts</OutDir>` evaluates to `artifacts/`. The
//!   consumer reaches the DLL by joining a file name onto this, and both
//!   spellings join to the same path. The walker does not replicate that
//!   fixup, because it lives in a targets file outside the chain the walker
//!   follows — so claiming it would be asserting something unverified rather
//!   than reporting something read.
//!
//! Nothing else is normalised: a wrong *directory* must still fail.
//!
//! **Both sides resolve the real SDK, and our side carries the LSP's global
//! properties.** Neither is optional detail. The SDK chain writes `OutDir`
//! itself, and it only computes an `OutputPath` for that write to read once
//! `Configuration` is defined — so a walk without a resolver, or without the
//! `Configuration`/`Platform` globals the LSP always injects
//! (`workspace::default_build_properties`), is answering a different question
//! from the one asked at runtime. An agreement reached there says nothing
//! about the configuration that ships. MSBuild needs no globals to match: the
//! SDK defaults `Configuration` to `Debug` on its side too.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use borzoi_msbuild::{OutputDirVerdict, parse_fsproj_with_imports, resolve_sdk, workloads};
use common::Oracle;
use tempfile::TempDir;

/// Declared bodies to put in the project's `<PropertyGroup>`. Reaches past
/// what the verdict models on purpose: under certain-implies-exact a decline
/// is free, so there is no reason to restrict the inputs to shapes we expect
/// to commit — only a wrong commit fails.
const BODIES: &[&str] = &[
    // Nothing declared, and the two properties that suppress or move the
    // framework segment without naming `OutDir`.
    "",
    "<AppendTargetFrameworkToOutputPath>false</AppendTargetFrameworkToOutputPath>",
    // Plain declared directories.
    "<OutDir>artifacts/</OutDir>",
    "<OutDir>artifacts</OutDir>",
    "<OutDir>a/b/c/</OutDir>",
    // Declared, and interacting with the properties that do *not* apply to it.
    "<OutDir>artifacts/</OutDir><AppendTargetFrameworkToOutputPath>false</AppendTargetFrameworkToOutputPath>",
    "<OutDir>artifacts/</OutDir><OutputPath>elsewhere/</OutputPath>",
    // `OutputPath` alone: we never claim a directory for it, MSBuild derives
    // `OutDir` from it. A decline here is expected; a commit would be a bug.
    "<OutputPath>elsewhere/</OutputPath>",
    // Configuration-dependent, the case that must decline rather than commit
    // to whichever configuration this evaluation happened to run under.
    "<OutDir>artifacts/$(Configuration)/</OutDir>",
    "<OutDir>$(Configuration)/out/</OutDir>",
    // Undefined references: the trap the verdict exists for.
    "<OutDir>$(SolutionDir)artifacts/</OutDir>",
    "<OutDir>$(SolutionDir)</OutDir>",
    "<OutDir>$(NoSuchPropertyAnywhere)/out/</OutDir>",
    // Defined references, including a defined-but-empty one.
    "<Root>rooted</Root><OutDir>$(Root)/out/</OutDir>",
    "<Empty></Empty><OutDir>$(Empty)out/</OutDir>",
    // Later write wins.
    "<OutDir>first/</OutDir><OutDir>second/</OutDir>",
    // Degenerate spellings.
    "<OutDir></OutDir>",
    "<OutDir>   </OutDir>",
    // Shapes the property pass refuses outright.
    "<OutDir>@(Thing)/out/</OutDir>",
    "<OutDir>%(Meta)/out/</OutDir>",
    // The SDK's own `<OutDir Condition="'$(OutDir)' == ''">$(OutputPath)</OutDir>`
    // means a user write carrying the *same* condition never fires. Whichever
    // side of it we record, the answer must still be MSBuild's.
    "<OutDir Condition=\"'$(OutDir)' == ''\">artifacts/</OutDir>",
    "<OutDir Condition=\"'$(OutDir)' != ''\">artifacts/</OutDir>",
    // The other SDK properties that move the output. The walker does not
    // model them, so it must not commit — and the `Default` arm asserting
    // nothing is what makes that safe rather than wrong.
    "<BaseOutputPath>elsewhere/</BaseOutputPath>",
    "<UseArtifactsOutput>true</UseArtifactsOutput>",
    // Declarations that *keep* the standard layout. Declining them is a
    // coverage gap, never a wrong directory.
    "<OutputPath>bin/$(Configuration)/</OutputPath>",
    "<AppendTargetFrameworkToOutputPath>true</AppendTargetFrameworkToOutputPath>",
    // Configuration-dependent writes, each hidden from a different check: a
    // gate whose value never mentions the configuration, and a value laundered
    // through a helper property. Committing to either would name a directory
    // that exists for one configuration alone.
    "<OutDir Condition=\"'$(Configuration)' == 'Debug'\">debug-out/</OutDir>\
     <OutDir Condition=\"'$(Configuration)' == 'Release'\">release-out/</OutDir>",
    // …and the same thing laundered through a helper property.
    "<Which>$(Configuration)</Which><OutDir>out/$(Which)/</OutDir>",
    // A gated sibling of an unconditioned write: skipped in this evaluation,
    // taken in the build the user actually ran.
    "<OutDir>common/</OutDir>\
     <OutDir Condition=\"'$(Configuration)' == 'Release'\">release/</OutDir>",
    // The other build dimension the LSP pins as a global.
    "<OutDir>artifacts/$(Platform)/</OutDir>",
];

/// Whole documents, for the shapes that need structure outside a single
/// `<PropertyGroup>` — control flow and cleanly-skipped alternatives, neither
/// of which the property walk visits.
const DOCUMENTS: &[&str] = &[
    "<Project Sdk=\"Microsoft.NET.Sdk\">\
     <PropertyGroup><TargetFramework>net10.0</TargetFramework>\
     <OutDir>common/</OutDir></PropertyGroup>\
     <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\
     <OutDir>release/</OutDir></PropertyGroup></Project>",
    "<Project Sdk=\"Microsoft.NET.Sdk\">\
     <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>\
     <Choose><When Condition=\"'$(Configuration)' == 'Debug'\">\
     <PropertyGroup><OutDir>debug/</OutDir></PropertyGroup></When>\
     <Otherwise><PropertyGroup><OutDir>ship/</OutDir></PropertyGroup></Otherwise>\
     </Choose></Project>",
];

/// The two sides as *directories*: separators unified, and at most one
/// trailing separator dropped. See the module docs for why each is
/// semantics-free here and what is deliberately left alone.
fn as_directory(path: &str) -> String {
    let unified = path.replace('\\', "/");
    unified.strip_suffix('/').unwrap_or(&unified).to_owned()
}

/// The workload context of this process, so our SDK resolution consults the
/// same user-local roots the oracle child's dotnet host does. Mirrors
/// `fsproj_packageref_diff`'s helper of the same name.
fn workload_env_from_process() -> (Option<PathBuf>, bool) {
    let non_empty = |var: &str| std::env::var_os(var).filter(|value| !value.is_empty());
    let user_dotnet_root = non_empty("DOTNET_CLI_HOME")
        .or_else(|| non_empty("HOME"))
        .map(|home| PathBuf::from(home).join(".dotnet"));
    let overrides_present = std::env::var_os("DOTNETSDK_WORKLOAD_MANIFEST_ROOTS").is_some()
        || std::env::var_os("DOTNETSDK_WORKLOAD_MANIFEST_IGNORE_DEFAULT_ROOTS").is_some()
        || non_empty("DOTNETSDK_WORKLOAD_PACK_ROOTS").is_some();
    (user_dotnet_root, overrides_present)
}

#[test]
fn declared_output_dirs_agree_with_msbuild() {
    let mut oracle = Oracle::spawn();
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("DOTNET_ROOT is not set; run under nix develop"));
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        // The fixture tempdirs have no global.json above them.
        global_json_pins_workload_set: false,
    };
    let sdk = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let names: Vec<String> = ["OutDir", "Configuration"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut committed = 0usize;
    let mut declined = 0usize;

    let cases: Vec<(String, String)> = BODIES
        .iter()
        .map(|body| {
            (
                (*body).to_string(),
                format!(
                    "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
                     <TargetFramework>net10.0</TargetFramework>\n    {body}\n  \
                     </PropertyGroup>\n</Project>\n"
                ),
            )
        })
        .chain(
            DOCUMENTS
                .iter()
                .map(|d| ((*d).to_string(), (*d).to_string())),
        )
        .collect();

    for (body, xml) in &cases {
        let tmp = TempDir::new().expect("temp dir");
        let path = tmp.path().join("P.fsproj");
        std::fs::write(&path, xml).expect("write project");

        // The globals the LSP injects, given to **both** sides. Ours alone would
        // be asking the two a different question: the SDK computes `OutputPath`
        // — which its own `OutDir` write reads — only once `Configuration` is
        // defined, so an oracle evaluated without it answers about a project
        // configuration nobody builds.
        let globals: Vec<(String, String)> = vec![
            ("Configuration".to_owned(), "Debug".to_owned()),
            ("Platform".to_owned(), "AnyCPU".to_owned()),
        ];
        let global_map: HashMap<String, String> = globals.iter().cloned().collect();
        let parsed = parse_fsproj_with_imports(
            xml,
            &path,
            &global_map,
            &common::oracle_environment(),
            Some(&sdk),
            None,
        )
        .expect("well-formed");

        let Some(theirs) = oracle.project(xml, &names, Some(&path), &globals) else {
            // MSBuild rejected the document; we must not have committed a
            // directory for it.
            assert!(
                !matches!(parsed.output_dir, OutputDirVerdict::Declared { .. }),
                "we committed an output directory for a document MSBuild \
                 rejects: {body:?} => {:?}",
                parsed.output_dir
            );
            continue;
        };
        let real = as_directory(theirs.get("OutDir").map(String::as_str).unwrap_or_default());

        match &parsed.output_dir {
            OutputDirVerdict::Unknown | OutputDirVerdict::Default => declined += 1,
            OutputDirVerdict::Declared { path: ours } => {
                committed += 1;
                assert_eq!(
                    as_directory(ours),
                    real,
                    "our committed output directory disagrees with MSBuild for {body:?}"
                );
            }
        }
    }

    // Neither branch may swallow the corpus: all-declines would make the
    // agreement vacuous, and all-commits would mean the trap cases stopped
    // being traps.
    assert!(
        committed > 0 && declined > 0,
        "committed={committed} declined={declined}"
    );
}

/// **The objection this branch was stopped on**, asked of the machine instead of
/// of my reading of MSBuild.
///
/// The entry project's body evaluates *before* `Sdk.targets`, so a document that
/// arrives later can overwrite an `OutDir` the entry declared: `Directory.Build.targets`
/// (imported at the end of the SDK chain), a package `.targets`, or an SDK
/// extension hook. A literal `<OutDir>artifacts/</OutDir>` is certified against
/// every *global* — nothing can substitute into it — but that says nothing about
/// a later write, and a wrong directory here sends the consumer to look for a
/// producer's DLL where the build never put one.
///
/// Each cluster lays the sidecars on disk and asks MSBuild what `OutDir` really
/// is. The contract is the file's: `Declared` must equal it; the other arms make
/// no claim.
/// One document cluster: the entry project's `<PropertyGroup>` body, and the
/// sidecar files laid beside (or above) it.
struct Cluster {
    label: &'static str,
    body: &'static str,
    files: &'static [(&'static str, &'static str)],
}

#[test]
fn a_later_document_may_not_be_overwritten_by_a_declared_entry_body() {
    let mut oracle = Oracle::spawn();
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("DOTNET_ROOT is not set; run under nix develop"));
    let (user_dotnet_root, overrides_present) = workload_env_from_process();
    let workload_env = workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        global_json_pins_workload_set: false,
    };
    let sdk = |name: &str| resolve_sdk(&dotnet_root, None, name, None, None, &workload_env);
    let names: Vec<String> = ["OutDir"].iter().map(|s| s.to_string()).collect();

    let clusters: &[Cluster] = &[
        // The bare objection: an unconditioned overwrite in the file the SDK
        // imports last.
        Cluster {
            label: "dbt-overwrites",
            body: "<OutDir>artifacts/</OutDir>",
            files: &[(
                "Directory.Build.targets",
                "<Project><PropertyGroup><OutDir>overwritten/</OutDir></PropertyGroup></Project>\n",
            )],
        },
        // The same, gated on a build dimension: which document wins is a
        // property of the build the user ran, not of the text.
        Cluster {
            label: "dbt-overwrites-gated",
            body: "<OutDir>artifacts/</OutDir>",
            files: &[(
                "Directory.Build.targets",
                "<Project><PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\
                 <OutDir>debug-only/</OutDir></PropertyGroup></Project>\n",
            )],
        },
        // A `Directory.Build.props` write is *earlier* than the entry body, so
        // the entry's literal still wins — the control that keeps the cluster
        // from passing by declining everything with a sidecar present.
        Cluster {
            label: "dbp-loses-to-entry",
            body: "<OutDir>artifacts/</OutDir>",
            files: &[(
                "Directory.Build.props",
                "<Project><PropertyGroup><OutDir>earlier/</OutDir></PropertyGroup></Project>\n",
            )],
        },
        // An SDK **extension-hook** import: `Microsoft.Common.targets` imports
        // whatever `$(CustomAfterMicrosoftCommonTargets)` names, so a user file
        // reached only through a property the SDK reads can overwrite the entry
        // body's literal. The design notes named this route specifically.
        Cluster {
            label: "hook-after-common-targets",
            body: "<CustomAfterMicrosoftCommonTargets>$(MSBuildProjectDirectory)/hook.targets\
             </CustomAfterMicrosoftCommonTargets><OutDir>artifacts/</OutDir>",
            files: &[(
                "hook.targets",
                "<Project><PropertyGroup><OutDir>hooked/</OutDir></PropertyGroup></Project>\n",
            )],
        },
        // The props-side twin, which lands *before* the entry body — so the
        // entry's literal legitimately wins and this is the coverage control.
        Cluster {
            label: "hook-before-common-props",
            body: "<OutDir>artifacts/</OutDir>",
            files: &[(
                "hook.props",
                "<Project><PropertyGroup><OutDir>hooked-early/</OutDir></PropertyGroup></Project>\n",
            )],
        },
        // The hole in the completeness premise. `note_document_not_scanned` is
        // gated on the *import site* being outside the SDK subtree, but an
        // extension-hook import is sited in an SDK document and imports a
        // **user** one. Point the hook at a path the walker cannot pin — a
        // property function in a value position — and the hook's own `<OutDir>`
        // is never counted, so the entry's literal certifies against a document
        // nobody read. Real MSBuild builds to `hooked/`.
        Cluster {
            label: "hook-path-unpinnable",
            body: "<OutDir>artifacts/</OutDir>",
            files: &[
                (
                    "Directory.Build.props",
                    "<Project><PropertyGroup><CustomBeforeMicrosoftCommonTargets>\
                     $([MSBuild]::GetPathOfFileAbove('hook.targets'))\
                     </CustomBeforeMicrosoftCommonTargets></PropertyGroup></Project>\n",
                ),
                (
                    "../hook.targets",
                    "<Project><PropertyGroup><OutDir>hooked/</OutDir></PropertyGroup></Project>\n",
                ),
            ],
        },
        // Nothing declared in the entry body at all: the later document is the
        // only writer, so `Default` would be a claim that the standard layout
        // holds when it does not.
        Cluster {
            label: "dbt-only-writer",
            body: "",
            files: &[(
                "Directory.Build.targets",
                "<Project><PropertyGroup><OutDir>only/</OutDir></PropertyGroup></Project>\n",
            )],
        },
    ];

    let mut checked = 0usize;
    for Cluster { label, body, files } in clusters {
        let tmp = TempDir::new().expect("temp dir");
        // The project lives one level down so a sidecar named `../x` is
        // genuinely *above* it — which is where a `GetPathOfFileAbove` hook
        // looks, and the shape a repo-root file takes in a real tree.
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("project dir");
        let path = dir.join("P.fsproj");
        let xml = format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
             <TargetFramework>net10.0</TargetFramework>\n    {body}\n  \
             </PropertyGroup>\n</Project>\n"
        );
        std::fs::write(&path, &xml).expect("write project");
        for (name, contents) in files.iter() {
            let at = dir.join(name);
            std::fs::create_dir_all(at.parent().expect("sidecar parent")).expect("sidecar dir");
            std::fs::write(&at, contents).expect("write sidecar");
        }

        // The globals the LSP injects, given to **both** sides. Ours alone would
        // be asking the two a different question: the SDK computes `OutputPath`
        // — which its own `OutDir` write reads — only once `Configuration` is
        // defined, so an oracle evaluated without it answers about a project
        // configuration nobody builds.
        let globals: Vec<(String, String)> = vec![
            ("Configuration".to_owned(), "Debug".to_owned()),
            ("Platform".to_owned(), "AnyCPU".to_owned()),
        ];
        let global_map: HashMap<String, String> = globals.iter().cloned().collect();
        let parsed = parse_fsproj_with_imports(
            &xml,
            &path,
            &global_map,
            &common::oracle_environment(),
            Some(&sdk),
            None,
        )
        .expect("well-formed");

        let theirs = oracle
            .project(&xml, &names, Some(&path), &globals)
            .expect("MSBuild evaluates these documents");
        let real = as_directory(theirs.get("OutDir").map(String::as_str).unwrap_or_default());
        eprintln!(
            "  {label:<24} ours {:?}  msbuild {real:?}",
            parsed.output_dir
        );
        checked += 1;

        match &parsed.output_dir {
            OutputDirVerdict::Unknown => {}
            OutputDirVerdict::Default => assert!(
                real.ends_with("bin/Debug/net10.0") || real.ends_with("bin\\Debug\\net10.0"),
                "{label}: we reported the default layout, but MSBuild builds to {real:?}"
            ),
            OutputDirVerdict::Declared { path: ours } => assert_eq!(
                as_directory(ours),
                real,
                "{label}: we committed an output directory the build does not use"
            ),
        }
    }
    assert_eq!(checked, clusters.len());
}
