//! Phase 7 — fail-loud robustness harness.
//!
//! The importer fails *loud*: it never silently substitutes a fallback for
//! content it doesn't understand. This
//! file pins that as an executable invariant against adversarial input —
//! single-byte mutations of a valid DLL, truncated prefixes, and a curated
//! corpus of real assemblies.
//!
//! ## The fail-loud totality contract
//!
//! [`Ecma335Assembly::parse`] is deliberately *shallow*: it parses the
//! PE/metadata image and projects only the manifest identity, deferring all
//! signature/type/constraint decoding to the [`EcmaView`] walk accessors. That
//! keeps opening an assembly cheap — we never force a full metadata projection
//! of, say, `System.Private.CoreLib` just to read its identity.
//!
//! So for *any* input bytes, `parse` *followed by walking the view* must reach
//! exactly one of:
//!
//! 1. `Ok(view)` whose walk also succeeds — the view is *self-consistent*:
//!    every [`EcmaView`] accessor returns `Ok` and the whole entity tree is
//!    walkable without panicking.
//! 2. A loud, well-formed [`ImportError`]: `format!("{e}")` neither panics nor
//!    is empty (the diagnostic the LSP surfaces). It may surface from `parse`
//!    itself *or*, because decoding is lazy, from a walk accessor
//!    (`enumerate_type_defs` / `fsharp_resources`). A loud `Err` at walk time
//!    is the *same* fail-loud outcome as one from `parse`, merely deferred —
//!    not a contract violation.
//! 3. A panic *during parse* — an **acceptable loud failure**. The in-crate
//!    ECMA-335 reader bailing on corrupt bytes is a crash, not silent
//!    corruption (gospel P5: a clear crash beats a quiet wrong answer). We
//!    deliberately do **not** wrap `parse` in `catch_unwind` for production;
//!    this harness catches the panic only to keep the test process alive and
//!    to record the offending byte.
//!
//! The contract *forbids*: a hang, a process abort, or outcome (1) followed by
//! a **panic** while consuming the view. A walk accessor that *panics* — as
//! opposed to returning a loud `Err` — is deferred corruption that only
//! detonates downstream, and stays forbidden: phase 2 of [`probe`] is left
//! un-`catch_unwind`-wrapped so such a panic is a hard test failure.

use std::path::PathBuf;
use std::sync::OnceLock;

use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

use proptest::prelude::*;

use borzoi_assembly::pdb::embedded_portable_pdb;
use borzoi_assembly::pdb::{PdbError, PortablePdb, codeview_pdb_reference};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, ImportError, Member};

use crate::common::{
    ensure_minilib_built, ensure_minilib_fs_built, ensure_minilib_fs_ext_built,
    ensure_minilib_pdb_built,
};

/// Outcome class of a single [`probe`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Parsed and fully walked without incident.
    Ok,
    /// A loud, well-formed `ImportError` surfaced — from `parse` or, because
    /// decoding is lazy, from a walk accessor. Either is a fail-loud outcome.
    Err,
    /// Parse panicked — acceptable loud failure (recorded, not a test
    /// failure, for the mutation/truncation cases).
    Panicked,
}

/// Catch a panic from the parser *quietly*: no backtrace, just the compact
/// one-liner that the `--nocapture` workflow reads.
///
/// This harness *expects* the parser to panic on some corrupt inputs (D5
/// tolerates parse panics), and the exhaustive sweep alone trips dozens of them;
/// a full backtrace per panic would bury the `Ok / Err / Panicked` signal.
///
/// It used to get that by installing a custom process-global panic hook. That
/// was containable when this file was its own test binary, but it now shares one
/// with every other `assembly` case group — and a hook that never restores would
/// strip the backtrace from *their* failures and misreport them as `[fail_loud]`
/// panics. So silence per-thread, for the duration of this call only, and print
/// the captured panic ourselves.
///
/// Note the silence ends with the call: a panic *outside* one of these — notably
/// the phase-2 walk, which is deliberately not caught — still gets libtest's full
/// treatment. That is the genuine-bug signal this harness exists to surface.
fn catch_quiet<T>(f: impl FnOnce() -> T) -> std::thread::Result<T> {
    let result = catch_unwind_silent(f);
    if result.is_err()
        && let Some(p) = take_silenced_panic()
    {
        eprintln!("[fail_loud] caught panic at {}: {}", p.location, p.message);
    }
    result
}

