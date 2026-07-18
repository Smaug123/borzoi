//! Shared helpers for the assembly crate's integration tests.
//!
//! The crate has one test binary (`tests/all/main.rs`); this is a `mod`-declared
//! submodule of it, so the case groups that need it
//! (`tests/all/assembly_diff.rs`, `tests/all/well_known_attributes_sync.rs`)
//! reach it as `crate::common`. (A `tests/foo.rs` *outside* `all/` would be
//! compiled as a second test binary, which is the trap
//! `all_case_groups_are_declared` guards.)
//!
//! The `workspace_root` / `corpus_root` / `invoke_fcs_dump` cluster here is
//! duplicated in the CST and LSP crates' `tests/all/common/mod.rs`, and in
//! msbuild's (unfolded) `tests/common/mod.rs`.
//! Each surface is stable and small; a shared dev-only harness crate adds
//! more moving parts than it removes.

#![allow(dead_code)] // each importer uses a different subset.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_spawn::BoundedCommand;
use tempfile::TempDir;

/// Budget for one `dotnet build` (a fixture project, or `tools/fcs-dump`).
///
/// A cold build restores packages and runs a compiler, which is legitimately
/// minutes, so the bound sits far above the driver's per-child default: it is
/// there to stop a build that has *stalled* — blocked on a NuGet lock held by a
/// concurrent run in a sibling worktree, say — from hanging the suite forever,
/// not to police a slow one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

// ============================================================================
// Workspace pathing
// ============================================================================

/// The workspace root, two `..` jumps above the assembly crate's
/// `CARGO_MANIFEST_DIR`. `tools/fcs-dump` lives there.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

// ============================================================================
// fcs-dump invocation
// ============================================================================

/// Run `fcs-dump <subcommand> <source>` and return its stdout as a UTF-8 string.
///
/// Honours `BORZOI_FCS_DUMP` (path to a pre-built self-contained binary)
/// when set; otherwise builds `tools/fcs-dump` **once** per test binary and
/// execs the resulting assembly on every call.
///
/// The build-once strategy avoids the race that `dotnet run` causes: `dotnet
/// run` does an MSBuild incremental-build check on every invocation, mutating
/// `obj/Release/net10.0/*.cache`. When N parallel test threads all call `dotnet
/// run` concurrently they race on those shared files, producing non-deterministic
/// build failures. By building once under a `OnceLock` and then exec-ing the
/// already-built binary directly, the hot path has no shared mutable state.
pub fn invoke_fcs_dump(subcommand: &str, source: &Path) -> String {
    let cmd = if let Some(bin) = std::env::var_os("BORZOI_FCS_DUMP") {
        let mut c = Command::new(bin);
        c.arg(subcommand).arg(source);
        c
    } else {
        let bin = ensure_fcs_dump_built();
        let mut c = Command::new("dotnet");
        c.arg(bin).arg(subcommand).arg(source);
        c
    };

    let out = BoundedCommand::new(cmd).run_ok(format_args!("fcs-dump {subcommand}"));
    String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8")
}

/// Build `tools/fcs-dump` once (thread-safe) and return the path to the
/// produced `.dll`. All subsequent callers get the cached path; only the
/// first caller pays the `dotnet build` cost (typically a fast up-to-date
/// check on a warm cache).
fn ensure_fcs_dump_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = fcs_dump_project_dir();
            let mut cmd = Command::new("dotnet");
            cmd.args(["build", "-c", "Release", "--nologo"])
                .arg(&project);
            BoundedCommand::new(cmd)
                .timeout(BUILD_TIMEOUT)
                .run_ok("dotnet build fcs-dump");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("fcs-dump.dll")
        })
        .as_path()
}

fn fcs_dump_project_dir() -> PathBuf {
    workspace_root().join("tools").join("fcs-dump")
}

