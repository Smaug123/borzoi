//! Reader-agnostic F#-resource helpers.
//!
//! Classifying a manifest resource name by its pickle prefix, inflating the
//! compressed variants, and selecting the host CCU's signature resource for the
//! measure overlay depend only on the resource name and bytes — not on how they
//! were read — so they live here rather than in [`crate::Ecma335Assembly`].

use crate::view::{FSharpResource, ResourceKind};

/// Maximum inflated size of one raw-deflate payload. F# signature pickles and
/// embedded PDB/source blobs are compiler metadata, not user data; real payloads
/// are small enough that a 64 MiB single-resource ceiling leaves substantial
/// headroom while preventing deflate bombs from allocating until process abort.
pub(crate) const MAX_DEFLATE_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Classify a resource name by its `FSharp*.` prefix, returning the detected
/// kind plus the trailing suffix (the assembly logical name — e.g.
/// `FSharpSignatureData.MyLib` → `(SignatureData, "MyLib")`). The prefix list
/// mirrors `PrettyNaming.fs` 1-for-1; longer-prefix variants come first so
/// `FSharpSignatureCompressedDataB.` is not shadowed by
/// `FSharpSignatureCompressedData.`.
pub(crate) fn classify_fsharp_resource(name: &str) -> Option<(ResourceKind, &str)> {
    // Order matters: every prefix that is another's strict extension must come
    // first. The `*B` and `*CompressedData*` names are all such extensions of
    // `FSharp{Signature,Optimization}`.
    const TABLE: &[(&str, ResourceKind)] = &[
        // Compressed-secondary (longest)
        (
            "FSharpSignatureCompressedDataB.",
            ResourceKind::SignatureCompressedDataB,
        ),
        (
            "FSharpOptimizationCompressedDataB.",
            ResourceKind::OptimizationCompressedDataB,
        ),
        // Compressed-primary
        (
            "FSharpSignatureCompressedData.",
            ResourceKind::SignatureCompressedData,
        ),
        (
            "FSharpOptimizationCompressedData.",
            ResourceKind::OptimizationCompressedData,
        ),
        // Uncompressed-secondary
        ("FSharpSignatureDataB.", ResourceKind::SignatureDataB),
        ("FSharpOptimizationDataB.", ResourceKind::OptimizationDataB),
        // Uncompressed-primary
        ("FSharpSignatureData.", ResourceKind::SignatureData),
        ("FSharpOptimizationData.", ResourceKind::OptimizationData),
        // FSharp.Core-only (no `Data` token in the prefix)
        (
            "FSharpSignatureInfo.",
            ResourceKind::SignatureDataFSharpCore,
        ),
        (
            "FSharpOptimizationInfo.",
            ResourceKind::OptimizationDataFSharpCore,
        ),
    ];
    for (prefix, kind) in TABLE {
        if let Some(rest) = name.strip_prefix(prefix) {
            return Some((*kind, rest));
        }
    }
    None
}

/// Whether a resource kind is deflate-compressed on the wire.
pub(crate) fn is_compressed_kind(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::SignatureCompressedData
            | ResourceKind::SignatureCompressedDataB
            | ResourceKind::OptimizationCompressedData
            | ResourceKind::OptimizationCompressedDataB
    )
}

/// Inflate raw-deflate bytes — no gzip or zlib header. Mirrors F#'s
/// `DeflateStream(raw, CompressionMode.Decompress)`: the resource payload is
/// exactly the deflate stream with no framing. The inflated output is capped:
/// compressed resources are untrusted DLL bytes, and raw `read_to_end` would
/// let a tiny deflate bomb allocate until the process aborts.
pub(crate) fn decompress_deflate(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    decompress_deflate_limited(bytes, MAX_DEFLATE_OUTPUT_BYTES)
}

/// Inflate a raw-deflate stream whose uncompressed length is declared by its
/// outer format. The declared length is used as the immediate `Read::take`
/// bound, but absurd declarations are rejected before decompression so a
/// malicious header cannot simply move the allocation target from the stream to
/// the size field.
pub(crate) fn decompress_deflate_exact(
    bytes: &[u8],
    expected_output_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    if expected_output_bytes > MAX_DEFLATE_OUTPUT_BYTES {
        return Err(deflate_limit_error(expected_output_bytes));
    }

    let out = decompress_deflate_limited(bytes, expected_output_bytes)?;
    if out.len() != expected_output_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "deflate output was {} bytes, expected {expected_output_bytes}",
                out.len()
            ),
        ));
    }
    Ok(out)
}

