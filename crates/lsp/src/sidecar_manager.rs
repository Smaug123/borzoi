//! Runtime supervisor for the C# sidecar.
//!
//! The [`crate::csharp_sidecar`] module is the low-level client (spawn,
//! handshake, one request-response). This is the *policy* layer the semantic
//! env build drives: it holds **one** lazily-spawned bundled sidecar for the
//! server's lifetime, reuses it across every C# `<ProjectReference>` of every
//! project, respawns it after a transport failure (D10), and — crucially —
//! degrades to under-resolution (gospel P5 / D5) whenever the sidecar is
//! unavailable or a build fails. A C# ref that can't be resolved costs its
//! types; it never errors the whole assembly env or crashes the server.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::csharp_sidecar::{SidecarError, SidecarHandle, start_bundled_sidecar};

/// Owns the single reused sidecar process. Held on
/// [`crate::semantic::SemanticState`]; spawned on the first C# reference
/// encountered, never before (a project with no `.csproj` refs never pays for
/// a sidecar). [`Default`] is the un-spawned state.
#[derive(Default)]
pub struct SidecarManager {
    /// The live sidecar, or `None` if not yet spawned / dropped after a
    /// transport failure so the next call respawns.
    handle: Option<SidecarHandle>,
    /// The `dotnet_root` [`Self::handle`] was spawned against. The sidecar
    /// registers MSBuild **process-wide** from the `dotnet` it was launched
    /// with, so one handle can only faithfully evaluate projects that resolve
    /// to that same SDK root. When a project resolves to a *different* root
    /// (separate `global.json` / `sdk.paths`), we tear the handle down and
    /// respawn rather than silently evaluate under the wrong SDK.
    spawned_root: Option<PathBuf>,
    /// Latched once the bundled sidecar is found to be *structurally*
    /// unavailable this session (built without a .NET SDK, so there is no DLL
    /// to run at all). Stops us re-probing discovery — and re-logging — on every
    /// C# ref of every build. A stable *spawn/handshake* fault (a bad `dotnet`
    /// path, a protocol mismatch) does **not** latch this: the failing project
    /// caches its degraded env, but a *different* project resolving to a working
    /// SDK root can still spawn. An initialize timeout is transient and leaves
    /// even the current project's env uncached. The fast-path latch stays
    /// reserved for the genuinely-nothing-to-run case.
    unavailable: bool,
}

/// The outcome of resolving one C# reference's metadata.
#[derive(Debug, Default)]
pub struct CsharpMetadata {
    /// The metadata DLL paths (entry project + its transitive C# closure).
    /// Empty on any failure.
    pub dlls: Vec<PathBuf>,
    /// A *transport* failure (timeout, broken pipe, or dead process) interrupted
    /// this build — the result is incomplete but a retry after respawn may
    /// succeed. The caller should therefore avoid caching an env built from it
    /// (a logical build error, by contrast, is stable and leaves this `false`).
    pub retryable: bool,
}

/// Whether [`SidecarManager::ensure_handle`] left a usable process, or which
/// cache policy the caller must apply to its degraded result.
enum HandleAvailability {
    Ready,
    StableFailure,
    TransientFailure,
}

// `SidecarHandle` owns process/pipe handles and isn't `Debug`; summarise
// instead so `SemanticState` can still derive `Debug`.
impl std::fmt::Debug for SidecarManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarManager")
            .field("spawned", &self.handle.is_some())
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

