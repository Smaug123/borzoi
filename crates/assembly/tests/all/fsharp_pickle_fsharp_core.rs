//! Always-on regression test: the F# signature-data unpickler must decode
//! the *real, shipped* `FSharp.Core.dll` end-to-end.
//!
//! The corpus-gated tests in `fsharp_pickle_diff.rs`
//! (`header_decodes_fsharp_core` / `full_walk_decodes_fsharp_core`) only run
//! when `BORZOI_CORPUS` points at a *built* FSharp.Core — but the pinned
//! `fsharp-src` flake input is source-only, so they silently skip and the
//! genuine FSharp.Core decode path went untested. This test uses the
//! always-present copy that `tools/fcs-dump`'s build drops next to
//! `fcs-dump.dll` (`common::ensure_fsharp_core_dll`), so the decode is pinned
//! in every lane.
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once); the Nix
//! devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, FSharpResource, ResourceKind, unpickle_signature,
};

use crate::common::ensure_fsharp_core_dll;

fn primary(rs: &[FSharpResource]) -> &FSharpResource {
    rs.iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureData
                    | ResourceKind::SignatureCompressedData
                    | ResourceKind::SignatureDataFSharpCore
            )
        })
        .expect("no primary signature resource on FSharp.Core")
}

fn b_stream(rs: &[FSharpResource]) -> Option<&[u8]> {
    rs.iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureDataB | ResourceKind::SignatureCompressedDataB
            )
        })
        .map(|r| r.payload.as_slice())
}

/// A full phase-1 walk of the real, shipped FSharp.Core must succeed.
///
/// This is the integration oracle for the unpickler: it only stays green
/// when every wire shape FSharp.Core's signature data reaches decodes. It
/// guards the two blockers cleared so far — the osgn idempotent re-link in
/// `IResumableStateMachine`'s member signatures, and the full expression-tree
/// decode for attribute arguments (`[<AttributeUsage(… ||| …)>]` pickles an
/// inline `App(Lambda(…, ILAsm …))`). Any unported shape would fail loudly
/// here (D6.5), pinpointing itself, rather than silently corrupting the walk.
#[test]
fn unpickles_real_fsharp_core_end_to_end() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse FSharp.Core");
    let resources = view
        .fsharp_resources()
        .expect("fsharp_resources on FSharp.Core");
    let sig = primary(&resources);
    let stream_b = b_stream(&resources);

    let ccu = unpickle_signature(&sig.payload, stream_b)
        .expect("unpickle real FSharp.Core signature end-to-end");

    // Sanity: a real FSharp.Core has hundreds of tycons and vals, and every
    // reserved OSGN slot must be densely populated (the walker hard-errors on
    // an unlinked slot in `finalize`, so reaching here already proves that —
    // these assertions guard against a decode that silently shrank the tables).
    assert!(
        ccu.tables.tycons.len() > 50,
        "expected >50 tycons; got {}",
        ccu.tables.tycons.len()
    );
    assert!(
        ccu.tables.vals.len() > 200,
        "expected >200 vals; got {}",
        ccu.tables.vals.len()
    );
    assert_eq!(ccu.tables.tycons.len() as u32, ccu.header.ntycons);
    assert_eq!(ccu.tables.typars.len() as u32, ccu.header.ntypars);
    assert_eq!(ccu.tables.vals.len() as u32, ccu.header.nvals);
}
