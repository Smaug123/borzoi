//! Wire-format types for the JSON-RPC dialogue with the C# sidecar.
//!
//! The protocol is a strict subset of JSON-RPC 2.0 with LSP-style length-
//! prefixed framing. The methods on the wire are `initialize`,
//! `buildMetadata`, and `shutdown`; an earlier draft of the plan also
//! included `invalidate`, but D9 of `docs/completed/csharp-sidecar-plan.md` now
//! handles cache invalidation structurally (the content-addressed cache
//! key re-derives on every call) so no client-side notification is
//! needed.
//!
//! Property names on the wire are camelCase to match what the C# side emits
//! when `JsonNamingPolicy.CamelCase` is in effect.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

/// Wire-protocol version the client speaks. The sidecar advertises its own
/// version in the `initialize` response; any mismatch is fatal. Bumped in
/// phase 4 alongside the addition of the required `contentHash` field on the
/// `buildMetadata` success response; bumped again to `0.4.0` for the
/// addition of the required `projectTfms` field on `BuildMetadataParams`
/// (phase 3 of `docs/completed/multi-tfm-resolution-plan.md`).
pub const PROTOCOL_VERSION: &str = "0.4.0";

/// Sidecar-specific JSON-RPC error code. The `data` field on the error then
/// carries a [`RawSidecarErrorData`] discriminating the actual kind.
pub(crate) const SIDECAR_ERROR_CODE: i64 = -32000;

#[derive(Serialize, Debug)]
pub(crate) struct JsonRpcRequest<'a, P> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: P,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcResponse {
    #[allow(dead_code)] // present for completeness / future strict validation
    pub jsonrpc: Option<String>,
    pub id: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcError {
    pub code: i64,
    pub message: String,
    /// Sidecar-defined errors (code [`SIDECAR_ERROR_CODE`]) carry a
    /// structured payload with a discriminating `kind`. Generic JSON-RPC
    /// errors (parse / invalid request / method not found / invalid params)
    /// have no `data`.
    pub data: Option<serde_json::Value>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InitializeParams<'a> {
    pub workspace_root: &'a str,
    pub dotnet_root: &'a str,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The sidecar's wire-protocol version. Must equal [`PROTOCOL_VERSION`].
    pub protocol_version: String,
    /// The .NET runtime version the sidecar is running on (e.g. `10.0.7`).
    pub runtime_version: String,
    /// Roslyn assembly informational version string, or `None` if the sidecar
    /// could not resolve it. Phase 3 always populates this.
    pub roslyn_version: Option<String>,
}

/// Parameters for the `buildMetadata` request.
///
/// `project_tfms` is the closure-wide TFM map produced by
/// [`crate::project_assets::transitive_project_tfms`]: every csproj in the
/// requested project's `<ProjectReference>` closure (top csproj included)
/// keyed to the short-form TFM NuGet's restore selected for it. The
/// sidecar uses this to drive per-project workspace construction so each
/// node builds under the producer TFM rather than the consumer's. As of
/// protocol `0.4.0` the field is required on the wire — passing an empty
/// map is valid (the sidecar will simply not find an entry for the top
/// csproj and use `target_framework` for the load), but the field itself
/// must be present.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildMetadataParams<'a> {
    pub csproj_path: &'a str,
    pub configuration: &'a str,
    pub target_framework: &'a str,
    pub project_tfms: &'a BTreeMap<PathBuf, String>,
}

