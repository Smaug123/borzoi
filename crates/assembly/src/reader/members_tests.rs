//! Stage 5 correctness oracles for the member table walk.
//!
//! - **Structural properties** (no external reference; durable): members are
//!   materialised 1:1 with the table rows; every accessor `MethodId` indexes a
//!   real method on the same type; every signature scope/attribute handle is in
//!   range; `is_literal` agrees with `Constant`-row presence; the
//!   parameter/return/generic-param attribute positions are actually populated.
//! - **Fuzz**: the member walk runs inside `read_types`, whose
//!   never-panic-on-arbitrary/-truncated/-mutated coverage lives in
//!   [`super::typedefs_tests`]; the member-signature decoders' own fuzz lives in
//!   [`super::signature_tests`].

use super::Error;
use super::ids::{MethodId, TypeDefId, TypeRefId};
use super::members::{decode_constant, validate_list_starts};
use super::metadata::MetadataFile;
use super::model::{Constant, MemberHandle, TypeDef};
use super::signature::{ModifiedType, RetType, TypeScope, TypeSig};
use super::tables::table;
use super::test_fixtures::all_dlls;
use super::typedefs::read_types;

// ============================================================================
// Structural property oracles (no external reference; durable)
// ============================================================================

/// Members are materialised 1:1 with the metadata tables: summing every type's
/// member lists reproduces the `MethodDef`/`Field`/`Property`/`Event` row counts
/// exactly, so none is dropped, duplicated, or misattributed.
#[test]
fn member_totals_match_table_row_counts() {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let types = read_types(&md).expect("type walk");

        let sum = |f: &dyn Fn(&TypeDef) -> usize| -> usize { types.type_defs.iter().map(f).sum() };
        assert_eq!(
            sum(&|td| td.methods.len()),
            md.rows[table::METHOD_DEF] as usize,
            "method total in {}",
            dll.display()
        );
        assert_eq!(
            sum(&|td| td.fields.len()),
            md.rows[table::FIELD] as usize,
            "field total in {}",
            dll.display()
        );
        assert_eq!(
            sum(&|td| td.properties.len()),
            md.rows[table::PROPERTY] as usize,
            "property total in {}",
            dll.display()
        );
        assert_eq!(
            sum(&|td| td.events.len()),
            md.rows[table::EVENT] as usize,
            "event total in {}",
            dll.display()
        );
    }
}

/// A member-list "start RID" array must be a partition of `1..=count`: starting
/// at 1, non-decreasing, and within `1..=count+1`. An out-of-range or
/// out-of-order start is refused loudly rather than clamped into a valid-looking
/// (but row-dropping) range.
#[test]
fn list_start_validation_refuses_malformed_partitions() {
    // Well-formed: starts at 1, non-decreasing, `count+1` empty-tail sentinels.
    assert_eq!(validate_list_starts(&[1, 1, 3, 5], 4), Ok(()));
    assert_eq!(validate_list_starts(&[1, 5], 4), Ok(())); // last run [5, 5) empty
    assert_eq!(validate_list_starts(&[1], 0), Ok(())); // empty table: 1 == count+1
    // First start must be exactly 1 (no leading orphan rows).
    assert_eq!(
        validate_list_starts(&[2, 3], 4),
        Err(Error::TableIndexOutOfRange)
    );
    // A zero start is never a valid 1-based list index.
    assert_eq!(
        validate_list_starts(&[0, 1], 4),
        Err(Error::TableIndexOutOfRange)
    );
    // A start past `count + 1` dangles beyond the table.
    assert_eq!(
        validate_list_starts(&[1, 6], 4),
        Err(Error::TableIndexOutOfRange)
    );
    // Non-decreasing is required (a decreasing run would drop rows silently).
    assert_eq!(
        validate_list_starts(&[1, 3, 2], 4),
        Err(Error::TableIndexOutOfRange)
    );
}

/// Every property/event accessor `MethodId` indexes a real method on the *same*
/// type, and at least one accessor exists across the corpus (so the linkage path
/// is actually exercised).
#[test]
fn accessor_method_ids_resolve_on_same_type() {
    let mut linked = 0usize;
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let types = read_types(&md).expect("type walk");

        for td in &types.type_defs {
            let n = td.methods.len();
            let mut check = |id: Option<MethodId>| {
                if let Some(MethodId(m)) = id {
                    assert!(
                        (m as usize) < n,
                        "accessor MethodId {m} out of range ({n} methods) in {}",
                        dll.display()
                    );
                    linked += 1;
                }
            };
            for p in &td.properties {
                check(p.getter);
                check(p.setter);
            }
            for e in &td.events {
                check(e.add);
                check(e.remove);
                check(e.raise);
            }
        }
    }
    assert!(linked > 0, "no property/event accessor linkage exercised");
}