/// Path to a real `FSharp.Core.dll` that is *always present* in the
/// checkout, unlike the corpus one (`locate_fsharp_core`, gated on a
/// built `BORZOI_CORPUS`). Building `tools/fcs-dump` copies the
/// FSharp.Compiler.Service dependency's `FSharp.Core.dll` into the same
/// output directory as `fcs-dump.dll`, so we reuse the build-once helper
/// and return the sibling. This lets unpickler tests pin behaviour against
/// the genuine shipped FSharp.Core in every lane, not just under `nix
/// develop`.
pub fn ensure_fsharp_core_dll() -> PathBuf {
    let fcs_dump = ensure_fcs_dump_built();
    fcs_dump
        .parent()
        .expect("fcs-dump.dll has a parent dir")
        .join("FSharp.Core.dll")
}

/// `dotnet --list-sdks` under the default deadline (it is a directory listing, not
/// a build, so it has no business taking anywhere near that long).
///
/// The exit status is deliberately *not* asserted, as it never was: the callers
/// judge the listing by whether it contains what they need, and quote the whole
/// thing when it doesn't — a more useful failure than the status alone.
fn list_sdks() -> std::process::Output {
    let mut cmd = Command::new("dotnet");
    cmd.arg("--list-sdks");
    BoundedCommand::new(cmd)
        .run()
        .expect("run `dotnet --list-sdks`")
}

/// A matching `(FSharp.Core.dll, FSharp.Core.xml)` pair shipped *together* in the
/// .NET SDK's `FSharp/` directory — the dll and its sibling doc XML are the same
/// build, so the XML's `<member>` keys correspond to the dll's members (unlike
/// [`ensure_fsharp_core_dll`], whose NuGet-sourced dll has no sibling XML). Used
/// by the `doc_id` F#-differential to diff our generated IDs against the F#
/// compiler's own keys. Located via `dotnet --list-sdks` so it tracks whichever
/// SDK the lane provides; panics with a clear message if none carries the pair
/// (the SDK is already required by the `dotnet build` fixtures, so this holds in
/// every lane that runs those).
pub fn ensure_sdk_fsharp_core() -> (PathBuf, PathBuf) {
    let out = list_sdks();
    let listing = String::from_utf8_lossy(&out.stdout);
    for line in listing.lines() {
        // Each line is `<version> [<sdk-root>]`, e.g. `10.0.300 [/…/sdk]`.
        let Some((version, rest)) = line.split_once(' ') else {
            continue;
        };
        let root = rest.trim().trim_start_matches('[').trim_end_matches(']');
        let fsharp = Path::new(root).join(version.trim()).join("FSharp");
        let dll = fsharp.join("FSharp.Core.dll");
        let xml = fsharp.join("FSharp.Core.xml");
        if dll.is_file() && xml.is_file() {
            return (dll, xml);
        }
    }
    panic!(
        "no SDK ships a matching FSharp.Core.dll + FSharp.Core.xml under \
         <sdk>/FSharp/ (from `dotnet --list-sdks`):\n{listing}"
    );
}

