//! Val-side decoders: `u_ValData`, `u_Val`, `u_member_info`,
//! `u_MemberFlags`, `u_member_kind`, `u_ValReprInfo`, `u_TyparReprInfo`,
//! `u_ArgReprInfo`, `u_slotsig`, `u_slotparam`, `u_ranges`,
//! `u_parentref`.
//!
//! ### FCS source map
//!
//! - `u_member_kind`    — `TypedTreePickle.fs:2066-2073`. 5-tag byte.
//! - `u_MemberFlags`    — `:2086-2097`. `u_tup6 u_bool u_bool u_bool
//!   u_bool u_bool u_member_kind`; the *second* wire bool is
//!   `_x3UnusedBoolInFormat` and is dropped.
//! - `u_TyparReprInfo`  — `:2619-2622`. `(u_ident, u_kind)`.
//! - `u_ArgReprInfo`    — `:2606-2617`. `(u_attribs, u_option u_ident)`
//!   — state-aware because `u_attribs` may recurse through `u_expr`,
//!   `u_ty`, etc.
//! - `u_ValReprInfo`    — `:2624-2628`. `(u_list u_TyparReprInfo, u_list
//!   (u_list u_ArgReprInfo), u_ArgReprInfo)`.
//! - `u_ranges`         — `:2641`. `u_option (u_tup2 u_range u_range)`.
//! - `u_slotparam`      — `:3926-3930`. `u_tup6 (u_option u_string) u_ty
//!   u_bool u_bool u_bool u_attribs` — three bools are
//!   `(is_in_arg, is_out_arg, is_optional)`.
//! - `u_slotsig`        — `:3932-3936`. `u_tup6 u_string u_ty
//!   u_tyar_specs u_tyar_specs (u_list (u_list u_slotparam)) (u_option
//!   u_ty)`. The two `u_tyar_specs` runs publish into the typar OSGN
//!   table, so this decoder is state-aware end-to-end.
//! - `u_member_info`    — `:3246-3254`. `u_tup4 u_tcref u_MemberFlags
//!   (u_list u_slotsig) u_bool`.
//! - `u_parentref`      — `:3216-3222`. 2-tag byte.
//! - `u_ValData`        — `:3278-3329`. 13-tuple in wire order; the
//!   `u_ranges` field expands to the model's `(range, other_range)`
//!   pair. The trailing `u_used_space1 u_xmldoc` is the
//!   "extended in-memory format" xmldoc marker — `None` for the
//!   normal cross-CCU pickle.
//! - `u_Val`            — `:3331`. `u_osgn_decl st.ivals u_ValData`.
//!   The osgn-decl wrapper reads the stamp index, validates against
//!   the val OSGN table, decodes the body inline, and links the slot.

use crate::error::ImportError;
use crate::fsharp_pickle::access::{read_access, read_range};
use crate::fsharp_pickle::attribs::read_attribs;
use crate::fsharp_pickle::consts::read_const;
use crate::fsharp_pickle::leaves::{read_kind, read_xmldoc};
use crate::fsharp_pickle::model::{
    PickledArgReprInfo, PickledMemberFlags, PickledMemberInfo, PickledMemberKind, PickledParentRef,
    PickledRange, PickledSlotParam, PickledSlotSig, PickledTyparReprInfo, PickledVal,
    PickledValReprInfo,
};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::reader::PickleReader;
use crate::fsharp_pickle::typar::{read_ident, read_tyar_specs};
use crate::fsharp_pickle::types::{read_tcref, read_ty};

/// `u_member_kind` (`TypedTreePickle.fs:2066-2073`).
pub(crate) fn read_member_kind(
    reader: &mut PickleReader<'_>,
) -> Result<PickledMemberKind, ImportError> {
    let tag = reader.read_byte("u_member_kind tag")?;
    Ok(match tag {
        0 => PickledMemberKind::Member,
        1 => PickledMemberKind::PropertyGet,
        2 => PickledMemberKind::PropertySet,
        3 => PickledMemberKind::Constructor,
        4 => PickledMemberKind::ClassConstructor,
        other => {
            return Err(ImportError::UnsupportedPickleTag {
                context: "u_member_kind",
                tag: u32::from(other),
            });
        }
    })
}

