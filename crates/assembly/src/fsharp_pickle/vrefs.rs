//! `u_vref` and `u_nonlocal_val_ref` — pickled value references.
//!
//! ### FCS source map
//!
//! - `u_vref`             — `TypedTreePickle.fs:2032-2038`. Tag byte
//!   dispatches `VRefLocal` (tag 0, osgn lookup) vs `VRefNonLocal`
//!   (tag 1).
//! - `u_nonlocal_val_ref` — `:2010-2030`. Six wire reads in order:
//!   `u_tcref`, `u_option u_string`, `u_bool`, `u_string`, `u_int`,
//!   `u_option u_ty`.
//!
//! Phase 6b4 supports both branches. Tag 0 (`VRefLocal`) reads a
//! compressed-int stamp index into the val OSGN table; we validate the
//! index against the pre-sized table and stash the raw stamp on
//! `PickledVRef::Local`. Resolution defers until projection time —
//! matching FCS's lazy `ValRef` cell semantics.
//!
//! Note we do not eagerly clone the linked `PickledVal` body into the
//! `Local` variant. The body sits in `PhaseOneState.ivals[stamp]`, and
//! callers either look it up post-walk or chase the index themselves.

use crate::error::ImportError;
use crate::fsharp_pickle::model::{PickledNonLocalValRef, PickledVRef};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::types::{read_tcref, read_ty};

