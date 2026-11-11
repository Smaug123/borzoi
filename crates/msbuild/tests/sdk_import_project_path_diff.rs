//! Differential: how `<Import Sdk="X" Project="P"/>` resolves `P`, against
//! real `dotnet msbuild`.
//!
//! MSBuild imports `Path.Combine(sdkResult.Path, P)`, normalised
//! (`FileUtilities.NormalizePath`, Evaluator.cs). So `P` may climb out of the
//! SDK directory with `..` — the Web SDK's `Sdk.Server.targets` imports
//! `Microsoft.NET.Sdk.Web.ProjectSystem`'s
//! `..\targets\Microsoft.NET.Sdk.Web.BeforeCommon.targets` — and an absolute
//! `P` names its own file.
//!
//! The SDK here is a temp directory MSBuild's default resolver finds through
//! `MSBuildSDKsPath`. It holds marker files inside the SDK's `Sdk/` directory,
//! in its sibling `targets/`, in another SDK, and outside every SDK; each
//! appends its own name to `Seen`. A generated project imports respellings of
//! those files — `.` and `ghost/..` segments, either separator, doubled
//! separators, `%2e` escapes, absolute forms — with repeats, so MSBuild's
//! duplicate-import skip is graded alongside resolution. One `dotnet msbuild
//! -getProperty:Seen` grades a whole project; we must commit (no diagnostic)
//! and agree exactly.
//!
//! Runs under `nix develop` like the sibling `dotnet msbuild` differentials.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod common;

use borzoi_msbuild::{SdkPaths, SdkResolution, SdkResolveError, parse_fsproj_with_imports};
use borzoi_oracle_harness::BoundedCommand;
use common::SplitMix64;

const MSBUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// Marker files, each as the path components that reach it from the SDK's
/// `Sdk/` directory (`sdks/MySdk/Sdk`).
const TARGETS: &[(&str, &[&str])] = &[
    ("E", &["E.props"]),
    ("S", &["sub", "S.props"]),
    ("D", &["sub", "deeper", "D.props"]),
    ("T", &["..", "targets", "T.props"]),
    ("O", &["..", "..", "Other", "Sdk", "O.props"]),
    ("X", &["..", "..", "..", "outside", "X.props"]),
];

/// Imports per generated project.
const IMPORTS_PER_PROJECT: usize = 30;

struct Layout {
    _dir: tempfile::TempDir,
    /// Canonical temp root, so absolute spellings agree with what both
    /// evaluators see (macOS's `/var` → `/private/var`).
    root: PathBuf,
}

impl Layout {
    fn sdks(&self) -> PathBuf {
        self.root.join("sdks")
    }

    fn sdk_root(&self) -> PathBuf {
        self.sdks().join("MySdk").join("Sdk")
    }

    fn new() -> Layout {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalise tempdir");
        let layout = Layout { _dir: dir, root };
        let sdk_root = layout.sdk_root();
        std::fs::create_dir_all(&sdk_root).expect("mkdir sdk");
        for name in ["Sdk.props", "Sdk.targets"] {
            std::fs::write(sdk_root.join(name), "<Project/>").expect("write sdk entry");
        }
        for (marker, components) in TARGETS {
            let path = normalise(&components.iter().fold(sdk_root.clone(), |p, c| p.join(c)));
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir marker dir");
            std::fs::write(
                &path,
                format!(
                    "<Project><PropertyGroup><Seen>$(Seen);{marker}</Seen></PropertyGroup></Project>"
                ),
            )
            .expect("write marker");
        }
        layout
    }
}

/// Lexical `.`/`..` collapse — what `Path.GetFullPath` does.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// One spelling of `components` (relative to the SDK root), or of the
/// target's absolute path.
fn respell(rng: &mut SplitMix64, layout: &Layout, components: &[&str]) -> String {
    let separator = |rng: &mut SplitMix64| -> &'static str { rng.pick(&["/", "\\", "//", "/\\"]) };
    let mut parts: Vec<String> = Vec::new();
    let absolute = rng.below(6) == 0;
    let segments: Vec<String> = if absolute {
        let full = normalise(&components.iter().fold(layout.sdk_root(), |p, c| p.join(c)));
        full.components()
            .skip(1) // the root `/`
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    } else {
        if rng.below(4) == 0 {
            parts.push(".".to_string());
        }
        components.iter().map(|c| (*c).to_string()).collect()
    };
    for segment in segments {
        match rng.below(8) {
            0 => parts.push(".".to_string()),
            1 => {
                parts.push("ghost".to_string());
                parts.push("..".to_string());
            }
            2 => parts.push("%2e".to_string()),
            _ => {}
        }
        let segment = if segment == ".." && rng.below(4) == 0 {
            "%2e%2e".to_string()
        } else {
            segment
        };
        parts.push(segment);
    }
    let mut out = String::new();
    if absolute {
        out.push('/');
    }
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(separator(rng));
        }
        out.push_str(part);
    }
    out
}

/// The variables both sides see: what `dotnet` needs, plus `MSBuildSDKsPath`.
fn child_environment(layout: &Layout) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| {
            matches!(key.as_str(), "PATH" | "HOME" | "TMPDIR")
                || key.starts_with("DOTNET_")
                || key.starts_with("NUGET_")
        })
        .collect();
    env.insert(
        "MSBuildSDKsPath".to_string(),
        layout.sdks().to_string_lossy().into_owned(),
    );
    env
}