impl SidecarManager {
    /// The metadata DLLs backing a C# `.csproj` reference: the referenced
    /// project's own Roslyn-emitted metadata DLL, plus one per project in its
    /// transitive C# `<ProjectReference>` closure (the sidecar expands that
    /// closure itself — see the sidecar plan's D7). Deduplicating across calls
    /// is the caller's job (metadata DLLs are content-addressed, so a project
    /// reached via two direct refs yields the same path).
    ///
    /// Returns an empty vec — **never** an error — when the sidecar is
    /// unavailable or the build fails, so a C# ref degrades to under-resolution
    /// without disturbing the package / framework / F#-ref DLLs already in the
    /// env (D5). All failures are logged.
    ///
    /// `workspace_root` is where the sidecar publishes its content-addressed
    /// metadata DLLs (`<root>/obj/borzoi/csharp-sidecar/`); it is fixed
    /// at the first spawn and reused. `project_tfms` is the closure-wide TFM map
    /// (from [`crate::project_assets::resolve_transitive_project_tfms`]); an
    /// empty map is valid.
    #[allow(clippy::too_many_arguments)]
    pub fn metadata_dlls_for_csproj(
        &mut self,
        dotnet_exe: &Path,
        dotnet_root: &Path,
        workspace_root: &Path,
        csproj: &Path,
        configuration: &str,
        target_framework: &str,
        project_tfms: &BTreeMap<PathBuf, String>,
    ) -> CsharpMetadata {
        match self.ensure_handle(dotnet_exe, dotnet_root, workspace_root) {
            HandleAvailability::Ready => {}
            HandleAvailability::StableFailure => {
                // A bad executable path, missing sidecar, or protocol mismatch
                // cannot self-heal between two requests. Cache the C#-less env.
                return CsharpMetadata {
                    dlls: Vec::new(),
                    retryable: false,
                };
            }
            HandleAvailability::TransientFailure => {
                // An initialize request can time out before a handle is installed.
                // Its local handle has already been dropped; leave the env
                // uncached so the next request starts a fresh process.
                return CsharpMetadata {
                    dlls: Vec::new(),
                    retryable: true,
                };
            }
        }
        let handle = self
            .handle
            .as_mut()
            .expect("HandleAvailability::Ready means a handle exists");
        match handle.build_metadata(csproj, configuration, target_framework, project_tfms) {
            Ok(result) => {
                let mut dlls = Vec::with_capacity(1 + result.transitive_project_refs.len());
                dlls.push(result.metadata_dll_path);
                dlls.extend(
                    result
                        .transitive_project_refs
                        .into_iter()
                        .map(|r| r.metadata_dll_path),
                );
                CsharpMetadata {
                    dlls,
                    retryable: false,
                }
            }
            Err(err) if is_transport_error(&err) => {
                // The pipe/process is broken. Drop the handle so the next call
                // respawns a fresh sidecar (D10 respawn-on-crash), and flag the
                // result retryable so the caller doesn't cache this project's
                // env without the C# ref (the respawn would otherwise only help
                // *other* projects).
                tracing::warn!(
                    csproj = %csproj.display(),
                    error = %err,
                    "C# sidecar transport failure; dropping handle to respawn on the next request"
                );
                self.handle = None;
                self.spawned_root = None;
                CsharpMetadata {
                    dlls: Vec::new(),
                    retryable: true,
                }
            }
            Err(err) => {
                // A logical per-request failure (`BuildFailed`, `LoadFailed`,
                // `RestoreRequired`, a bad RPC): the sidecar itself is healthy
                // and can keep serving other refs, so we keep the handle and
                // skip only this reference. Not retryable — the same inputs
                // would fail identically, so caching the degraded env is fine.
                tracing::warn!(
                    csproj = %csproj.display(),
                    error = %err,
                    "C# sidecar buildMetadata failed; skipping this reference (its types will be unresolved)"
                );
                CsharpMetadata {
                    dlls: Vec::new(),
                    retryable: false,
                }
            }
        }
    }

    /// Ensure a live handle exists, spawning one on first use. Never spawns
    /// again once [`Self::unavailable`] latches. The outcome distinguishes a
    /// stable configuration failure from an initialize timeout that may recover
    /// on a fresh process.
    fn ensure_handle(
        &mut self,
        dotnet_exe: &Path,
        dotnet_root: &Path,
        workspace_root: &Path,
    ) -> HandleAvailability {
        // A live handle bound to a *different* SDK root can't be reused: the
        // sidecar's MSBuild registration is process-wide, so it would evaluate
        // this project under the wrong SDK. Tear it down and respawn.
        if self.handle.is_some() && self.spawned_root.as_deref() != Some(dotnet_root) {
            tracing::info!(
                old = ?self.spawned_root,
                new = %dotnet_root.display(),
                "dotnet_root changed; restarting the C# sidecar for the new SDK"
            );
            self.handle = None; // `Drop` reaps the child.
            self.spawned_root = None;
        }
        if self.handle.is_some() {
            return HandleAvailability::Ready;
        }
        if self.unavailable {
            return HandleAvailability::StableFailure;
        }
        match start_bundled_sidecar(dotnet_exe, workspace_root, dotnet_root) {
            Ok(handle) => {
                tracing::info!("started C# sidecar for project-reference metadata");
                self.handle = Some(handle);
                self.spawned_root = Some(dotnet_root.to_path_buf());
                HandleAvailability::Ready
            }
            Err(SidecarError::BundledSidecarUnavailable) => {
                // Structural: no DLL to run. Latch so we don't re-probe/re-log.
                tracing::info!(
                    "C# sidecar is not bundled in this build; C# project references \
                     will be unresolved this session (F# refs and packages are unaffected)"
                );
                self.unavailable = true;
                HandleAvailability::StableFailure
            }
            Err(err) => {
                let retryable = is_retryable_start_error(&err);
                tracing::warn!(
                    error = %err,
                    "failed to start the C# sidecar; C# project references unresolved this build"
                );
                if retryable {
                    HandleAvailability::TransientFailure
                } else {
                    HandleAvailability::StableFailure
                }
            }
        }
    }
}

