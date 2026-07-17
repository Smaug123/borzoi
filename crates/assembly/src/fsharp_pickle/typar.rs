//! `u_ident`, `u_tyar_spec_data`, and the osgn-decl wrappers
//! `u_tyar_spec` / `u_tyar_specs`.
//!
//! ### FCS source map
//!
//! - `u_ident`          — `TypedTreePickle.fs:1913-1916`: `u_string`
//!   + `u_range`.
//! - `u_tyar_spec_data` — `:2389-2411`: `u_tup5 u_ident u_attribs
//!   u_int64 u_tyar_constraints u_xmldoc`.
//! - `u_tyar_spec`      — `:2413-2414`: `u_osgn_decl itypars
//!   u_tyar_spec_data`.
//! - `u_tyar_specs`     — `:2416`: `u_list u_tyar_spec`.
//!
//! The osgn-decl wrapper reads a compressed-int stamp index, decodes
//! the body, and writes it into the typar OSGN table. FCS re-links a
//! stamp idempotently (the same generalised typar is pickled inline in
//! several `TType_forall`s), so an identical re-link is a no-op; only a
//! *conflicting* re-link is a hard error
//! (`ImportError::OsgnConflictingRelink`).

use crate::error::ImportError;
use crate::fsharp_pickle::access::read_range;
use crate::fsharp_pickle::attribs::read_attribs;
use crate::fsharp_pickle::constraints::read_tyar_constraints;
use crate::fsharp_pickle::leaves::read_xmldoc;
use crate::fsharp_pickle::model::{PickledIdent, PickledTyparSpecData};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::reader::PickleReader;

/// `u_ident` (`TypedTreePickle.fs:1913-1916`). A `u_string` (resolved
/// through the strings table) plus a `u_range`.
pub(crate) fn read_ident(reader: &mut PickleReader<'_>) -> Result<PickledIdent, ImportError> {
    let name = reader.read_string("u_ident name")?;
    let range = read_range(reader)?;
    Ok(PickledIdent { name, range })
}

