//! Tycon representation decoders: `u_tycon_repr`, `u_tycon_objmodel_data`,
//! `u_tycon_objmodel_kind`, `u_unioncase_spec`, `u_recdfield_spec`,
//! `u_rfield_table`, `u_exnc_repr`, plus the `u_attribs_ext u_xmldoc`
//! helper used by both record-field and union-case specs.
//!
//! ### FCS source map
//!
//! - `u_list_ext`              — `TypedTreePickle.fs:764-774`. Length
//!   prefix with the high bit (`0x80000000`) signalling that an
//!   `extra` item precedes the body.
//! - `u_attribs_ext extra`     — `:3052`. `u_list_ext extra u_attrib`.
//! - `u_recdfield_spec`        — `:3093-3123`. 11-tuple in wire order:
//!   `is_mutable, is_volatile, ty, is_static, is_secret, option const,
//!   ident, (xmldoc + property attribs via u_attribs_ext), field
//!   attribs, xmldoc sig, access`.
//! - `u_rfield_table`          — `:3125-3126`. `u_list u_recdfield_spec`.
//! - `u_unioncase_spec`        — `:3054-3076`. 7 wire reads; the
//!   middle `u_string` (deprecated compiled name) is dropped by FCS
//!   and by us.
//! - `u_tycon_objmodel_kind`   — `:3256-3267`. 7-tag dispatcher.
//! - `u_tycon_objmodel_data`   — `:3042-3050`. `u_tup3
//!   u_tycon_objmodel_kind u_vrefs u_rfield_table`.
//! - `u_tycon_repr`            — `:2961-3040`. Closure-returning
//!   dispatcher; the closure is resolved against `flag_bit` (entity
//!   flags `ReservedBitForPickleFormatTyconReprFlag` = `0x10` per
//!   `TypedTree.fs:435`) at `u_entity_spec_data` time. We mirror that
//!   by decoding into an intermediate `PickledTyconReprClosure` and
//!   resolving it post-hoc via `resolve_repr`.
//! - `u_exnc_repr`             — `:3078-3086`. 4-tag dispatcher.

use crate::error::ImportError;
use crate::fsharp_pickle::access::read_access;
use crate::fsharp_pickle::attribs::read_attrib;
use crate::fsharp_pickle::consts::read_const;
use crate::fsharp_pickle::il::{read_il_type, read_il_type_ref};
use crate::fsharp_pickle::leaves::read_xmldoc;
use crate::fsharp_pickle::model::{
    PickledAttribute, PickledExnRepr, PickledILType, PickledRecdField, PickledTyconObjModelData,
    PickledTyconObjModelKind, PickledTyconRepr, PickledType, PickledUnionCase, PickledXmlDoc,
};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::types::{read_tcref, read_ty};
use crate::fsharp_pickle::val::read_slotsig;
use crate::fsharp_pickle::vrefs::read_vref;

/// `u_attribs_ext u_xmldoc` (`TypedTreePickle.fs:3052,764-774`). Reads
/// a length-prefixed list of `u_attrib`s; if the length's high bit is
/// set, an `u_xmldoc` extra precedes the body. Returns `(extra,
/// attribs)`.
///
/// State-aware because `u_attrib` recurses through `u_attribkind` /
/// `u_attrib_expr` / `u_ty`, any of which may populate the typar OSGN
/// table via `TType_forall`.
pub(crate) fn read_attribs_ext(
    state: &mut PhaseOneState<'_>,
    context: &'static str,
) -> Result<(Option<PickledXmlDoc>, Vec<PickledAttribute>), ImportError> {
    let n_raw = state.reader.read_uint32(context)?;
    let has_extra = (n_raw & 0x8000_0000) != 0;
    let extra = if has_extra {
        Some(read_xmldoc(&mut state.reader)?)
    } else {
        None
    };
    let real_len = (n_raw & 0x7FFF_FFFF) as usize;
    let remaining = state.reader.remaining();
    if real_len > remaining {
        return Err(ImportError::MalformedPickleHeader {
            detail: format!(
                "{context}: ext list length {real_len} exceeds remaining bytes {remaining}"
            ),
        });
    }
    let mut out = Vec::with_capacity(real_len);
    for _ in 0..real_len {
        out.push(read_attrib(state)?);
    }
    Ok((extra, out))
}

