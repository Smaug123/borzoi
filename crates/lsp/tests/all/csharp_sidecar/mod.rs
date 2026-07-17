//! Integration test for the C# sidecar (`tools/csharp-sidecar/`).
//!
//! Builds the sidecar via `dotnet build`, spawns it, completes the
//! `initialize` handshake, and exercises `buildMetadata` end-to-end. Phase 4
//! drives a real Roslyn metadata-only emit through a SHA-256 content-addressed
//! cache; on success we get a path to a DLL inside the workspace's
//! `obj/borzoi/csharp-sidecar/<prefix>/` directory whose filename
//! is the lowercase-hex cache key, and which we can parse with [`Ecma335Assembly`].
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

mod cache;
mod differential;
mod errors;
mod multi_tfm;
mod project_ref;
mod smoke;
mod support;