/// [`catch_quiet`] must actually *consume* the captured panic to report it —
/// otherwise it would leave a stale one behind for the next caller to
/// misattribute, and print nothing itself.
///
/// The probes only reach this path when the parser genuinely panics, which the
/// non-`ignore`d corpus may not do on any given day; a direct case keeps the
/// reporting wired up regardless.
#[test]
fn catch_quiet_reports_and_consumes_the_panic() {
    let caught = catch_quiet(|| panic!("corrupt input"));
    assert!(
        caught.is_err(),
        "the panic should be caught, not propagated"
    );
    assert!(
        take_silenced_panic().is_none(),
        "catch_quiet should have taken the captured panic to print it; leaving it \
         would let the next caller report this one as its own"
    );
}

/// Recursively walk the entire entity tree, forcing every `Vec`/`Box` to be
/// visited. Returns the total node count (entities + members). Its only job
/// is to prove an `Ok` view is fully reachable; the traversal terminating
/// without panicking *is* the assertion.
fn structural_self_check(entities: &[Entity]) -> usize {
    fn walk(e: &Entity, acc: &mut usize) {
        *acc += 1 + e.members.len();
        for m in &e.members {
            // Touch each discriminant so the match can't be elided.
            match m {
                Member::Method(_) | Member::Field(_) | Member::Property(_) | Member::Event(_) => {}
            }
        }
        for n in &e.nested_types {
            walk(n, acc);
        }
    }
    let mut acc = 0;
    for e in entities {
        walk(e, &mut acc);
    }
    acc
}

/// A loud failure must carry a non-empty, panic-free diagnostic — `Display` is
/// *our* code (`error.rs`), so a panic or an empty render is itself a genuine
/// bug, asserted here rather than tolerated. Returns [`Outcome::Err`].
fn loud_err(e: &ImportError) -> Outcome {
    let rendered = format!("{e}");
    assert!(!rendered.is_empty(), "ImportError Display rendered empty");
    Outcome::Err
}

/// [`loud_err`], for the PDB reader's own error type.
fn loud_pdb_err(e: &PdbError) -> Outcome {
    let rendered = format!("{e}");
    assert!(!rendered.is_empty(), "PdbError Display rendered empty");
    Outcome::Err
}

