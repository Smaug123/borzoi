//! Errors surfaced by the C# sidecar client.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use super::protocol::SidecarErrorKind;

#[derive(Debug)]
pub enum SidecarError {
    /// Failed to spawn the sidecar process (e.g. `dotnet` not on PATH).
    /// Carries the path the spawn attempted.
    Spawn { program: PathBuf, source: io::Error },
    /// The supplied sidecar DLL path does not exist. We check this before
    /// invoking `dotnet`, because otherwise the muxer would print its own
    /// error banner to stdout and exit, and the failure would surface as a
    /// framing/I/O error that discards the path.
    SidecarDllMissing { path: PathBuf },
    /// `build.rs` did not publish a bundled sidecar DLL (e.g. `dotnet` was
    /// missing at crate-build time). The discovery shim refuses to spawn
    /// rather than silently mis-resolving to the empty path.
    BundledSidecarUnavailable,
    /// Generic I/O failure on stdin/stdout of the sidecar process.
    Io(io::Error),
    /// The sidecar did not complete a request within its deadline. The whole
    /// round trip is covered, including writing the request to stdin.
    RequestTimedOut { method: String, after: Duration },
    /// The sidecar emitted bytes that do not conform to the LSP-style
    /// length-prefixed framing.
    Framing(String),
    /// Failed to (de)serialise a JSON-RPC message.
    Json(serde_json::Error),
    /// The sidecar returned a generic JSON-RPC error (parse error, invalid
    /// params, method not found): a JSON-RPC framing fault on our part.
    Rpc { code: i64, message: String },
    /// The sidecar returned a typed application error. The `kind` carries
    /// structured detail (load diagnostics, missing-path info, etc.).
    Sidecar {
        kind: SidecarErrorKind,
        message: String,
    },
    /// The sidecar's response carried a different (or missing) `id` than the
    /// matching request. The protocol is synchronous, so any mismatch is a
    /// hard error.
    UnexpectedResponseId { expected: u64, got: Option<u64> },
    /// The sidecar's `initialize` response advertised a protocol version the
    /// client does not understand.
    ProtocolVersionMismatch {
        client: &'static str,
        sidecar: String,
    },
    /// The sidecar process exited before we received the expected response,
    /// or exited non-zero after `shutdown`.
    ProcessExited { code: Option<i32> },
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarError::Spawn { program, source } => {
                write!(
                    f,
                    "failed to spawn sidecar ({}): {source}",
                    program.display()
                )
            }
            SidecarError::SidecarDllMissing { path } => {
                write!(f, "sidecar DLL not found: {}", path.display())
            }
            SidecarError::BundledSidecarUnavailable => write!(
                f,
                "bundled csharp-sidecar DLL was not published at crate-build time; \
                 install a .NET 10 SDK and rebuild this crate to enable it"
            ),
            SidecarError::Io(e) => write!(f, "sidecar I/O error: {e}"),
            SidecarError::RequestTimedOut { method, after } => write!(
                f,
                "sidecar request {method:?} did not complete within {after:?}"
            ),
            SidecarError::Framing(msg) => write!(f, "sidecar framing error: {msg}"),
            SidecarError::Json(e) => write!(f, "sidecar JSON error: {e}"),
            SidecarError::Rpc { code, message } => {
                write!(f, "sidecar RPC error {code}: {message}")
            }
            SidecarError::Sidecar { kind, message } => {
                write!(f, "sidecar error ({kind:?}): {message}")
            }
            SidecarError::UnexpectedResponseId { expected, got } => match got {
                Some(id) => write!(
                    f,
                    "sidecar response id mismatch: expected {expected}, got {id}"
                ),
                None => write!(f, "sidecar response missing id (expected {expected})"),
            },
            SidecarError::ProtocolVersionMismatch { client, sidecar } => write!(
                f,
                "sidecar protocol version mismatch: client expects {client}, sidecar reports {sidecar}"
            ),
            SidecarError::ProcessExited { code } => match code {
                Some(c) => write!(f, "sidecar process exited with code {c}"),
                None => write!(f, "sidecar process exited (signalled)"),
            },
        }
    }
}

impl std::error::Error for SidecarError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SidecarError::Spawn { source, .. } => Some(source),
            SidecarError::Io(e) => Some(e),
            SidecarError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for SidecarError {
    fn from(e: io::Error) -> Self {
        SidecarError::Io(e)
    }
}

impl From<serde_json::Error> for SidecarError {
    fn from(e: serde_json::Error) -> Self {
        SidecarError::Json(e)
    }
}
