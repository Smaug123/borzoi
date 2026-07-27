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

mod common;

use std::collections::HashMap;

use borzoi_msbuild::{OutputDirVerdict, parse_fsproj_with_imports};
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
];

/// The two sides as *directories*: separators unified, and at most one
/// trailing separator dropped. See the module docs for why each is
/// semantics-free here and what is deliberately left alone.
fn as_directory(path: &str) -> String {
    let unified = path.replace('\\', "/");
    unified.strip_suffix('/').unwrap_or(&unified).to_owned()
}

#[test]
fn declared_output_dirs_agree_with_msbuild() {
    let mut oracle = Oracle::spawn();
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

        let empty = HashMap::new();
        let parsed = parse_fsproj_with_imports(
            &xml,
            &path,
            &empty,
            &common::oracle_environment(),
            None,
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