fn msbuild_seen(proj: &Path, env: &HashMap<String, String>) -> String {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(proj.parent().unwrap());
    cmd.env_clear();
    cmd.envs(env);
    cmd.args(["msbuild", "-nologo", "-getProperty:Seen"]);
    cmd.arg(proj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", proj.display()));
    String::from_utf8(out.stdout)
        .expect("msbuild stdout is UTF-8")
        .trim()
        .to_string()
}

fn check_seed(seed: u64) {
    let layout = Layout::new();
    let mut rng = SplitMix64(seed);
    let imports: Vec<String> = (0..IMPORTS_PER_PROJECT)
        .map(|_| {
            let (_, components) = rng.pick(TARGETS);
            respell(&mut rng, &layout, components)
        })
        .collect();
    let body: String = imports
        .iter()
        .map(|p| format!("  <Import Sdk=\"MySdk\" Project=\"{p}\" />\n"))
        .collect();
    let xml = format!("<Project>\n{body}</Project>\n");
    let proj_dir = layout.root.join("proj");
    std::fs::create_dir_all(&proj_dir).expect("mkdir proj");
    let proj = proj_dir.join("p.proj");
    std::fs::write(&proj, &xml).expect("write project");

    let env = child_environment(&layout);
    let sdk_root = layout.sdk_root();
    let resolver = |name: &str| -> Result<SdkResolution, SdkResolveError> {
        if name == "MySdk" {
            Ok(SdkResolution::Single(SdkPaths {
                root: sdk_root.clone(),
                props: sdk_root.join("Sdk.props"),
                targets: sdk_root.join("Sdk.targets"),
            }))
        } else {
            Err(SdkResolveError::NotFound)
        }
    };
    let parsed =
        parse_fsproj_with_imports(&xml, &proj, &HashMap::new(), &env, Some(&resolver), None)
            .unwrap_or_else(|e| panic!("seed {seed}: our parse: {e}"));
    assert!(
        parsed.diagnostics.is_empty(),
        "seed {seed}: we declined where MSBuild evaluates cleanly:\n{xml}\n{:#?}",
        parsed.diagnostics
    );
    let ours = parsed.properties.get("Seen").cloned().unwrap_or_default();
    let theirs = msbuild_seen(&proj, &env);
    assert_eq!(ours, theirs, "seed {seed}: Seen differs for\n{xml}");
}

#[test]
fn sdk_import_project_paths_resolve_as_msbuild_does() {
    for seed in 0..4 {
        check_seed(seed);
    }
}

#[test]
#[ignore = "soak: fresh seed, BORZOI_MSBUILD_SOAK_SEED / _CASES"]
fn sdk_import_project_paths_soak() {
    let (seed, cases) = common::soak_parameters("sdk_import_project_paths", 20);
    for case in 0..cases as u64 {
        check_seed(seed.wrapping_add(case));
    }
}

/// The real Web SDK: `Sdk.Server.targets` imports
/// `Microsoft.NET.Sdk.Web.ProjectSystem`'s
/// `..\targets\Microsoft.NET.Sdk.Web.BeforeCommon.targets`, which defaults
/// `EventSourceSupport` to `true` for an AOT-published app before
/// `Microsoft.NET.Sdk`'s targets run. We must reach it, commit the value, and
/// agree with MSBuild.
#[test]
fn web_sdk_before_common_targets_is_imported() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalise tempdir");
    let proj = root.join("Web.fsproj");
    let xml = r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <PublishAot>true</PublishAot>
  </PropertyGroup>
</Project>
"#;
    std::fs::write(&proj, xml).expect("write project");

    let dotnet_root = common::dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = common::workload_env_from_process();
    let workload_env = borzoi_msbuild::workloads::WorkloadEnvironment {
        user_dotnet_root: user_dotnet_root.as_deref(),
        overrides_present,
        // The fixture tempdir has no global.json above it.
        global_json_pins_workload_set: false,
    };
    let resolver = |name: &str| {
        borzoi_msbuild::resolve_sdk(&dotnet_root, None, name, None, None, &workload_env)
    };
    let parsed = parse_fsproj_with_imports(
        xml,
        &proj,
        &HashMap::new(),
        &common::oracle_environment(),
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}"));
    let declined_imports: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| {
            matches!(
                &d.kind,
                borzoi_msbuild::DiagnosticKind::UnsupportedConstruct { element }
                    if element.starts_with("Import")
            ) || matches!(&d.kind, borzoi_msbuild::DiagnosticKind::ImportFailed { .. })
        })
        .collect();
    assert!(
        declined_imports.is_empty(),
        "the Web SDK chain declined an import: {declined_imports:#?}"
    );
    assert!(
        !parsed.property_provenance_untrusted("EventSourceSupport"),
        "EventSourceSupport untrusted: {:?}",
        parsed.property_untrusted_cause("EventSourceSupport")
    );
    let ours = parsed
        .properties
        .get("EventSourceSupport")
        .cloned()
        .unwrap_or_default();

    let mut cmd = Command::new("dotnet");
    cmd.current_dir(&root);
    cmd.env_clear();
    cmd.envs(std::env::vars().filter(|(key, _)| {
        matches!(key.as_str(), "PATH" | "HOME" | "TMPDIR")
            || key.starts_with("DOTNET_")
            || key.starts_with("NUGET_")
    }));
    cmd.args(["msbuild", "-nologo", "-getProperty:EventSourceSupport"]);
    cmd.arg(&proj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", proj.display()));
    let theirs = String::from_utf8(out.stdout)
        .expect("msbuild stdout is UTF-8")
        .trim()
        .to_string();
    assert_eq!(theirs, "true", "MSBuild no longer exercises the import");
    assert_eq!(ours, theirs);
}
