//! End-to-end smoke test for Phase 8 — the bundled csharp-sidecar.
//!
//! Scenario: an F# project declares a `<ProjectReference>` to a sibling
//! C# project. We
//!   1. parse the fsproj via [`parse_fsproj`] (the F# end is at least
//!      structurally loadable),
//!   2. spawn the sidecar that `build.rs` published under `OUT_DIR`,
//!      using the [`start_bundled_sidecar`] discovery shim — *not* the
//!      hand-built DLL the rest of the integration tests use,
//!   3. drive `buildMetadata` over the referenced csproj,
//!   4. parse the emitted metadata DLL with [`Ecma335Assembly`] and
//!      assert the public C# type defined by the fixture is enumerable.
//!
//! This is the smallest end-to-end loop that exercises every Phase 1–8
//! surface in concert. There is no F# binder yet (that's a future
//! phase); the fsproj parse stands in for the F# side until it exists.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::csharp_sidecar::start_bundled_sidecar;
use borzoi::project_assets::resolve_transitive_project_tfms;
use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_msbuild::{DiagnosticKind, parse_fsproj};
use borzoi_spawn::BoundedCommand;

#[test]
fn fsproj_to_csharp_metadata_e2e() {
    let dotnet = find_dotnet();
    let workspace_root = workspace_root();
    let dotnet_root = dotnet_root_for_tests();

    let fixture = make_fixture();

    // F# end: the fsproj is SDK-shorthand (`<Project Sdk="Microsoft.NET.Sdk">`),
    // which phase 7a flags as `UnsupportedConstruct` while deferring SDK
    // resolution to a later phase. Everything else about the walk should
    // be clean: exactly one explicit Compile, exactly one
    // ProjectReference pointing at the sibling csproj, and no other
    // diagnostics.
    let fsproj_src = std::fs::read_to_string(fixture.fsproj_path()).expect("read fsproj");
    let parsed = parse_fsproj(
        &fsproj_src,
        fixture.fsproj_path(),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("parse fsproj");
    let kinds: Vec<&DiagnosticKind> = parsed.diagnostics.iter().map(|d| &d.kind).collect();
    assert_eq!(
        kinds,
        [&DiagnosticKind::UnsupportedConstruct {
            element: "Project Sdk=\"Microsoft.NET.Sdk\"".to_string()
        }],
        "fsproj should parse with only the SDK-shorthand diagnostic, got {:?}",
        parsed.diagnostics,
    );
    assert_eq!(
        parsed.items.len(),
        1,
        "fsproj should declare exactly one Compile, got {:?}",
        parsed.items,
    );
    // ProjectReference: the fsproj declares `..\csharp\Lib.csproj`.
    // The parser does not canonicalise (no filesystem touch), so the
    // resolved include is `<fsproj-dir>/../csharp/Lib.csproj` — same
    // *canonical* file as `fixture.csproj_path()` but a different
    // PathBuf. Compare via canonicalise() so the test asserts identity
    // of file, not byte-equality of paths.
    assert_eq!(
        parsed.project_references.len(),
        1,
        "fsproj should declare exactly one ProjectReference, got {:?}",
        parsed.project_references,
    );
    let pr_canon = std::fs::canonicalize(&parsed.project_references[0].include)
        .expect("canonicalise resolved ProjectReference");
    let csproj_canon =
        std::fs::canonicalize(fixture.csproj_path()).expect("canonicalise fixture csproj");
    assert_eq!(
        pr_canon, csproj_canon,
        "ProjectReference should resolve to the sibling Lib.csproj",
    );

    // MSBuildWorkspace consults `project.assets.json`, which is produced
    // by `dotnet restore`. Without it, OpenProjectAsync surfaces an
    // NU1100/NU1101 diagnostic and the emit fails.
    restore_once(&dotnet, fixture.csharp_dir());

    let mut handle = start_bundled_sidecar(&dotnet, &workspace_root, &dotnet_root)
        .expect("start_bundled_sidecar");

    // After `dotnet restore` lands `obj/project.assets.json`, the LSP-style
    // helper can derive the closure-wide TFM map a real caller would send.
    let project_tfms = resolve_transitive_project_tfms(fixture.csproj_path(), "net10.0")
        .expect("resolve closure-wide TFM map for sidecar fixture");
    let result = handle
        .build_metadata(fixture.csproj_path(), "Debug", "net10.0", &project_tfms)
        .expect("buildMetadata of the C# fixture should succeed");

    assert!(
        result.metadata_dll_path.exists(),
        "expected metadata DLL at {}",
        result.metadata_dll_path.display(),
    );

    let bytes = std::fs::read(&result.metadata_dll_path).expect("read emitted DLL");
    let view = Ecma335Assembly::parse(&bytes).expect("parse emitted DLL");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate emitted type defs");
    let fqns: Vec<String> = entities
        .iter()
        .map(|e| {
            if e.namespace.is_empty() {
                e.name.clone()
            } else {
                format!("{}.{}", e.namespace.join("."), e.name)
            }
        })
        .collect();
    assert!(
        fqns.iter().any(|f| f == "BundledE2E.Greeter"),
        "expected BundledE2E.Greeter from sidecar emit, got {fqns:?}",
    );

    handle.shutdown().expect("shutdown clean");
}

/// Materialises a `csharp/` + `fsharp/` fixture tree under `target/`.
/// The fixture is rebuilt on every run with a fresh nonce so the
/// content-addressed sidecar cache (which lives under
/// `<workspace>/obj/borzoi/csharp-sidecar/`) is forced to
/// miss — otherwise a previous run's DLL would shadow the one we
/// expect this run to emit. (We do not assert `!from_cache` because
/// the test asserts the *DLL contents*, which a stale cache would
/// silently invalidate.)
struct Fixture {
    csharp_dir: PathBuf,
    csproj_path: PathBuf,
    fsproj_path: PathBuf,
}

impl Fixture {
    fn csharp_dir(&self) -> &Path {
        &self.csharp_dir
    }
    fn csproj_path(&self) -> &Path {
        &self.csproj_path
    }
    fn fsproj_path(&self) -> &Path {
        &self.fsproj_path
    }
}

fn make_fixture() -> Fixture {
    let nonce = unique_nonce();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("csharp-sidecar-bundled-e2e-{nonce}"));
    let csharp = root.join("csharp");
    let fsharp = root.join("fsharp");
    std::fs::create_dir_all(&csharp).expect("mkdir csharp fixture");
    std::fs::create_dir_all(&fsharp).expect("mkdir fsharp fixture");

    std::fs::write(
        csharp.join("Lib.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>BundledE2E</RootNamespace>\n",
            "    <AssemblyName>BundledE2E.Lib</AssemblyName>\n",
            "    <Nullable>enable</Nullable>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write Lib.csproj");
    std::fs::write(
        csharp.join("Lib.cs"),
        // The nonce keeps the source distinct across runs so the
        // sidecar's content-addressed cache misses; without it, the
        // assertions still pass but the test would silently cease to
        // exercise the emit path on subsequent runs.
        format!(
            "// nonce {nonce}\nnamespace BundledE2E;\n\npublic sealed class Greeter\n{{\n    public string Hello() => \"hi\";\n}}\n",
        ),
    )
    .expect("write Lib.cs");

    std::fs::write(
        fsharp.join("App.fsproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>BundledE2E.App</RootNamespace>\n",
            "  </PropertyGroup>\n",
            "  <ItemGroup>\n",
            "    <Compile Include=\"App.fs\" />\n",
            "    <ProjectReference Include=\"..\\csharp\\Lib.csproj\" />\n",
            "  </ItemGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write App.fsproj");
    std::fs::write(
        fsharp.join("App.fs"),
        "module BundledE2E.App\n\nlet main () = ()\n",
    )
    .expect("write App.fs");

    Fixture {
        csproj_path: csharp.join("Lib.csproj"),
        fsproj_path: fsharp.join("App.fsproj"),
        csharp_dir: csharp,
    }
}

/// `dotnet restore` runs at most once per process. The fixture directory
/// is unique per test invocation (nonce in the path), so a single `Once`
/// is enough — there is no second fixture to confuse it with.
fn restore_once(dotnet: &Path, fixture_dir: &Path) {
    static RESTORED: Once = Once::new();
    RESTORED.call_once(|| {
        let mut cmd = Command::new(dotnet);
        cmd.arg("restore").current_dir(fixture_dir);
        // A cold restore fetches packages, which is legitimately minutes: the
        // bound is there to stop a *stalled* restore (blocked on a NuGet lock
        // held by a concurrent run in a sibling worktree, say) from hanging the
        // suite forever, not to police a slow one.
        BoundedCommand::new(cmd)
            .timeout(Duration::from_secs(1800))
            .run_ok(format_args!(
                "`dotnet restore` for {}",
                fixture_dir.display()
            ));
    });
}

fn find_dotnet() -> PathBuf {
    let mut cmd = Command::new("dotnet");
    cmd.arg("--version");
    BoundedCommand::new(cmd)
        .run_ok("`dotnet --version` (the .NET SDK is required — run inside `nix develop`)");
    PathBuf::from("dotnet")
}

fn dotnet_root_for_tests() -> PathBuf {
    std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/lsp/`; the workspace root sits two
    // directories up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

fn unique_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock running")
        .as_nanos()
}
