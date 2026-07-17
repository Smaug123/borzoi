//! Entity-side decoders: `u_entity_spec_data`, `u_entity_spec`,
//! `u_tcaug`, `u_modul_typ`, and the `walk_ccu_info` entry point that
//! drives the depth-first phase-1 walk.
//!
//! ### FCS source map
//!
//! - `u_tcaug`            — `TypedTreePickle.fs:3183-3211`. 9-tuple
//!   terminated by `u_space 1`.
//! - `u_entity_spec_data` — `:3128-3181`. 17-tuple. Field 8
//!   (`u_tycon_repr`) is a closure; field 13 (`u_int64` flags)
//!   carries `ReservedBitForPickleFormatTyconReprFlag` (`0x10` per
//!   `TypedTree.fs:435`) which discriminates the inner-tag-2 branch
//!   of `u_tycon_repr`. After decoding, the bit is cleared from the
//!   stored flags so consumers see the same shape FCS does.
//! - `u_entity_spec`      — `:3213-3214`. `u_osgn_decl st.ientities
//!   u_entity_spec_data`.
//! - `u_modul_typ`        — `:3333-3335`. `u_tup3 u_istype (u_qlist
//!   u_Val) (u_qlist u_entity_spec)`. `u_qlist` shares wire format
//!   with `u_list`. The body is wrapped in `u_lazy`, which
//!   pre-pends a 7×prim_u_int32 frame; we read the frame, decode the
//!   body inline, and assert the consumed byte-delta matches the
//!   recorded length.
//! - `unpickleCcuInfo`    — `:3978-3987`. `u_tup4
//!   unpickleModuleOrNamespace u_string u_bool (u_space 3)`. The
//!   root entity is itself a `u_entity_spec` osgn-decl, so we land
//!   the root index, then read the working-directory string, the
//!   `usesQuotations` flag, and consume the trailing 3 reserved
//!   zero bytes.

use crate::error::ImportError;
use crate::fsharp_pickle::access::{read_access, read_cpath, read_istype, read_range};
use crate::fsharp_pickle::attribs::read_attribs;
use crate::fsharp_pickle::leaves::{read_kind, read_xmldoc};
use crate::fsharp_pickle::model::{
    PickledEntity, PickledModulType, PickledOsgnTables, PickledTcAug, PickledType,
};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::repr::{read_exnc_repr, read_tycon_repr, resolve_repr};
use crate::fsharp_pickle::typar::read_tyar_specs;
use crate::fsharp_pickle::types::read_ty;
use crate::fsharp_pickle::val::read_val;
use crate::fsharp_pickle::vrefs::read_vref;

/// Phase-1 result. Bundles everything `unpickleCcuInfo` produces *other*
/// than the header — `unpickle_signature` retains ownership of the
/// header and assembles the final [`PickledCcu`] after the phase-1
/// state has been dropped (releasing the borrow on `header.strings` /
/// `header.pubpaths` that the reader held).
pub(crate) struct PhaseOneResult {
    pub(crate) root_entity: u32,
    pub(crate) compile_time_working_dir: String,
    pub(crate) uses_quotations: bool,
    pub(crate) tables: PickledOsgnTables,
}

/// Bit mask for `EntityFlags.ReservedBitForPickleFormatTyconReprFlag`
/// (`TypedTree.fs:435`). When set on the entity flags word, signals
/// that the `u_tycon_repr` inner-tag-2 branch should be interpreted
/// as a provider-generated type rather than `TAsmRepr`.
const RESERVED_BIT_FOR_PICKLE_FORMAT_TYCON_REPR_FLAG: i64 = 0b0000_0000_0001_0000;