/// A field is `is_literal` exactly when it owns a `Constant` row (only `Literal`
/// fields do). Cross-checks the flag decode against an independent table.
#[test]
fn literal_fields_have_constant_rows() {
    use super::tables::{Coded, Tables};
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let tables = Tables::new(&md).expect("table layout");
        let types = read_types(&md).expect("type walk");

        // Field RIDs (1-based) that own a Constant row.
        let mut const_fields = std::collections::HashSet::new();
        for i in 0..tables.row_count(table::CONSTANT) {
            let row = tables.row(table::CONSTANT, i).expect("constant row");
            if let Some(p) = tables
                .decode_coded(Coded::HasConstant, row.coded(1))
                .unwrap()
                && p.table == table::FIELD
            {
                const_fields.insert(p.rid);
            }
        }

        // Fields are materialised in FieldList (RID) order across types, so a
        // running counter recovers each field's RID.
        let mut field_rid = 1u32;
        let mut literal_seen = false;
        for td in &types.type_defs {
            for f in &td.fields {
                assert_eq!(
                    f.is_literal,
                    const_fields.contains(&field_rid),
                    "field `{}` (rid {field_rid}) literal/constant mismatch in {}",
                    f.name,
                    dll.display()
                );
                literal_seen |= f.is_literal;
                field_rid += 1;
            }
        }
        // The C# corpus has `const` fields; pin that the literal path fires.
        if dll
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("MiniLib.dll")
        {
            assert!(literal_seen, "no literal field seen in {}", dll.display());
        }
    }
}

/// `decode_constant` reads each supported `ELEMENT_TYPE` (II.23.1.16) as a
/// little-endian value of the right width, keeps `char`/string values as raw
/// UTF-16 code units (losslessly — unpaired surrogates and all), reads a
/// 4-byte-zero null `CLASS`, and carries floats as raw bits (the model derives
/// `Eq`/`Hash`, so it cannot store native floats).
#[test]
fn decode_constant_reads_each_element_type() {
    // ELEMENT_TYPE_* codes, repeated here so the test pins the wire encoding
    // independently of the decoder's private constants.
    const BOOLEAN: u8 = 0x02;
    const CHAR: u8 = 0x03;
    const I1: u8 = 0x04;
    const U1: u8 = 0x05;
    const I2: u8 = 0x06;
    const U2: u8 = 0x07;
    const I4: u8 = 0x08;
    const U4: u8 = 0x09;
    const I8: u8 = 0x0a;
    const U8: u8 = 0x0b;
    const R4: u8 = 0x0c;
    const R8: u8 = 0x0d;
    const STRING: u8 = 0x0e;
    const CLASS: u8 = 0x12;

    // Any non-zero byte is `true`.
    assert_eq!(
        decode_constant(BOOLEAN, &[0x00]),
        Some(Constant::Bool(false))
    );
    assert_eq!(
        decode_constant(BOOLEAN, &[0x01]),
        Some(Constant::Bool(true))
    );
    assert_eq!(
        decode_constant(BOOLEAN, &[0xff]),
        Some(Constant::Bool(true))
    );

    // A raw UTF-16 code unit, little-endian — kept verbatim, even an unpaired
    // surrogate (valid CLI metadata, but not a Rust `char`).
    assert_eq!(
        decode_constant(CHAR, &[0x41, 0x00]),
        Some(Constant::Char(0x41))
    );
    assert_eq!(
        decode_constant(CHAR, &[0x3b, 0x00]),
        Some(Constant::Char(0x3b))
    );
    assert_eq!(
        decode_constant(CHAR, &[0x00, 0xd8]),
        Some(Constant::Char(0xd800))
    );

    // Signed widths sign-extend; unsigned widths zero-extend.
    assert_eq!(decode_constant(I1, &[0xff]), Some(Constant::Int(-1)));
    assert_eq!(decode_constant(U1, &[0xff]), Some(Constant::UInt(255)));
    assert_eq!(
        decode_constant(I2, &[0x00, 0x80]),
        Some(Constant::Int(-32768))
    );
    assert_eq!(
        decode_constant(U2, &[0xff, 0xff]),
        Some(Constant::UInt(65535))
    );
    assert_eq!(
        decode_constant(I4, &[0x05, 0x00, 0x00, 0x00]),
        Some(Constant::Int(5))
    );
    assert_eq!(
        decode_constant(I4, &[0xff, 0xff, 0xff, 0xff]),
        Some(Constant::Int(-1))
    );
    assert_eq!(
        decode_constant(U4, &[0xff, 0xff, 0xff, 0xff]),
        Some(Constant::UInt(u64::from(u32::MAX)))
    );
    assert_eq!(decode_constant(I8, &[0xff; 8]), Some(Constant::Int(-1)));
    assert_eq!(
        decode_constant(U8, &[0xff; 8]),
        Some(Constant::UInt(u64::MAX))
    );

    // Floats round-trip through their raw little-endian bit pattern.
    assert_eq!(
        decode_constant(R4, &1.5f32.to_le_bytes()),
        Some(Constant::F32(1.5f32.to_bits()))
    );
    assert_eq!(
        decode_constant(R8, &2.5f64.to_le_bytes()),
        Some(Constant::F64(2.5f64.to_bits()))
    );

    // A string is raw UTF-16 code units; the empty string is a zero-length blob,
    // not absent; an unpaired surrogate is preserved, not rejected.
    let hi: Vec<u8> = "hi".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(
        decode_constant(STRING, &hi),
        Some(Constant::Str(vec![0x68, 0x69]))
    );
    assert_eq!(decode_constant(STRING, &[]), Some(Constant::Str(vec![])));
    assert_eq!(
        decode_constant(STRING, &[0x00, 0xd8]),
        Some(Constant::Str(vec![0xd800]))
    );

    // A null reference default stores a 4-byte zero.
    assert_eq!(
        decode_constant(CLASS, &[0x00, 0x00, 0x00, 0x00]),
        Some(Constant::Null)
    );
}

