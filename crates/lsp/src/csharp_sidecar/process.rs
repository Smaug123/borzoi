//! Spawn and supervise the C# sidecar process; speak its JSON-RPC dialect.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::error::SidecarError;
use super::protocol::{
    BuildMetadataParams, BuildMetadataResult, InitializeParams, InitializeResult, JsonRpcRequest,
    JsonRpcResponse, PROTOCOL_VERSION, SIDECAR_ERROR_CODE, SidecarErrorKind,
};

/// Handle to a running sidecar process. Requests are dispatched synchronously
/// (the sidecar processes one at a time) — multiple handles for the same
/// sidecar are not supported.
pub struct SidecarHandle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    init: InitializeResult,
}

impl SidecarHandle {
    /// Sidecar's reported runtime / Roslyn / protocol versions, captured at
    /// handshake time.
    pub fn initialize_result(&self) -> &InitializeResult {
        &self.init
    }

    /// Send `shutdown`, then wait for the child to exit. Returns an error if
    /// the sidecar exits non-zero.
    pub fn shutdown(mut self) -> Result<(), SidecarError> {
        let _: () = self.request("shutdown", serde_json::Value::Null)?;
        let status = self.child.wait()?;
        if !status.success() {
            return Err(SidecarError::ProcessExited {
                code: status.code(),
            });
        }
        Ok(())
    }

    /// Request a metadata-only DLL for a csproj.
    ///
    /// On success, the sidecar has emitted a metadata-only DLL via Roslyn
    /// and atomically published it inside the workspace's
    /// `obj/borzoi/csharp-sidecar/` directory. On failure, the
    /// error carries a typed [`SidecarErrorKind`] — `BuildFailed` for
    /// Roslyn-detected compile errors, `LoadFailed` for MSBuild faults,
    /// `CsprojNotFound` for a bad path, `CacheUnwritable` for IO problems.
    ///
    /// `project_tfms` is the closure-wide TFM map produced by
    /// [`crate::project_assets::transitive_project_tfms`]: every csproj in
    /// the requested project's `<ProjectReference>` closure (top csproj
    /// included) keyed to the short-form TFM NuGet's restore selected for
    /// it. Callers that don't yet have a closure (e.g. one-off integration
    /// tests) may pass an empty map.
    pub fn build_metadata(
        &mut self,
        csproj_path: &Path,
        configuration: &str,
        target_framework: &str,
        project_tfms: &BTreeMap<PathBuf, String>,
    ) -> Result<BuildMetadataResult, SidecarError> {
        let csproj_str = csproj_path
            .to_str()
            .ok_or_else(|| SidecarError::Framing("csproj path is not valid UTF-8".into()))?;
        self.request(
            "buildMetadata",
            BuildMetadataParams {
                csproj_path: csproj_str,
                configuration,
                target_framework,
                project_tfms,
            },
        )
    }

    /// Issue a JSON-RPC request, wait for the response, and deserialise its
    /// `result` field. `null` results deserialise to `()`.
    ///
    /// Synchronous — the sidecar processes requests serially, so we never
    /// have more than one in flight.
    fn request<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<R, SidecarError> {
        let id = self.next_id;
        self.next_id += 1;
        let bytes = serde_json::to_vec(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })?;
        write_message(&mut self.stdin, &bytes)?;

