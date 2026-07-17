//! Publishes the C# sidecar into Cargo's `OUT_DIR` so the crate can locate it
//! at runtime via `env!("CSHARP_SIDECAR_DLL")`. Cargo invalidates this script
//! only when one of the watched sidecar inputs (csproj or any `.cs` file)
//! changes; `dotnet build` is itself incremental, so a stray invalidation
//! costs at most an MSBuild "nothing to do" pass.
//!
//! If `dotnet` is not on `PATH`, or the build fails for any other reason,
//! we emit a `cargo:warning=…` and an empty `CSHARP_SIDECAR_DLL` env var.
//! That lets the rest of the crate compile and run on machines without
//! a .NET SDK; only consumers of the bundled-sidecar API see an explicit
//! "no DLL was published" error at the point of use, rather than a silent
//! mis-spawn or a confusing "file not found" later.
//!
//! Per `docs/completed/csharp-sidecar-plan.md` D11: this is the distribution half
//! of phase 8. The matching runtime half lives in
//! `src/csharp_sidecar/process.rs::start_bundled_sidecar`.
//!
//! **Scope.** The bundled DLL path is baked in via `env!` at compile
//! time and points inside Cargo's `OUT_DIR`. That works for any consumer
//! that runs from the same source checkout the compile happened in —
//! `cargo run`, `cargo test`, an LSP launched from the working tree.
//! Anything that copies the binary out of `target/` breaks the path:
//!
//!   * `cargo install` keeps the sidecar in `target/` and only copies
//!     the binary;
//!   * `nix build` compiles in an ephemeral sandbox, then copies the
//!     binary to `$out/bin/`; the OUT_DIR path baked in points at a
//!     sandbox location that no longer exists by the time the binary
//!     runs. (And `dotnet-sdk_10` isn't in the package's `buildInputs`
//!     today, so the bake is usually the empty-string fallback rather
//!     than a stale path — either way callers see a clean
//!     `BundledSidecarUnavailable` error.)
//!
//! The runtime half of D13 now exists: `start_bundled_sidecar` discovery
//! (`process.rs::resolve_sidecar_dll`) tries `$BORZOI_SIDECAR_DLL` and an
//! executable-relative install layout *before* this baked `OUT_DIR` path, so a
//! co-located or overridden sidecar wins for packaged builds and this path is
//! the in-tree developer fallback. What remains is the *packaging* half —
//! teaching `nix build` to build the sidecar and install it beside the binary
//! (or surface it via the env var) — which is still out of scope here.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Packaged builds (the Nix derivation) supply the sidecar out-of-band — a
    // separate `buildDotnetModule` publish tree wired in through the
    // `BORZOI_SIDECAR_DLL` runtime override — and deliberately carry no
    // .NET SDK in the build sandbox. Let them opt out of the in-tree `OUT_DIR`
    // build entirely: without this, `try_build` fails on the absent `dotnet`
    // and emits a `cargo:warning` claiming C# support is unavailable at
    // runtime, which is *false* for such a build (discovery finds the
    // co-installed sidecar). A rerun fires if the flag toggles.
    println!("cargo:rerun-if-env-changed=BORZOI_SIDECAR_SKIP_INTREE_BUILD");
    if std::env::var_os("BORZOI_SIDECAR_SKIP_INTREE_BUILD").is_some_and(|v| !v.is_empty()) {
        // Empty baked path → discovery's tier-3 fallback is a no-op, leaving the
        // override / executable-relative tiers to supply the DLL. This is the
        // intended packaging path, so emit no `cargo:warning`; the plain
        // `println!` is hidden unless the caller passes `-vv`.
        println!("cargo:rustc-env=CSHARP_SIDECAR_DLL=");
        println!(
            "csharp-sidecar in-tree build skipped (BORZOI_SIDECAR_SKIP_INTREE_BUILD set); \
             expecting the sidecar to be supplied via install layout or override"
        );
        return;
    }

    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    // CARGO_MANIFEST_DIR is `crates/lsp/`; the C# sidecar lives at the
    // workspace root in `tools/csharp-sidecar/`.
    let sidecar_dir = Path::new(&manifest_dir)
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root")
        .join("tools")
        .join("csharp-sidecar");
    let csproj = sidecar_dir.join("csharp-sidecar.csproj");

    // Cargo rerun semantics:
    //   * a *file* path triggers a rerun when that file's mtime changes
    //     (i.e. edits to a specific file we already know about), and
    //   * a *directory* path triggers a rerun when the directory's mtime
    //     changes — which on every POSIX filesystem covers
    //     adds-and-removes of children but not edits to existing children.
    // We need both: file watches catch edits to known sources, dir watches
    // catch a newly-added `.cs` picked up by the SDK's default Compile glob.
    // Without the latter, cargo would happily reuse a stale bundled DLL until
    // an already-watched file changes or `target/` is wiped.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", csproj.display());
    watch_sidecar_inputs(&sidecar_dir);

    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR");
    let publish_dir = Path::new(&out_dir).join("sidecar");
    let intermediate_dir = Path::new(&out_dir).join("sidecar-obj");

    match try_build(&csproj, &publish_dir, &intermediate_dir) {
        Ok(dll) => {
            println!("cargo:rustc-env=CSHARP_SIDECAR_DLL={}", dll.display());
        }
        Err(err) => {
            // Emit an empty env var so `env!("CSHARP_SIDECAR_DLL")` always
            // resolves at compile time. The discovery shim treats an empty
            // value as "no bundled binary" and surfaces a clear error.
            println!("cargo:rustc-env=CSHARP_SIDECAR_DLL=");
            println!(
                "cargo:warning=csharp-sidecar build skipped: {err}. \
                 The bundled-discovery API will return an error at runtime; \
                 install a .NET 10 SDK and rebuild to enable it."
            );
            // Try to make a later, successful build pick up automatically.
            // Watching `PATH` and `DOTNET_ROOT` covers the common SDK-
            // installation scenarios (a new shell, a fresh `nix develop`,
            // an explicit `export DOTNET_ROOT=…`). A user who drops a
            // `dotnet` binary into an existing PATH entry without
            // touching either env var will need `cargo clean` to pick it
            // up — but the warning above already points at that path.
            println!("cargo:rerun-if-env-changed=PATH");
            println!("cargo:rerun-if-env-changed=DOTNET_ROOT");
        }
    }
}

