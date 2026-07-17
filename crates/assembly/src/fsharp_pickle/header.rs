//! Decode the F# pickle phase-2 header.
//!
//! Wire format (from `unpickleObjWithDanglingCcus` at
//! `dotnet/fsharp/src/Compiler/TypedTree/TypedTreePickle.fs:1037-1085`):
//!
//! ```text
//! phase-2 := ccu_refs           // u_array u_encoded_ccuref
//!            z1                 // u_int  (sign-encoded ntycons)
//!            ntypars, nvals     // u_tup2 u_int u_int
//!            [nanoninfos]       // u_int  iff z1 < 0
//!            strings            // u_array u_encoded_string  (= u_prim_string)
//!            pubpaths           // u_array (u_array u_int)
//!            nlerefs            // u_array (u_tup2 u_int (u_array u_int))
//!            simpletys          // u_array u_int
//!            phase1_bytes       // u_byte_memory
//! ```
//!
//! `u_tup5` does not introduce any framing — the five elements are
//! decoded sequentially, sharing the same cursor (see `u_tup5` at
//! `:495-502`).

use crate::error::ImportError;
use crate::fsharp_pickle::model::{CcuRef, PickledHeader, PickledNleRef};
use crate::fsharp_pickle::reader::PickleReader;

/// Read one `u_encoded_ccuref`: leading tag byte (must be `0`) plus a
/// length-prefixed UTF-8 name. `TypedTreePickle.fs:842-845`.
fn read_encoded_ccu_ref(reader: &mut PickleReader<'_>) -> Result<CcuRef, ImportError> {
    let tag = reader.read_byte("phase 2: ccu_ref tag")?;
    if tag != 0 {
        return Err(ImportError::UnsupportedPickleTag {
            context: "u_encoded_ccuref tag",
            tag: u32::from(tag),
        });
    }
    let name = reader.read_string_raw("phase 2: ccu_ref name")?;
    Ok(CcuRef { name })
}

/// Read one `u_encoded_pubpath` = `u_array u_int`. `:860`.
fn read_encoded_pubpath(reader: &mut PickleReader<'_>) -> Result<Vec<u32>, ImportError> {
    reader.read_array("phase 2: pubpath element", |r| {
        r.read_uint32("phase 2: pubpath string-index")
    })
}

/// Read one `u_encoded_nleref` = `u_tup2 u_int (u_array u_int)`. `:877`.
fn read_encoded_nleref(reader: &mut PickleReader<'_>) -> Result<PickledNleRef, ImportError> {
    let ccu = reader.read_uint32("phase 2: nleref ccu-index")?;
    let path = reader.read_array("phase 2: nleref path element", |r| {
        r.read_uint32("phase 2: nleref path name-index")
    })?;
    Ok(PickledNleRef { ccu, path })
}

/// Read one `u_encoded_simpletyp` = `u_int` (an `nleref` table index).
/// `:914`.
fn read_encoded_simpletyp(reader: &mut PickleReader<'_>) -> Result<u32, ImportError> {
    reader.read_uint32("phase 2: simpletyp nleref-index")
}