/// Parse `bytes` and fully walk the resulting view, classifying the outcome
/// per the fail-loud totality contract. Never itself panics for the
/// *parse-phase* — a parser panic is caught and returned as
/// [`Outcome::Panicked`].
///
/// A panic while walking a *successfully parsed* view is **not** caught: that
/// is deferred corruption (forbidden by the contract) and must surface as a
/// hard test failure rather than be silently tolerated.
fn probe(bytes: &[u8]) -> Outcome {
    // Phase 1 — parse. A panic here is an acceptable loud failure; a returned
    // `Err` is a loud failure.
    let parsed = catch_quiet(|| Ecma335Assembly::parse(bytes));
    let view = match parsed {
        Err(_panic) => return Outcome::Panicked,
        Ok(Err(e)) => return loud_err(&e),
        Ok(Ok(view)) => view,
    };

    // Phase 2 — walk the successfully parsed view *without* catching panics.
    // parse is shallow (it decodes only the manifest identity), so the walk
    // accessors do the real signature/type decoding and may legitimately
    // return a loud `Err` — the same fail-loud outcome as an `Err` from
    // `parse`, merely deferred, so we classify it as `Outcome::Err`.
    //
    // What stays forbidden is a *panic* while walking: that is deferred
    // corruption that detonates downstream, not a loud failure. This phase is
    // intentionally not wrapped in `catch_unwind`, so such a panic propagates
    // to a hard test failure — the genuine-bug signal this harness exists to
    // catch.
    let _ = view.identity();
    let _ = view.assembly_refs();
    let entities = match view.enumerate_type_defs() {
        Ok(entities) => entities,
        Err(e) => return loud_err(&e),
    };
    let _ = structural_self_check(&entities);
    if let Err(e) = view.fsharp_resources() {
        return loud_err(&e);
    }

    // Phase 3 — the PDB surface. The LSP feeds these the same untrusted
    // bytes (go-to-source reads real-world DLLs), so they are under the same
    // totality contract. Extraction (`embedded_portable_pdb`, the debug
    // directory walk) is parse-phase: a panic is an acceptable loud failure,
    // caught here like phase 1's. A successfully *extracted* blob is then
    // walked via [`probe_pdb_blob`], whose accessor phase stays un-wrapped.
    let extracted = catch_quiet(|| embedded_portable_pdb(bytes));
    match extracted {
        Err(_panic) => return Outcome::Panicked,
        Ok(Err(e)) => return loud_pdb_err(&e),
        Ok(Ok(None)) => {}
        Ok(Ok(Some(pdb_blob))) => match probe_pdb_blob(&pdb_blob) {
            Outcome::Ok => {}
            other => return other,
        },
    }
    match catch_quiet(|| codeview_pdb_reference(bytes)) {
        Err(_panic) => return Outcome::Panicked,
        Ok(Err(e)) => return loud_pdb_err(&e),
        Ok(Ok(reference)) => {
            // `file_name` is pure slicing over the already-validated path;
            // touching it proves the reference is consumable.
            let _ = reference.as_ref().and_then(|r| r.file_name());
        }
    }
    Outcome::Ok
}

/// [`probe`]'s analogue for a portable-PDB blob — the byte stream
/// `PortablePdb::read` consumes, which the LSP also reads from *external*
/// `.pdb` files on disk (`goto_source`), so arbitrary bytes reach it without
/// passing through any PE container validation first.
///
/// Same contract split as [`probe`]: `read` is parse-phase (a panic is an
/// acceptable loud failure, caught and classified); the accessor walk over a
/// successful read is *not* wrapped — an accessor panic is deferred
/// corruption and must surface as a hard test failure. Accessor `Err`s are
/// loud failures and fine.
fn probe_pdb_blob(blob: &[u8]) -> Outcome {
    let pdb = match catch_quiet(|| PortablePdb::read(blob)) {
        Err(_panic) => return Outcome::Panicked,
        Ok(Err(e)) => return loud_pdb_err(&e),
        Ok(Ok(pdb)) => pdb,
    };

    // Walk every accessor over every row — un-wrapped, so a panic fails the
    // test. A returned `Err` is a loud (deferred) failure; note it but keep
    // walking, so one bad row cannot shadow a panic in a later one.
    let mut saw_err = false;
    let _ = pdb.id();
    for rid in 1..=pdb.document_count() {
        match pdb.document_name(rid) {
            Ok(_) => {}
            Err(e) => {
                loud_pdb_err(&e);
                saw_err = true;
            }
        }
        match pdb.document_embedded_source(rid) {
            Ok(_) => {}
            Err(e) => {
                loud_pdb_err(&e);
                saw_err = true;
            }
        }
    }
    for rid in 1..=pdb.method_debug_info_count() {
        match pdb.method_first_sequence_point(rid) {
            Ok(_) => {}
            Err(e) => {
                loud_pdb_err(&e);
                saw_err = true;
            }
        }
    }
    match pdb.sourcelink_json() {
        Ok(_) => {}
        Err(e) => {
            loud_pdb_err(&e);
            saw_err = true;
        }
    }
    if saw_err { Outcome::Err } else { Outcome::Ok }
}