/// The newest `Microsoft.NETCore.App.Ref` reference-pack directory the
/// installed SDK ships (the `ref/<tfm>/` folder holding ~170 reference
/// assemblies — the exact surface the compiler resolves against). Located
/// via `dotnet --list-sdks` like [`ensure_sdk_fsharp_core`]; panics with a
/// clear message if no SDK carries one (the SDK is already required by the
/// `dotnet build` fixtures, so this holds in every lane that runs those).
///
/// "Newest" (max on the *numerically parsed* version-directory name, then
/// on the numerically parsed TFM) rather than "first": a machine with
/// several SDKs must sweep one deterministic pack, and the newest matches
/// what `net10.0`+ builds here actually reference. Numeric parsing matters —
/// lexicographically `"9.0.10"` sorts *after* `"10.0.8"`, which would pick a
/// .NET 9 pack on a multi-SDK machine and fail budgets calibrated for 10.
pub fn sdk_ref_pack_dir() -> PathBuf {
    /// The leading numeric value of each dot-separated segment, so
    /// `"10.0.8"` → `[10, 0, 8]` and `"net10.0"` → `[0, 10, 0]`-ish
    /// (`"net10"` has leading digits after the alphabetic prefix is
    /// stripped). Non-numeric tails (`-preview…`) are ignored — a preview
    /// ties with its release, and either satisfies the sweep.
    fn version_key(name: &str) -> Vec<u64> {
        name.split('.')
            .map(|seg| {
                let digits: String = seg
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                digits.parse().unwrap_or(0)
            })
            .collect()
    }
    let out = list_sdks();
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut candidates = std::collections::BTreeMap::new();
    for line in listing.lines() {
        let Some((_version, rest)) = line.split_once(' ') else {
            continue;
        };
        let root = rest.trim().trim_start_matches('[').trim_end_matches(']');
        // `<root>` is `<dotnet>/sdk`; the packs live beside it.
        let packs = Path::new(root)
            .parent()
            .map(|p| p.join("packs").join("Microsoft.NETCore.App.Ref"));
        let Some(packs) = packs.filter(|p| p.is_dir()) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&packs) else {
            continue;
        };
        for entry in entries.flatten() {
            let ref_dir = entry.path().join("ref");
            let Ok(tfms) = std::fs::read_dir(&ref_dir) else {
                continue;
            };
            for tfm in tfms.flatten() {
                let dir = tfm.path();
                if dir.is_dir() {
                    candidates.insert(
                        (
                            version_key(&entry.file_name().to_string_lossy()),
                            version_key(&tfm.file_name().to_string_lossy()),
                        ),
                        dir,
                    );
                }
            }
        }
    }
    match candidates.into_iter().next_back() {
        Some((_, dir)) => dir,
        None => panic!(
            "no SDK ships a Microsoft.NETCore.App.Ref reference pack \
             (from `dotnet --list-sdks`):\n{listing}"
        ),
    }
}

/// Root of the F# corpus the differential tests walk, taken from the
/// `BORZOI_CORPUS` environment variable.
///
/// `nix develop` sets this to the pinned `fsharp-src` flake input — a
/// content-addressed checkout of the F# compiler in the Nix store (see
/// `flake.nix`). There is no on-disk fallback; run the corpus tests under
/// `nix develop`, or point `BORZOI_CORPUS` at a local F# checkout.
///
/// Panics if the variable is unset or does not resolve to a directory.
pub fn corpus_root() -> PathBuf {
    let root = match std::env::var_os("BORZOI_CORPUS") {
        Some(p) => PathBuf::from(p),
        None => panic!(
            "BORZOI_CORPUS is not set. Run the corpus tests under \
             `nix develop` (which points it at the pinned `fsharp-src` flake \
             input), or set it to a local F# compiler checkout."
        ),
    };
    assert!(
        root.is_dir(),
        "F# corpus root {root:?} (from BORZOI_CORPUS) is not a directory."
    );
    root
}

// ============================================================================
// Assembly fixture builders
// ============================================================================

/// The `tests/fixtures/assembly` directory holding every fixture project.
fn fixtures_root() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"))
        .join("tests")
        .join("fixtures")
        .join("assembly")
}

/// Recursively copy `src` into `dst`, skipping any `bin`/`obj` directory.
///
/// Excluding the build outputs matters twice over: they are large, and a
/// copied `obj/` carries absolute paths from the *original* location baked
/// into `project.assets.json` and the MSBuild caches, which would make the
/// copy build incrementally against the wrong tree. Dropping them yields a
/// pristine source tree that builds from scratch in its new home.
fn copy_tree_no_build_dirs(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap_or_else(|e| panic!("create {}: {e}", dst.display()));
    for entry in std::fs::read_dir(src).unwrap_or_else(|e| panic!("read {}: {e}", src.display())) {
        let entry = entry.expect("read fixture tree entry");
        let name = entry.file_name();
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            if name == "bin" || name == "obj" {
                continue;
            }
            copy_tree_no_build_dirs(&from, &to);
        } else {
            std::fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
        }
    }
}

