//! Differential test: compare [`target_frameworks`]'s output against
//! `dotnet msbuild -getProperty:TargetFrameworks,TargetFramework` for the
//! real-world fsproj fixtures that exercise multi-TFM declaration shapes
//! (single, plural, self-referential rewrite).
//!
//! MSBuild is the reference implementation. If our enumeration differs
//! for any fixture, the parser has missed a substitution / condition
//! evaluation step that the LSP's eventual TFM-selection layer would
//! get wrong by the same margin.
//!
//! The infrastructure mirrors [`fsproj_msbuild_diff`]: run from the
//! repo root so `global.json` discovery doesn't pick up the corpus's
//! `global.json`, `-p:DISABLE_ARCADE=true` so the SDK resolver
//! doesn't hang on the unreachable Arcade pin, and a scrubbed
//! environment so inherited shell variables don't leak into MSBuild as
//! initial properties.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use borzoi_msbuild::{parse_fsproj_with_imports, target_frameworks};
use borzoi_oracle_harness::BoundedCommand;
use serde::Deserialize;

#[test]
fn fsharp_core_proto() {
    run_diff(
        "src/FSharp.Core/FSharp.Core.fsproj",
        &[("Configuration", "Proto")],
    );
}

#[test]
fn fsharp_core_release() {
    run_diff(
        "src/FSharp.Core/FSharp.Core.fsproj",
        &[("Configuration", "Release")],
    );
}

#[test]
fn fsharp_compiler_service_release() {
    // FCS's TFM rewrite (`$(FSharpNetCoreProductTargetFramework);$(TargetFrameworks)`)
    // is gated on `'$(FSharpNetCoreProductTargetFramework)' != ''`. MSBuild
    // gets that property from `eng/TargetFrameworks.props`, which is
    // imported indirectly through `Directory.Build.props`. Our import
    // resolver doesn't yet chase that chain end-to-end (an orthogonal gap
    // — tracked separately), so without priming we'd be diffing import
    // resolution rather than TFM enumeration. Passing the value as an
    // initial property to **both** sides keeps the oracle focused on the
    // self-reference rewrite + condition-evaluation path this test exists
    // to pin.
    run_diff(
        "src/Compiler/FSharp.Compiler.Service.fsproj",
        &[
            ("Configuration", "Release"),
            ("FSharpNetCoreProductTargetFramework", "net10.0"),
        ],
    );
}

fn run_diff(rel_fsproj: &str, extras: &[(&str, &str)]) {
    let corpus = common::corpus_root();
    let joined = corpus.join(rel_fsproj);
    assert!(joined.is_file(), "missing fixture {}", joined.display());
    let fsproj = std::fs::canonicalize(&joined)
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", joined.display()));

    let source = std::fs::read_to_string(&fsproj)
        .unwrap_or_else(|e| panic!("read {}: {e}", fsproj.display()));
    let mut props: HashMap<String, String> = HashMap::new();
    for (k, v) in extras {
        props.insert((*k).into(), (*v).into());
    }
    // Same DISABLE_ARCADE flag as the items differential test; both
    // sides must walk the same `Directory.Build.props` branch or
    // condition-gated property writes diverge.
    props.insert("DISABLE_ARCADE".into(), "true".into());
    let project = parse_fsproj_with_imports(
        &source,
        &fsproj,
        &props,
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));

    let ours = target_frameworks(&project);
    let theirs = run_msbuild_properties(&fsproj, extras);
    let expected = theirs.declared_tfms();

    assert_eq!(
        ours, expected,
        "TargetFramework enumeration disagrees with MSBuild for {rel_fsproj}\n  \
         ours:   {ours:?}\n  \
         theirs: {expected:?}\n  \
         MSBuild raw: TargetFrameworks={:?}, TargetFramework={:?}",
        theirs.target_frameworks, theirs.target_framework,
    );
}

/// Budget for one `dotnet msbuild` evaluation. A cold one restores packages and
/// walks the whole SDK import chain, which is legitimately minutes, so the bound
/// is far above the harness's per-request default: it is there to stop an
/// evaluation that has *stalled* — blocked on a NuGet lock held by a concurrent
/// run in a sibling worktree, say — from hanging the suite forever, not to police
/// a slow one.
const MSBUILD_TIMEOUT: Duration = Duration::from_secs(1800);

fn run_msbuild_properties(fsproj: &Path, extras: &[(&str, &str)]) -> MsbuildProperties {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    // Identical environment scrubbing to the items differential test —
    // see that file's docs for why. Anything we let through here could
    // flip a condition gate and silently change which TFM list MSBuild
    // reports.
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
    cmd.args([
        "msbuild",
        "-nologo",
        "-getProperty:TargetFrameworks,TargetFramework",
        "-p:DISABLE_ARCADE=true",
    ]);
    for (k, v) in extras {
        cmd.arg(format!("-p:{k}={v}"));
    }
    cmd.arg(fsproj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", fsproj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    let envelope: PropertiesEnvelope = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "could not parse msbuild JSON for {}: {e}\n--- stdout ---\n{stdout}",
            fsproj.display()
        )
    });
    envelope.properties
}

#[derive(Deserialize)]
struct PropertiesEnvelope {
    #[serde(rename = "Properties")]
    properties: MsbuildProperties,
}

#[derive(Deserialize)]
struct MsbuildProperties {
    /// MSBuild always emits both keys (with empty string when unset),
    /// so default-on-missing isn't strictly needed, but tolerate it in
    /// case a future SDK changes the shape.
    #[serde(default, rename = "TargetFrameworks")]
    target_frameworks: String,
    #[serde(default, rename = "TargetFramework")]
    target_framework: String,
}

impl MsbuildProperties {
    /// Apply the same plural-wins, singular-fallback policy
    /// [`target_frameworks`] does, against MSBuild's reported values.
    /// Yields the list we expect our parser to produce.
    fn declared_tfms(&self) -> Vec<String> {
        let plural: Vec<String> = self
            .target_frameworks
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if !plural.is_empty() {
            return plural;
        }
        let singular = self.target_framework.trim();
        if !singular.is_empty() {
            return vec![singular.to_string()];
        }
        Vec::new()
    }
}