/// `decode_constant` returns `None` (the parent gets no default — the assembly
/// is not sunk) rather than guessing: a blob too short *or too long* for its
/// declared width, an odd-length string, a `CLASS` blob that is not exactly four
/// zero bytes, and an unsupported element type all decode to nothing.
#[test]
fn decode_constant_rejects_malformed_blobs() {
    const BOOLEAN: u8 = 0x02;
    const CHAR: u8 = 0x03;
    const I4: u8 = 0x08;
    const R8: u8 = 0x0d;
    const STRING: u8 = 0x0e;
    const CLASS: u8 = 0x12;
    const VALUETYPE: u8 = 0x11; // not in the supported set

    // Too short for the declared width.
    assert_eq!(decode_constant(I4, &[0x00, 0x00]), None);
    assert_eq!(decode_constant(R8, &[0x00; 4]), None);
    // Too *long* for the declared fixed width — a fixed-width type must match its
    // size exactly, not silently take a prefix (else trailing bytes from
    // malformed/hand-authored metadata surface a bogus default).
    assert_eq!(decode_constant(I4, &[0x05, 0x00, 0x00, 0x00, 0x00]), None);
    assert_eq!(decode_constant(I4, &[0xff; 8]), None);
    assert_eq!(decode_constant(BOOLEAN, &[0x01, 0x00]), None);
    assert_eq!(decode_constant(CHAR, &[0x41, 0x00, 0x00]), None);
    assert_eq!(decode_constant(R8, &[0x00; 9]), None);
    // Odd-length UTF-16 blob (a half code unit).
    assert_eq!(decode_constant(STRING, &[0x41]), None);
    // A `CLASS` constant must be exactly four zero bytes — a non-zero or
    // wrong-length blob is malformed, not a null default.
    assert_eq!(decode_constant(CLASS, &[0x01, 0x00, 0x00, 0x00]), None);
    assert_eq!(decode_constant(CLASS, &[0x00, 0x00, 0x00]), None);
    assert_eq!(decode_constant(CLASS, &[]), None);
    // Unsupported element type.
    assert_eq!(decode_constant(VALUETYPE, &[0x00; 4]), None);
}

