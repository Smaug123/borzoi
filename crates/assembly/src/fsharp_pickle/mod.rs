//! F# pickle-stream unpickler.
//!
//! F# signature data (the body of a `FSharpSignatureData.<asm>` managed
//! resource, after the gzip/deflate framing stripped by phase 5) is laid
//! out as **two streams in one buffer**:
//!
//! - **phase 2** — metadata tables: a list of CCU refs, the four count
//!   fields (`ntycons`/`ntypars`/`nvals`/`nanoninfos`) that pre-allocate
//!   stamp slots in phase 1, and five interning tables (strings,
//!   pubpaths, nlerefs, simpletys, and the phase-1 byte blob itself).
//! - **phase 1** — entity / typar / val bodies. Cross-references inside
//!   phase 1 are encoded as compressed-int indices into the stamp tables
//!   from phase 2.
//!
//! Phase 6b4 walks the phase-1 body inline: `unpickle_signature` decodes
//! the phase-2 header, attaches its interning tables to a fresh phase-1
//! reader sourced from `PickledHeader.phase1_bytes`, and then runs
//! `walk_ccu_info` over the entity / val / typar OSGN tables. The
//! result is a fully-populated `PickledCcu` with no unresolved stubs.
//!
//! Reference: `dotnet/fsharp/src/Compiler/TypedTree/TypedTreePickle.fs:929`
//! (`pickleObjWithDanglingCcus`) and `:1037` (`unpickleObjWithDanglingCcus`).

#[allow(dead_code)]
mod access;
#[allow(dead_code)]
mod attribs;
#[allow(dead_code)]
mod constraints;
#[allow(dead_code)]
mod consts;
#[allow(dead_code)]
mod entity;
#[allow(dead_code)]
mod expr;
mod header;
#[allow(dead_code)]
mod il;
#[allow(dead_code)]
mod leaves;
#[allow(dead_code)]
mod measure;
pub mod model;
#[allow(dead_code)]
mod osgn;
mod reader;
#[allow(dead_code)]
mod repr;
#[allow(dead_code)]
mod typar;
#[allow(dead_code)]
mod types;
#[allow(dead_code)]
mod val;
#[allow(dead_code)]
mod vrefs;

pub use model::{CcuRef, PickledCcu, PickledHeader, PickledNleRef};

use crate::error::ImportError;
use osgn::PhaseOneState;
use reader::PickleReader;

/// Stack reservation for the dedicated phase-1 walk thread.
///
/// The walk's recursion depth is bounded at 1024 levels
/// (`reader::MAX_RECURSION_DEPTH`), but the per-level native-stack cost
/// is decided by the compiler, not by us — an unoptimised
/// `read_entity_spec` level measures upward of 16 KiB, so even 128
/// levels of the heaviest chain can exceed a default 2 MiB thread stack
/// despite the depth bound. Rather than inherit whatever stack the
/// caller's thread happens to have, the walk runs on its own thread
/// with this reservation, making "depth bound × worst frame fits the
/// stack" a guarantee this crate owns. The value is sized empirically
/// (1024 entity levels measure under 32 MiB unoptimised):
/// `tests/all/fsharp_pickle_fail_loud.rs` drives the heaviest chain to
/// exactly the depth bound, so CI re-validates the envelope on every
/// toolchain bump. Reservation is virtual — pages commit only when
/// touched — so the size costs nothing on the (shallow) real-assembly
/// path.
const PICKLE_WALK_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Unpickle an F# signature-data resource payload.
///
/// `primary` is the inflated bytes from the matching `FSharpSignatureData`
/// / `FSharpSignatureCompressedData` resource (phase 5 has already
/// stripped any deflate framing). `stream_b` is the sibling
/// `FSharpSignatureDataB` / `FSharpSignatureCompressedDataB` payload when
/// present — F# ≥ 9 splits nullness and some typar-constraint annotations
/// across that second stream, and FCS feeds it to
/// `unpickleObjWithDanglingCcus` as `phase1bytesB`. Pass `None` for older
/// or B-less assemblies; the unpickler then behaves as if the B stream
/// were empty (matching `u_byteB`'s "implicitly 0 if not present"
/// fallback at `TypedTreePickle.fs:370-371`).
///
/// Phase 6b4 returns a fully-walked `PickledCcu`: the phase-2 header is
/// decoded, the phase-1 body is then walked depth-first against three
/// OSGN tables (`tycons` / `typars` / `vals`), and every reserved stamp
/// slot must end up linked. The caller receives dense
/// `PickledOsgnTables` plus the root entity index, the CCU's mangled
/// name, and the `usesQuotations` flag.
pub fn unpickle_signature(
    primary: &[u8],
    stream_b: Option<&[u8]>,
) -> Result<PickledCcu, ImportError> {
    // The walk itself is pure; the thread exists only to pin the stack
    // envelope (see `PICKLE_WALK_STACK_BYTES`). A scoped thread lets the
    // closure borrow `primary` / `stream_b` directly. A spawn refusal
    // (thread / address-space exhaustion) is an environmental failure
    // surfaced as a loud per-assembly error — not a panic — so the
    // caller degrades this one assembly rather than the process. A
    // panic on the walk thread is re-raised on the caller's thread
    // unchanged.
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("fsharp-pickle-walk".to_string())
            .stack_size(PICKLE_WALK_STACK_BYTES)
            .spawn_scoped(scope, || unpickle_signature_inner(primary, stream_b))
            .map_err(|e| ImportError::PickleWalkThreadSpawnFailed {
                detail: e.to_string(),
            })?;
        match handle.join() {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

fn unpickle_signature_inner(
    primary: &[u8],
    stream_b: Option<&[u8]>,
) -> Result<PickledCcu, ImportError> {
    // Phase 2 (header) — consume from the primary stream up to the end
    // of the metadata block. The phase-1 bytes are a length-prefixed
    // blob *inside* the header, so we re-wrap them as a fresh reader
    // below.
    let mut header_reader = PickleReader::new_dual(primary, stream_b);
    let header = header::read_header(&mut header_reader)?;
    header_reader.expect_eof("phase 2: trailing bytes after header")?;

    // Phase 1 (body) — run the walker over the inner byte stream. The
    // B-stream survives the phase-2 boundary verbatim: FCS shares the
    // same `phase1bytesB` across both phases.
    //
    // The walker is scoped so that `state` (which borrows
    // `header.strings` / `header.pubpaths` via the reader) drops
    // before we assemble the final `PickledCcu` — otherwise the
    // borrow checker rejects moving `header` into the struct.
    let phase1_bytes = header.phase1_bytes.clone();
    let result = {
        let mut phase1_reader = PickleReader::new_dual(&phase1_bytes, stream_b);
        phase1_reader.attach_tables(&header.strings, &header.pubpaths);
        let state = PhaseOneState::new(phase1_reader, &header)?;
        entity::walk_ccu_info(state)?
    };
    Ok(model::PickledCcu {
        header,
        root_entity: result.root_entity,
        compile_time_working_dir: result.compile_time_working_dir,
        uses_quotations: result.uses_quotations,
        tables: result.tables,
    })
}
