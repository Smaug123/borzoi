//! Fail-loud robustness harness for the F# pickle unpickler.
//!
//! `tests/all/fail_loud.rs` fuzzes whole DLLs, but the pickle decoder sits
//! behind resource extraction there, so mutation fuzzing almost never
//! reaches deep phase-1 structure. This file drives
//! [`unpickle_signature`] directly with adversarial payloads.
//!
//! The contract mirrors the ECMA-side harness: for *any* input bytes,
//! `unpickle_signature` must return `Ok` or a loud, well-formed
//! [`ImportError`]. A panic, a process abort (stack overflow), a hang,
//! or an unbounded allocation are all forbidden — the LSP feeds this
//! decoder bytes from arbitrary third-party DLLs.
//!
//! The deterministic regressions pin the stack-overflow class
//! specifically: the recursive phase-1 decoders (`u_ty`,
//! `u_measure_expr`, `u_expr`, `u_ILType`, `u_entity_spec`) can be
//! driven one stack frame per *byte* (e.g. `u_ty` tag 3 `TType_fun`
//! recurses into its domain after consuming a single byte), so before
//! the depth bound existed a few megabytes of tag bytes aborted the
//! process.

use proptest::prelude::*;

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, FSharpResource, ImportError, ResourceKind, unpickle_signature,
};

use crate::common::ensure_deep_curry_built;

// ============================================================================
// Wire-format encoders (mirror `PickleReader`'s compressed-int format,
// `TypedTreePickle.fs:240-248`).
// ============================================================================

/// Encode one compressed u32: `0x00..=0x7F` literal, `0x80..=0xBF`
/// two-byte, else `0xFF` + 4 LE bytes.
fn enc_u32(v: u32) -> Vec<u8> {
    if v <= 0x7F {
        vec![v as u8]
    } else if v <= 0x3FFF {
        vec![0x80 | (v >> 8) as u8, (v & 0xFF) as u8]
    } else {
        let mut out = vec![0xFF];
        out.extend_from_slice(&v.to_le_bytes());
        out
    }
}

/// Encode `u_prim_string`: compressed-int length + UTF-8 bytes.
fn enc_string(s: &str) -> Vec<u8> {
    let mut out = enc_u32(s.len() as u32);
    out.extend_from_slice(s.as_bytes());
    out
}

/// Wrap a phase-1 body in a minimal valid phase-2 header: no ccu-refs,
/// `ntycons` entity stamps, no typars/vals, strings `["M", ""]` (the
/// entity name and the dropped-string slot), empty pubpaths / nlerefs /
/// simpletys.
fn wrap_in_header(ntycons: u32, phase1: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(phase1.len() + 32);
    bytes.push(0); // ccu_refs: length 0
    bytes.extend(enc_u32(ntycons)); // z1 >= 0 → ntycons, no anon table
    bytes.push(0); // ntypars
    bytes.push(0); // nvals
    bytes.push(2); // strings: length 2
    bytes.extend(enc_string("M"));
    bytes.extend(enc_string(""));
    bytes.push(0); // pubpaths: length 0
    bytes.push(0); // nlerefs: length 0
    bytes.push(0); // simpletys: length 0
    bytes.extend(enc_u32(phase1.len() as u32)); // phase1_bytes blob
    bytes.extend_from_slice(phase1);
    bytes
}

/// The `u_entity_spec` wire prefix for the root entity, from the osgn
/// index up to (and including) the `Some` tag of the `TypeAbbrev`
/// option — i.e. positioned so the next byte is consumed by `u_ty`.
/// Field order per `u_entity_spec_data` (`TypedTreePickle.fs:3128`).
fn entity_prefix_to_type_abbrev() -> Vec<u8> {
    vec![
        0, // u_entity_spec osgn index 0
        0, // typars: length 0
        0, // logical_name: string index 0 = "M"
        0, // compiled_name: None
        1, 1, 0, 1, 1, // range: file 1, (1,0)-(1,1)
        0, // pub_path: None
        0, // access
        0, // repr_access
        0, // attribs: length 0
        0, // u_tycon_repr: outer tag 0 = NoRepr
        1, // type_abbrev: Some — next bytes are a u_ty
    ]
}