        let response_bytes = read_message(&mut self.stdout)?;
        let response: JsonRpcResponse = serde_json::from_slice(&response_bytes)?;
        if response.id != Some(id) {
            return Err(SidecarError::UnexpectedResponseId {
                expected: id,
                got: response.id,
            });
        }
        if let Some(err) = response.error {
            if err.code == SIDECAR_ERROR_CODE {
                let data = err.data.ok_or_else(|| {
                    SidecarError::Framing("sidecar error response missing `data` payload".into())
                })?;
                let kind = SidecarErrorKind::from_data(data)?;
                return Err(SidecarError::Sidecar {
                    kind,
                    message: err.message,
                });
            }
            return Err(SidecarError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        let result_value = response.result.unwrap_or(serde_json::Value::Null);
        Ok(serde_json::from_value(result_value)?)
    }
}

/// Spawn the sidecar by invoking `dotnet <sidecar_dll>` and complete the
/// `initialize` handshake. The caller supplies a workspace root and a
/// `dotnet` runtime root — both are validated for non-emptiness by the sidecar
/// during the handshake.
///
/// Phase 2 reports them back through `initialize` but does not yet consume
/// `dotnet_root` directly: the sidecar finds MSBuild via
/// `MSBuildLocator.RegisterDefaults`, which discovers the SDK from PATH. The
/// parameter is preserved so later phases can pin a specific SDK install.
pub fn start_sidecar(
    dotnet_exe: &Path,
    sidecar_dll: &Path,
    workspace_root: &Path,
    dotnet_root: &Path,
) -> Result<SidecarHandle, SidecarError> {
    // Validate the DLL up-front. If we skipped this, `dotnet` would still
    // spawn successfully, then print its own banner to stdout and exit —
    // which the handshake would surface as a framing error that discards
    // the path that's actually wrong.
    if !sidecar_dll.exists() {
        return Err(SidecarError::SidecarDllMissing {
            path: PathBuf::from(sidecar_dll),
        });
    }

    let mut command = Command::new(dotnet_exe);
    command
        .arg(sidecar_dll)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr inherits so sidecar panic traces surface to the LSP log.
        .stderr(Stdio::inherit());

    let mut child =
        crate::spawn::spawn_serialised(&mut command).map_err(|e| SidecarError::Spawn {
            program: PathBuf::from(dotnet_exe),
            source: e,
        })?;
    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));

    let mut handle = SidecarHandle {
        child,
        stdin,
        stdout,
        next_id: 1,
        // Filled by the handshake below.
        init: InitializeResult {
            protocol_version: String::new(),
            runtime_version: String::new(),
            roslyn_version: None,
        },
    };

    let workspace_str = workspace_root
        .to_str()
        .ok_or_else(|| SidecarError::Framing("workspace_root is not valid UTF-8".into()))?;
    let dotnet_str = dotnet_root
        .to_str()
        .ok_or_else(|| SidecarError::Framing("dotnet_root is not valid UTF-8".into()))?;

    let init: InitializeResult = handle.request(
        "initialize",
        InitializeParams {
            workspace_root: workspace_str,
            dotnet_root: dotnet_str,
        },
    )?;

    if init.protocol_version != PROTOCOL_VERSION {
        return Err(SidecarError::ProtocolVersionMismatch {
            client: PROTOCOL_VERSION,
            sidecar: init.protocol_version,
        });
    }
    handle.init = init;
    Ok(handle)
}

/// Environment override for the sidecar DLL location. Highest-priority source
/// in [`bundled_sidecar_dll`] discovery: a packager — or the eventual Nix
/// wrapper (see [`start_bundled_sidecar`]) — can point the LSP at an
/// out-of-tree sidecar without having to match the beside-the-executable
/// layout. A set-but-missing value is honoured as-is (it surfaces as
/// [`SidecarError::SidecarDllMissing`] at spawn), never silently overridden by
/// a lower-priority candidate — an explicit override means "use exactly this".
const SIDECAR_DLL_ENV: &str = "BORZOI_SIDECAR_DLL";

/// Beside-the-executable install layout:
/// `<exe_dir>/csharp-sidecar/csharp-sidecar.dll`. A published sidecar is a whole
/// directory (the DLL plus its `.deps.json`, `.runtimeconfig.json`, and
/// dependency assemblies), so it lives in its own subdirectory next to the
/// binary rather than loose. The Nix-packaging follow-up (plan D13) installs the
/// publish tree here — or sets [`SIDECAR_DLL_ENV`] via a `wrapProgram` wrapper.
const EXE_RELATIVE_SIDECAR: &str = "csharp-sidecar/csharp-sidecar.dll";