// ============================================================================
// Base-DLL byte caches (built once per test binary via the common helpers).
// ============================================================================

fn minilib_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| std::fs::read(ensure_minilib_built()).expect("read MiniLib.dll"))
}

fn minilib_fs_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| std::fs::read(ensure_minilib_fs_built()).expect("read MiniLibFs.dll"))
}

fn minilib_pdb_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| std::fs::read(ensure_minilib_pdb_built()).expect("read MiniLibPdb.dll"))
}

/// The *extracted, inflated* portable-PDB blob of the MiniLibPdb fixture —
/// the byte stream `PortablePdb::read` consumes, and the shape of an external
/// `.pdb` file on disk. Mutating this (rather than the whole DLL) aims every
/// case at the PDB metadata engine instead of spending most of them on the PE
/// container.
fn minilib_pdb_blob() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        embedded_portable_pdb(minilib_pdb_bytes())
            .expect("extract MiniLibPdb embedded PDB")
            .expect("MiniLibPdb must embed a portable PDB (DebugType=embedded)")
    })
}

// ============================================================================
// Single-byte mutation + truncation property tests.
//
// Panics during parse are *tolerated* here (logged, not failed). The hard
// assertions live inside `probe`: a well-formed `Err`, and no *panic* while
// walking a successful parse (a deferred loud `Err` is fine — decoding is
// lazy).
// ============================================================================

proptest! {
    // `failure_persistence: None`: these live in an integration-test binary
    // (no `lib.rs`/`main.rs` for proptest's default `SourceParallel` anchor),
    // so on-disk regression seeds aren't available. That's fine here — a
    // counterexample is a genuine robustness bug to fix at the root (the
    // in-crate ECMA-335 reader), and proptest still shrinks and reports
    // the minimal failing input in the failure message. Setting `None`
    // explicitly also silences the misleading "failed to find lib.rs" warning.
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Flip one byte of MiniLib.dll (C#, tiny) and assert the parser never
    /// fails *quietly*. XOR with a non-zero mask guarantees the byte changes
    /// and shrinks cleanly (offset → 0, mask → 1).
    #[test]
    fn single_byte_flip_minilib_is_total(
        (offset, mask) in (0..minilib_bytes().len(), 1u8..=u8::MAX),
    ) {
        let mut mutated = minilib_bytes().to_vec();
        mutated[offset] ^= mask;
        if probe(&mutated) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] MiniLib: parse panicked on byte {offset} xor {mask:#04x} \
                 — acceptable loud failure (D5)"
            );
        }
    }

    /// Same, on MiniLibFs.dll (F#) — drives the unpickler through the F#
    /// signature/optimisation resource path under mutation.
    #[test]
    fn single_byte_flip_minilib_fs_is_total(
        (offset, mask) in (0..minilib_fs_bytes().len(), 1u8..=u8::MAX),
    ) {
        let mut mutated = minilib_fs_bytes().to_vec();
        mutated[offset] ^= mask;
        if probe(&mutated) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] MiniLibFs: parse panicked on byte {offset} xor {mask:#04x} \
                 — acceptable loud failure (D5)"
            );
        }
    }

    /// A truncated prefix is a malformation class single-byte flips don't
    /// reach (premature EOF). Exercises the PE/CLI-header and pickle
    /// `UnexpectedEndOfStream` paths.
    #[test]
    fn truncated_prefix_minilib_is_total(prefix_len in 0..=minilib_bytes().len()) {
        if probe(&minilib_bytes()[..prefix_len]) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] MiniLib: parse panicked on {prefix_len}-byte prefix \
                 — acceptable loud failure (D5)"
            );
        }
    }

    /// Truncated F# DLL — can sever the metadata mid-pickle-resource.
    #[test]
    fn truncated_prefix_minilib_fs_is_total(prefix_len in 0..=minilib_fs_bytes().len()) {
        if probe(&minilib_fs_bytes()[..prefix_len]) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] MiniLibFs: parse panicked on {prefix_len}-byte prefix \
                 — acceptable loud failure (D5)"
            );
        }
    }

    /// Flip one byte of MiniLibPdb.dll — the only fixture whose image
    /// carries an embedded portable PDB, so this drives the debug-directory
    /// walk, the embedded-PDB inflate, and the PDB metadata engine under
    /// mutation alongside the usual ECMA-335 surface.
    #[test]
    fn single_byte_flip_minilib_pdb_is_total(
        (offset, mask) in (0..minilib_pdb_bytes().len(), 1u8..=u8::MAX),
    ) {
        let mut mutated = minilib_pdb_bytes().to_vec();
        mutated[offset] ^= mask;
        if probe(&mutated) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] MiniLibPdb: parse panicked on byte {offset} xor {mask:#04x} \
                 — acceptable loud failure (D5)"
            );
        }
    }

    /// Flip one byte of the *extracted* portable-PDB blob and read it
    /// directly — the external-`.pdb`-file path (`goto_source` reads those
    /// straight from disk), which no DLL-level mutation reaches.
    #[test]
    fn single_byte_flip_pdb_blob_is_total(
        (offset, mask) in (0..minilib_pdb_blob().len(), 1u8..=u8::MAX),
    ) {
        let mut mutated = minilib_pdb_blob().to_vec();
        mutated[offset] ^= mask;
        if probe_pdb_blob(&mutated) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] PDB blob: read panicked on byte {offset} xor {mask:#04x} \
                 — acceptable loud failure (D5)"
            );
        }
    }

    /// A truncated PDB blob — premature EOF mid-table, the malformation
    /// class byte flips don't reach.
    #[test]
    fn truncated_prefix_pdb_blob_is_total(prefix_len in 0..=minilib_pdb_blob().len()) {
        if probe_pdb_blob(&minilib_pdb_blob()[..prefix_len]) == Outcome::Panicked {
            eprintln!(
                "[fail_loud] PDB blob: read panicked on {prefix_len}-byte prefix \
                 — acceptable loud failure (D5)"
            );
        }
    }
}