/// Every scope handle inside a member signature, and every attribute-constructor
/// handle at every captured position, indexes a live arena slot.
#[test]
fn member_handles_are_in_range() {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let types = read_types(&md).expect("type walk");
        let def_count = types.type_defs.len() as u32;
        let ref_count = types.type_refs.len() as u32;
        let member_ref_count = md.rows[table::MEMBER_REF];

        let check_attr = |attr: &super::model::RawAttribute| match attr.ctor {
            MemberHandle::MethodDef(TypeDefId(d), MethodId(m)) => {
                assert!(d < def_count, "attr ctor owner in range");
                assert!(
                    (m as usize) < types.type_defs[d as usize].methods.len(),
                    "attr ctor method index in range"
                );
            }
            MemberHandle::MemberRef(id) => assert!(id.0 < member_ref_count, "attr ctor memberref"),
        };

        for td in &types.type_defs {
            for m in &td.methods {
                if let Ok(sig) = &m.signature {
                    if let RetType::Type(t) = &sig.return_type {
                        check_scopes(t, def_count, ref_count);
                    }
                    for p in &sig.parameters {
                        check_scopes(&p.ty, def_count, ref_count);
                        p.attributes.iter().for_each(&check_attr);
                    }
                    sig.return_attributes.iter().for_each(&check_attr);
                }
                m.attributes.iter().for_each(&check_attr);
                for gp in &m.generic_params {
                    gp.attributes.iter().for_each(&check_attr);
                    for c in &gp.constraints {
                        c.attributes.iter().for_each(&check_attr);
                        if let Ok(sig) = &c.ty {
                            check_scopes(sig, def_count, ref_count);
                        }
                    }
                }
            }
            for f in &td.fields {
                if let Ok(t) = &f.signature {
                    check_scopes(t, def_count, ref_count);
                }
                f.attributes.iter().for_each(&check_attr);
            }
            for p in &td.properties {
                if let Ok(t) = &p.signature {
                    check_scopes(t, def_count, ref_count);
                }
                p.attributes.iter().for_each(&check_attr);
            }
            for e in &td.events {
                if let Ok(t) = &e.event_type {
                    check_scopes(t, def_count, ref_count);
                }
                e.attributes.iter().for_each(&check_attr);
            }
            for gp in &td.generic_params {
                gp.attributes.iter().for_each(&check_attr);
            }
        }
    }
}

fn check_scopes(mt: &ModifiedType, def_count: u32, ref_count: u32) {
    let check = |scope: &TypeScope| match scope {
        TypeScope::Definition(TypeDefId(d)) => assert!(*d < def_count, "TypeDef scope in range"),
        TypeScope::Reference(TypeRefId(r)) => assert!(*r < ref_count, "TypeRef scope in range"),
    };
    // The position's modifier run names types too — every one of those tokens
    // must resolve, exactly like the type's own.
    for m in &mt.mods {
        check(&m.modifier);
    }
    match &mt.ty {
        TypeSig::Named { scope, .. } => check(scope),
        TypeSig::Generic { scope, args, .. } => {
            check(scope);
            for a in args {
                check_scopes(a, def_count, ref_count);
            }
        }
        TypeSig::SzArray(inner) | TypeSig::Array { element: inner, .. } | TypeSig::ByRef(inner) => {
            check_scopes(inner, def_count, ref_count)
        }
        TypeSig::Ptr(inner) => {
            if let Some(p) = inner {
                check_scopes(p, def_count, ref_count);
            }
        }
        TypeSig::Primitive(_)
        | TypeSig::TypeVar(_)
        | TypeSig::MethodVar(_)
        | TypeSig::TypedByRef => {}
    }
}

/// The parameter, return, and generic-parameter attribute positions are actually
/// populated — i.e. stage 5 wired up more than the type-level attributes. The C#
/// corpus carries `[ParamArray]` (a parameter attribute), `[Nullable]`/
/// `[NullableContext]` (return/parameter attributes), and `[IsUnmanaged]`/
/// `[Nullable]` (generic-parameter attributes).
#[test]
fn per_position_attributes_are_captured() {
    let mut param_attrs = 0usize;
    let mut return_attrs = 0usize;
    let mut gp_attrs = 0usize;
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let md = MetadataFile::read(&bytes).expect("container parse");
        let types = read_types(&md).expect("type walk");
        for td in &types.type_defs {
            gp_attrs += td
                .generic_params
                .iter()
                .map(|gp| gp.attributes.len())
                .sum::<usize>();
            for m in &td.methods {
                gp_attrs += m
                    .generic_params
                    .iter()
                    .map(|gp| gp.attributes.len())
                    .sum::<usize>();
                if let Ok(sig) = &m.signature {
                    return_attrs += sig.return_attributes.len();
                    param_attrs += sig
                        .parameters
                        .iter()
                        .map(|p| p.attributes.len())
                        .sum::<usize>();
                }
            }
        }
    }
    assert!(param_attrs > 0, "no parameter attributes captured");
    assert!(return_attrs > 0, "no return attributes captured");
    assert!(gp_attrs > 0, "no generic-parameter attributes captured");
}
