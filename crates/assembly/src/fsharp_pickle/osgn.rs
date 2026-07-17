//! Observably-shared graph-node (OSGN) tables and the phase-1 walker state.
//!
//! FCS's `NodeInTable.Create` (`TypedTreePickle.fs:520-570`) pre-allocates `n`
//! blank stub nodes per stamp table; `u_osgn_decl` then fires `LinkNode`
//! during the depth-first body walk (`:604-609`), and `u_osgn_ref` is a pure
//! index read (`:596-602`) — the resulting `_ref` value points into the same
//! stub the future `_decl` will link, which is how FCS breaks the
//! entity/typar/val mutual-reference cycle. `LinkNode` overwrites the stub
//! *unconditionally*, so a stamp may be linked more than once (the same
//! generalised node pickled inline in several `TType_forall`s); post-walk,
//! FCS only validates that no stub is left *un*linked (`:1026-1031`), not
//! that each was linked once.
//!
//! Rust port:
//!
//! - `OsgnTable<T>` stores `Vec<Option<T>>` of pre-sized capacity. `link`
//!   writes a decoded body into a slot, tolerating an identical re-link as a
//!   no-op (matching FCS) and hard-erroring only on a *conflicting* re-link.
//!   `read_ref` is the pure-index variant — callers store the index and
//!   either dereference post-walk via `finalize` or chase via `get`.
//! - `PhaseOneState` wraps `PickleReader` plus the three tables
//!   (`itycons`, `itypars`, `ivals`). Walker entry points
//!   (`read_entity_spec`, `read_val`, `read_tyar_spec`) thread
//!   `&mut PhaseOneState`; everything reachable from `read_ty` is also
//!   state-aware because `u_ty` tag 5 (`Forall`) recurses through
//!   `u_tyar_specs`, which writes to the typar OSGN table.

use crate::error::ImportError;
use crate::fsharp_pickle::model::{
    PickledEntity, PickledHeader, PickledOsgnTables, PickledTyparSpecData, PickledVal,
};
use crate::fsharp_pickle::reader::PickleReader;

/// One OSGN stamp table. `slots[i] == None` is FCS's `NewUnlinked`;
/// `slots[i] == Some(body)` is `LinkNode`-d.
pub(crate) struct OsgnTable<T> {
    kind: &'static str,
    slots: Vec<Option<T>>,
}

impl<T> OsgnTable<T> {
    pub(crate) fn new(kind: &'static str, n: usize) -> Self {
        let mut slots = Vec::with_capacity(n);
        slots.resize_with(n, || None);
        Self { kind, slots }
    }

