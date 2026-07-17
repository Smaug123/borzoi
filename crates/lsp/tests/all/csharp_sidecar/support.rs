//! Shared fixtures and helpers for the C# sidecar integration tests.
//!
//! Split out of the former single-file `csharp_sidecar.rs`; pure code
//! motion (helpers are unchanged beyond `pub(crate)` visibility).

pub use std::collections::{BTreeMap, HashMap};
pub use std::path::{Path, PathBuf};
pub use std::process::Command;
pub use std::sync::{Mutex, MutexGuard};
pub use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use borzoi::csharp_sidecar::{
    PROTOCOL_VERSION, SidecarError, SidecarErrorKind, SidecarHandle, TransitiveProjectRef,
    start_bundled_sidecar,
};
pub use borzoi::project_assets::resolve_transitive_project_tfms;
pub use borzoi_assembly::test_support::{NormalisedAssembly, normalise_entities};
pub use borzoi_assembly::{Access, Ecma335Assembly, EcmaView, Entity};
pub use borzoi_spawn::BoundedCommand;

/// Budget for one `dotnet` build/restore of a fixture.
///
/// A cold restore-and-build fetches packages and runs a compiler, which is
/// legitimately minutes, so the bound sits far above the driver's per-child
/// default: it is there to stop a build that has *stalled* — blocked on a NuGet
/// lock held by a concurrent run in a sibling worktree, say — from hanging the
/// suite forever, not to police a slow one.
pub(crate) const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Builds a top/leaf project pair under `fixture_root` and returns
/// `(top_csproj, leaf_cs)`. Identical between the cache-hit and cascade-
/// invalidation tests, so factored here. The per-test `label` is mixed into
/// the leaf type name to avoid any chance of cache-path coincidence across
/// fixtures that happen to share a nonce.
pub(crate) fn write_project_reference_fixture(
    fixture_root: &Path,
    nonce: u128,
    label: &str,
) -> (PathBuf, PathBuf) {
    let top_dir = fixture_root.join("top");
    let leaf_dir = fixture_root.join("leaf");
    std::fs::create_dir_all(&top_dir).expect("create top dir");
    std::fs::create_dir_all(&leaf_dir).expect("create leaf dir");

    std::fs::write(
        leaf_dir.join("Leaf.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>LeafFixture</RootNamespace>\n",
            "    <AssemblyName>LeafFixture</AssemblyName>\n",
            "    <Deterministic>true</Deterministic>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write leaf csproj");
    let leaf_cs = leaf_dir.join("Leaf.cs");
    std::fs::write(
        &leaf_cs,
        format!(
            "// {label} nonce {nonce}\nnamespace LeafFixture;\n\npublic sealed class LeafType {{ }}\n"
        ),
    )
    .expect("write leaf cs");

    std::fs::write(
        top_dir.join("Top.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>TopFixture</RootNamespace>\n",
            "    <AssemblyName>TopFixture</AssemblyName>\n",
            "    <Deterministic>true</Deterministic>\n",
            "  </PropertyGroup>\n",
            "  <ItemGroup>\n",
            "    <ProjectReference Include=\"../leaf/Leaf.csproj\" />\n",
            "  </ItemGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write top csproj");
    let top_csproj = top_dir.join("Top.csproj");
    std::fs::write(
        top_dir.join("Top.cs"),
        format!(
            "// {label} nonce {nonce}\nusing LeafFixture;\nnamespace TopFixture;\n\npublic sealed class TopType {{ public LeafType? Leaf; }}\n"
        ),
    )
    .expect("write top cs");

    (top_csproj, leaf_cs)
}

/// Build the same csproj two ways — sidecar emit and `dotnet build
/// -p:ProduceReferenceAssembly=true` — read both DLLs via [`Ecma335Assembly`],
/// filter to types whose root namespace is `user_namespace` (so compiler-
/// injected attributes under `Microsoft.CodeAnalysis` / `System.Runtime`
/// don't pollute the diff), normalise both to [`NormalisedAssembly`], and
/// assert equality.
pub(crate) fn diff_sidecar_against_ref_assembly(
    dotnet: &Path,
    fixture_subdir: &str,
    csproj_name: &str,
    asm_name: &str,
    user_namespace: &str,
) {
    let fixture = workspace_root()
        .join("tools/csharp-sidecar/test-fixtures")
        .join(fixture_subdir);
    let _fixture_lock = lock_fixture(dotnet, &fixture);

    let mut handle = build_and_start(dotnet);
    let csproj = fixture.join(csproj_name);
    let sidecar = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect("sidecar buildMetadata returns a metadata DLL");
    handle.shutdown().expect("shutdown clean");

    let ref_dll = build_ref_assembly(dotnet, &fixture, asm_name, "Debug", "net10.0");
    assert!(
        ref_dll.exists(),
        "ProduceReferenceAssembly did not produce a DLL at {}",
        ref_dll.display(),
    );

    let sidecar_norm = read_and_normalise(&sidecar.metadata_dll_path, asm_name, user_namespace);
    let dotnet_norm = read_and_normalise(&ref_dll, asm_name, user_namespace);

    assert_eq!(
        sidecar_norm, dotnet_norm,
        "sidecar emit and `dotnet build -p:ProduceReferenceAssembly=true` \
         disagree on the {user_namespace} surface.\n  \
         sidecar: {sidecar_norm:#?}\n  dotnet:  {dotnet_norm:#?}",
    );
}

/// Run `dotnet build -p:ProduceReferenceAssembly=true
/// -p:TargetFramework=<tfm>` inside the fixture dir and return the expected
/// ref-assembly output path. The MSBuild `ProduceReferenceAssembly`
/// machinery emits at `obj/<config>/<tfm>/ref/<AsmName>.dll` regardless of
/// the rest of the build configuration; we don't read MSBuild's output to
/// discover it.
///
/// Passing `TargetFramework` explicitly is required for multi-TFM csprojs:
/// without it, `dotnet build` picks the first listed TFM in
/// `<TargetFrameworks>`, which would be `netstandard2.0` for the multi-tfm
/// leaf — disagreeing with the sidecar's NuGet-selected `net6.0` and
/// rendering the differential meaningless. For single-TFM csprojs the
/// override is redundant but harmless.
pub(crate) fn build_ref_assembly(
    dotnet: &Path,
    fixture: &Path,
    asm_name: &str,
    configuration: &str,
    tfm: &str,
) -> PathBuf {
    let mut cmd = Command::new(dotnet);
    cmd.args([
        "build",
        "--configuration",
        configuration,
        "--nologo",
        "-p:ProduceReferenceAssembly=true",
        &format!("-p:TargetFramework={tfm}"),
    ])
    .current_dir(fixture);
    BoundedCommand::new(cmd)
        .timeout(BUILD_TIMEOUT)
        .run_ok(format_args!("dotnet build of {}", fixture.display()));
    fixture
        .join("obj")
        .join(configuration)
        .join(tfm)
        .join("ref")
        .join(format!("{asm_name}.dll"))
}

/// Parse a DLL, project to [`Entity`]s, keep only those whose root
/// namespace component equals `user_namespace`, and reduce to a
/// [`NormalisedAssembly`]. Filtering by namespace strips compiler-injected
/// types (`Microsoft.CodeAnalysis.EmbeddedAttribute`,
/// `System.Runtime.CompilerServices.Nullable*Attribute`, ...) that vary
/// between Roslyn's two emit modes and would otherwise drown the signal.
pub(crate) fn read_and_normalise(
    dll: &Path,
    asm_label: &str,
    user_namespace: &str,
) -> NormalisedAssembly {
    let bytes = std::fs::read(dll).unwrap_or_else(|e| panic!("read {}: {e}", dll.display()));
    let view =
        Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {}: {e:?}", dll.display()));
    let entities = view
        .enumerate_type_defs()
        .unwrap_or_else(|e| panic!("enumerate {}: {e:?}", dll.display()));
    let user: Vec<Entity> = entities
        .into_iter()
        .filter(|e| e.namespace.first().map(String::as_str) == Some(user_namespace))
        .collect();
    normalise_entities(asm_label, &user)
}

/// Locate `dotnet` on PATH. Tests run in the Nix devShell, which always
/// provides the SDK; missing `dotnet` is a fatal environment error, not
/// a soft skip.
pub(crate) fn find_dotnet() -> PathBuf {
    let mut cmd = Command::new("dotnet");
    cmd.arg("--version");
    BoundedCommand::new(cmd)
        .run_ok("`dotnet --version` (the .NET SDK is required — run inside `nix develop`)");
    PathBuf::from("dotnet")
}

/// Best-effort discovery of the .NET install root for tests. Phase 3 still
/// only validates non-emptiness; real-world callers will derive this properly.
pub(crate) fn dotnet_root_for_tests() -> PathBuf {
    std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub(crate) fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/lsp/`; the workspace root sits two
    // directories up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

/// Returns a value that should be unique per process invocation. Used to
/// salt fixture source so the cache-key tests don't pick up published DLLs
/// from a previous `cargo test` run.
pub(crate) fn unique_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock running")
        .as_nanos()
}

/// Resolve the closure-wide TFM map a real LSP caller would pass to
/// `buildMetadata`. Every fixture in this file targets `net10.0`, so a
/// shared helper keyed on the csproj path is enough — tests don't need
/// to thread the TFM string around. Caller is responsible for ensuring
/// the fixture has been `dotnet restore`d before invoking this (every
/// call site here pairs it with a prior [`lock_fixture`] guard).
pub(crate) fn project_tfms_for(csproj: &Path) -> BTreeMap<PathBuf, String> {
    resolve_transitive_project_tfms(csproj, "net10.0").unwrap_or_else(|e| {
        panic!(
            "resolve closure-wide TFM map for {}: {e:?}",
            csproj.display()
        )
    })
}

/// Spawn the sidecar via [`start_bundled_sidecar`] — i.e. the DLL that
/// `build.rs` published into Cargo's `OUT_DIR/sidecar/` at crate-build
/// time. Tests used to drive their own `dotnet build` on the csproj
/// alongside it, but that produced two independent build outputs that
/// stomped on the project-local `obj/` (CS0579 duplicate-attribute) and
/// duplicated the SDK-install work. The bundled flavour is what
/// production callers use, and a single dotnet-build invocation also
/// removes the `Once` latch this function used to need.
pub(crate) fn build_and_start(dotnet: &Path) -> SidecarHandle {
    let workspace_root = workspace_root();
    let dotnet_root = dotnet_root_for_tests();
    start_bundled_sidecar(dotnet, &workspace_root, &dotnet_root).expect("start_bundled_sidecar")
}

/// Acquire exclusive access to a fixture's working tree (and especially
/// its `obj/`) for the duration of the returned guard, restoring NuGet
/// packages on first use.
///
/// MSBuild design-time builds — both the sidecar's `MSBuildWorkspace.
/// OpenProjectAsync` and the `dotnet build` we shell out to for the
/// ref-assembly differential — write incremental artefacts under
/// `obj/<Config>/<TFM>/`, including `.NETCoreApp,Version=v*.Assembly
/// Attributes.cs`. The targets that produce that file are not safe
/// against another MSBuild instance touching the same path concurrently,
/// so two cargo test threads aimed at the same shared fixture race and
/// one fails with "Could not write lines to file ... already exists".
/// Per-test fixtures created under `target/` with a unique nonce don't
/// share `obj/` and don't contend, but they pay no real cost here either.
///
/// The lock is keyed on the absolute fixture directory and lives in a
/// leaked `&'static Mutex<bool>` so the returned guard has a `'static`
/// lifetime — callers can simply bind it to `_fixture_lock` and let the
/// drop at scope end release it. The boolean it guards is the
/// "restored?" latch: first acquirer runs `dotnet restore` while holding
/// the lock, so the restore itself is also serialised against any build.
#[must_use = "the returned guard releases the fixture lock when dropped"]
pub(crate) fn lock_fixture(dotnet: &Path, fixture_dir: &Path) -> MutexGuard<'static, bool> {
    static REGISTRY: Mutex<Option<HashMap<PathBuf, &'static Mutex<bool>>>> = Mutex::new(None);
    let mu: &'static Mutex<bool> = {
        let mut guard = REGISTRY.lock().expect("fixture-lock registry poisoned");
        let map = guard.get_or_insert_with(HashMap::new);
        match map.get(fixture_dir) {
            Some(existing) => existing,
            None => {
                let fresh: &'static Mutex<bool> = Box::leak(Box::new(Mutex::new(false)));
                map.insert(fixture_dir.to_path_buf(), fresh);
                fresh
            }
        }
    };
    let mut restored = mu.lock().expect("fixture mutex poisoned");
    if !*restored {
        let mut cmd = Command::new(dotnet);
        cmd.arg("restore").current_dir(fixture_dir);
        BoundedCommand::new(cmd)
            .timeout(BUILD_TIMEOUT)
            .run_ok(format_args!("dotnet restore for {}", fixture_dir.display()));
        *restored = true;
    }
    restored
}