/// Success-path response for `buildMetadata`. Phase 4 populates `from_cache`
/// from the content-addressed lookup and `content_hash` with the 32-byte
/// SHA-256 cache key over the build's inputs (D6). Phase 5 populates
/// `transitive_project_refs` with one entry per project in the requested
/// project's `<ProjectReference>` closure (sorted by csproj path for wire
/// stability).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildMetadataResult {
    /// Absolute path to the published metadata DLL. Lives inside the
    /// workspace root's `obj/borzoi/csharp-sidecar/<prefix>/`
    /// directory; the filename is `<content_hash hex>.dll`.
    pub metadata_dll_path: PathBuf,
    /// 32-byte SHA-256 cache key over the build's inputs. The wire format is
    /// a 64-character lowercase hex string; this field is the decoded bytes.
    /// Two calls with byte-identical inputs return the same value.
    #[serde(deserialize_with = "deserialize_content_hash")]
    pub content_hash: [u8; 32],
    /// Whether the sidecar reused a cached DLL (`true`) or re-emitted
    /// (`false`). On a cache hit the `diagnostics` array is empty even if the
    /// original emit produced warnings — those were attached to that call's
    /// response, and a cache hit deliberately does not re-drive Roslyn.
    pub from_cache: bool,
    /// Roslyn diagnostics surfaced by the emit. Empty on cache hits (see
    /// `from_cache`); may be non-empty on cache misses even when the emit
    /// succeeds (warnings, informational diagnostics).
    pub diagnostics: Vec<CompilerDiagnostic>,
    /// Metadata DLLs the sidecar emitted for transitively-referenced
    /// csprojs. Sorted by `csproj_path` for wire stability. The list is
    /// the transitive closure (so an N-deep chain produces N-1 entries
    /// here, one per non-root project), with the root project's own
    /// metadata DLL only appearing as `metadata_dll_path` above.
    pub transitive_project_refs: Vec<TransitiveProjectRef>,
}

/// Hex-decode the 64-char lowercase string the sidecar sends into a fixed
/// `[u8; 32]`. Anything other than 32 bytes of valid lowercase hex is a wire
/// protocol violation; we refuse to silently truncate or zero-pad.
fn deserialize_content_hash<'de, D>(d: D) -> Result<[u8; 32], D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    if s.len() != 64 {
        return Err(serde::de::Error::invalid_length(
            s.len(),
            &"64 hex characters",
        ));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = hex_nibble(chunk[0]).ok_or_else(|| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Char(chunk[0] as char),
                &"lowercase hex digit",
            )
        })?;
        let lo = hex_nibble(chunk[1]).ok_or_else(|| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Char(chunk[1] as char),
                &"lowercase hex digit",
            )
        })?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        _ => None,
    }
}

/// Lowercase-hex render of a content hash; matches the wire format the
/// sidecar emits and what `git` uses for object names.
pub fn content_hash_hex(hash: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in hash {
        let _ = fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
    }
    out
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransitiveProjectRef {
    pub csproj_path: PathBuf,
    pub metadata_dll_path: PathBuf,
}

/// On-the-wire shape of a Roslyn compiler diagnostic. Mirrors the shape
/// documented in `docs/completed/csharp-sidecar-plan.md` D8.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompilerDiagnostic {
    /// Compiler diagnostic id (e.g. `"CS0103"`).
    pub id: String,
    /// Roslyn `DiagnosticSeverity` rendered as its enum name: `"Hidden"`,
    /// `"Info"`, `"Warning"`, `"Error"`.
    pub severity: String,
    pub message: String,
    /// Source file the diagnostic points at, if any. `None` for synthesised
    /// diagnostics that aren't tied to a particular source location.
    pub file_path: Option<String>,
    /// 0-based source range, present iff the diagnostic has an in-source
    /// location.
    pub range: Option<DiagnosticRange>,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRange {
    pub start: DiagnosticPosition,
    pub end: DiagnosticPosition,
}

/// 0-based line/character pair, matching the LSP convention.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticPosition {
    pub line: i32,
    pub character: i32,
}

