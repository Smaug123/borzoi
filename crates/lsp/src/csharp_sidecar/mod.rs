//! Client for the C# project-reference sidecar.
//!
//! The sidecar is a separate managed (.NET) process the LSP spawns to load
//! and bind C# `.csproj` files via Roslyn. This module supplies the Rust-side
//! supervisor: spawn, handshake, request-response, shutdown. See
//! `docs/completed/csharp-sidecar-plan.md` for the overall design.
//!
//! Phase 4 carries `initialize`, `buildMetadata`, and `shutdown`. On the
//! happy path `build_metadata` returns a [`BuildMetadataResult`] with the
//! emitted DLL's absolute path, its 32-byte SHA-256 content hash, and a
//! `from_cache` flag. The sidecar derives the cache key from the build's
//! inputs (csproj bytes, source content, reference hashes, compilation
//! options) and publishes DLLs at
//! `<workspace>/obj/borzoi/csharp-sidecar/<prefix>/<hash>.dll`;
//! identical inputs hit the cache on the second call.

mod error;
mod process;
pub mod protocol;

pub use error::SidecarError;
#[cfg(test)]
pub(crate) use process::start_sidecar_with_timeout;
pub use process::{SidecarHandle, start_bundled_sidecar, start_sidecar};
pub use protocol::{
    BuildMetadataResult, CompilerDiagnostic, DiagnosticPosition, DiagnosticRange, InitializeResult,
    PROTOCOL_VERSION, SidecarErrorKind, TransitiveProjectRef, WorkspaceDiagnostic,
    content_hash_hex,
};