// Direct `Command` calls are sanctioned here: a build script runs
// single-threaded in its own process, so the concurrent-spawn
// descriptor leak the workspace clippy.toml guards against (see
// crates/lsp/src/spawn.rs) cannot arise.
#[allow(clippy::disallowed_methods)]
fn try_build(
    csproj: &Path,
    publish_dir: &Path,
    intermediate_dir: &Path,
) -> Result<PathBuf, String> {
    // Fail-fast probe: if `dotnet --version` doesn't run, the SDK isn't
    // available and there's no point launching the slower `build` command
    // just to read the same error.
    let probe = Command::new("dotnet")
        .arg("--version")
        .output()
        .map_err(|e| format!("`dotnet --version` failed to spawn: {e}"))?;
    if !probe.status.success() {
        return Err(format!(
            "`dotnet --version` exited {:?}: {}",
            probe.status,
            String::from_utf8_lossy(&probe.stderr).trim(),
        ));
    }

    // `--output` only redirects the final published outputs; MSBuild still
    // writes its intermediates (`AssemblyInfo.cs`, `project.assets.json`,
    // the per-target `bin/` and `obj/` trees) next to the csproj by
    // default. That fails in vendored or otherwise read-only source trees
    // even when Cargo's `OUT_DIR` is writable. Routing
    // `BaseIntermediateOutputPath` / `BaseOutputPath` into `OUT_DIR` keeps
    // every write within Cargo's sandbox. The trailing separator is a
    // load-bearing MSBuild quirk — without it, MSBuild treats the property
    // as a filename prefix and produces e.g. `…/sidecar-objobj/`.
    let intermediate_str = format!(
        "{}{}",
        intermediate_dir.display(),
        std::path::MAIN_SEPARATOR
    );
    let publish_str = format!("{}{}", publish_dir.display(), std::path::MAIN_SEPARATOR);

    // Override `DefaultItemExcludes` so the SDK's `Compile` glob does *not*
    // pick up the generated `AssemblyInfo.cs` left behind in a project-local
    // `obj/` tree (e.g. from a developer running `dotnet build` directly in
    // `tools/csharp-sidecar/` for debugging). Without this, the next
    // `cargo build` fails CS0579 with a duplicate-attribute error because
    // *both* the project-local `obj/.../AssemblyInfo.cs` and the freshly
    // emitted OUT_DIR copy end up in the compile set.
    //
    // The SDK default — `$(BaseOutputPath)/**;$(BaseIntermediateOutputPath)/**` —
    // doesn't help us: those resolve to absolute paths under OUT_DIR after the
    // redirects above, so the project-relative Compile glob never matches them
    // and there's nothing to lose by replacing the value. We're deliberately
    // using a single pattern: MSBuild's list-splitting on `;` interacts badly
    // here, and `bin/` never contains `.cs` files anyway, so a one-element
    // exclude covers the only real failure mode.
    let status = Command::new("dotnet")
        .args(["build", "--configuration", "Release", "--nologo"])
        .arg("--output")
        .arg(publish_dir)
        .arg(format!("-p:BaseIntermediateOutputPath={intermediate_str}"))
        .arg(format!("-p:BaseOutputPath={publish_str}"))
        .arg("-p:DefaultItemExcludes=obj/**/*.cs")
        .arg(csproj)
        .status()
        .map_err(|e| format!("spawn `dotnet build`: {e}"))?;
    if !status.success() {
        return Err(format!("`dotnet build` exited {status:?}"));
    }

    let dll = publish_dir.join("csharp-sidecar.dll");
    if !dll.exists() {
        return Err(format!(
            "dotnet build reported success but no DLL at {}",
            dll.display()
        ));
    }
    Ok(dll)
}

/// Emit `cargo:rerun-if-changed` for every `.cs` source under `dir`,
/// skipping dotnet's build outputs (`bin`, `obj`) and the test-fixtures
/// tree. Deliberately does **not** watch directories: cargo's
/// `rerun-if-changed=<dir>` is *recursive* — it fingerprints the entire
/// subtree — so a single dir watch on `tools/csharp-sidecar/` would
/// invalidate the sidecar every time the integration tests' `dotnet
/// restore` rewrote a file under `test-fixtures/*/obj/`, defeating
/// the per-subdir skips below.
///
/// The trade-off is that adding a brand-new `.cs` file to the sidecar
/// crate will not, by itself, retrigger this build script — the new
/// file isn't on any watch list yet. The csproj watch above catches
/// the common case where the developer edits the project alongside.
/// In the rarer case of adding a `.cs` without touching the csproj,
/// either edit `build.rs` (or `cargo clean -p borzoi`) to
/// invalidate.
fn watch_sidecar_inputs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            if matches!(name.to_str(), Some("bin" | "obj" | "test-fixtures")) {
                continue;
            }
            watch_sidecar_inputs(&path);
        } else if path.extension().and_then(|e| e.to_str()) == Some("cs") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