/// Copy the whole fixtures tree into a fresh temp dir and
/// `dotnet build -c Release` the `fixture` project inside that copy,
/// returning the temp dir (kept alive by the caller's `OnceLock`) and the
/// path to the produced `dll` within it.
///
/// The private copy is what makes fixture builds safe under concurrency.
/// `cargo test` runs test binaries in parallel, and MSBuild follows
/// `ProjectReference` edges transitively (`MiniLibFsExt` rebuilds
/// `MiniLib`), so building in the shared checkout let two `dotnet build`
/// processes race on a fixture's `obj/`/`bin/` — e.g. `MiniLib.deps.json`
/// via `MSB4018: The "GenerateDepsFile" task failed unexpectedly`, or a
/// half-written `MiniLibFs.dll`. Isolating each build in its own tree copy
/// removes the shared writable state entirely, so no cross-thread or
/// cross-process lock is needed. The whole tree (not just `fixture`) is
/// copied so a `ProjectReference` to a sibling still resolves.
///
/// The returned `TempDir` is parked in the caller's `'static OnceLock`, so
/// its `Drop` never runs and the copy outlives the process — the fixtures
/// are tiny and land under the OS temp dir, and callers hold a
/// `&'static Path` into the build output that must stay readable for the
/// whole run, so eager cleanup isn't an option here.
fn build_fixture(fixture: &str, dll: &str) -> (TempDir, PathBuf) {
    let workspace = TempDir::new().expect("create fixture build temp dir");
    copy_tree_no_build_dirs(&fixtures_root(), workspace.path());
    let project = workspace.path().join(fixture);
    let mut cmd = Command::new("dotnet");
    cmd.args(["build", "-c", "Release", "--nologo"])
        .arg(&project);
    BoundedCommand::new(cmd)
        .timeout(BUILD_TIMEOUT)
        .run_ok(format_args!("dotnet build {fixture} fixture"));
    let dll = project
        .join("bin")
        .join("Release")
        .join("net10.0")
        .join(dll);
    (workspace, dll)
}

/// Build the MiniLib C# test fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as `ensure_fcs_dump_built` —
/// each test binary pays the build cost on its first invocation and
/// subsequent calls return the cached path with no shared mutable state.
///
/// MiniLib is intentionally minimal: one public class in a namespace,
/// no members. It pins the phase-2 "skeleton only" projection — namespace,
/// name, kind, access, base type, interfaces, nested-type tree.
pub fn ensure_minilib_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MiniLib", "MiniLib.dll"))
        .1
        .as_path()
}

/// Build the MiniLibFs F# test fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// MiniLibFs covers the F#-specific entity kinds (Module / Union / Record /
/// Exception) that ECMA-335 flags alone can't distinguish. The phase-4a
/// `CompilationMappingAttribute` decoder is what makes the projection
/// surface these kinds; without it every entity falls through to `Class`.
pub fn ensure_minilib_fs_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MiniLibFs", "MiniLibFs.dll"))
        .1
        .as_path()
}

/// Build the MiniLibFsExt F# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// MiniLibFsExt augments the C# `MiniLib.Counter` type with one instance
/// F#-native extension method (`type Counter with member this.Tripled`)
/// and one static F#-native extension (`type Counter with static
/// member Make`). Pins the phase-4c handling of F#-native extensions:
/// the F# compiler emits these as static methods on a synthetic
/// `Extensions` module class with the target-type name mangled into the
/// IL MethodDef (`Counter.Tripled`, `Counter.Make.Static`). F# does NOT
/// emit `[ExtensionAttribute]` on these; FCS still reports them through
/// `IsExtensionMember = true` and a stripped-receiver
/// `CurriedParameterGroups`. Both projectors must agree on the
/// IL-shaped signature including the re-prepended receiver, and on the
/// `extension` flag (set only for the instance shape).
pub fn ensure_minilib_fs_ext_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MiniLibFsExt", "MiniLibFsExt.dll"))
        .1
        .as_path()
}