// ============================================================================
// Coverage floor + corpus sweep (plain #[test]s — deterministic).
// ============================================================================

/// Deterministic exhaustive offset sweep: flip *every* byte of MiniLib.dll
/// (xor `0xFF`) and confirm corruption is actually *detected loud* — i.e. at
/// least one mutation is rejected with an `Err`. Guards the vacuous green
/// where the parser would silently accept anything. Doubles as full
/// offset-coverage that the random mutation property only samples.
///
/// We don't require `Ok > 0` (a maximally fragile DLL could reject every
/// flip) nor say anything about panics (zero is the happy case); the floor is
/// "the importer says no, loudly, to corrupt input".
#[test]
fn corruption_is_detected_loud() {
    let base = minilib_bytes();
    let mut err = 0usize;
    let mut ok = 0usize;
    let mut panicked = 0usize;
    for offset in 0..base.len() {
        let mut mutated = base.to_vec();
        mutated[offset] ^= 0xFF;
        match probe(&mutated) {
            Outcome::Ok => ok += 1,
            Outcome::Err => err += 1,
            Outcome::Panicked => panicked += 1,
        }
    }
    eprintln!(
        "[fail_loud] MiniLib exhaustive 0xFF flip over {} bytes: Ok={ok} Err={err} Panicked={panicked}",
        base.len(),
    );
    assert!(
        err > 0,
        "no single-byte mutation of MiniLib.dll was rejected with an Err — \
         the importer appears to accept arbitrary corruption silently",
    );
}