/// True for errors that mean the sidecar process/pipe is broken — so the handle
/// must be dropped and a fresh one spawned next time — as opposed to a logical
/// per-request failure (`Sidecar`/`Rpc`) a healthy sidecar keeps serving past.
fn is_transport_error(err: &SidecarError) -> bool {
    matches!(
        err,
        SidecarError::Io(_)
            | SidecarError::RequestTimedOut { .. }
            | SidecarError::Framing(_)
            | SidecarError::Json(_)
            | SidecarError::UnexpectedResponseId { .. }
            | SidecarError::ProcessExited { .. }
    )
}

/// A request timeout during `initialize` is transport-transient even though no
/// handle has yet been installed for the normal drop-on-error branch to clear.
fn is_retryable_start_error(err: &SidecarError) -> bool {
    matches!(err, SidecarError::RequestTimedOut { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::csharp_sidecar::start_sidecar_with_timeout;

    #[test]
    fn transport_errors_are_classified_for_respawn() {
        // Broken pipe / framing / process death → respawn.
        assert!(is_transport_error(&SidecarError::Io(
            std::io::Error::other("x")
        )));
        assert!(is_transport_error(&SidecarError::Framing("bad".into())));
        assert!(is_transport_error(&SidecarError::ProcessExited {
            code: Some(1)
        }));
        assert!(is_transport_error(&SidecarError::UnexpectedResponseId {
            expected: 1,
            got: None
        }));
        assert!(is_transport_error(&SidecarError::RequestTimedOut {
            method: "buildMetadata".to_string(),
            after: Duration::from_millis(10),
        }));
        // Logical per-request failures → keep the healthy handle.
        assert!(!is_transport_error(&SidecarError::Rpc {
            code: -32601,
            message: "no".into()
        }));
    }

    #[test]
    fn initialize_timeout_is_retryable() {
        assert!(is_retryable_start_error(&SidecarError::RequestTimedOut {
            method: "initialize".to_string(),
            after: Duration::from_millis(10),
        }));
        assert!(!is_retryable_start_error(
            &SidecarError::ProtocolVersionMismatch {
                client: "client",
                sidecar: "server".to_string(),
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_drops_the_handle_and_marks_the_result_retryable() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("slow-sidecar.sh");
        std::fs::write(
            &script,
            r#"
read_request() {
    IFS= read -r header || exit 1
    length=${header#Content-Length: }
    length=${length%?}
    IFS= read -r blank || exit 1
    dd bs=1 count="$length" of=/dev/null 2>/dev/null
}

read_request
body='{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"0.4.0","runtimeVersion":"fake","roslynVersion":null}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
read_request
sleep 1
"#,
        )
        .unwrap();
        let timeout = Duration::from_millis(100);
        let handle =
            start_sidecar_with_timeout(Path::new("sh"), &script, tmp.path(), tmp.path(), timeout)
                .expect("fake sidecar should complete initialize");
        let mut mgr = SidecarManager {
            handle: Some(handle),
            spawned_root: Some(tmp.path().to_path_buf()),
            unavailable: false,
        };

        let meta = mgr.metadata_dlls_for_csproj(
            Path::new("sh"),
            tmp.path(),
            tmp.path(),
            &tmp.path().join("Slow.csproj"),
            "Debug",
            "net10.0",
            &BTreeMap::new(),
        );

        assert!(meta.dlls.is_empty());
        assert!(meta.retryable);
        assert!(mgr.handle.is_none(), "the timed-out handle is poisoned");
        assert!(mgr.spawned_root.is_none());
    }

    #[test]
    fn spawn_failure_degrades_to_empty_and_is_not_retryable() {
        // A bogus `dotnet` path makes the spawn fail (the bundled DLL exists in
        // the dev tree, so discovery gets past the availability check and only
        // the process spawn fails). The call degrades to an empty DLL set, and
        // is **not** retryable: a bad `dotnet` path won't fix itself mid-session,
        // so the caller caches the C#-less env rather than re-spawn on every
        // request. It still doesn't latch the `unavailable` fast-path — that's
        // reserved for the structural "no DLL bundled at all" case — so a
        // *different* project resolving to a working SDK root can still spawn.
        let mut mgr = SidecarManager::default();
        let meta = mgr.metadata_dlls_for_csproj(
            Path::new("/nonexistent/dotnet"),
            Path::new("/nonexistent"),
            Path::new("/tmp"),
            Path::new("/tmp/Whatever.csproj"),
            "Debug",
            "net10.0",
            &BTreeMap::new(),
        );
        assert!(
            meta.dlls.is_empty(),
            "spawn failure must degrade to no DLLs"
        );
        assert!(
            !meta.retryable,
            "a spawn failure is a stable degradation (the env won't self-heal), not retryable"
        );
        assert!(
            !mgr.unavailable,
            "a spawn failure must not latch `unavailable` (reserved for the not-bundled case)"
        );
        assert!(
            mgr.handle.is_none(),
            "no handle should be held after a failed spawn"
        );
    }
}