/// `u_nonlocal_val_ref` (`TypedTreePickle.fs:2010-2030`). Six wire
/// reads make up the `NonLocalValOrMemberRef`:
/// 1. `u_tcref` — the enclosing entity.
/// 2. `u_option u_string` — `MemberParentMangledName`.
/// 3. `u_bool` — `MemberIsOverride`.
/// 4. `u_string` — `LogicalName`.
/// 5. `u_int` — `TotalArgCount`.
/// 6. `u_option u_ty` — disambiguating partial type.
///
/// Takes `&mut PhaseOneState` because `u_tcref` and `u_ty` are
/// state-aware (the former may resolve a Local tcref index against
/// the entity OSGN table; the latter recurses through `TType_forall`
/// which writes to the typar OSGN table).
pub(crate) fn read_nonlocal_val_ref(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledNonLocalValRef, ImportError> {
    let enclosing_entity = read_tcref(state)?;
    let member_parent_mangled_name =
        state.read_option("u_nonlocal_val_ref MemberParentMangledName", |s| {
            s.reader
                .read_string("u_nonlocal_val_ref MemberParentMangledName body")
        })?;
    let member_is_override = state
        .reader
        .read_bool("u_nonlocal_val_ref MemberIsOverride")?;
    let logical_name = state.reader.read_string("u_nonlocal_val_ref LogicalName")?;
    let total_arg_count = state
        .reader
        .read_uint32("u_nonlocal_val_ref TotalArgCount")?;
    let partial_type = state.read_option("u_nonlocal_val_ref partialType", read_ty)?;
    Ok(PickledNonLocalValRef {
        enclosing_entity,
        member_parent_mangled_name,
        member_is_override,
        logical_name,
        total_arg_count,
        partial_type,
    })
}

/// `u_vref` (`:2032-2038`). Tag 0 reads a `u_local_item_ref st.ivals`
/// — a compressed-int stamp index into the val OSGN table. Tag 1
/// reads a `u_nonlocal_val_ref`. We don't dereference the local stamp
/// here; the table entry may not be linked yet (the val body
/// containing this `VRefLocal` could be earlier in walk order than
/// the val it points at).
pub(crate) fn read_vref(state: &mut PhaseOneState<'_>) -> Result<PickledVRef, ImportError> {
    let tag = state.reader.read_byte("u_vref tag")?;
    match tag {
        0 => {
            let idx = state.reader.read_uint32("u_vref Local stamp")?;
            state.ivals.check_index_in_range(idx)?;
            Ok(PickledVRef::Local(idx))
        }
        1 => Ok(PickledVRef::NonLocal(read_nonlocal_val_ref(state)?)),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_vref tag",
            tag: u32::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{Nullness, PickledTcRef, PickledType};
    use crate::fsharp_pickle::reader::PickleReader;

    fn make_state<'a>(bytes: &'a [u8], strings: &'a [String]) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, 0)
    }

    fn make_state_vals<'a>(
        bytes: &'a [u8],
        strings: &'a [String],
        nvals: usize,
    ) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, nvals)
    }

    #[test]
    fn vref_nonlocal_minimal() {
        // tag 1, then NonLocalValRef:
        //   tcref = NonLocal(2)
        //   member_parent_mangled_name = None
        //   member_is_override = false
        //   logical_name = "foo"
        //   total_arg_count = 0
        //   partial_type = None
        let strings = vec!["foo".to_string()];
        let bytes = vec![
            1u8, // u_vref tag = NonLocal
            1u8, 2u8, // u_tcref NonLocal nleref idx 2
            0u8, // option None (mangled-parent-name)
            0u8, // bool false (is_override)
            0u8, // u_string idx 0 = "foo"
            0u8, // total_arg_count = 0
            0u8, // option None (partial_type)
        ];
        let mut s = make_state(&bytes, &strings);
        let v = read_vref(&mut s).unwrap();
        let PickledVRef::NonLocal(nl) = v else {
            panic!("expected NonLocal")
        };
        assert_eq!(nl.enclosing_entity, PickledTcRef::NonLocal(2));
        assert!(nl.member_parent_mangled_name.is_none());
        assert!(!nl.member_is_override);
        assert_eq!(nl.logical_name, "foo");
        assert_eq!(nl.total_arg_count, 0);
        assert!(nl.partial_type.is_none());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn vref_nonlocal_with_member_parent_and_partial_type() {
        // tag 1, NonLocalValRef:
        //   tcref = Local(0)
        //   member_parent_mangled_name = Some("MyClass")
        //   member_is_override = true
        //   logical_name = "Foo"
        //   total_arg_count = 2
        //   partial_type = Some(AppSimple(simpletyp_idx=1, ambivalent))
        //
        // The Local(0) tcref needs ntycons >= 1 in the state, since
        // `read_tcref` bounds-checks against the tycons table.
        let strings = vec!["MyClass".to_string(), "Foo".to_string()];
        let bytes = vec![
            1u8, // u_vref tag
            0u8, 0u8, // u_tcref Local stamp 0
            1u8, 0u8,  // option Some, string idx 0 = "MyClass"
            0x01, // bool true
            1u8,  // string idx 1 = "Foo"
            2u8,  // total_arg_count = 2
            1u8,  // option Some
            1u8,  // u_ty tag = AppSimple
            1u8,  // simpletyp idx 1 (B-stream absent ⇒ nullness=Ambivalent)
        ];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: &[Vec<u32>] = &[];
        r.attach_tables(&strings, pubpaths);
        let mut s = PhaseOneState::with_capacities(r, 1, 0, 0);
        let PickledVRef::NonLocal(nl) = read_vref(&mut s).unwrap() else {
            panic!("expected NonLocal")
        };
        assert_eq!(nl.enclosing_entity, PickledTcRef::Local(0));
        assert_eq!(nl.member_parent_mangled_name.as_deref(), Some("MyClass"));
        assert!(nl.member_is_override);
        assert_eq!(nl.logical_name, "Foo");
        assert_eq!(nl.total_arg_count, 2);
        assert_eq!(
            nl.partial_type,
            Some(PickledType::AppSimple {
                simpletyp_index: 1,
                nullness: Nullness::Ambivalent,
            }),
        );
        assert!(s.reader.is_eof());
    }

    #[test]
    fn vref_local_returns_index() {
        // tag 0, then compressed-int stamp = 5. ivals table sized 8.
        let strings: Vec<String> = vec![];
        let bytes = vec![0u8, 5u8];
        let mut s = make_state_vals(&bytes, &strings, 8);
        let v = read_vref(&mut s).unwrap();
        assert_eq!(v, PickledVRef::Local(5));
        assert!(s.reader.is_eof());
    }

    #[test]
    fn vref_local_out_of_range_errors() {
        // tag 0, stamp = 9, table sized 4.
        let strings: Vec<String> = vec![];
        let bytes = vec![0u8, 9u8];
        let mut s = make_state_vals(&bytes, &strings, 4);
        match read_vref(&mut s) {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "vals",
                index: 9,
                max: 4,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn vref_unknown_tag_errors() {
        let strings: Vec<String> = vec![];
        let bytes = vec![9u8];
        let mut s = make_state(&bytes, &strings);
        match read_vref(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_vref tag",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