/// Anti-vacuity floors for the PDB arm.
///
/// First: the *pristine* fixture must probe clean end to end — an embedded
/// PDB present, extracted, and fully walked with no error. Without this, the
/// mutation properties above could be green because the fixture never had a
/// PDB to corrupt (the vacuous pass the harness exists to prevent).
#[test]
fn pristine_pdb_fixture_probes_clean() {
    assert_eq!(
        probe(minilib_pdb_bytes()),
        Outcome::Ok,
        "the unmutated MiniLibPdb fixture must parse and walk cleanly, PDB included",
    );
    assert_eq!(
        probe_pdb_blob(minilib_pdb_blob()),
        Outcome::Ok,
        "the unmutated extracted PDB blob must read and walk cleanly",
    );
    // The walk is only meaningful if there is something to walk.
    let pdb = PortablePdb::read(minilib_pdb_blob()).expect("pristine blob reads");
    assert!(pdb.document_count() > 0, "fixture PDB carries documents");
    assert!(
        pdb.method_debug_info_count() > 0,
        "fixture PDB carries method debug info"
    );
}

/// Second: exhaustive 0xFF flip over every byte of the extracted PDB blob —
/// corruption of the PDB stream must be *detected loud* at least once, and
/// (per the property above, sampled; here, exhaustively) never panic the
/// accessor walk. Mirrors [`corruption_is_detected_loud`] for the ECMA side.
#[test]
fn pdb_corruption_is_detected_loud() {
    let base = minilib_pdb_blob();
    let mut err = 0usize;
    let mut ok = 0usize;
    let mut panicked = 0usize;
    for offset in 0..base.len() {
        let mut mutated = base.to_vec();
        mutated[offset] ^= 0xFF;
        match probe_pdb_blob(&mutated) {
            Outcome::Ok => ok += 1,
            Outcome::Err => err += 1,
            Outcome::Panicked => panicked += 1,
        }
    }
    eprintln!(
        "[fail_loud] PDB blob exhaustive 0xFF flip over {} bytes: Ok={ok} Err={err} Panicked={panicked}",
        base.len(),
    );
    assert!(
        err > 0,
        "no single-byte mutation of the PDB blob was rejected with an Err — \
         the PDB reader appears to accept arbitrary corruption silently",
    );
}

/// Curated corpus sweep over *unmutated* real assemblies. The three built
/// fixtures are always included (MiniLibFs carries F# pickle resources, so it
/// drives the unpickler). `BORZOI_ROBUSTNESS_CORPUS`, if set, adds every
/// `*.dll` in that directory — point it at a real FSharp.Core.dll /
/// System.Runtime.dll for a richer local run.
///
/// On a real, unmutated DLL a clean `Err` (unsupported construct) is fine —
/// the point is it's *loud*. A panic here is a genuine finding, not tolerated
/// (distinct from the mutation tests).
#[test]
fn curated_corpus_never_fails_quietly() {
    let mut corpus: Vec<PathBuf> = vec![
        ensure_minilib_built().to_path_buf(),
        ensure_minilib_fs_built().to_path_buf(),
        ensure_minilib_fs_ext_built().to_path_buf(),
        // Carries an embedded portable PDB, so `probe`'s PDB phase gets a
        // real debug directory to walk here (the siblings build without one).
        ensure_minilib_pdb_built().to_path_buf(),
    ];

    if let Some(dir) = std::env::var_os("BORZOI_ROBUSTNESS_CORPUS") {
        let dir = PathBuf::from(dir);
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read BORZOI_ROBUSTNESS_CORPUS dir {dir:?}: {e}"))
        {
            let path = entry.expect("corpus dir entry").path();
            if path.extension().and_then(|e| e.to_str()) == Some("dll") {
                corpus.push(path);
            }
        }
    }

    for path in &corpus {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read corpus DLL {path:?}: {e}"));
        let outcome = probe(&bytes);
        assert_ne!(
            outcome,
            Outcome::Panicked,
            "unmutated corpus DLL {path:?} made the parser panic — genuine finding, not tolerated",
        );
        // Ok or Err are both acceptable loud results on a real DLL.
    }
}