/// Read the whole phase-2 header. Consumes the cursor up to the start of
/// what would be the post-header bytes; for a well-formed signature
/// resource that is end-of-stream.
pub(crate) fn read_header(reader: &mut PickleReader<'_>) -> Result<PickledHeader, ImportError> {
    let ccu_refs = reader.read_array("phase 2: ccu_refs", read_encoded_ccu_ref)?;

    // z1 is sign-encoded: negative magnitude signals "anon-record-info
    // table is present" (introduced with F# 3.0+). `:1072-1075`.
    let z1 = reader.read_int32("phase 2: z1 (ntycons)")?;
    let (ntycons, has_anon) = if z1 < 0 {
        // F# uses `~~~z1` (bitwise complement) to recover ntycons. Mirror
        // exactly so the off-by-one is correct: !z1 in Rust on i32.
        let ntycons = (!z1) as u32;
        (ntycons, true)
    } else {
        (z1 as u32, false)
    };

    let ntypars = reader.read_uint32("phase 2: ntypars")?;
    let nvals = reader.read_uint32("phase 2: nvals")?;
    let nanoninfos = if has_anon {
        reader.read_uint32("phase 2: nanoninfos")?
    } else {
        0
    };

    let strings = reader.read_array("phase 2: strings", |r| {
        r.read_string_raw("phase 2: strings element")
    })?;
    let pubpaths = reader.read_array("phase 2: pubpaths", read_encoded_pubpath)?;
    let nlerefs = reader.read_array("phase 2: nlerefs", read_encoded_nleref)?;
    let simpletys = reader.read_array("phase 2: simpletys", read_encoded_simpletyp)?;
    let phase1_bytes = reader.read_byte_memory("phase 2: phase1_bytes")?.to_vec();

    // Validate every cross-table index the header tables carry, at the
    // boundary (parse, don't validate): a consumer that receives a
    // `PickledHeader` may dereference any entry without re-checking. The
    // F# compiler's writer interns values as it references them, so a
    // dangling index can only mean corruption — refused, never clamped.
    // (Phase-1 *body* indices into these tables are checked at their own
    // decode sites; this covers the header's internal references.)
    let check = |kind: &'static str, index: u32, len: usize| {
        if (index as usize) < len {
            Ok(())
        } else {
            Err(ImportError::DanglingPickleRef { kind, index })
        }
    };
    for path in &pubpaths {
        for &s in path {
            check("string", s, strings.len())?;
        }
    }
    for nleref in &nlerefs {
        check("ccuref", nleref.ccu, ccu_refs.len())?;
        for &s in &nleref.path {
            check("string", s, strings.len())?;
        }
    }
    for &n in &simpletys {
        check("nleref", n, nlerefs.len())?;
    }

    Ok(PickledHeader {
        ccu_refs,
        ntycons,
        ntypars,
        nvals,
        nanoninfos,
        strings,
        pubpaths,
        nlerefs,
        simpletys,
        phase1_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: encode a small unsigned value (< 0x80) as a single byte.
    fn b(v: u8) -> Vec<u8> {
        vec![v]
    }

    /// Encode a length-prefixed UTF-8 string for `u_prim_string`.
    fn enc_string(s: &str) -> Vec<u8> {
        let mut out = b(s.len() as u8);
        out.extend_from_slice(s.as_bytes());
        out
    }

    /// Encode `u_encoded_ccuref` = leading `0` tag + `u_prim_string`.
    fn enc_ccuref(name: &str) -> Vec<u8> {
        let mut out = b(0);
        out.extend(enc_string(name));
        out
    }

    /// Encode `u_array` with a list of pre-encoded elements.
    fn enc_array(n: u8, elements: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b(n);
        for e in elements {
            out.extend(e);
        }
        out
    }

    #[test]
    fn empty_header_decodes() {
        // Minimum-content header: zero of everything, no anon-info,
        // empty phase1_bytes.
        let mut bytes = Vec::new();
        bytes.extend(b(0)); // ccu_refs.length = 0
        bytes.extend(b(0)); // z1 = 0 → ntycons = 0, has_anon = false
        bytes.extend(b(0)); // ntypars
        bytes.extend(b(0)); // nvals
        // nanoninfos skipped (has_anon = false)
        bytes.extend(b(0)); // strings.length
        bytes.extend(b(0)); // pubpaths.length
        bytes.extend(b(0)); // nlerefs.length
        bytes.extend(b(0)); // simpletys.length
        bytes.extend(b(0)); // phase1_bytes.length = 0

        let mut r = PickleReader::new(&bytes);
        let h = read_header(&mut r).unwrap();
        assert_eq!(h.ccu_refs, vec![]);
        assert_eq!(h.ntycons, 0);
        assert_eq!(h.ntypars, 0);
        assert_eq!(h.nvals, 0);
        assert_eq!(h.nanoninfos, 0);
        assert_eq!(h.strings, Vec::<String>::new());
        assert!(h.pubpaths.is_empty());
        assert!(h.nlerefs.is_empty());
        assert!(h.simpletys.is_empty());
        assert!(h.phase1_bytes.is_empty());
        assert!(r.is_eof());
    }

    #[test]
    fn handcrafted_header_with_one_of_each_decodes() {
        let mut bytes = Vec::new();
        // ccu_refs: ["MyCcu"]
        bytes.extend(enc_array(1, vec![enc_ccuref("MyCcu")]));
        // z1 = 3 (positive → ntycons = 3, no anon)
        bytes.extend(b(3));
        // ntypars, nvals
        bytes.extend(b(7));
        bytes.extend(b(11));
        // strings: ["foo", "bar"]
        bytes.extend(enc_array(2, vec![enc_string("foo"), enc_string("bar")]));
        // pubpaths: [[0, 1]] — one pubpath of length 2
        bytes.extend(enc_array(1, vec![enc_array(2, vec![b(0), b(1)])]));
        // nlerefs: [(0, [1])] — one nleref
        let mut nleref0 = Vec::new();
        nleref0.extend(b(0)); // ccu = 0
        nleref0.extend(enc_array(1, vec![b(1)])); // path = [1]
        bytes.extend(enc_array(1, vec![nleref0]));
        // simpletys: [0]
        bytes.extend(enc_array(1, vec![b(0)]));
        // phase1_bytes: 3-byte blob 0xAA 0xBB 0xCC
        bytes.extend(b(3));
        bytes.extend([0xAA, 0xBB, 0xCC]);

        let mut r = PickleReader::new(&bytes);
        let h = read_header(&mut r).unwrap();
        assert_eq!(
            h.ccu_refs,
            vec![CcuRef {
                name: "MyCcu".to_string()
            }]
        );
        assert_eq!(h.ntycons, 3);
        assert_eq!(h.ntypars, 7);
        assert_eq!(h.nvals, 11);
        assert_eq!(h.nanoninfos, 0);
        assert_eq!(h.strings, vec!["foo".to_string(), "bar".to_string()]);
        assert_eq!(h.pubpaths, vec![vec![0, 1]]);
        assert_eq!(
            h.nlerefs,
            vec![PickledNleRef {
                ccu: 0,
                path: vec![1]
            }]
        );
        assert_eq!(h.simpletys, vec![0]);
        assert_eq!(h.phase1_bytes, vec![0xAA, 0xBB, 0xCC]);
        assert!(r.is_eof());
    }

    #[test]
    fn negative_z1_signals_anon_count_and_uses_bitwise_complement() {
        // z1 encoded as the four-byte form holding the i32 bit pattern
        // for -4. !(-4) = 3, so ntycons should be 3.
        let mut bytes = Vec::new();
        // ccu_refs: empty
        bytes.extend(b(0));
        // z1 = -4: bit pattern 0xFFFFFFFC. Encoded as 0xFF + 4 LE bytes.
        bytes.push(0xFF);
        bytes.extend((-4i32).to_le_bytes());
        // ntypars, nvals
        bytes.extend(b(0));
        bytes.extend(b(0));
        // nanoninfos present because z1 < 0
        bytes.extend(b(2));
        // strings empty, pubpaths empty, nlerefs empty, simpletys empty
        bytes.extend(b(0));
        bytes.extend(b(0));
        bytes.extend(b(0));
        bytes.extend(b(0));
        // phase1_bytes empty
        bytes.extend(b(0));

        let mut r = PickleReader::new(&bytes);
        let h = read_header(&mut r).unwrap();
        assert_eq!(h.ntycons, 3);
        assert_eq!(h.nanoninfos, 2);
    }

    /// Build header bytes with one ccuref + one string, and the given
    /// pubpaths/nlerefs/simpletys sections (already `u_array`-encoded).
    fn header_with_tables(pubpaths: Vec<u8>, nlerefs: Vec<u8>, simpletys: Vec<u8>) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(enc_array(1, vec![enc_ccuref("MyCcu")])); // ccu_refs
        bytes.extend(b(0)); // z1
        bytes.extend(b(0)); // ntypars
        bytes.extend(b(0)); // nvals
        bytes.extend(enc_array(1, vec![enc_string("foo")])); // strings
        bytes.extend(pubpaths);
        bytes.extend(nlerefs);
        bytes.extend(simpletys);
        bytes.extend(b(0)); // phase1_bytes empty
        bytes
    }

    #[test]
    fn pubpath_string_index_out_of_range_is_refused() {
        // pubpaths: [[7]] — string index 7 with only 1 string interned.
        let bytes = header_with_tables(
            enc_array(1, vec![enc_array(1, vec![b(7)])]),
            enc_array(0, vec![]),
            enc_array(0, vec![]),
        );
        let mut r = PickleReader::new(&bytes);
        match read_header(&mut r) {
            Err(ImportError::DanglingPickleRef {
                kind: "string",
                index: 7,
            }) => {}
            other => panic!("expected DanglingPickleRef for pubpath string, got {other:?}"),
        }
    }

    #[test]
    fn nleref_ccu_index_out_of_range_is_refused() {
        // nlerefs: [(4, [0])] — ccu index 4 with only 1 ccuref.
        let mut nleref0 = Vec::new();
        nleref0.extend(b(4));
        nleref0.extend(enc_array(1, vec![b(0)]));
        let bytes = header_with_tables(
            enc_array(0, vec![]),
            enc_array(1, vec![nleref0]),
            enc_array(0, vec![]),
        );
        let mut r = PickleReader::new(&bytes);
        match read_header(&mut r) {
            Err(ImportError::DanglingPickleRef {
                kind: "ccuref",
                index: 4,
            }) => {}
            other => panic!("expected DanglingPickleRef for nleref ccu, got {other:?}"),
        }
    }

    #[test]
    fn nleref_path_string_index_out_of_range_is_refused() {
        // nlerefs: [(0, [9])] — path string index 9 with only 1 string.
        let mut nleref0 = Vec::new();
        nleref0.extend(b(0));
        nleref0.extend(enc_array(1, vec![b(9)]));
        let bytes = header_with_tables(
            enc_array(0, vec![]),
            enc_array(1, vec![nleref0]),
            enc_array(0, vec![]),
        );
        let mut r = PickleReader::new(&bytes);
        match read_header(&mut r) {
            Err(ImportError::DanglingPickleRef {
                kind: "string",
                index: 9,
            }) => {}
            other => panic!("expected DanglingPickleRef for nleref path, got {other:?}"),
        }
    }

    #[test]
    fn simpletyp_nleref_index_out_of_range_is_refused() {
        // simpletys: [3] — nleref index 3 with zero nlerefs.
        let bytes = header_with_tables(
            enc_array(0, vec![]),
            enc_array(0, vec![]),
            enc_array(1, vec![b(3)]),
        );
        let mut r = PickleReader::new(&bytes);
        match read_header(&mut r) {
            Err(ImportError::DanglingPickleRef {
                kind: "nleref",
                index: 3,
            }) => {}
            other => panic!("expected DanglingPickleRef for simpletyp, got {other:?}"),
        }
    }

    #[test]
    fn bad_ccuref_tag_errors() {
        let mut bytes = Vec::new();
        bytes.extend(enc_array(
            1,
            vec![{
                let mut v = b(0x05); // not 0!
                v.extend(enc_string("X"));
                v
            }],
        ));
        let mut r = PickleReader::new(&bytes);
        match read_header(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_encoded_ccuref tag",
                tag: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