/// Typed discriminator the sidecar attaches to every application error. Kept
/// in sync with `SidecarErrorKind` in `tools/csharp-sidecar/Protocol.cs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarErrorKind {
    /// `buildMetadata` dispatched before `initialize` completed.
    NotInitialized,
    /// The method exists on the wire but its implementation is reserved for
    /// a later phase. No current method actually returns this; reserved
    /// for forward compatibility with later additions to the wire surface.
    NotImplemented,
    /// MSBuildLocator could not find a .NET SDK to bind to.
    SdkUnavailable,
    /// The csproj path supplied to `buildMetadata` does not exist.
    CsprojNotFound { csproj_path: String },
    /// MSBuildWorkspace reported a hard load failure. Carries the
    /// workspace-level diagnostics it produced during the load.
    LoadFailed {
        diagnostics: Vec<WorkspaceDiagnostic>,
    },
    /// Roslyn's `Emit().Success` was `false`. The sidecar surfaces the
    /// compiler diagnostics and refuses to publish a DLL (per D8 there is no
    /// stale-cache fallback). Workspace-level diagnostics from the load
    /// step are passed through verbatim so the caller can render both
    /// classes in the same UI surface.
    BuildFailed {
        diagnostics: Vec<CompilerDiagnostic>,
        workspace_diagnostics: Vec<WorkspaceDiagnostic>,
    },
    /// The sidecar could not create or write into its cache directory.
    /// `cache_path` is the absolute path it tried; `detail` is the OS
    /// error message.
    CacheUnwritable { cache_path: String, detail: String },
    /// The sidecar cannot bind to Roslyn's internal deterministic-key API.
    /// Raised at `initialize` time. Almost always means the sidecar binary
    /// was built against a Roslyn version that differs from the one it's
    /// running with — rebuild the sidecar.
    IncompatibleRoslyn,
    /// `projectTfms` did not contain an entry for a csproj the sidecar needs
    /// to load — either the top csproj or one of its transitive
    /// `<ProjectReference>` targets. Phase 4 hard-errors here rather than
    /// silently falling back to the consumer TFM (per D5 of
    /// `docs/completed/multi-tfm-resolution-plan.md`): a missing entry in the closure
    /// map is a Rust-side bug and we want it loud. `csproj_path` is the
    /// project the sidecar tried and failed to look up.
    MissingProjectTfm { csproj_path: String },
    /// The sidecar returned a `kind` value the client does not understand.
    /// Surfaced verbatim so the user can act on it; treat as an upgrade
    /// mismatch.
    Other { kind: String },
}

/// On-the-wire shape of a workspace diagnostic. Mirrors the subset of
/// Roslyn's `WorkspaceDiagnostic` the sidecar serialises.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDiagnostic {
    /// Roslyn `WorkspaceDiagnosticKind` rendered as its enum name, e.g.
    /// `"Warning"`, `"Failure"`.
    pub kind: String,
    pub message: String,
    pub file_path: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSidecarErrorData {
    pub kind: String,
    #[serde(default)]
    pub diagnostics: serde_json::Value,
    #[serde(default)]
    pub workspace_diagnostics: Vec<WorkspaceDiagnostic>,
    #[serde(default)]
    pub csproj_path: Option<String>,
    #[serde(default)]
    pub cache_path: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

impl SidecarErrorKind {
    /// Parse the typed payload carried by a sidecar JSON-RPC error response.
    /// Unknown `kind` values come back as [`SidecarErrorKind::Other`] so a
    /// newer sidecar talking to older Rust still surfaces a comprehensible
    /// error rather than a serde failure.
    pub(crate) fn from_data(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        let raw: RawSidecarErrorData = serde_json::from_value(value)?;
        Ok(match raw.kind.as_str() {
            "NotInitialized" => SidecarErrorKind::NotInitialized,
            "NotImplemented" => SidecarErrorKind::NotImplemented,
            "SdkUnavailable" => SidecarErrorKind::SdkUnavailable,
            "CsprojNotFound" => SidecarErrorKind::CsprojNotFound {
                csproj_path: raw.csproj_path.unwrap_or_default(),
            },
            "LoadFailed" => SidecarErrorKind::LoadFailed {
                diagnostics: parse_workspace_diagnostics(raw.diagnostics)?,
            },
            "BuildFailed" => SidecarErrorKind::BuildFailed {
                diagnostics: parse_compiler_diagnostics(raw.diagnostics)?,
                workspace_diagnostics: raw.workspace_diagnostics,
            },
            "CacheUnwritable" => SidecarErrorKind::CacheUnwritable {
                cache_path: raw.cache_path.unwrap_or_default(),
                detail: raw.detail.unwrap_or_default(),
            },
            "IncompatibleRoslyn" => SidecarErrorKind::IncompatibleRoslyn,
            "MissingProjectTfm" => SidecarErrorKind::MissingProjectTfm {
                csproj_path: raw.csproj_path.unwrap_or_default(),
            },
            other => SidecarErrorKind::Other {
                kind: other.to_string(),
            },
        })
    }
}

/// `data.diagnostics` is `WorkspaceDiagnostic[]` for `LoadFailed` and
/// `CompilerDiagnostic[]` for `BuildFailed`. We deserialise lazily — keep
/// the raw `Value` until the kind tag picks a shape — so a single field on
/// the wire can carry either without a discriminator.
fn parse_workspace_diagnostics(
    v: serde_json::Value,
) -> Result<Vec<WorkspaceDiagnostic>, serde_json::Error> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(v)
}