    /// Bounds-check a stamp index against the table's pre-allocated
    /// length. Raises `OsgnIndexOutOfRange` if the stamp is beyond the
    /// reserved capacity.
    pub(crate) fn check_index_in_range(&self, idx: u32) -> Result<(), ImportError> {
        if (idx as usize) >= self.slots.len() {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: self.kind,
                index: idx,
                max: self.slots.len(),
            })
        } else {
            Ok(())
        }
    }

    /// FCS `LinkNode` (`TypedTreePickle.fs:604-609`): publish a decoded
    /// body into the slot at `idx`. Returns the index so the caller can
    /// record a stable handle (the entity-tree walker stores root + child
    /// indices rather than re-traversing the table).
    ///
    /// FCS's per-node `Link` (`TypedTree.fs:1080`/`2456`/`3382`) overwrites
    /// the stub's fields *unconditionally*, and `check` (`:1026-1031`) only
    /// asserts every stub ends up linked — never that it is linked exactly
    /// once. A stamp is therefore legitimately re-declared whenever the
    /// same generalised node is pickled inline in more than one place: e.g.
    /// an interface method's `'T` is re-emitted as a fresh `TType_forall`
    /// typar binder in both the abstract slot's `vref` partial-type *and*
    /// the same member's `tcaug.adhoc` `vref` partial-type, reusing the one
    /// typar stamp. Both decode to identical data (it is the same source
    /// node), so an idempotent re-link is accepted as a no-op.
    ///
    /// A re-link whose body *differs* cannot arise from a valid pickle (FCS
    /// only re-pickles the same node), so it is failed loud as
    /// [`ImportError::OsgnConflictingRelink`] — stricter than FCS's silent
    /// overwrite, yet provably never rejecting input FCS accepts.
    pub(crate) fn link(&mut self, idx: u32, body: T) -> Result<u32, ImportError>
    where
        T: PartialEq,
    {
        self.check_index_in_range(idx)?;
        let slot = &mut self.slots[idx as usize];
        match slot {
            Some(existing) if *existing != body => {
                return Err(ImportError::OsgnConflictingRelink {
                    kind: self.kind,
                    index: idx,
                });
            }
            // Idempotent re-link with identical data — a no-op, as in FCS.
            Some(_) => {}
            None => *slot = Some(body),
        }
        Ok(idx)
    }

    /// FCS `u_osgn_ref` (`:596-602`): read a compressed-int index and
    /// return it. The body may or may not have been linked yet — the
    /// resolution defers to `get` (post-walk) or to `finalize`.
    pub(crate) fn read_ref(
        &self,
        reader: &mut PickleReader<'_>,
        context: &'static str,
    ) -> Result<u32, ImportError> {
        let idx = reader.read_uint32(context)?;
        self.check_index_in_range(idx)?;
        Ok(idx)
    }

    /// Post-walk lookup. Returns the linked body or
    /// `OsgnSlotNotLinked` if the slot was never declared.
    #[allow(dead_code)] // Used by future post-walk consumers and tests.
    pub(crate) fn get(&self, idx: u32) -> Result<&T, ImportError> {
        self.check_index_in_range(idx)?;
        self.slots[idx as usize]
            .as_ref()
            .ok_or(ImportError::OsgnSlotNotLinked {
                kind: self.kind,
                index: idx,
            })
    }

    /// Consume the table and assert every slot was linked. Matches
    /// FCS's `check` (`:1026-1031`). Returns the dense `Vec<T>` for
    /// downstream consumers.
    pub(crate) fn finalize(self) -> Result<Vec<T>, ImportError> {
        let kind = self.kind;
        self.slots
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                slot.ok_or(ImportError::OsgnSlotNotLinked {
                    kind,
                    index: i as u32,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
}

/// Bundle of mutable state threaded through the phase-1 walker. The
/// reader and tables share a lifetime; finalising the state hands the
/// caller a dense `PickledOsgnTables` and discards the reader.
///
/// Fields are `pub(crate)` so call sites can split-borrow the reader
/// and a table without going through accessor methods — necessary
/// because Rust's borrow checker can't see through opaque getters
/// (e.g. `state.reader_mut()` and `state.itypars_mut()` both
/// conflict from the borrow checker's view, but
/// `state.reader.read_uint32(...)` and `state.itypars.link(...)`
/// touch disjoint fields and compose freely).
pub(crate) struct PhaseOneState<'a> {
    pub(crate) reader: PickleReader<'a>,
    pub(crate) itycons: OsgnTable<PickledEntity>,
    pub(crate) itypars: OsgnTable<PickledTyparSpecData>,
    pub(crate) ivals: OsgnTable<PickledVal>,
    /// Length of the phase-2 `nlerefs` table, so a body-encoded
    /// `ERefNonLocal` index is bounds-checked at decode time (matching
    /// how OSGN stamps are handled) instead of storing a dangling index
    /// a later consumer would trust.
    pub(crate) nlerefs_len: usize,
    /// Length of the phase-2 `simpletys` table; see
    /// [`Self::nlerefs_len`].
    pub(crate) simpletys_len: usize,
}

impl<'a> PhaseOneState<'a> {
    /// Build the phase-1 state. The OSGN counts come from the
    /// (untrusted) phase-2 header; we cap their sum against the
    /// phase-1 body length to reject malformed signatures that would
    /// otherwise pre-allocate huge tables before any consistency check
    /// fires. Every linked slot must be reached by a body byte from
    /// the phase-1 stream, so `ntycons + ntypars + nvals` cannot
    /// exceed the stream's length.
    pub(crate) fn new(
        reader: PickleReader<'a>,
        header: &PickledHeader,
    ) -> Result<Self, ImportError> {
        let cap = reader.total_len();
        for (kind, count) in [
            ("ntycons", header.ntycons),
            ("ntypars", header.ntypars),
            ("nvals", header.nvals),
        ] {
            if count as usize > cap {
                return Err(ImportError::MalformedPickleHeader {
                    detail: format!(
                        "{kind} {count} exceeds phase-1 body length {cap}; refusing to allocate"
                    ),
                });
            }
        }
        let total = (header.ntycons as usize)
            .saturating_add(header.ntypars as usize)
            .saturating_add(header.nvals as usize);
        if total > cap {
            return Err(ImportError::MalformedPickleHeader {
                detail: format!(
                    "OSGN counts sum to {total} but phase-1 body is only {cap} bytes; refusing to allocate"
                ),
            });
        }
        Ok(Self {
            reader,
            itycons: OsgnTable::new("tycons", header.ntycons as usize),
            itypars: OsgnTable::new("typars", header.ntypars as usize),
            ivals: OsgnTable::new("vals", header.nvals as usize),
            nlerefs_len: header.nlerefs.len(),
            simpletys_len: header.simpletys.len(),
        })
    }

    /// Test-only constructor: build a state with explicit table
    /// capacities. Production code uses `new(reader, header)`; tests
    /// for isolated decoders use this to skip the header decode. The
    /// `nlerefs`/`simpletys` lengths default to the same generous
    /// headroom as the fixtures' hand-coded indices assume; bounds
    /// tests override the fields directly.
    #[cfg(test)]
    pub(crate) fn with_capacities(
        reader: PickleReader<'a>,
        ntycons: usize,
        ntypars: usize,
        nvals: usize,
    ) -> Self {
        Self {
            reader,
            itycons: OsgnTable::new("tycons", ntycons),
            itypars: OsgnTable::new("typars", ntypars),
            ivals: OsgnTable::new("vals", nvals),
            nlerefs_len: 256,
            simpletys_len: 256,
        }
    }

    /// State-aware list decoder. Mirrors `PickleReader::read_array`
    /// but threads `&mut Self` through the element closure so the
    /// element can recurse through OSGN-touching decoders.
    pub(crate) fn read_array<T>(
        &mut self,
        context: &'static str,
        mut elt: impl FnMut(&mut Self) -> Result<T, ImportError>,
    ) -> Result<Vec<T>, ImportError> {
        let n = self.reader.read_uint32(context)? as usize;
        let remaining = self.reader.remaining();
        if n > remaining {
            return Err(ImportError::MalformedPickleHeader {
                detail: format!("{context}: array length {n} exceeds remaining bytes {remaining}"),
            });
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(elt(self)?);
        }
        Ok(out)
    }

    /// State-aware option decoder. Mirrors `PickleReader::read_option`.
    pub(crate) fn read_option<T>(
        &mut self,
        context: &'static str,
        mut payload: impl FnMut(&mut Self) -> Result<T, ImportError>,
    ) -> Result<Option<T>, ImportError> {
        let tag = self.reader.read_byte(context)?;
        match tag {
            0 => Ok(None),
            1 => Ok(Some(payload(self)?)),
            other => Err(ImportError::UnsupportedPickleTag {
                context,
                tag: u32::from(other),
            }),
        }
    }

    /// State-aware reverse-index list decoder. Mirrors
    /// `PickleReader::read_list_revi`. The closure receives the
    /// reverse element index (`n-1`, `n-2`, … `0`).
    pub(crate) fn read_list_revi<T>(
        &mut self,
        context: &'static str,
        mut elt: impl FnMut(&mut Self, u32) -> Result<T, ImportError>,
    ) -> Result<Vec<T>, ImportError> {
        let n = self.reader.read_uint32(context)? as usize;
        let remaining = self.reader.remaining();
        if n > remaining {
            return Err(ImportError::MalformedPickleHeader {
                detail: format!("{context}: list length {n} exceeds remaining bytes {remaining}",),
            });
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let ridx = (n - 1 - i) as u32;
            out.push(elt(self, ridx)?);
        }
        Ok(out)
    }

    /// Finalise the three tables into dense vectors. Errors if any
    /// slot was never linked.
    pub(crate) fn finalize(self) -> Result<PickledOsgnTables, ImportError> {
        Ok(PickledOsgnTables {
            tycons: self.itycons.finalize()?,
            typars: self.itypars.finalize()?,
            vals: self.ivals.finalize()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_reader(bytes: &[u8]) -> PickleReader<'_> {
        PickleReader::new(bytes)
    }

    #[test]
    fn link_then_get_round_trip() {
        let mut tbl: OsgnTable<u32> = OsgnTable::new("vals", 4);
        tbl.link(2, 99).expect("link ok");
        assert_eq!(*tbl.get(2).unwrap(), 99);
    }

    #[test]
    fn link_out_of_range_errors() {
        let mut tbl: OsgnTable<u32> = OsgnTable::new("vals", 2);
        match tbl.link(7, 0) {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "vals",
                index: 7,
                max: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn link_idempotent_relink_is_ok() {
        // FCS's `LinkNode` overwrites a stub unconditionally, so re-linking
        // the same stamp with identical data is a legal no-op — the shape a
        // generalised typar pickled inline in two `TType_forall`s produces.
        let mut tbl: OsgnTable<u32> = OsgnTable::new("typars", 4);
        tbl.link(2, 99).unwrap();
        tbl.link(2, 99).expect("identical re-link is a no-op");
        assert_eq!(*tbl.get(2).unwrap(), 99);
    }

    #[test]
    fn link_conflicting_relink_errors() {
        // A re-link whose body differs cannot come from a valid pickle — it
        // is the signature of a stream misalignment, so fail loud.
        let mut tbl: OsgnTable<u32> = OsgnTable::new("vals", 4);
        tbl.link(2, 99).unwrap();
        match tbl.link(2, 100) {
            Err(ImportError::OsgnConflictingRelink {
                kind: "vals",
                index: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_ref_returns_index_without_resolution() {
        let tbl: OsgnTable<u32> = OsgnTable::new("vals", 4);
        let bytes = [3u8];
        let mut r = make_reader(&bytes);
        let idx = tbl.read_ref(&mut r, "ctx").unwrap();
        assert_eq!(idx, 3);
    }

    #[test]
    fn read_ref_out_of_range_errors() {
        let tbl: OsgnTable<u32> = OsgnTable::new("vals", 2);
        let bytes = [5u8];
        let mut r = make_reader(&bytes);
        match tbl.read_ref(&mut r, "ctx") {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "vals",
                index: 5,
                max: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn finalize_unlinked_errors() {
        let mut tbl: OsgnTable<u32> = OsgnTable::new("vals", 3);
        tbl.link(0, 11).unwrap();
        tbl.link(2, 13).unwrap();
        // Slot 1 was never linked.
        match tbl.finalize() {
            Err(ImportError::OsgnSlotNotLinked {
                kind: "vals",
                index: 1,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn finalize_dense_when_all_linked() {
        let mut tbl: OsgnTable<u32> = OsgnTable::new("vals", 3);
        tbl.link(0, 11).unwrap();
        tbl.link(1, 12).unwrap();
        tbl.link(2, 13).unwrap();
        assert_eq!(tbl.finalize().unwrap(), vec![11, 12, 13]);
    }

    #[test]
    fn osgn_table_len_matches_capacity() {
        let tbl: OsgnTable<u32> = OsgnTable::new("typars", 7);
        assert_eq!(tbl.len(), 7);
    }

    #[test]
    fn phase_one_state_new_rejects_count_exceeding_body() {
        // Untrusted header advertises 1 000 000 tycons, but the phase-1
        // body is 3 bytes — refuse to allocate.
        let header = PickledHeader {
            ccu_refs: vec![],
            ntycons: 1_000_000,
            ntypars: 0,
            nvals: 0,
            nanoninfos: 0,
            strings: vec![],
            pubpaths: vec![],
            nlerefs: vec![],
            simpletys: vec![],
            phase1_bytes: vec![],
        };
        let bytes = [0u8, 1u8, 2u8];
        let reader = PickleReader::new_dual(&bytes, None);
        match PhaseOneState::new(reader, &header) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("ntycons"), "detail: {detail}");
                assert!(detail.contains("1000000"), "detail: {detail}");
            }
            Err(other) => panic!("expected MalformedPickleHeader, got {other:?}"),
            Ok(_) => panic!("expected MalformedPickleHeader, got Ok"),
        }
    }

    #[test]
    fn phase_one_state_new_rejects_count_sum_exceeding_body() {
        // Each count individually fits within the 5-byte body, but their
        // sum doesn't — the additive guard catches this.
        let header = PickledHeader {
            ccu_refs: vec![],
            ntycons: 3,
            ntypars: 3,
            nvals: 3,
            nanoninfos: 0,
            strings: vec![],
            pubpaths: vec![],
            nlerefs: vec![],
            simpletys: vec![],
            phase1_bytes: vec![],
        };
        let bytes = [0u8, 1u8, 2u8, 3u8, 4u8];
        let reader = PickleReader::new_dual(&bytes, None);
        match PhaseOneState::new(reader, &header) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("sum"), "detail: {detail}");
            }
            Err(other) => panic!("expected MalformedPickleHeader, got {other:?}"),
            Ok(_) => panic!("expected MalformedPickleHeader, got Ok"),
        }
    }

    #[test]
    fn phase_one_state_new_accepts_counts_within_body() {
        let header = PickledHeader {
            ccu_refs: vec![],
            ntycons: 2,
            ntypars: 1,
            nvals: 3,
            nanoninfos: 0,
            strings: vec![],
            pubpaths: vec![],
            nlerefs: vec![],
            simpletys: vec![],
            phase1_bytes: vec![],
        };
        let bytes = vec![0u8; 32];
        let reader = PickleReader::new_dual(&bytes, None);
        let state = PhaseOneState::new(reader, &header).expect("counts fit");
        assert_eq!(state.itycons.len(), 2);
        assert_eq!(state.itypars.len(), 1);
        assert_eq!(state.ivals.len(), 3);
    }
}