/// `u_tcaug` (`TypedTreePickle.fs:3183-3211`). 9-tuple followed by
/// `u_space 1` (one reserved zero byte). The hash/equals-with-c slot
/// is a 3-tuple of vrefs; FCS upgrades it post-decode to a 4-tuple
/// with a `None`, but we keep the wire shape.
pub(crate) fn read_tcaug(state: &mut PhaseOneState<'_>) -> Result<PickledTcAug, ImportError> {
    let compare = state.read_option("u_tcaug compare", |s| {
        let a = read_vref(s)?;
        let b = read_vref(s)?;
        Ok((a, b))
    })?;
    let compare_withc = state.read_option("u_tcaug compare_withc", read_vref)?;
    let hash_and_equals_withc = state.read_option("u_tcaug hash_and_equals_withc", |s| {
        let a = read_vref(s)?;
        let b = read_vref(s)?;
        let c = read_vref(s)?;
        Ok((a, b, c))
    })?;
    let equals = state.read_option("u_tcaug equals", |s| {
        let a = read_vref(s)?;
        let b = read_vref(s)?;
        Ok((a, b))
    })?;
    let adhoc = state.read_array("u_tcaug adhoc element", |s| {
        let name = s.reader.read_string("u_tcaug adhoc name")?;
        let v = read_vref(s)?;
        Ok((name, v))
    })?;
    let interfaces = state.read_array("u_tcaug interfaces element", |s| {
        let ty = read_ty(s)?;
        let flag = s.reader.read_bool("u_tcaug interfaces flag")?;
        crate::fsharp_pickle::access::read_dummy_range(&mut s.reader)?;
        Ok((ty, flag))
    })?;
    let super_type: Option<PickledType> = state.read_option("u_tcaug super", read_ty)?;
    let is_abstract = state.reader.read_bool("u_tcaug is_abstract")?;
    state.reader.read_space(1, "u_tcaug reserved space")?;
    Ok(PickledTcAug {
        compare,
        compare_withc,
        hash_and_equals_withc,
        equals,
        adhoc,
        interfaces,
        super_type,
        is_abstract,
    })
}

/// `u_modul_typ` (`TypedTreePickle.fs:3333-3335`) wrapped in
/// `u_lazy` (`:797-810`). FCS's `u_lazy` reads seven 4-byte little-
/// endian framing ints (body length + six fixup positions the
/// unpickler discards), then decodes the body inline against the
/// same reader.
///
/// The frame header gives us the body length in bytes; we assert
/// that the inline decode advances the cursor by exactly that many
/// bytes (raises [`ImportError::MalformedPickleLazyFrame`] on
/// mismatch). This is the load-bearing check against drift between
/// our walker and the FCS wire layout.
fn read_lazy_modul_typ(state: &mut PhaseOneState<'_>) -> Result<PickledModulType, ImportError> {
    let len = state.reader.read_uint32_le("u_lazy len")?;
    let _otycons_idx1 = state.reader.read_uint32_le("u_lazy otyconsIdx1")?;
    let _otycons_idx2 = state.reader.read_uint32_le("u_lazy otyconsIdx2")?;
    let _otypars_idx1 = state.reader.read_uint32_le("u_lazy otyparsIdx1")?;
    let _otypars_idx2 = state.reader.read_uint32_le("u_lazy otyparsIdx2")?;
    let _ovals_idx1 = state.reader.read_uint32_le("u_lazy ovalsIdx1")?;
    let _ovals_idx2 = state.reader.read_uint32_le("u_lazy ovalsIdx2")?;
    let pos_before = state.reader.pos();
    let body = read_modul_typ(state)?;
    let pos_after = state.reader.pos();
    let consumed = (pos_after - pos_before) as u32;
    if consumed != len {
        return Err(ImportError::MalformedPickleLazyFrame {
            expected: len,
            actual: consumed,
        });
    }
    Ok(body)
}