/// Build the FsExtIndex F# fixture once and return the path to the produced
/// `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// FsExtIndex augments `System.String` with the instance-extension shapes the
/// per-method `is_extension_method` overlay under-flags — a generic-method
/// extension and an optional-parameter extension — plus a plain instance
/// extension, a static extension, and a non-extension `let`. It pins that the
/// OV-0.5 name index (`Entity::extension_member_names`) reads the pickled
/// `IsExtensionMember ∧ IsInstance` bit per val and so names every instance
/// extension (including the two the overlay misses) and neither the static nor
/// the plain binding. Not diffed against fcs-dump (the generic-member surface
/// diverges, an unrelated gap) — the test asserts the index directly.
pub fn ensure_fs_ext_index_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("FsExtIndex", "FsExtIndex.dll"))
        .1
        .as_path()
}

/// Build the SigHiddenUnion F# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// SigHiddenUnion is a single generic union `Teq<'a,'b>` whose representation
/// is hidden by a signature file (`Teq.fsi` exposes only an opaque `type
/// Teq<'a,'b>`). The F# compiler lowers the union repr to `TNoRepr` in the
/// signature pickle a cross-assembly consumer reads, yet the compiled class
/// keeps `CompilationMapping(SumType)`, so ECMA still classifies it a union.
/// It pins that the projector seals such a signature-hidden union to a
/// knowably-empty `union_case_names` (`Some(vec![])`) rather than the
/// unknowable `None` — the regression behind `open`ing such a namespace
/// deferring every dotted head (the `TypeEquality.Teq` shape).
pub fn ensure_sig_hidden_union_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("SigHiddenUnion", "SigHiddenUnion.dll"))
        .1
        .as_path()
}

/// Build the MeasureAttrArgs F# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// MeasureAttrArgs pairs the two `Expr.Op` attribute-argument shapes the
/// decoder handles — an array literal (`[<Tags([| 1; 2; 3 |])>]` →
/// `Expr.Op(TOp.Array, …)`) and an `obj`-parameter coercion
/// (`[<Boxed("hi")>]` → `Expr.Op(TOp.Coerce, …)`) — with a `[<Measure>]`
/// type. It pins the regression that the F# measure overlay survives a
/// non-constant attribute argument elsewhere in the assembly: before the
/// decoder slice such an argument failed the whole CCU decode, and the
/// recorded skipped-overlay policy then left the measure as `Class` instead
/// of enriching it to `Measure`. Unlike MiniLibFs this fixture is not diffed
/// against fcs-dump (the array parameter's element-nullness rendering
/// diverges, an unrelated gap), so the test asserts only the measure kind.
pub fn ensure_measure_attr_args_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MeasureAttrArgs", "MeasureAttrArgs.dll"))
        .1
        .as_path()
}

/// Build the LiteralConsts F# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// LiteralConsts carries one `[<Literal>]` of each previously-unsupported,
/// literal-expressible `u_const` tag (`sbyte`/`byte`/`int16`/`uint16`/
/// `uint32`/`int64`/`uint64`/`single`/`double`/`char`/`decimal`) plus a
/// constant `int64` attribute argument, beside a `[<Measure>] type m`. It
/// pins the regression that the F# measure overlay survives a wide-typed
/// literal or attribute argument elsewhere in the assembly: before the
/// full `u_const` tag set landed, such a value failed the whole CCU decode
/// and the recorded skipped-overlay policy left the measure as `Class`
/// instead of enriching it to `Measure`. Like MeasureAttrArgs it is not
/// diffed against fcs-dump; the test asserts only the measure kind.
pub fn ensure_literal_consts_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("LiteralConsts", "LiteralConsts.dll"))
        .1
        .as_path()
}

/// Build the DeepCurry F# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// DeepCurry carries a single 200-parameter *curried* function, whose
/// pickled signature type is a right-nested `TType_fun` chain 200
/// levels deep — the deepest valid-compiler-output type shape per byte
/// of source. It pins the unpickler's recursion bound from below: the
/// bound guards against adversarial one-frame-per-byte streams, but
/// must clear what the F# compiler actually emits for machine-generated
/// curried code with a wide margin. Not diffed against fcs-dump; the
/// test asserts only that the signature unpickles.
pub fn ensure_deep_curry_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("DeepCurry", "DeepCurry.dll"))
        .1
        .as_path()
}