pub(crate) fn decompress_deflate_limited(
    bytes: &[u8],
    max_output_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    use flate2::read::DeflateDecoder;
    use std::io::Read;

    let mut out = Vec::new();
    DeflateDecoder::new(bytes)
        .take(read_take_limit(max_output_bytes))
        .read_to_end(&mut out)?;
    if out.len() > max_output_bytes {
        return Err(deflate_limit_error(max_output_bytes));
    }
    Ok(out)
}

fn read_take_limit(max_output_bytes: usize) -> u64 {
    max_output_bytes
        .checked_add(1)
        .map(|n| n as u64)
        .unwrap_or(u64::MAX)
}

fn deflate_limit_error(limit: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("deflate output exceeded the limit of {limit} bytes"),
    )
}

/// The host CCU's *primary* signature-data resource, if present. Mirrors FCS's
/// `decodeSignatureData` selection: any of the three primary kinds
/// (uncompressed, compressed, `FSharp.Core` self-pickle) counts, but the
/// `B`-stream variants do not — those are the sibling nullness/extra-constraints
/// payload from [`b_stream_signature_resource_for_host`]. The host-suffix filter
/// is load-bearing: `fsc --standalone` copies dependent CCUs' pickle resources
/// verbatim under their original suffix, and those foreign pickles must not
/// drive the host's measure merge.
pub(crate) fn primary_signature_resource_for_host<'a>(
    resources: &'a [FSharpResource],
    host_name: &str,
) -> Option<&'a FSharpResource> {
    resources
        .iter()
        .find(|r| is_primary_signature_kind(r.kind) && resource_suffix_equals(&r.name, host_name))
}

/// The host CCU's sibling *B-stream* signature payload, if present. F# ≥ 9 emits
/// a second resource carrying nullness annotations and the extra typar-
/// constraint tags (`NotSupportsNull` / `AllowsRefStruct`); the unpickler
/// threads it through as `phase1bytesB`. Same host-suffix filter as
/// [`primary_signature_resource_for_host`].
pub(crate) fn b_stream_signature_resource_for_host<'a>(
    resources: &'a [FSharpResource],
    host_name: &str,
) -> Option<&'a [u8]> {
    resources
        .iter()
        .find(|r| is_secondary_signature_kind(r.kind) && resource_suffix_equals(&r.name, host_name))
        .map(|r| r.payload.as_slice())
}

/// Whether any *foreign* CCU's primary signature pickle is embedded — a
/// primary signature resource whose suffix is not `host_name`. `fsc
/// --standalone` copies dependency CCUs' pickle resources in verbatim under
/// their own suffix, so their presence means the host pickle does not describe
/// every F# TypeDef in the image: the copied TypeDefs belong to those foreign
/// CCUs. Callers that key host-pickle data on bare names (which can collide
/// across CCUs) must restrict themselves to assemblies where this is `false`.
pub(crate) fn foreign_signature_data_present(
    resources: &[FSharpResource],
    host_name: &str,
) -> bool {
    resources
        .iter()
        .any(|r| is_primary_signature_kind(r.kind) && !resource_suffix_equals(&r.name, host_name))
}

fn is_primary_signature_kind(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::SignatureData
            | ResourceKind::SignatureCompressedData
            | ResourceKind::SignatureDataFSharpCore
    )
}

fn is_secondary_signature_kind(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::SignatureDataB | ResourceKind::SignatureCompressedDataB
    )
}

/// Whether the resource's pickle-prefix-stripped suffix equals `host_name`.
/// Suffixes can contain dots (`FSharpSignatureData.FsLexYacc.Runtime` →
/// `FsLexYacc.Runtime`), so a naive `rsplit_once('.')` is wrong; the only
/// correct way to recover the suffix is to strip the known prefix family via
/// [`classify_fsharp_resource`].
fn resource_suffix_equals(resource_name: &str, host_name: &str) -> bool {
    classify_fsharp_resource(resource_name).is_some_and(|(_, suffix)| suffix == host_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deflate(bytes: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::DeflateEncoder};
        use std::io::Write;

        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn capped_deflate_allows_output_at_the_limit() {
        let compressed = deflate(&[0u8; 32]);
        let out = decompress_deflate_limited(&compressed, 32).unwrap();
        assert_eq!(out, [0u8; 32]);
    }

    #[test]
    fn capped_deflate_rejects_output_past_the_limit() {
        let compressed = deflate(&[0u8; 32]);
        let err = decompress_deflate_limited(&compressed, 31).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn exact_deflate_rejects_declared_size_past_the_global_limit() {
        let compressed = deflate(&[]);
        let err = decompress_deflate_exact(&compressed, MAX_DEFLATE_OUTPUT_BYTES + 1).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn exact_deflate_rejects_stream_longer_than_declared_size() {
        let compressed = deflate(&[0u8; 32]);
        let err = decompress_deflate_exact(&compressed, 31).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