/// Locate the sidecar DLL, or `None` if no candidate is present. Discovery
/// order, highest priority first:
///
/// 1. **`$BORZOI_SIDECAR_DLL`** ([`SIDECAR_DLL_ENV`]) — explicit override,
///    returned even when the file is absent so a mistyped path fails loudly
///    rather than silently falling back.
/// 2. **Beside the executable** — `<exe_dir>/{EXE_RELATIVE_SIDECAR}` (the
///    installed layout), used only if it exists.
/// 3. **The baked build-script path** — `env!("CSHARP_SIDECAR_DLL")`, the
///    `OUT_DIR` copy `build.rs` published; used only if it exists. This is the
///    in-tree developer path (`cargo run`/`cargo test` from the checkout that
///    compiled the crate), where the executable has no sidecar sibling.
///
/// Pure over its inputs (env value, `current_exe`, baked path, and an existence
/// probe) so the precedence is unit-testable without touching the process
/// environment or the filesystem.
fn resolve_sidecar_dll(
    override_var: Option<&std::ffi::OsStr>,
    exe: Option<&Path>,
    baked: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    // 1. Explicit override wins unconditionally (even if missing — see docs).
    if let Some(value) = override_var
        && !value.is_empty()
    {
        return Some(PathBuf::from(value));
    }
    // 2. Beside the executable (installed layout).
    if let Some(dir) = exe.and_then(Path::parent) {
        let candidate = dir.join(EXE_RELATIVE_SIDECAR);
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    // 3. Baked OUT_DIR path (in-tree dev builds).
    if !baked.is_empty() {
        let baked = PathBuf::from(baked);
        if exists(&baked) {
            return Some(baked);
        }
    }
    None
}

/// Path of the bundled sidecar DLL, discovered via [`resolve_sidecar_dll`]
/// against the real process environment: `$BORZOI_SIDECAR_DLL`,
/// [`std::env::current_exe`], and the `build.rs`-baked `CSHARP_SIDECAR_DLL`.
/// `None` means no sidecar was found at any location — the crate was built
/// without a .NET SDK *and* no installed/override copy exists.
fn bundled_sidecar_dll() -> Option<PathBuf> {
    let override_var = std::env::var_os(SIDECAR_DLL_ENV);
    let exe = std::env::current_exe().ok();
    resolve_sidecar_dll(
        override_var.as_deref(),
        exe.as_deref(),
        env!("CSHARP_SIDECAR_DLL"),
        |p| p.is_file(),
    )
}

/// Spawn the sidecar using the DLL that `build.rs` published into
/// `OUT_DIR/sidecar/` at crate-build time. Use this from runtime callers
/// that don't care where the DLL lives on disk — only that one ships with
/// the crate.
///
/// Returns [`SidecarError::BundledSidecarUnavailable`] if no sidecar DLL is
/// found at any discovery location. Test-only callers that build the sidecar
/// themselves should keep using [`start_sidecar`] directly.
///
/// **Where the DLL lives.** `bundled_sidecar_dll` tries, in order (see
/// `resolve_sidecar_dll` for the precedence rules):
///
/// 1. `$BORZOI_SIDECAR_DLL` — an explicit override for packagers, or a
///    Nix `wrapProgram` wrapper pointing at a co-installed sidecar derivation.
/// 2. `<exe_dir>/csharp-sidecar/csharp-sidecar.dll` — the beside-the-executable
///    install layout, so a binary copied out of `target/` (e.g. by an installer
///    that also copies the published sidecar tree) still finds it.
/// 3. The `build.rs`-baked `OUT_DIR` copy — the in-tree developer path
///    (`cargo run`/`cargo test` from the compiling checkout), where the
///    executable has no sidecar sibling.
///
/// The remaining gap is *populating* location 2 (or 1) for packaged builds:
/// `nix build` does not yet build the sidecar at all (the crane source filter
/// prunes `tools/csharp-sidecar` and the package derivation has no .NET SDK),
/// and `cargo install` copies only the binary. That is plan-doc D13's
/// Nix-packaging follow-up — a sidecar derivation installed beside the binary
/// (or surfaced via the `BORZOI_SIDECAR_DLL` override). Until it lands, the
/// bundled API works for in-tree builds and any install that co-locates the
/// sidecar itself.
pub fn start_bundled_sidecar(
    dotnet_exe: &Path,
    workspace_root: &Path,
    dotnet_root: &Path,
) -> Result<SidecarHandle, SidecarError> {
    let dll = bundled_sidecar_dll().ok_or(SidecarError::BundledSidecarUnavailable)?;
    start_sidecar(dotnet_exe, &dll, workspace_root, dotnet_root)
}

impl Drop for SidecarHandle {
    /// Best-effort cleanup for the unhappy path: if the caller never reached
    /// [`SidecarHandle::shutdown`] (panic, mid-request error, or a stray
    /// `drop`), reap the child so it does not linger as a zombie.
    ///
    /// We `kill` before `wait` because struct fields drop in declaration
    /// order *after* the `Drop` impl returns — so at this point the child's
    /// stdin pipe is still open, and a well-behaved sidecar reading stdin
    /// would block forever rather than see EOF. `kill` is a no-op if the
    /// child has already exited.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_message<W: Write>(stream: &mut W, body: &[u8]) -> Result<(), SidecarError> {
    write!(stream, "Content-Length: {}\r\n\r\n", body.len())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn read_message<R: BufRead>(stream: &mut R) -> Result<Vec<u8>, SidecarError> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = stream.read_line(&mut line)?;
        if n == 0 {
            return Err(SidecarError::Framing(
                "EOF while reading message header".into(),
            ));
        }
        // The blank line terminates the header block.
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = Some(
                rest.trim()
                    .parse::<usize>()
                    .map_err(|e| SidecarError::Framing(format!("invalid Content-Length: {e}")))?,
            );
        }
        // Other headers (Content-Type, etc.) are ignored per LSP convention.
    }

    let len =
        content_length.ok_or_else(|| SidecarError::Framing("missing Content-Length".into()))?;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `build.rs` actually published the sidecar (discovery tier 3).
    /// Checks the baked `CSHARP_SIDECAR_DLL` directly rather than going through
    /// [`bundled_sidecar_dll`], so it is independent of the ambient environment
    /// — a developer shell or CI that exports `$BORZOI_SIDECAR_DLL` (a
    /// supported override) must not make this fail. We deliberately do not skip
    /// on an empty value: every developer build is expected to either run
    /// `dotnet build` successfully or fail loudly. If the SDK ever goes missing
    /// in CI, this is the first signal.
    #[test]
    fn bundled_sidecar_dll_is_published() {
        let baked = env!("CSHARP_SIDECAR_DLL");
        assert!(
            !baked.is_empty(),
            "build.rs should have published a sidecar DLL — run inside `nix develop`"
        );
        let dll = Path::new(baked);
        assert!(dll.is_file(), "bundled sidecar DLL is missing at {dll:?}");
        assert_eq!(
            dll.file_name().and_then(|s| s.to_str()),
            Some("csharp-sidecar.dll"),
        );
    }

    /// A file-existence probe backed by an explicit allow-set — no filesystem
    /// access, so the precedence tests are hermetic and order-independent.
    fn exists_in<'a>(present: &'a [&'a Path]) -> impl Fn(&Path) -> bool + 'a {
        move |p| present.contains(&p)
    }

    #[test]
    fn override_wins_even_when_absent() {
        // An explicit override is returned verbatim regardless of existence, so
        // a mistyped path fails loudly at spawn rather than silently falling
        // back to a stale bundled copy.
        let exe = PathBuf::from("/opt/app/bin/lsp");
        let beside = exe.parent().unwrap().join(EXE_RELATIVE_SIDECAR);
        let baked = "/checkout/target/out/sidecar/csharp-sidecar.dll";
        let got = resolve_sidecar_dll(
            Some(std::ffi::OsStr::new("/custom/sidecar.dll")),
            Some(&exe),
            baked,
            // Both lower-priority candidates exist, yet the override still wins.
            exists_in(&[&beside, Path::new(baked)]),
        );
        assert_eq!(got, Some(PathBuf::from("/custom/sidecar.dll")));
    }

    #[test]
    fn empty_override_is_ignored() {
        // An empty env value (how a shell exports an unset-but-declared var) is
        // treated as "no override", so discovery proceeds to the next source.
        let baked = "/checkout/target/out/sidecar/csharp-sidecar.dll";
        let got = resolve_sidecar_dll(
            Some(std::ffi::OsStr::new("")),
            None,
            baked,
            exists_in(&[Path::new(baked)]),
        );
        assert_eq!(got, Some(PathBuf::from(baked)));
    }

    #[test]
    fn exe_relative_beats_baked() {
        // Installed layout: the sidecar sits beside the binary. It is preferred
        // over the baked path (which for a copied-out binary is stale or dead).
        let exe = PathBuf::from("/opt/app/bin/lsp");
        let beside = exe.parent().unwrap().join(EXE_RELATIVE_SIDECAR);
        let baked = "/checkout/target/out/sidecar/csharp-sidecar.dll";
        let got = resolve_sidecar_dll(
            None,
            Some(&exe),
            baked,
            exists_in(&[&beside, Path::new(baked)]),
        );
        assert_eq!(got, Some(beside));
    }

    #[test]
    fn falls_back_to_baked_when_no_sibling() {
        // In-tree dev: the test/binary under target/ has no sidecar sibling, so
        // discovery uses the baked OUT_DIR copy.
        let exe = PathBuf::from("/checkout/target/debug/deps/test-abc");
        let baked = "/checkout/target/out/sidecar/csharp-sidecar.dll";
        let got = resolve_sidecar_dll(None, Some(&exe), baked, exists_in(&[Path::new(baked)]));
        assert_eq!(got, Some(PathBuf::from(baked)));
    }

    #[test]
    fn none_when_nothing_present() {
        // No override, no sibling, no baked path (crate built without an SDK) —
        // discovery reports unavailable rather than a bogus path.
        let exe = PathBuf::from("/opt/app/bin/lsp");
        let got = resolve_sidecar_dll(None, Some(&exe), "", exists_in(&[]));
        assert_eq!(got, None);
    }

    #[test]
    fn missing_dll_returns_path_specific_error() {
        let missing = Path::new("/definitely/does/not/exist/csharp-sidecar.dll");
        let result = start_sidecar(
            Path::new("dotnet"),
            missing,
            Path::new("/tmp"),
            Path::new("/tmp"),
        );
        match result {
            Err(SidecarError::SidecarDllMissing { path }) => assert_eq!(path, missing),
            Err(other) => panic!("expected SidecarDllMissing, got {other:?}"),
            Ok(_) => panic!("expected an error, got Ok"),
        }
    }
}