/// Build the MiniLibPdb F# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// MiniLibPdb is the PDB arm of the fail-loud robustness harness: it
/// builds with `DebugType=embedded` + `EmbedAllSources` (the sibling
/// fixtures build with `DebugType=none`), so its image carries an
/// embedded portable PDB with documents, sequence points, and an
/// embedded-source blob for the mutation/truncation sweeps to corrupt.
pub fn ensure_minilib_pdb_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MiniLibPdb", "MiniLibPdb.dll"))
        .1
        .as_path()
}

/// Build the MemberShapes C# fixture once and return the path to the
/// produced `.dll`. Same build-once pattern as [`ensure_minilib_built`].
///
/// MemberShapes carries the happy-path method/ctor/parameter cluster of the
/// projector tests as a real csc-compiled assembly, so those tests can be
/// exercised through `Ecma335Assembly::parse` (the byte path).
pub fn ensure_member_shapes_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MemberShapes", "MemberShapes.dll"))
        .1
        .as_path()
}

/// Build the DocIds C# fixture once and return the path to the produced
/// `.dll`. The sibling `DocIds.xml` (same directory, `.xml` extension) holds
/// Roslyn's documentation comment IDs — the oracle the doc-id differential test
/// diffs the Rust generator against. The fixture's `.csproj` sets
/// `GenerateDocumentationFile`, so the build emits both files.
pub fn ensure_doc_ids_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("DocIds", "DocIds.dll"))
        .1
        .as_path()
}

/// Build the MetadataEmitter tool once and run it for `shape`, returning the
/// raw PE bytes it writes to stdout.
///
/// The emitter uses in-box `System.Reflection.Metadata` to fabricate
/// assemblies whose metadata exhibits shapes no C#/F# compiler emits — the
/// candidate mechanism for the defensive "fails-loud" projector tests. Bytes
/// go through stdout so there are no temp files to manage.
pub fn emit_metadata_fixture(shape: &str) -> Vec<u8> {
    let bin = ensure_metadata_emitter_built();
    let mut cmd = Command::new("dotnet");
    cmd.arg(bin).arg(shape);
    BoundedCommand::new(cmd)
        .run_ok(format_args!("MetadataEmitter {shape}"))
        .stdout
}

fn ensure_metadata_emitter_built() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    BUILT
        .get_or_init(|| build_fixture("MetadataEmitter", "MetadataEmitter.dll"))
        .1
        .as_path()
}

/// `dotnet build -c Release` an arbitrary project directory. Unlike the
/// `ensure_*` builders this captures the compiler output and returns it in
/// the error, because callers (the generative differential) build
/// *generated* source: a failure message must carry enough to reproduce
/// without re-running the test.
///
/// No build-wide lock is needed: the caller (`compile_generated`) builds
/// each source in its own content-addressed directory under
/// `CARGO_TARGET_TMPDIR` with no `ProjectReference` to any shared fixture,
/// so concurrent builds touch disjoint trees — the same isolation the
/// fixture builders get from their private tree copy. Only the *launch* is
/// serialised, by the shared spawn lock inside [`BoundedCommand`].
///
/// Returns `Ok(())` on success; `Err(combined stdout+stderr)` on failure.
pub fn dotnet_build_captured(project_dir: &Path) -> Result<(), String> {
    let mut cmd = Command::new("dotnet");
    cmd.args(["build", "-c", "Release", "--nologo"])
        .arg(project_dir);
    // A build that never finishes is a failure of *this* build, so it joins the
    // compiler's own errors in the returned `Err` rather than aborting the sweep:
    // the caller compiles generated sources and reports each failure against its
    // input.
    let out = BoundedCommand::new(cmd)
        .timeout(BUILD_TIMEOUT)
        .run()
        .map_err(|e| format!("dotnet build {}: {e}", project_dir.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "dotnet build {} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
            project_dir.display(),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ))
    }
}