/// `u_recdfield_spec` (`TypedTreePickle.fs:3093-3123`). 11-tuple in
/// wire order. The `u_attribs_ext u_xmldoc` split produces the
/// `xmldoc` and `property_attribs` fields on `PickledRecdField`.
pub(crate) fn read_recdfield_spec(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledRecdField, ImportError> {
    let is_mutable = state.reader.read_bool("u_recdfield_spec IsMutable")?;
    let is_volatile = state.reader.read_bool("u_recdfield_spec IsVolatile")?;
    let ty = read_ty(state)?;
    let is_static = state.reader.read_bool("u_recdfield_spec IsStatic")?;
    let is_secret = state.reader.read_bool("u_recdfield_spec IsSecret")?;
    let literal_value = state
        .reader
        .read_option("u_recdfield_spec LiteralValue", read_const)?;
    let ident = crate::fsharp_pickle::typar::read_ident(&mut state.reader)?;
    let (xmldoc, property_attribs) = read_attribs_ext(state, "u_recdfield_spec property_attribs")?;
    let field_attribs = crate::fsharp_pickle::attribs::read_attribs(state)?;
    let xmldoc_sig = state.reader.read_string("u_recdfield_spec XmlDocSig")?;
    let access = read_access(&mut state.reader)?;
    Ok(PickledRecdField {
        is_mutable,
        is_volatile,
        ty,
        is_static,
        is_secret,
        literal_value,
        ident,
        property_attribs,
        field_attribs,
        xmldoc,
        xmldoc_sig,
        access,
    })
}

/// `u_rfield_table` (`:3125-3126`). `u_list u_recdfield_spec`.
pub(crate) fn read_rfield_table(
    state: &mut PhaseOneState<'_>,
) -> Result<Vec<PickledRecdField>, ImportError> {
    state.read_array("u_rfield_table element", read_recdfield_spec)
}

/// `u_unioncase_spec` (`:3054-3076`). 7 wire reads. Field 3 (the
/// deprecated compiled-name `u_string`) is dropped — FCS computes
/// the compiled name from `Id` when needed.
pub(crate) fn read_unioncase_spec(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledUnionCase, ImportError> {
    let fields = read_rfield_table(state)?;
    let return_ty = read_ty(state)?;
    let _deprecated_compiled_name = state.reader.read_string("u_unioncase_spec _c (dropped)")?;
    let ident = crate::fsharp_pickle::typar::read_ident(&mut state.reader)?;
    let (xmldoc, attribs) = read_attribs_ext(state, "u_unioncase_spec attribs")?;
    let xmldoc_sig = state.reader.read_string("u_unioncase_spec XmlDocSig")?;
    let access = read_access(&mut state.reader)?;
    Ok(PickledUnionCase {
        fields,
        return_ty,
        ident,
        attribs,
        xmldoc,
        xmldoc_sig,
        access,
    })
}

/// `u_tycon_objmodel_kind` (`:3256-3267`). 7-tag dispatcher. Tag 3
/// (`Delegate`) reads an `u_slotsig` payload; the slotsig is
/// state-aware (publishes into the typar OSGN table via
/// `u_tyar_specs`).
pub(crate) fn read_tycon_objmodel_kind(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledTyconObjModelKind, ImportError> {
    let tag = state.reader.read_byte("u_tycon_objmodel_kind tag")?;
    Ok(match tag {
        0 => PickledTyconObjModelKind::Class,
        1 => PickledTyconObjModelKind::Interface,
        2 => PickledTyconObjModelKind::Struct,
        3 => PickledTyconObjModelKind::Delegate(Box::new(read_slotsig(state)?)),
        4 => PickledTyconObjModelKind::Enum,
        5 => PickledTyconObjModelKind::Union,
        6 => PickledTyconObjModelKind::Record,
        other => {
            return Err(ImportError::UnsupportedPickleTag {
                context: "u_tycon_objmodel_kind",
                tag: u32::from(other),
            });
        }
    })
}

/// `u_tycon_objmodel_data` (`:3042-3050`). 3-tuple: kind, vslots
/// (`u_vrefs` = `u_list u_vref`), and the underlying record-field
/// table. The cases list (`fsobjmodel_cases`) is initialised to
/// empty by FCS at this point; `UnionWithStaticFields` (outer tag 2)
/// re-injects it after this decoder returns.
pub(crate) fn read_tycon_objmodel_data(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledTyconObjModelData, ImportError> {
    let kind = read_tycon_objmodel_kind(state)?;
    let vslots = state.read_array("u_tycon_objmodel_data vslots element", read_vref)?;
    let rfields = read_rfield_table(state)?;
    Ok(PickledTyconObjModelData {
        kind,
        vslots,
        rfields,
    })
}

/// Intermediate `u_tycon_repr` result. FCS returns a `flagBit ->
/// TyconRepr` closure; we capture the closure's captured state and
/// resolve it via [`resolve_repr`] once `u_entity_spec_data` has
/// extracted the flag bit from the entity flags word
/// (`ReservedBitForPickleFormatTyconReprFlag` = `0x10`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickledTyconReprClosure {
    NoRepr,
    Record(Vec<PickledRecdField>),
    Union(Vec<PickledUnionCase>),
    /// Outer 1 / inner 2. `flag_bit=false` ⇒ `TAsmRepr`;
    /// `flag_bit=true` ⇒ provider type (hard-error in 6b4 —
    /// MiniLibFs has none).
    ILTypeOrProvider(PickledILType),
    FSharpObjectModel(PickledTyconObjModelData),
    Measureable(PickledType),
    UnionWithStaticFields {
        cases: Vec<PickledUnionCase>,
        objmodel: PickledTyconObjModelData,
    },
}

/// `u_tycon_repr` (`:2961-3040`). Outer tag dispatcher; outer 1 reads
/// an inner tag and dispatches again. Returns the *closure
/// intermediate* — the flag-bit resolution happens in
/// `entity::read_entity_spec_data` after the flags word has been
/// consumed.
pub(crate) fn read_tycon_repr(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledTyconReprClosure, ImportError> {
    let outer = state.reader.read_byte("u_tycon_repr outer tag")?;
    match outer {
        0 => Ok(PickledTyconReprClosure::NoRepr),
        1 => {
            let inner = state.reader.read_byte("u_tycon_repr inner tag")?;
            match inner {
                0 => Ok(PickledTyconReprClosure::Record(read_rfield_table(state)?)),
                1 => Ok(PickledTyconReprClosure::Union(state.read_array(
                    "u_tycon_repr inner-1 union element",
                    read_unioncase_spec,
                )?)),
                2 => {
                    let v = read_il_type(&mut state.reader)?;
                    Ok(PickledTyconReprClosure::ILTypeOrProvider(v))
                }
                3 => Ok(PickledTyconReprClosure::FSharpObjectModel(
                    read_tycon_objmodel_data(state)?,
                )),
                4 => Ok(PickledTyconReprClosure::Measureable(read_ty(state)?)),
                other => Err(ImportError::UnsupportedPickleTag {
                    context: "u_tycon_repr inner tag",
                    tag: u32::from(other),
                }),
            }
        }
        2 => {
            // u_array u_unioncase_spec, then u_tycon_objmodel_data.
            let cases =
                state.read_array("u_tycon_repr outer-2 cases element", read_unioncase_spec)?;
            let objmodel = read_tycon_objmodel_data(state)?;
            Ok(PickledTyconReprClosure::UnionWithStaticFields { cases, objmodel })
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_tycon_repr outer tag",
            tag: u32::from(other),
        }),
    }
}

/// Apply the flag bit to a [`PickledTyconReprClosure`] to produce the
/// final [`PickledTyconRepr`]. The flag bit only affects the
/// inner-tag-2 / outer-tag-1 branch: `false` ⇒ `AsmRepr`, `true` ⇒
/// provider type (which FCS handles by walking the `iILModule` cache;
/// we don't carry one in 6b4, so it's a hard error).
pub(crate) fn resolve_repr(
    closure: PickledTyconReprClosure,
    flag_bit: bool,
) -> Result<PickledTyconRepr, ImportError> {
    Ok(match closure {
        PickledTyconReprClosure::NoRepr => PickledTyconRepr::NoRepr,
        PickledTyconReprClosure::Record(v) => PickledTyconRepr::Record(v),
        PickledTyconReprClosure::Union(v) => PickledTyconRepr::Union(v),
        PickledTyconReprClosure::ILTypeOrProvider(v) => {
            if flag_bit {
                return Err(ImportError::UnsupportedSignature {
                    detail:
                        "u_tycon_repr outer-1/inner-2 with format flag set: F# type providers are \
                         not supported"
                            .to_owned(),
                });
            }
            PickledTyconRepr::AsmRepr(v)
        }
        PickledTyconReprClosure::FSharpObjectModel(v) => PickledTyconRepr::FSharpObjectModel(v),
        PickledTyconReprClosure::Measureable(t) => PickledTyconRepr::Measureable(t),
        PickledTyconReprClosure::UnionWithStaticFields { cases, objmodel } => {
            PickledTyconRepr::UnionWithStaticFields { cases, objmodel }
        }
    })
}

/// `u_exnc_repr` (`:3078-3086`). 4-tag dispatcher: `Abbrev(tcref)`,
/// `Asm(iltyperef)`, `Fresh(rfield_table)`, `None`.
pub(crate) fn read_exnc_repr(state: &mut PhaseOneState<'_>) -> Result<PickledExnRepr, ImportError> {
    let tag = state.reader.read_byte("u_exnc_repr tag")?;
    Ok(match tag {
        0 => PickledExnRepr::Abbrev(read_tcref(state)?),
        1 => PickledExnRepr::Asm(read_il_type_ref(&mut state.reader)?),
        2 => PickledExnRepr::Fresh(read_rfield_table(state)?),
        3 => PickledExnRepr::None,
        other => {
            return Err(ImportError::UnsupportedPickleTag {
                context: "u_exnc_repr",
                tag: u32::from(other),
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{Nullness, PickledTcRef};
    use crate::fsharp_pickle::reader::PickleReader;

    fn make_state<'a>(bytes: &'a [u8], strings: &'a [String]) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, 0)
    }

    /// Helper: a one-byte `u_ty` AppSimple(0, ambivalent) — tag 1 then
    /// simpletyp index 0; B-stream absent so nullness defaults to
    /// `Ambivalent`. Used wherever the test needs an arbitrary type
    /// payload without exercising the type decoder.
    const APPSIMPLE_0: [u8; 2] = [1u8, 0u8];

    fn appsimple_assert(ty: &PickledType) {
        assert!(matches!(
            ty,
            PickledType::AppSimple {
                simpletyp_index: 0,
                nullness: Nullness::Ambivalent,
            }
        ));
    }

    #[test]
    fn attribs_ext_no_extra() {
        // length 0 (no high bit), empty list.
        let bytes = vec![0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let (extra, attribs) = read_attribs_ext(&mut s, "attribs_ext_no_extra ctx").unwrap();
        assert!(extra.is_none());
        assert!(attribs.is_empty());
    }

    #[test]
    fn attribs_ext_with_xmldoc_extra() {
        // Length = 0x80000000 (high bit set, real length 0). The
        // compressed-int encoding of 0x80000000 needs the 5-byte
        // 0xFF + 4-byte LE form.
        let strings: Vec<String> = vec![];
        let mut bytes = vec![
            0xFFu8, // 5-byte marker
            0x00, 0x00, 0x00, 0x80, // 0x80000000 little-endian
            // xmldoc (extra): u_array u_string length 0
            0u8,
        ];
        // No attribs in the body since real_len = 0.
        let _ = &mut bytes; // suppress unused-mut warning if any
        let mut s = make_state(&bytes, &strings);
        let (extra, attribs) =
            read_attribs_ext(&mut s, "attribs_ext_with_xmldoc_extra ctx").unwrap();
        assert!(extra.is_some());
        assert!(extra.unwrap().lines.is_empty());
        assert!(attribs.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn recdfield_spec_minimal_round_trip() {
        // is_mutable=false, is_volatile=false, ty=AppSimple(0), is_static=false,
        // is_secret=false, no literal, ident (string idx 0, range (1,0)-(1,1) in file 1),
        // attribs_ext (no extra, empty list), field_attribs empty, xmldoc_sig="" (idx 1),
        // access empty.
        let strings = vec!["x".to_string(), "".to_string()];
        let bytes = vec![
            0u8,
            0u8, // is_mutable, is_volatile
            APPSIMPLE_0[0],
            APPSIMPLE_0[1], // ty
            0u8,
            0u8, // is_static, is_secret
            0u8, // option literal = None
            0u8,
            1u8,
            1u8,
            0u8,
            1u8,
            1u8, // ident
            0u8, // attribs_ext length 0, no extra
            0u8, // field attribs list len 0
            1u8, // xmldoc_sig idx 1 = ""
            0u8, // access list len 0
        ];
        let mut s = make_state(&bytes, &strings);
        let f = read_recdfield_spec(&mut s).unwrap();
        assert!(!f.is_mutable);
        assert!(!f.is_volatile);
        appsimple_assert(&f.ty);
        assert!(!f.is_static);
        assert!(!f.is_secret);
        assert!(f.literal_value.is_none());
        assert_eq!(f.ident.name, "x");
        assert!(f.property_attribs.is_empty());
        assert!(f.field_attribs.is_empty());
        assert!(f.xmldoc.is_none());
        assert_eq!(f.xmldoc_sig, "");
        assert!(f.access.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn rfield_table_two_fields() {
        let strings = vec!["a".to_string(), "b".to_string(), "".to_string()];
        let one_field = |name_idx: u8| {
            vec![
                0u8,
                0u8, // bools
                APPSIMPLE_0[0],
                APPSIMPLE_0[1], //
                0u8,
                0u8, // bools
                0u8, // option literal None
                name_idx,
                1u8,
                1u8,
                0u8,
                1u8,
                1u8, // ident
                0u8, // attribs_ext len 0
                0u8, // field attribs len 0
                2u8, // xmldoc_sig idx 2 = ""
                0u8, // access
            ]
        };
        let mut bytes = vec![2u8]; // table list length 2
        bytes.extend(one_field(0));
        bytes.extend(one_field(1));
        // Need file-name idx 1 to validate range
        let mut s = make_state(&bytes, &strings);
        let v = read_rfield_table(&mut s).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].ident.name, "a");
        assert_eq!(v[1].ident.name, "b");
        assert!(s.reader.is_eof());
    }

    #[test]
    fn unioncase_spec_round_trip() {
        // empty fields, return_ty AppSimple(0), deprecated cname="" (idx 1),
        // ident "C" (idx 0), attribs_ext empty, xmldoc_sig="" (idx 1), access empty.
        let strings = vec!["C".to_string(), "".to_string()];
        let bytes = vec![
            0u8, // rfield_table length 0
            APPSIMPLE_0[0],
            APPSIMPLE_0[1], //
            1u8,            // deprecated compiled name idx 1 = ""
            0u8,
            1u8,
            1u8,
            0u8,
            1u8,
            1u8, // ident "C"
            0u8, // attribs_ext length 0
            1u8, // xmldoc_sig idx 1 = ""
            0u8, // access
        ];
        let mut s = make_state(&bytes, &strings);
        let c = read_unioncase_spec(&mut s).unwrap();
        assert!(c.fields.is_empty());
        appsimple_assert(&c.return_ty);
        assert_eq!(c.ident.name, "C");
        assert!(c.attribs.is_empty());
        assert!(c.xmldoc.is_none());
        assert_eq!(c.xmldoc_sig, "");
        assert!(c.access.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tycon_objmodel_kind_no_payload_tags() {
        for (tag, expected) in [
            (0u8, PickledTyconObjModelKind::Class),
            (1, PickledTyconObjModelKind::Interface),
            (2, PickledTyconObjModelKind::Struct),
            (4, PickledTyconObjModelKind::Enum),
            (5, PickledTyconObjModelKind::Union),
            (6, PickledTyconObjModelKind::Record),
        ] {
            let bytes = [tag];
            let strings: Vec<String> = vec![];
            let mut s = make_state(&bytes, &strings);
            assert_eq!(read_tycon_objmodel_kind(&mut s).unwrap(), expected);
        }
    }

    #[test]
    fn tycon_objmodel_kind_unknown_errors() {
        let bytes = [9u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        match read_tycon_objmodel_kind(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_tycon_objmodel_kind",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tycon_objmodel_data_class_no_vslots() {
        let strings: Vec<String> = vec![];
        let bytes = vec![
            0u8, // kind = Class
            0u8, // vslots list length 0
            0u8, // rfield_table length 0
        ];
        let mut s = make_state(&bytes, &strings);
        let d = read_tycon_objmodel_data(&mut s).unwrap();
        assert_eq!(d.kind, PickledTyconObjModelKind::Class);
        assert!(d.vslots.is_empty());
        assert!(d.rfields.is_empty());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tycon_repr_no_repr() {
        let bytes = vec![0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let closure = read_tycon_repr(&mut s).unwrap();
        assert_eq!(closure, PickledTyconReprClosure::NoRepr);
        assert_eq!(
            resolve_repr(closure, false).unwrap(),
            PickledTyconRepr::NoRepr
        );
    }

    #[test]
    fn tycon_repr_record_branch() {
        // outer 1, inner 0, empty rfield_table.
        let bytes = vec![1u8, 0u8, 0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let closure = read_tycon_repr(&mut s).unwrap();
        assert_eq!(closure, PickledTyconReprClosure::Record(vec![]));
        match resolve_repr(closure, false).unwrap() {
            PickledTyconRepr::Record(v) => assert!(v.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tycon_repr_union_branch() {
        // outer 1, inner 1, empty list of union cases.
        let bytes = vec![1u8, 1u8, 0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let closure = read_tycon_repr(&mut s).unwrap();
        assert_eq!(closure, PickledTyconReprClosure::Union(vec![]));
    }

    #[test]
    fn tycon_repr_il_type_with_flag_false_yields_asmrepr() {
        // outer 1, inner 2, il_type = Void (tag 0).
        let bytes = vec![1u8, 2u8, 0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let closure = read_tycon_repr(&mut s).unwrap();
        match resolve_repr(closure, false).unwrap() {
            PickledTyconRepr::AsmRepr(PickledILType::Void) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tycon_repr_il_type_with_flag_true_hard_errors() {
        let bytes = vec![1u8, 2u8, 0u8]; // outer 1, inner 2, il_type = Void
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let closure = read_tycon_repr(&mut s).unwrap();
        match resolve_repr(closure, true) {
            Err(ImportError::UnsupportedSignature { .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tycon_repr_outer_unknown_errors() {
        let bytes = vec![9u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        match read_tycon_repr(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_tycon_repr outer tag",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tycon_repr_inner_unknown_errors() {
        let bytes = vec![1u8, 9u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        match read_tycon_repr(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_tycon_repr inner tag",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn exnc_repr_abbrev_branch() {
        // tag 0, then u_tcref NonLocal idx 2.
        let bytes = vec![0u8, 1u8, 2u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let r = read_exnc_repr(&mut s).unwrap();
        assert_eq!(r, PickledExnRepr::Abbrev(PickledTcRef::NonLocal(2)));
    }

    #[test]
    fn exnc_repr_fresh_empty() {
        // tag 2, then empty rfield_table.
        let bytes = vec![2u8, 0u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        let r = read_exnc_repr(&mut s).unwrap();
        assert_eq!(r, PickledExnRepr::Fresh(vec![]));
    }

    #[test]
    fn exnc_repr_none_branch() {
        let bytes = vec![3u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        assert_eq!(read_exnc_repr(&mut s).unwrap(), PickledExnRepr::None);
    }

    #[test]
    fn exnc_repr_unknown_errors() {
        let bytes = vec![5u8];
        let strings: Vec<String> = vec![];
        let mut s = make_state(&bytes, &strings);
        match read_exnc_repr(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_exnc_repr",
                tag: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