/// Assert the fail-loud contract on one payload: a `Result` comes back
/// (rather than a panic/abort, which would kill the process before the
/// assertion), and any `Err` formats to a non-empty diagnostic.
fn assert_fails_loud(primary: &[u8], stream_b: Option<&[u8]>) -> Result<(), ImportError> {
    match unpickle_signature(primary, stream_b) {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = format!("{e}");
            assert!(!msg.is_empty(), "ImportError displayed as empty string");
            Err(e)
        }
    }
}

// ============================================================================
// Deterministic regressions: one-byte-per-stack-frame chains. Before the
// depth bound these aborted the process with a stack overflow.
// ============================================================================

/// `u_ty` tag 3 (`TType_fun`) recurses into its domain after one byte,
/// so N bytes of `3` drive N stack frames.
#[test]
fn deep_fun_type_chain_fails_loud_rather_than_overflowing_the_stack() {
    let mut phase1 = entity_prefix_to_type_abbrev();
    phase1.extend(std::iter::repeat_n(3u8, 2_000_000));
    let payload = wrap_in_header(1, &phase1);
    match assert_fails_loud(&payload, None) {
        Err(ImportError::PickleRecursionLimitExceeded { .. }) => {}
        other => panic!("expected PickleRecursionLimitExceeded, got {other:?}"),
    }
}

/// `u_measure_expr` tag 1 (`Measure.Inv`) is the measure-side one-byte
/// recursion; reached via `u_ty` tag 6 (`TType_measure`).
#[test]
fn deep_measure_chain_fails_loud_rather_than_overflowing_the_stack() {
    let mut phase1 = entity_prefix_to_type_abbrev();
    phase1.push(6); // u_ty tag 6 = TType_measure
    phase1.extend(std::iter::repeat_n(1u8, 2_000_000));
    let payload = wrap_in_header(1, &phase1);
    match assert_fails_loud(&payload, None) {
        Err(ImportError::PickleRecursionLimitExceeded { .. }) => {}
        other => panic!("expected PickleRecursionLimitExceeded, got {other:?}"),
    }
}

/// Nested `u_entity_spec`s are the most stack-expensive guarded cycle
/// (each level runs the whole 17-field entity decode plus the lazy
/// modul-typ frame). This pins two things at once: the guard covers the
/// entity cycle, and the bound is small enough that reaching it on the
/// *heaviest* chain still fits the native stack.
#[test]
fn deep_module_nesting_fails_loud_rather_than_overflowing_the_stack() {
    const DEPTH: u32 = 5_000;

    // Build innermost-first: entity(d) embeds entity(d+1) as the sole
    // element of its modul-typ entity list.
    let mut child: Vec<u8> = Vec::new(); // innermost: no nested entity
    for d in (0..DEPTH).rev() {
        let mut body = vec![
            0, // u_istype: ModuleWithSuffix (tag 0)
            0, // vals: length 0
        ];
        if child.is_empty() {
            body.push(0); // entities: length 0
        } else {
            body.push(1); // entities: length 1
            body.extend_from_slice(&child);
        }

        let mut entity = enc_u32(d); // u_entity_spec osgn index
        entity.extend([
            0, // typars: length 0
            0, // logical_name: string index 0 = "M"
            0, // compiled_name: None
            1, 1, 0, 1, 1, // range
            0, // pub_path: None
            0, // access
            0, // repr_access
            0, // attribs: length 0
            0, // u_tycon_repr: NoRepr
            0, // type_abbrev: None
            0, 0, 0, 0, 0, 0, 0, 0, 0, // tcaug: 8 empty fields + u_space 1
            1, // dropped string: index 1 = ""
            0, // typar_kind: Type
            0, 0, // flags: 0i64 (lo, hi)
            0, // cpath: None
        ]);
        // u_lazy frame: body length + six discarded fixup words.
        entity.extend_from_slice(&(body.len() as u32).to_le_bytes());
        for _ in 0..6 {
            entity.extend_from_slice(&0u32.to_le_bytes());
        }
        entity.extend_from_slice(&body);
        entity.extend([
            3, // exn_repr: None (tag 3)
            0, // xmldoc: absent
        ]);
        child = entity;
    }

    let payload = wrap_in_header(DEPTH, &child);
    match assert_fails_loud(&payload, None) {
        Err(ImportError::PickleRecursionLimitExceeded { .. }) => {}
        other => panic!("expected PickleRecursionLimitExceeded, got {other:?}"),
    }
}

