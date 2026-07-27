//! Differential: the walker's [`OutputDirVerdict`] vs real MSBuild's evaluated
//! `OutDir`, over whole `.fsproj` documents.
//!
//! The contract is the crate's usual one, specialised to what the verdict
//! *licenses a consumer to do* — which is the only thing worth checking, since
//! the consumer looks for a DLL in the directory this names:
//!
//! - [`OutputDirVerdict::Declared`] with no configuration ⟹ MSBuild's `OutDir`
//!   is that string, exactly.
//! - [`OutputDirVerdict::Declared`] carrying a configuration ⟹ MSBuild's
//!   `OutDir` is that string with the configuration segment replaced by
//!   whatever configuration MSBuild evaluated under. Checked by substituting
//!   MSBuild's own `Configuration` into our wildcard, because a consumer
//!   searching that segment must find the real directory among the
//!   candidates.
//! - [`OutputDirVerdict::Default`] ⟹ MSBuild's `OutDir` is the default layout
//!   (`bin/<config>/<tfm>/`, or `bin/<config>/` with the TFM suppressed) —
//!   *not* merely "we said nothing". This arm is a positive claim: it tells
//!   the consumer to run its default-layout scan, so a project that quietly
//!   redirected its output must never land here.
//! - [`OutputDirVerdict::Unknown`] ⟹ no claim. MSBuild may say anything.
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
//! SDK defaults `Configuration` to `Debug` on its side, and the assertions
//! read MSBuild's own evaluated `Configuration` back rather than assuming it.

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
    // Nothing: the default layout, and the one that must stay a positive claim.
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
    // Configuration-dependent, the case that must be handed back rather than
    // committed to.
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

    for body in BODIES {
        let xml = format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
             <TargetFramework>net10.0</TargetFramework>\n    {body}\n  \
             </PropertyGroup>\n</Project>\n"
        );
        let tmp = TempDir::new().expect("temp dir");
        let path = tmp.path().join("P.fsproj");
        std::fs::write(&path, &xml).expect("write project");

        let globals = HashMap::from([
            ("Configuration".to_owned(), "Debug".to_owned()),
            ("Platform".to_owned(), "AnyCPU".to_owned()),
        ]);
        let parsed = parse_fsproj_with_imports(
            &xml,
            &path,
            &globals,
            &common::oracle_environment(),
            Some(&sdk),
            None,
        )
        .expect("well-formed");

        let Some(theirs) = oracle.project(&xml, &names, Some(&path)) else {
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
        let real_config = theirs.get("Configuration").cloned().unwrap_or_default();

        match &parsed.output_dir {
            OutputDirVerdict::Unknown => declined += 1,
            OutputDirVerdict::Default => {
                committed += 1;
                // A positive claim: the consumer will run its default-layout
                // scan, so the real directory must be one that scan reaches.
                let expected_with_tfm = format!("bin/{real_config}/net10.0");
                let expected_bare = format!("bin/{real_config}");
                assert!(
                    real == expected_with_tfm || real == expected_bare,
                    "we claimed the default layout but MSBuild writes to \
                     {real:?} (expected {expected_with_tfm:?} or \
                     {expected_bare:?}) for {body:?}"
                );
            }
            OutputDirVerdict::Declared {
                path: ours,
                configuration,
            } => {
                committed += 1;
                let ours = as_directory(ours);
                let expected = match configuration {
                    // The consumer substitutes each candidate configuration
                    // into this segment; MSBuild's own must be among them.
                    Some(cfg) => ours.replacen(cfg.as_str(), &real_config, 1),
                    None => ours,
                };
                assert_eq!(
                    expected, real,
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
