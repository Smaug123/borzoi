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

use crate::common::parse_fsproj_with_imports;
use borzoi_msbuild::{SdkPaths, SdkResolution, SdkResolveError};
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
    /// Where the SDKs live: `<root>/sdks`, or below a further directory.
    sdks: PathBuf,
}

impl Layout {
    fn sdks(&self) -> PathBuf {
        self.sdks.clone()
    }

    fn sdk_root(&self) -> PathBuf {
        self.sdks().join("MySdk").join("Sdk")
    }

    fn new() -> Layout {
        Layout::under("")
    }

    /// The SDKs below `<root>/<parent>`.
    fn under(parent: &str) -> Layout {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = borzoi_msbuild::observed::host_fs::canonicalize(dir.path())
            .expect("canonicalise tempdir");
        let sdks = root.join(parent).join("sdks");
        let layout = Layout {
            _dir: dir,
            root,
            sdks,
        };
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
/// target's absolute path. `escaped_separators` admits `%2f`/`%5c`, which the
/// evaluator declines: MSBuild decodes them after deciding what is a
/// separator.
fn respell(
    rng: &mut SplitMix64,
    layout: &Layout,
    components: &[&str],
    escaped_separators: bool,
) -> String {
    let separators: &[&str] = if escaped_separators {
        &["/", "\\", "//", "/\\", "%2f", "%5c"]
    } else {
        &["/", "\\", "//", "/\\"]
    };
    let separator = |rng: &mut SplitMix64| -> &'static str { rng.pick(separators) };
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

/// MSBuild's `Seen`, or its output when the evaluation fails.
fn msbuild_seen(proj: &Path, env: &HashMap<String, String>) -> Result<String, String> {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(proj.parent().unwrap());
    cmd.env_clear();
    cmd.envs(env);
    cmd.args(["msbuild", "-nologo", "-getProperty:Seen"]);
    cmd.arg(proj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run()
        .unwrap_or_else(|e| panic!("dotnet msbuild for {}: {e:?}", proj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    if out.status.success() {
        Ok(stdout.trim().to_string())
    } else {
        Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)))
    }
}

/// Write a project importing each of `imports` from `MySdk`, and evaluate it
/// both ways.
fn evaluate_both(
    layout: &Layout,
    imports: &[String],
) -> (
    String,
    borzoi_msbuild::ParsedProject,
    Result<String, String>,
) {
    let body: String = imports
        .iter()
        .map(|p| format!("  <Import Sdk=\"MySdk\" Project=\"{p}\" />\n"))
        .collect();
    let xml = format!("<Project>\n{body}</Project>\n");
    let proj_dir = layout.root.join("proj");
    std::fs::create_dir_all(&proj_dir).expect("mkdir proj");
    let proj = proj_dir.join("p.proj");
    std::fs::write(&proj, &xml).expect("write project");

    let env = child_environment(layout);
    let sdk_root = layout.sdk_root();
    let resolver = |name: &str,
                    _observer: &borzoi_msbuild::observed::Observer|
     -> Result<SdkResolution, SdkResolveError> {
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
            .unwrap_or_else(|e| panic!("our parse: {e}\n{xml}"));
    let theirs = msbuild_seen(&proj, &env);
    (xml, parsed, theirs)
}

/// Where we commit, MSBuild evaluates and agrees; where MSBuild fails, we
/// decline. With `must_commit` (an input inside the modelled domain), we must
/// also commit wherever MSBuild evaluates. Returns whether MSBuild evaluated.
fn assert_certain_implies_exact(
    label: &str,
    xml: &str,
    parsed: &borzoi_msbuild::ParsedProject,
    theirs: Result<String, String>,
    must_commit: bool,
) -> bool {
    match theirs {
        Ok(_) if !must_commit && !parsed.diagnostics.is_empty() => true,
        Ok(theirs) => {
            assert!(
                parsed.diagnostics.is_empty(),
                "{label}: we declined where MSBuild evaluates cleanly:\n{xml}\n{:#?}",
                parsed.diagnostics
            );
            let ours = parsed.properties.get("Seen").cloned().unwrap_or_default();
            assert_eq!(ours, theirs, "{label}: Seen differs for\n{xml}");
            true
        }
        Err(output) => {
            assert!(
                !parsed.diagnostics.is_empty(),
                "{label}: MSBuild fails but we committed {:?}:\n{xml}\n{output}",
                parsed.properties.get("Seen")
            );
            false
        }
    }
}

fn check_seed(seed: u64) {
    let layout = Layout::new();
    let mut rng = SplitMix64(seed);
    let imports: Vec<String> = (0..IMPORTS_PER_PROJECT)
        .map(|_| {
            let (_, components) = rng.pick(TARGETS);
            respell(&mut rng, &layout, components, false)
        })
        .collect();
    let (xml, parsed, theirs) = evaluate_both(&layout, &imports);
    let label = format!("seed {seed}");
    assert!(
        theirs.is_ok(),
        "{label}: every spelling names a file, yet MSBuild failed:\n{xml}\n{theirs:?}"
    );
    assert_certain_implies_exact(&label, &xml, &parsed, theirs, true);
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

/// A respelling with segments after the file name, and whether it lies in
/// the modelled domain. Some tails collapse lexically (`E.props/.`,
/// `E.props/ghost/..`) and MSBuild imports the file; some leave a trailing
/// separator (`E.props/`, `E.props/./`), name a child of a file
/// (`E.props/ghost`) or the SDK directory (`E.props/..`), and MSBuild fails.
/// Outside the domain: escaped separators, and a junk segment the `..` after
/// it erases — MSBuild acts on what it holds (splits the list, rejects the
/// NUL, globs), so collapsing it away would commit to the wrong file.
fn respell_with_tail(rng: &mut SplitMix64, layout: &Layout) -> (String, bool) {
    let (_, components) = rng.pick(TARGETS);
    let escaped_separators = rng.below(4) == 0;
    let mut out = respell(rng, layout, components, escaped_separators);
    let junk = rng.below(3) == 0;
    if junk {
        let junk = rng.pick(&["%00", "a;b", "*", " x", "%3b", "%C3%A9", "x?"]);
        out = format!("{junk}/../{out}");
    }
    for _ in 0..rng.below(3) {
        out.push_str(rng.pick(&["/", "\\"]));
        out.push_str(rng.pick(&[".", "ghost", "..", "%2e", ""]));
    }
    (out, !escaped_separators && !junk)
}

/// One import per project: an import MSBuild fails fails the whole
/// evaluation, so the verdict is per spelling.
fn check_tail_cases(seed: u64, cases: usize) -> (usize, usize) {
    let layout = Layout::new();
    let mut rng = SplitMix64(seed);
    let (mut imported, mut failed) = (0, 0);
    for case in 0..cases {
        let (import, in_domain) = respell_with_tail(&mut rng, &layout);
        let (xml, parsed, theirs) = evaluate_both(&layout, std::slice::from_ref(&import));
        let label = format!("seed {seed} case {case}");
        if assert_certain_implies_exact(&label, &xml, &parsed, theirs, in_domain) {
            imported += 1;
        } else {
            failed += 1;
        }
    }
    (imported, failed)
}

#[test]
fn sdk_import_project_path_tails_resolve_as_msbuild_does() {
    let (imported, failed) = check_tail_cases(0, 16);
    // Both verdicts must occur, or the generator stopped reaching one side.
    assert!(
        imported > 0 && failed > 0,
        "imported {imported}, failed {failed}"
    );
}

/// Spellings outside the modelled domain whose `..` erases a segment MSBuild
/// acts on. Each fails or imports differently in MSBuild, so committing to the
/// collapsed path would be wrong; we must decline or agree.
#[test]
fn sdk_import_project_paths_outside_the_domain_never_commit_wrongly() {
    let layout = Layout::new();
    for import in [
        // MSB4102: a NUL in the path.
        "%00/../E.props",
        // A list: `<root>/E.props` and the project-relative `sub/../E.props`.
        "E.props;sub/../E.props",
        // Wildcards glob before normalising.
        "x?/../E.props",
        "*/../E.props",
        // Whitespace is trimmed from a list entry.
        " x/../E.props",
        // A decoded `\` is no separator when `..` collapses.
        ".%5c./../targets/T.props",
        "sub%5c../E.props",
        "sub%2f../E.props",
    ] {
        let import = import.to_string();
        let (xml, parsed, theirs) = evaluate_both(&layout, std::slice::from_ref(&import));
        assert_certain_implies_exact(&import, &xml, &parsed, theirs, false);
    }
}

/// An SDK root holding a character MSBuild re-parses in the combined import
/// path (`;` splits, `*`/`?` glob, `%XX` decodes): we decline or agree.
#[test]
fn sdk_roots_msbuild_would_reparse_never_commit_wrongly() {
    for parent in ["a;b", "st*r", "q?", "x%41"] {
        let layout = Layout::under(parent);
        for import in ["E.props", "../targets/T.props", "../../../../E.props"] {
            let import = import.to_string();
            let (xml, parsed, theirs) = evaluate_both(&layout, std::slice::from_ref(&import));
            assert_certain_implies_exact(
                &format!("{parent:?} {import}"),
                &xml,
                &parsed,
                theirs,
                false,
            );
        }
    }
}

#[test]
#[ignore = "soak: fresh seed, BORZOI_MSBUILD_SOAK_SEED / _CASES"]
fn sdk_import_project_path_tails_soak() {
    let (seed, cases) = common::soak_parameters("sdk_import_project_path_tails", 40);
    let (imported, failed) = check_tail_cases(seed, cases);
    println!("imported {imported}, failed {failed}");
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
    let root =
        borzoi_msbuild::observed::host_fs::canonicalize(dir.path()).expect("canonicalise tempdir");
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
    let resolver = |name: &str, observer: &borzoi_msbuild::observed::Observer| {
        borzoi_msbuild::resolve_sdk(
            &dotnet_root,
            None,
            name,
            None,
            None,
            &workload_env,
            observer,
        )
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