/// A chain *under* the bound must not trip the recursion error: it
/// unwinds normally and fails on genuine end-of-stream instead. Pins
/// the bound's direction (deep-but-legal input stays decodable). 1000
/// is ~5× the deepest valid-compiler-output walk we can provoke (the
/// DeepCurry fixture's 200-parameter curried chain) while staying
/// under the bound of 1024.
#[test]
fn shallow_fun_type_chain_is_not_rejected_by_the_depth_bound() {
    let mut phase1 = entity_prefix_to_type_abbrev();
    phase1.extend(std::iter::repeat_n(3u8, 1_000));
    let payload = wrap_in_header(1, &phase1);
    match assert_fails_loud(&payload, None) {
        Err(ImportError::PickleRecursionLimitExceeded { .. }) => {
            panic!("depth bound tripped on a 1000-deep chain — bound is too tight")
        }
        Err(_) => {} // end-of-stream (or similar) after clean unwind
        Ok(_) => panic!("truncated payload cannot decode successfully"),
    }
}

// ============================================================================
// The bound from below: real compiler output must decode.
// ============================================================================

/// A 200-parameter curried function is valid F# whose signature is a
/// 200-deep `TType_fun` chain, so the recursion bound must sit far
/// above what fsc emits for machine-generated curried code — not just
/// above FSharp.Core's observed 19. Rejecting one such signature would
/// (per the merge policy) drop the source-name / extension / measure
/// overlays for the entire assembly.
#[test]
fn real_deeply_curried_signature_unpickles() {
    let dll_bytes = std::fs::read(ensure_deep_curry_built()).expect("read DeepCurry.dll");
    let view = Ecma335Assembly::parse(&dll_bytes).expect("parse DeepCurry.dll");
    let resources = view.fsharp_resources().expect("enumerate F# resources");
    let primary = resources
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureData | ResourceKind::SignatureCompressedData
            )
        })
        .expect("DeepCurry must carry a primary signature resource");
    let stream_b = resources
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureDataB | ResourceKind::SignatureCompressedDataB
            )
        })
        .map(|r: &FSharpResource| r.payload.as_slice());

    let ccu = unpickle_signature(&primary.payload, stream_b)
        .expect("a 200-parameter curried signature is valid compiler output and must decode");
    assert!(
        ccu.header.strings.iter().any(|s| s == "deeplyCurried"),
        "strings table must contain the curried function's name"
    );
}

// ============================================================================
// Property: `unpickle_signature` is total — Ok or loud Err, never a panic,
// abort, hang, or unbounded allocation.
// ============================================================================

proptest! {
    // `failure_persistence: None` matches `tests/all/fail_loud.rs`: an
    // integration-test binary has no lib.rs anchor for the regression
    // directory, and a counterexample here is a root-cause bug to fix,
    // not a seed to replay. Proptest still shrinks and reports it.
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Arbitrary bytes for both streams. Mostly exercises the phase-2
    /// header decode (random bytes rarely form a valid header), so the
    /// structured properties below cover phase 1.
    #[test]
    fn unpickle_arbitrary_bytes_never_panics(
        primary in proptest::collection::vec(any::<u8>(), 0..4096),
        stream_b in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..512)),
    ) {
        let _ = assert_fails_loud(&primary, stream_b.as_deref());
    }

    /// A valid header whose phase-1 body enters `u_ty` and then feeds a
    /// long run of one tag byte. Runs of a self-recursive tag (`u_ty`
    /// tag 3, or tag 6 + measure tag 1) are exactly the
    /// one-frame-per-byte shape that overflowed the stack before the
    /// depth bound; other tags exercise the loud-refusal arms.
    #[test]
    fn unpickle_tag_runs_in_phase1_never_panic(
        tag in 0u8..=32,
        run_len in 1usize..200_000,
    ) {
        let mut phase1 = entity_prefix_to_type_abbrev();
        phase1.extend(std::iter::repeat_n(tag, run_len));
        let payload = wrap_in_header(1, &phase1);
        let _ = assert_fails_loud(&payload, None);
    }

    /// Same entry point, arbitrary (non-run) phase-1 tails, with and
    /// without a B stream: random walks through the phase-1 decoders.
    #[test]
    fn unpickle_arbitrary_phase1_tails_never_panic(
        tail in proptest::collection::vec(any::<u8>(), 0..2048),
        stream_b in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..256)),
    ) {
        let mut phase1 = entity_prefix_to_type_abbrev();
        phase1.extend_from_slice(&tail);
        let payload = wrap_in_header(1, &phase1);
        let _ = assert_fails_loud(&payload, stream_b.as_deref());
    }
}