/// `u_MemberFlags` (`:2086-2097`). The 2nd wire bool is
/// `_x3UnusedBoolInFormat` — FCS reads it but never uses it; we
/// drop it on the floor.
pub(crate) fn read_member_flags(
    reader: &mut PickleReader<'_>,
) -> Result<PickledMemberFlags, ImportError> {
    let is_instance = reader.read_bool("u_MemberFlags IsInstance")?;
    let _x3_unused = reader.read_bool("u_MemberFlags _x3UnusedBoolInFormat")?;
    let is_dispatch_slot = reader.read_bool("u_MemberFlags IsDispatchSlot")?;
    let is_override_or_explicit_impl =
        reader.read_bool("u_MemberFlags IsOverrideOrExplicitImpl")?;
    let is_final = reader.read_bool("u_MemberFlags IsFinal")?;
    let kind = read_member_kind(reader)?;
    Ok(PickledMemberFlags {
        is_instance,
        is_dispatch_slot,
        is_override_or_explicit_impl,
        is_final,
        kind,
    })
}

/// `u_TyparReprInfo` (`:2619-2622`).
pub(crate) fn read_typar_repr_info(
    reader: &mut PickleReader<'_>,
) -> Result<PickledTyparReprInfo, ImportError> {
    let ident = read_ident(reader)?;
    let kind = read_kind(reader)?;
    Ok(PickledTyparReprInfo { ident, kind })
}