fn parse_compiler_diagnostics(
    v: serde_json::Value,
) -> Result<Vec<CompilerDiagnostic>, serde_json::Error> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the on-wire shape of `BuildMetadataParams` after the 0.4.0 bump:
    /// the `projectTfms` field MUST appear, and its inner keys are the
    /// csproj paths as the producer/consumer closure knows them (no
    /// camelCase mangling of values).
    ///
    /// `BTreeMap` iteration is sorted by key, so the JSON is deterministic
    /// — the sidecar consumes it as an arbitrary dictionary but a stable
    /// shape makes snapshot/golden tests on either side practical.
    #[test]
    fn build_metadata_params_serialises_with_project_tfms() {
        let mut tfms = BTreeMap::new();
        tfms.insert(PathBuf::from("/repo/Top.csproj"), "net10.0".to_string());
        tfms.insert(PathBuf::from("/repo/Lib.csproj"), "net8.0".to_string());
        let params = BuildMetadataParams {
            csproj_path: "/repo/Top.csproj",
            configuration: "Debug",
            target_framework: "net10.0",
            project_tfms: &tfms,
        };
        let json = serde_json::to_value(&params).expect("serialise");
        assert_eq!(
            json,
            serde_json::json!({
                "csprojPath": "/repo/Top.csproj",
                "configuration": "Debug",
                "targetFramework": "net10.0",
                "projectTfms": {
                    "/repo/Lib.csproj": "net8.0",
                    "/repo/Top.csproj": "net10.0",
                },
            })
        );
    }

    /// Even when the closure is empty (e.g. ad-hoc callers that don't
    /// resolve `project.assets.json` first), the field still ships as an
    /// empty object. The 0.4.0 sidecar treats absence as a wire-protocol
    /// violation, so `{}` is the correct degenerate value.
    #[test]
    fn build_metadata_params_serialises_empty_project_tfms() {
        let tfms = BTreeMap::new();
        let params = BuildMetadataParams {
            csproj_path: "/repo/Top.csproj",
            configuration: "Debug",
            target_framework: "net10.0",
            project_tfms: &tfms,
        };
        let json = serde_json::to_value(&params).expect("serialise");
        assert_eq!(
            json.get("projectTfms"),
            Some(&serde_json::json!({})),
            "projectTfms must appear even when the closure is empty",
        );
    }

    /// Phase 4 surfaces `MissingProjectTfm` when the closure map omits a
    /// project the sidecar needs to load. The wire shape reuses the
    /// already-existing `csprojPath` field on `RawSidecarErrorData`, so a
    /// minimal payload with just `{ kind, csprojPath }` is enough.
    #[test]
    fn from_data_parses_missing_project_tfm() {
        let payload = serde_json::json!({
            "kind": "MissingProjectTfm",
            "csprojPath": "/repo/Leaf.csproj",
        });
        let kind = SidecarErrorKind::from_data(payload).expect("parse MissingProjectTfm");
        assert_eq!(
            kind,
            SidecarErrorKind::MissingProjectTfm {
                csproj_path: "/repo/Leaf.csproj".to_string(),
            }
        );
    }

    /// A `MissingProjectTfm` payload that somehow lost the `csprojPath`
    /// (older sidecar, hand-rolled test fixture) still parses — the field
    /// degrades to an empty string rather than failing the whole error
    /// path. Mirrors the `csproj_path.unwrap_or_default()` strategy used by
    /// the sibling `CsprojNotFound` arm.
    #[test]
    fn from_data_parses_missing_project_tfm_without_path() {
        let payload = serde_json::json!({ "kind": "MissingProjectTfm" });
        let kind = SidecarErrorKind::from_data(payload).expect("parse MissingProjectTfm");
        assert_eq!(
            kind,
            SidecarErrorKind::MissingProjectTfm {
                csproj_path: String::new(),
            }
        );
    }
}