/// `u_modul_typ` (`:3333-3335`). `u_tup3 u_istype (u_qlist u_Val)
/// (u_qlist u_entity_spec)`. `u_qlist` is wire-identical to `u_list`,
/// so we read both lists the same way.
pub(crate) fn read_modul_typ(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledModulType, ImportError> {
    let is_type = read_istype(&mut state.reader)?;
    let vals = state.read_array("u_modul_typ vals", read_val)?;
    let entities = state.read_array("u_modul_typ entities", read_entity_spec)?;
    Ok(PickledModulType {
        is_type,
        vals,
        entities,
    })
}

/// `u_entity_spec_data` (`:3128-3181`). 17-tuple in wire order. The
/// `u_tycon_repr` closure (field 8) is held as an intermediate; the
/// flag bit comes from field 13 (entity flags), so we read all 17
/// fields, then resolve the closure with the masked bit and strip
/// the bit from the stored flags.
pub(crate) fn read_entity_spec_data(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledEntity, ImportError> {
    let typars = read_tyar_specs(state)?;
    let logical_name = state.reader.read_string("u_entity_spec LogicalName")?;
    let compiled_name = state
        .reader
        .read_option("u_entity_spec CompiledName", |r| {
            r.read_string("u_entity_spec CompiledName body")
        })?;
    let range = read_range(&mut state.reader)?;
    let pub_path = state.reader.read_option("u_entity_spec PubPath", |r| {
        // u_pubpath: compressed-int index into pubpaths table.
        // We resolve through the attached table.
        crate::fsharp_pickle::access::read_pubpath(r)
    })?;
    let access = read_access(&mut state.reader)?;
    let repr_access = read_access(&mut state.reader)?;
    let attribs = read_attribs(state)?;
    let repr_closure = read_tycon_repr(state)?;
    let type_abbrev = state.read_option("u_entity_spec TypeAbbrev", read_ty)?;
    let tcaug = read_tcaug(state)?;
    let _dropped_string = state.reader.read_string("u_entity_spec _x10 (dropped)")?;
    let typar_kind = read_kind(&mut state.reader)?;
    let raw_flags = state.reader.read_int64("u_entity_spec flags")?;
    let cpath = state.read_option("u_entity_spec cpath", |s| read_cpath(&mut s.reader))?;
    let module_type = read_lazy_modul_typ(state)?;
    let exn_repr = read_exnc_repr(state)?;
    let xmldoc = state
        .reader
        .read_used_space1("u_entity_spec xmldoc", read_xmldoc)?;

    let flag_bit = (raw_flags & RESERVED_BIT_FOR_PICKLE_FORMAT_TYCON_REPR_FLAG) != 0;
    let repr = resolve_repr(repr_closure, flag_bit)?;
    // FCS clears the bit from the flags before storing
    // (`:3151`: `x11 &&& ~~~EntityFlags.ReservedBitForPickleFormatTyconReprFlag`).
    let flags = raw_flags & !RESERVED_BIT_FOR_PICKLE_FORMAT_TYCON_REPR_FLAG;

    Ok(PickledEntity {
        typars,
        logical_name,
        compiled_name,
        range,
        pub_path,
        access,
        repr_access,
        attribs,
        repr,
        type_abbrev,
        tcaug,
        typar_kind,
        flags,
        cpath,
        module_type,
        exn_repr,
        xmldoc,
    })
}

/// `u_entity_spec` (`:3213-3214`). Osgn-decl wrapper. Reads the stamp
/// index, validates against the entity OSGN table, decodes the body
/// inline, and links the slot. Returns the freshly-linked stamp
/// index so `u_modul_typ` (and the CCU root) can record stable
/// handles.
///
/// Depth-guarded: nested modules recurse `entity_spec → modul_typ →
/// entity_spec`, the most stack-expensive cycle in the walker (the
/// whole 17-field entity decode per level).
pub(crate) fn read_entity_spec(state: &mut PhaseOneState<'_>) -> Result<u32, ImportError> {
    state.reader.enter_recursion("u_entity_spec")?;
    let result = read_entity_spec_body(state);
    state.reader.exit_recursion();
    result
}

fn read_entity_spec_body(state: &mut PhaseOneState<'_>) -> Result<u32, ImportError> {
    let idx = state.reader.read_uint32("u_entity_spec osgn index")?;
    state.itycons.check_index_in_range(idx)?;
    let body = read_entity_spec_data(state)?;
    state.itycons.link(idx, body)
}

/// `unpickleCcuInfo` (`:3978-3987`). The phase-1 entry point: decode
/// the root entity, then the compile-time working directory, the
/// `usesQuotations` bool, and the three reserved zero bytes; check
/// that the primary stream is fully consumed; finalise the three
/// OSGN tables.
///
/// Consumes `state` by value because [`PhaseOneState::finalize`] is a
/// by-value method (it must move out the OSGN tables to validate that
/// every slot was linked). The caller (`unpickle_signature`) holds the
/// header in an enclosing scope and assembles the [`PickledCcu`] once
/// this function has returned and the reader's borrow on the header
/// has been released.
pub(crate) fn walk_ccu_info(mut state: PhaseOneState<'_>) -> Result<PhaseOneResult, ImportError> {
    let root_entity = read_entity_spec(&mut state)?;
    let compile_time_working_dir = state.reader.read_string("CcuInfo compileTimeWorkingDir")?;
    let uses_quotations = state.reader.read_bool("CcuInfo usesQuotations")?;
    state.reader.read_space(3, "CcuInfo reserved space")?;
    state
        .reader
        .expect_eof("phase 1: trailing bytes after CcuInfo")?;
    state
        .reader
        .expect_eof_b("phase 1: trailing bytes in B stream after CcuInfo")?;
    let tables = state.finalize()?;
    Ok(PhaseOneResult {
        root_entity,
        compile_time_working_dir,
        uses_quotations,
        tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{PickledExnRepr, PickledHeader, PickledTyconRepr};
    use crate::fsharp_pickle::reader::PickleReader;

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
    fn tcaug_empty_round_trip() {
        // All options None, all lists empty, no super_type, abstract=false.
        // Trailing u_space 1 (one zero byte).
        let strings: Vec<String> = vec![];
        let bytes = vec![
            0u8, // compare = None
            0u8, // compare_withc = None
            0u8, // hash_and_equals_withc = None
            0u8, // equals = None
            0u8, // adhoc list len 0
            0u8, // interfaces list len 0
            0u8, // super = None
            0u8, // is_abstract = false
            0u8, // u_space 1
        ];
        let mut s = make_state_caps(&bytes, &strings, 0, 0, 0);
        let aug = read_tcaug(&mut s).unwrap();
        assert!(aug.compare.is_none());
        assert!(aug.compare_withc.is_none());
        assert!(aug.hash_and_equals_withc.is_none());
        assert!(aug.equals.is_none());
        assert!(aug.adhoc.is_empty());
        assert!(aug.interfaces.is_empty());
        assert!(aug.super_type.is_none());
        assert!(!aug.is_abstract);
        assert!(s.reader.is_eof());
    }

    #[test]
    fn modul_typ_module_zero_vals_zero_entities() {
        // is_type = ModuleWithSuffix (tag 0), vals 0, entities 0.
        let strings: Vec<String> = vec![];
        let bytes = vec![0u8, 0u8, 0u8];
        let mut s = make_state_caps(&bytes, &strings, 0, 0, 0);
        let m = read_modul_typ(&mut s).unwrap();
        assert!(m.vals.is_empty());
        assert!(m.entities.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn lazy_modul_typ_round_trip() {
        // Body bytes: is_type=0, vals=0, entities=0. Body length = 3.
        // 7×u32_le header: len=3, then 6 zeroes for fixup positions.
        let strings: Vec<String> = vec![];
        let mut bytes = vec![];
        // len = 3 LE
        bytes.extend_from_slice(&3u32.to_le_bytes());
        for _ in 0..6 {
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        // body
        bytes.extend_from_slice(&[0u8, 0u8, 0u8]);
        let mut s = make_state_caps(&bytes, &strings, 0, 0, 0);
        let m = read_lazy_modul_typ(&mut s).unwrap();
        assert!(m.vals.is_empty());
        assert!(m.entities.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn lazy_modul_typ_length_mismatch_errors() {
        // Frame says len = 4 but the body is only 3 bytes long.
        let strings: Vec<String> = vec![];
        let mut bytes = vec![];
        bytes.extend_from_slice(&4u32.to_le_bytes());
        for _ in 0..6 {
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        bytes.extend_from_slice(&[0u8, 0u8, 0u8]); // body decodes as 3 bytes
        let mut s = make_state_caps(&bytes, &strings, 0, 0, 0);
        match read_lazy_modul_typ(&mut s) {
            Err(ImportError::MalformedPickleLazyFrame {
                expected: 4,
                actual: 3,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Build an `u_entity_spec_data` payload for an empty top-level
    /// module: no typars, name "M", no compiled name, range = file
    /// idx 1 / (1,0)-(1,1), no pubpath, public access, public repr
    /// access, no attribs, NoRepr (outer tag 0), no abbrev, empty
    /// tcaug, dropped string idx 1 = "", typar_kind Type, flags=0,
    /// no cpath, empty modul_typ, exn None, no xmldoc.
    fn empty_module_payload(strings: &[String]) -> Vec<u8> {
        let _ = strings; // strings supplied separately; indices baked in below.
        let mut p = vec![
            0u8, // typars list len 0
            0u8, // logical_name idx 0 = "M"
            0u8, // compiled_name = None
            1u8, 1u8, 0u8, 1u8, 1u8, // range
            0u8, // pub_path = None
            0u8, // access (empty)
            0u8, // repr_access (empty)
            0u8, // attribs len 0
            0u8, // u_tycon_repr outer tag 0 = NoRepr
            0u8, // type_abbrev = None
            // tcaug: 9 options/lists + space
            0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 1u8, // _dropped string idx 1 = ""
            0u8, // u_kind = Type
            0u8, 0u8, // flags = 0i64 (two compressed-int zeros: lo, hi)
            0u8, // cpath = None
        ];
        // lazy modul_typ: 7×u32_le frame + body (0,0,0)
        p.extend_from_slice(&3u32.to_le_bytes());
        for _ in 0..6 {
            p.extend_from_slice(&0u32.to_le_bytes());
        }
        p.extend_from_slice(&[0u8, 0u8, 0u8]);
        p.extend(vec![
            3u8, // exn_repr = None (tag 3)
            0u8, // xmldoc used_space1 = None
        ]);
        p
    }

    #[test]
    fn entity_spec_data_empty_module() {
        let strings = vec!["M".to_string(), "".to_string()];
        let bytes = empty_module_payload(&strings);
        let mut s = make_state_caps(&bytes, &strings, 0, 0, 0);
        let e = read_entity_spec_data(&mut s).unwrap();
        assert_eq!(e.logical_name, "M");
        assert!(e.compiled_name.is_none());
        assert!(e.attribs.is_empty());
        assert_eq!(e.repr, PickledTyconRepr::NoRepr);
        assert!(e.type_abbrev.is_none());
        assert!(matches!(e.exn_repr, PickledExnRepr::None));
        assert!(e.xmldoc.is_none());
        assert_eq!(e.flags, 0);
        assert!(s.reader.is_eof());
    }

    #[test]
    fn entity_spec_data_format_bit_masked_from_flags() {
        // Flags = 0x10 (ReservedBitForPickleFormatTyconReprFlag). With
        // NoRepr (outer tag 0), the flag bit is consumed only for
        // masking; the resolved repr is unaffected.
        let strings = vec!["M".to_string(), "".to_string()];
        let mut p = vec![
            0u8, // typars
            0u8, // logical_name = "M"
            0u8, // compiled_name = None
            1u8, 1u8, 0u8, 1u8, 1u8, // range
            0u8, // pub_path = None
            0u8, // access
            0u8, // repr_access
            0u8, // attribs
            0u8, // tycon_repr NoRepr
            0u8, // type_abbrev = None
            0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, // tcaug
            1u8, // dropped string
            0u8, // typar_kind
            // flags = 0x10 (compressed: lo word = 0x10, hi word = 0)
            0x10, 0u8, 0u8, // cpath = None
        ];
        p.extend_from_slice(&3u32.to_le_bytes());
        for _ in 0..6 {
            p.extend_from_slice(&0u32.to_le_bytes());
        }
        p.extend_from_slice(&[0u8, 0u8, 0u8]); // modul_typ body
        p.extend(vec![3u8, 0u8]); // exn None, xmldoc None
        let mut s = make_state_caps(&p, &strings, 0, 0, 0);
        let e = read_entity_spec_data(&mut s).unwrap();
        // Flag bit cleared from stored flags.
        assert_eq!(e.flags, 0);
        assert_eq!(e.repr, PickledTyconRepr::NoRepr);
    }

    #[test]
    fn entity_spec_osgn_decl_links_root() {
        // osgn idx 0 + empty module body. ntycons = 1.
        let strings = vec!["M".to_string(), "".to_string()];
        let mut bytes = vec![0u8]; // osgn idx 0
        bytes.extend(empty_module_payload(&strings));
        let mut s = make_state_caps(&bytes, &strings, 1, 0, 0);
        let idx = read_entity_spec(&mut s).unwrap();
        assert_eq!(idx, 0);
        assert!(s.itycons.get(0).is_ok());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn walk_ccu_info_empty_module() {
        // Root entity osgn-decl + ccu trailer.
        let strings = vec!["M".to_string(), "".to_string(), "/tmp/work".to_string()];
        let mut bytes = vec![0u8]; // osgn idx 0
        bytes.extend(empty_module_payload(&strings));
        // CcuInfo trailer:
        bytes.push(2u8); // compile_time_working_dir idx 2 = "/tmp/work"
        bytes.push(0u8); // uses_quotations = false
        bytes.extend([0u8, 0u8, 0u8]); // u_space 3
        let s = make_state_caps(&bytes, &strings, 1, 0, 0);
        let _header_unused = PickledHeader {
            ccu_refs: vec![],
            ntycons: 1,
            ntypars: 0,
            nvals: 0,
            nanoninfos: 0,
            strings: strings.clone(),
            pubpaths: vec![],
            nlerefs: vec![],
            simpletys: vec![],
            phase1_bytes: vec![],
        };
        let res = walk_ccu_info(s).unwrap();
        assert_eq!(res.root_entity, 0);
        assert_eq!(res.compile_time_working_dir, "/tmp/work");
        assert!(!res.uses_quotations);
        assert_eq!(res.tables.tycons.len(), 1);
        assert_eq!(res.tables.tycons[0].logical_name, "M");
    }
}