/// `u_tyar_spec_data` (`:2389-2411`). 5-tuple in order:
/// 1. `u_ident` — source identifier.
/// 2. `u_attribs` — attributes (e.g. `[<Measure>]`).
/// 3. `u_int64` — `TyparFlags` packed bitfield.
/// 4. `u_tyar_constraints` — F# constraints (primary stream tail
///    concatenated with the B-stream tail; see 6b1's `read_tyar_constraints`).
/// 5. `u_xmldoc` — documentation lines.
///
/// Takes `&mut PhaseOneState` because `u_attribs` recurses through
/// `u_attrib_expr` → `u_expr` → `u_ty`, which can hit `TType_forall`
/// and write to the typar OSGN table.
pub(crate) fn read_tyar_spec_data(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledTyparSpecData, ImportError> {
    let ident = read_ident(&mut state.reader)?;
    let attribs = read_attribs(state)?;
    let flags = state.reader.read_int64("u_tyar_spec_data flags")?;
    let constraints = read_tyar_constraints(state)?;
    let xmldoc = read_xmldoc(&mut state.reader)?;
    Ok(PickledTyparSpecData {
        ident,
        attribs,
        flags,
        constraints,
        xmldoc,
    })
}

/// `u_tyar_spec` (`:2413-2414`). Reads the osgn-decl prefix (compressed
/// int into `itypars`), decodes the body, and links it into the table.
/// Returns the freshly-linked stamp index so callers can record stable
/// handles into the typar table.
pub(crate) fn read_tyar_spec(state: &mut PhaseOneState<'_>) -> Result<u32, ImportError> {
    let idx = state.reader.read_uint32("u_tyar_spec osgn index")?;
    state.itypars.check_index_in_range(idx)?;
    let body = read_tyar_spec_data(state)?;
    state.itypars.link(idx, body)
}

/// `u_tyar_specs` (`:2416`). A list of `u_tyar_spec`s, each one
/// declaring a fresh typar in the OSGN table.
pub(crate) fn read_tyar_specs(state: &mut PhaseOneState<'_>) -> Result<Vec<u32>, ImportError> {
    state.read_array("u_tyar_specs element", read_tyar_spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{
        FSharpTyparConstraint, Nullness, PickledPos, PickledRange, PickledType,
    };

    fn make_state<'a>(bytes: &'a [u8], strings: &'a [String]) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, 0)
    }

    fn make_state_typars<'a>(
        bytes: &'a [u8],
        strings: &'a [String],
        ntypars: usize,
    ) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, ntypars, 0)
    }

    #[test]
    fn ident_round_trip() {
        // u_string idx 0 = "T", u_range with file idx 1 = "src.fs",
        // start = (1, 0), end = (1, 1).
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let bytes = vec![
            0u8, // name idx 0 = "T"
            1u8, // file idx 1 = "src.fs"
            1u8, // start_line
            0u8, // start_col
            1u8, // end_line
            1u8, // end_col
        ];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: &[Vec<u32>] = &[];
        r.attach_tables(&strings, pubpaths);
        let id = read_ident(&mut r).unwrap();
        assert_eq!(id.name, "T");
        assert_eq!(
            id.range,
            PickledRange {
                file: 1,
                start: PickledPos { line: 1, column: 0 },
                end: PickledPos { line: 1, column: 1 },
            }
        );
        assert!(r.is_eof());
    }

    #[test]
    fn tyar_spec_data_minimal_round_trip() {
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let bytes = vec![
            0u8, // u_ident name idx 0 = "T"
            1u8, // u_range file idx 1 = "src.fs"
            1u8, 0u8, // u_pos start
            1u8, 1u8, // u_pos end
            0u8, // u_attribs list len 0
            0u8, 0u8, // u_int64 flags = 0 (two compressed-int zeros: lo, hi)
            0u8, // u_tyar_constraints primary list len 0
            0u8, // u_xmldoc array len 0
        ];
        let mut s = make_state(&bytes, &strings);
        let data = read_tyar_spec_data(&mut s).unwrap();
        assert_eq!(data.ident.name, "T");
        assert!(data.attribs.is_empty());
        assert_eq!(data.flags, 0);
        assert!(data.constraints.is_empty());
        assert!(data.xmldoc.lines.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tyar_spec_data_with_constraint() {
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let bytes = vec![
            0u8, 1u8, 1u8, 0u8, 1u8, 1u8, // u_ident: name + range
            0u8, // u_attribs list len 0
            0u8, 0u8, // u_int64 flags = 0 (two compressed-int zeros)
            1u8, // u_tyar_constraints primary list len 1
            0u8, // constraint tag = CoercesTo
            1u8, // u_ty tag = AppSimple
            0u8, // simpletyp idx 0
            0u8, // u_xmldoc array len 0
        ];
        let mut s = make_state(&bytes, &strings);
        let data = read_tyar_spec_data(&mut s).unwrap();
        assert_eq!(data.ident.name, "T");
        assert_eq!(data.constraints.len(), 1);
        match &data.constraints[0] {
            FSharpTyparConstraint::CoercesTo(PickledType::AppSimple {
                simpletyp_index: 0,
                nullness: Nullness::Ambivalent,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tyar_spec_osgn_decl_round_trip() {
        // u_tyar_spec: osgn idx 0, then a minimal tyar_spec_data body.
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let bytes = vec![
            0u8, // osgn idx 0
            // u_tyar_spec_data:
            0u8, 1u8, 1u8, 0u8, 1u8, 1u8, // u_ident
            0u8, // u_attribs len 0
            0u8, 0u8, // flags = 0 (two compressed-int zeros)
            0u8, // constraints len 0
            0u8, // xmldoc len 0
        ];
        let mut s = make_state_typars(&bytes, &strings, 1);
        let idx = read_tyar_spec(&mut s).unwrap();
        assert_eq!(idx, 0);
        // slot is now linked
        assert!(s.itypars.get(0).is_ok());
    }

    #[test]
    fn tyar_specs_list_round_trip() {
        // Two tyar specs at indices 0, 1.
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let one_spec_at = |idx: u8| {
            vec![
                idx, // osgn idx
                0u8, 1u8, 1u8, 0u8, 1u8, 1u8, // u_ident
                0u8, // attribs
                0u8, 0u8, // flags = 0 (compressed lo, hi)
                0u8, // constraints
                0u8, // xmldoc
            ]
        };
        let mut bytes = vec![2u8]; // list length 2
        bytes.extend(one_spec_at(0));
        bytes.extend(one_spec_at(1));
        let mut s = make_state_typars(&bytes, &strings, 2);
        let idxs = read_tyar_specs(&mut s).unwrap();
        assert_eq!(idxs, vec![0, 1]);
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tyar_spec_out_of_range_errors() {
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let bytes = vec![5u8]; // osgn idx 5 but table has 2 slots
        let mut s = make_state_typars(&bytes, &strings, 2);
        match read_tyar_spec(&mut s) {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "typars",
                index: 5,
                max: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