/// `u_ArgReprInfo` (`:2606-2617`). `(u_attribs, u_option u_ident)` —
/// state-aware because `u_attribs` transitively reaches `u_expr` /
/// `u_ty` (which may write to the typar OSGN table via Forall).
pub(crate) fn read_arg_repr_info(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledArgReprInfo, ImportError> {
    let attribs = read_attribs(state)?;
    let name = state.read_option("u_ArgReprInfo Name", |s| read_ident(&mut s.reader))?;
    Ok(PickledArgReprInfo { attribs, name })
}

/// `u_ValReprInfo` (`:2624-2628`).
pub(crate) fn read_val_repr_info(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledValReprInfo, ImportError> {
    let typar_repr = state.read_array("u_ValReprInfo typar_repr", |s| {
        read_typar_repr_info(&mut s.reader)
    })?;
    let arg_repr = state.read_array("u_ValReprInfo arg_repr outer", |outer| {
        outer.read_array("u_ValReprInfo arg_repr inner", read_arg_repr_info)
    })?;
    let return_repr = read_arg_repr_info(state)?;
    Ok(PickledValReprInfo {
        typar_repr,
        arg_repr,
        return_repr,
    })
}

/// `u_ranges` (`TypedTreePickle.fs:2641`). `u_option (u_tup2 u_range
/// u_range)`. We split the pair into `(range, other_range)` on the
/// model side; `None` ⇒ both `None`.
pub(crate) fn read_ranges(
    reader: &mut PickleReader<'_>,
) -> Result<(Option<PickledRange>, Option<PickledRange>), ImportError> {
    let pair = reader.read_option("u_ranges", |r| {
        let a = read_range(r)?;
        let b = read_range(r)?;
        Ok((a, b))
    })?;
    Ok(match pair {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    })
}

/// `u_slotparam` (`:3926-3930`).
pub(crate) fn read_slotparam(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledSlotParam, ImportError> {
    let name = state.reader.read_option("u_slotparam Name", |r| {
        r.read_string("u_slotparam Name body")
    })?;
    let ty = read_ty(state)?;
    let is_in_arg = state.reader.read_bool("u_slotparam IsInArg")?;
    let is_out_arg = state.reader.read_bool("u_slotparam IsOutArg")?;
    let is_optional = state.reader.read_bool("u_slotparam IsOptional")?;
    let attribs = read_attribs(state)?;
    Ok(PickledSlotParam {
        name,
        ty,
        is_in_arg,
        is_out_arg,
        is_optional,
        attribs,
    })
}

/// `u_slotsig` (`:3932-3936`).
pub(crate) fn read_slotsig(state: &mut PhaseOneState<'_>) -> Result<PickledSlotSig, ImportError> {
    let name = state.reader.read_string("u_slotsig Name")?;
    let implemented_ty = read_ty(state)?;
    let class_typars = read_tyar_specs(state)?;
    let method_typars = read_tyar_specs(state)?;
    let params = state.read_array("u_slotsig params outer", |outer| {
        outer.read_array("u_slotsig params inner", read_slotparam)
    })?;
    let return_ty = state.read_option("u_slotsig ReturnTy", read_ty)?;
    Ok(PickledSlotSig {
        name,
        implemented_ty,
        class_typars,
        method_typars,
        params,
        return_ty,
    })
}

/// `u_member_info` (`:3246-3254`).
pub(crate) fn read_member_info(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledMemberInfo, ImportError> {
    let apparent_parent = read_tcref(state)?;
    let flags = read_member_flags(&mut state.reader)?;
    let implemented_slots = state.read_array("u_member_info ImplementedSlotSigs", read_slotsig)?;
    let is_implemented = state.reader.read_bool("u_member_info IsImplemented")?;
    Ok(PickledMemberInfo {
        apparent_parent,
        flags,
        implemented_slots,
        is_implemented,
    })
}

/// `u_parentref` (`:3216-3222`). Tag 0 → `None`; tag 1 → `Parent(tcref)`.
/// Threads `PhaseOneState` because the `Parent` branch decodes through
/// `u_tcref`, whose `Local` arm validates against the entity OSGN table.
pub(crate) fn read_parentref(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledParentRef, ImportError> {
    let tag = state.reader.read_byte("u_parentref tag")?;
    Ok(match tag {
        0 => PickledParentRef::None,
        1 => PickledParentRef::Parent(read_tcref(state)?),
        other => {
            return Err(ImportError::UnsupportedPickleTag {
                context: "u_parentref",
                tag: u32::from(other),
            });
        }
    })
}

/// `u_ValData` (`:3278-3329`). 13-tuple in wire order; `u_ranges`
/// expands to the model's `(range, other_range)` pair, and the
/// trailing `u_used_space1 u_xmldoc` produces the optional `xmldoc`
/// marker.
pub(crate) fn read_val_data(state: &mut PhaseOneState<'_>) -> Result<PickledVal, ImportError> {
    let logical_name = state.reader.read_string("u_ValData LogicalName")?;
    let compiled_name = state.reader.read_option("u_ValData CompiledName", |r| {
        r.read_string("u_ValData CompiledName body")
    })?;
    let (range, other_range) = read_ranges(&mut state.reader)?;
    let ty = read_ty(state)?;
    let flags = state.reader.read_int64("u_ValData flags")?;
    let member_info = state.read_option("u_ValData MemberInfo", read_member_info)?;
    let attribs = read_attribs(state)?;
    let repr_info = state.read_option("u_ValData ValReprInfo", read_val_repr_info)?;
    let xmldoc_sig = state.reader.read_string("u_ValData XmlDocSig")?;
    let access = read_access(&mut state.reader)?;
    let parent = read_parentref(state)?;
    let literal_value = state
        .reader
        .read_option("u_ValData LiteralValue", read_const)?;
    let xmldoc = state
        .reader
        .read_used_space1("u_ValData xmldoc", read_xmldoc)?;
    Ok(PickledVal {
        logical_name,
        compiled_name,
        range,
        other_range,
        ty,
        flags,
        member_info,
        attribs,
        repr_info,
        xmldoc_sig,
        access,
        parent,
        literal_value,
        xmldoc,
    })
}

/// `u_Val` (`:3331`). Reads the osgn-decl prefix (compressed-int
/// stamp), decodes the body, and links the slot. Returns the freshly-
/// linked stamp index so callers (`u_modul_typ` → vals list) can
/// record stable handles into the val table.
pub(crate) fn read_val(state: &mut PhaseOneState<'_>) -> Result<u32, ImportError> {
    let idx = state.reader.read_uint32("u_Val osgn index")?;
    state.ivals.check_index_in_range(idx)?;
    let body = read_val_data(state)?;
    state.ivals.link(idx, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{Nullness, PickledTcRef, PickledType};

    fn make_state<'a>(bytes: &'a [u8], strings: &'a [String]) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, 0)
    }

    fn make_state_caps<'a>(
        bytes: &'a [u8],
        strings: &'a [String],
        ntycons: usize,
        ntypars: usize,
        nvals: usize,
    ) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, ntycons, ntypars, nvals)
    }

    #[test]
    fn member_kind_round_trip() {
        for (tag, expected) in [
            (0u8, PickledMemberKind::Member),
            (1, PickledMemberKind::PropertyGet),
            (2, PickledMemberKind::PropertySet),
            (3, PickledMemberKind::Constructor),
            (4, PickledMemberKind::ClassConstructor),
        ] {
            let bytes = [tag];
            let mut r = PickleReader::new(&bytes);
            assert_eq!(read_member_kind(&mut r).unwrap(), expected);
        }
    }

    #[test]
    fn member_kind_unknown_errors() {
        let bytes = [7u8];
        let mut r = PickleReader::new(&bytes);
        match read_member_kind(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_member_kind",
                tag: 7,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn member_flags_drops_unused_bool() {
        // (is_instance=true, _x3=false, is_dispatch_slot=true,
        //  is_override=false, is_final=true, kind=PropertyGet)
        let bytes = vec![1u8, 0u8, 1u8, 0u8, 1u8, 1u8];
        let mut r = PickleReader::new(&bytes);
        let f = read_member_flags(&mut r).unwrap();
        assert!(f.is_instance);
        assert!(f.is_dispatch_slot);
        assert!(!f.is_override_or_explicit_impl);
        assert!(f.is_final);
        assert_eq!(f.kind, PickledMemberKind::PropertyGet);
    }

    #[test]
    fn typar_repr_info_round_trip() {
        // u_ident: name idx 0 = "T", range = file 1, (1,0)-(1,1).
        // u_kind: 0 = Type.
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let bytes = vec![0u8, 1u8, 1u8, 0u8, 1u8, 1u8, 0u8];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: &[Vec<u32>] = &[];
        r.attach_tables(&strings, pubpaths);
        let info = read_typar_repr_info(&mut r).unwrap();
        assert_eq!(info.ident.name, "T");
        assert_eq!(info.kind, crate::fsharp_pickle::model::TyparKind::Type);
        assert!(r.is_eof());
    }

    #[test]
    fn arg_repr_info_empty() {
        // u_attribs len 0, u_option ident = None.
        let bytes = vec![0u8, 0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let info = read_arg_repr_info(&mut s).unwrap();
        assert!(info.attribs.is_empty());
        assert!(info.name.is_none());
    }

    #[test]
    fn val_repr_info_three_typars_one_arg_group() {
        // typars: 3 typar_repr_info; args: 1 group of 0 ArgReprInfos; return: empty.
        // Each typar_repr_info: u_ident (string idx 0 + range) + u_kind=Type (0).
        let strings = vec!["T".to_string(), "src.fs".to_string()];
        let mut bytes = vec![3u8]; // list length 3 (typar_repr_info)
        for _ in 0..3 {
            bytes.extend([0u8, 1u8, 1u8, 0u8, 1u8, 1u8, 0u8]); // ident + kind=Type
        }
        bytes.extend([1u8, 0u8]); // outer list len 1, inner list len 0
        bytes.extend([0u8, 0u8]); // return ArgReprInfo: empty attribs, no name
        let mut s = make_state(&bytes, &strings);
        let info = read_val_repr_info(&mut s).unwrap();
        assert_eq!(info.typar_repr.len(), 3);
        assert_eq!(info.arg_repr.len(), 1);
        assert!(info.arg_repr[0].is_empty());
        assert!(info.return_repr.attribs.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn ranges_none_yields_two_nones() {
        let bytes = vec![0u8]; // option None
        let mut r = PickleReader::new(&bytes);
        let (a, b) = read_ranges(&mut r).unwrap();
        assert!(a.is_none());
        assert!(b.is_none());
    }

    #[test]
    fn ranges_some_yields_two_somes() {
        let strings = vec!["a".to_string(), "src.fs".to_string()];
        // option Some, then range1 (file 1, (1,0)-(1,1)), then range2 (file 1, (2,0)-(2,5))
        let bytes = vec![
            1u8, // option Some
            1u8, 1u8, 0u8, 1u8, 1u8, // range 1
            1u8, 2u8, 0u8, 2u8, 5u8, // range 2
        ];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: &[Vec<u32>] = &[];
        r.attach_tables(&strings, pubpaths);
        let (a, b) = read_ranges(&mut r).unwrap();
        assert!(a.is_some());
        assert!(b.is_some());
        assert_eq!(a.unwrap().start.line, 1);
        assert_eq!(b.unwrap().end.column, 5);
    }

    #[test]
    fn parentref_none() {
        let bytes = vec![0u8];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 0, 0, 0);
        assert_eq!(read_parentref(&mut s).unwrap(), PickledParentRef::None);
    }

    #[test]
    fn parentref_parent_with_nle() {
        // tag 1 then u_tcref NonLocal idx 2.
        let bytes = vec![1u8, 1u8, 2u8];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 0, 0, 0);
        assert_eq!(
            read_parentref(&mut s).unwrap(),
            PickledParentRef::Parent(PickledTcRef::NonLocal(2))
        );
    }

    #[test]
    fn parentref_unknown_tag_errors() {
        let bytes = vec![9u8];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 0, 0, 0);
        match read_parentref(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_parentref",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn val_data_minimal_round_trip() {
        // logical_name="x", no compiled_name, no ranges, ty=AppSimple(0,ambivalent),
        // flags=0, no member_info, no attribs, no repr_info,
        // xmldoc_sig="", empty access, parent=None, no literal, no xmldoc.
        let strings = vec!["x".to_string(), "".to_string()];
        let bytes = vec![
            0u8, // logical_name idx 0 = "x"
            0u8, // option compiled_name = None
            0u8, // option ranges = None
            1u8, 0u8, // u_ty: tag 1 (AppSimple), simpletyp idx 0; no B stream ⇒ ambivalent
            0u8, 0u8, // u_int64 flags = 0 (two compressed-int zeros)
            0u8, // option member_info = None
            0u8, // attribs list len 0
            0u8, // option repr_info = None
            1u8, // xmldoc_sig idx 1 = ""
            0u8, // access list len 0 ⇒ public
            0u8, // parentref tag 0 = ParentNone
            0u8, // option literal = None
            0u8, // u_used_space1 = None
        ];
        let mut s = make_state(&bytes, &strings);
        let v = read_val_data(&mut s).unwrap();
        assert_eq!(v.logical_name, "x");
        assert!(v.compiled_name.is_none());
        assert!(v.range.is_none());
        assert!(v.other_range.is_none());
        assert!(matches!(
            v.ty,
            PickledType::AppSimple {
                simpletyp_index: 0,
                nullness: Nullness::Ambivalent
            }
        ));
        assert_eq!(v.flags, 0);
        assert!(v.member_info.is_none());
        assert!(v.attribs.is_empty());
        assert!(v.repr_info.is_none());
        assert_eq!(v.xmldoc_sig, "");
        assert!(v.access.is_empty());
        assert_eq!(v.parent, PickledParentRef::None);
        assert!(v.literal_value.is_none());
        assert!(v.xmldoc.is_none());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn u_val_osgn_decl_links_slot() {
        // Stamp 0 of a 1-slot val table. Body is the minimal val_data above.
        let strings = vec!["x".to_string(), "".to_string()];
        let bytes = vec![
            0u8, // osgn idx 0
            // val_data (15 bytes, mirroring val_data_minimal_round_trip):
            0u8, // logical_name idx 0 = "x"
            0u8, // compiled_name None
            0u8, // ranges None
            1u8, 0u8, // u_ty AppSimple(0)
            0u8, 0u8, // flags = 0 (compressed)
            0u8, // member_info None
            0u8, // attribs len 0
            0u8, // repr_info None
            1u8, // xmldoc_sig idx 1 = ""
            0u8, // access None
            0u8, // parent ParentNone
            0u8, // literal None
            0u8, // used_space1 None
        ];
        let mut s = make_state_caps(&bytes, &strings, 0, 0, 1);
        let idx = read_val(&mut s).unwrap();
        assert_eq!(idx, 0);
        assert!(s.ivals.get(0).is_ok());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn slotparam_round_trip() {
        // name=Some("p"), ty=AppSimple(0), is_in=false, is_out=false, is_opt=false,
        // attribs len 0.
        let strings = vec!["p".to_string()];
        let bytes = vec![
            1u8, 0u8, // option Some, string idx 0 = "p"
            1u8, 0u8, // u_ty AppSimple, simpletyp 0
            0u8, 0u8, 0u8, // 3 bools
            0u8, // attribs len 0
        ];
        let mut s = make_state(&bytes, &strings);
        let p = read_slotparam(&mut s).unwrap();
        assert_eq!(p.name.as_deref(), Some("p"));
        assert!(!p.is_in_arg);
        assert!(!p.is_out_arg);
        assert!(!p.is_optional);
        assert!(p.attribs.is_empty());
        assert!(s.reader.is_eof());
    }
}
