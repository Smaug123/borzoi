//! Stage 6 correctness oracles for the custom-attribute decoder.
//!
//! - **Round-trip** (the prime property): a naive CA-blob encoder emits the
//!   spec's bytes for the supported value set, and `decode_blob(encode(x)) == x`
//!   for every generated `x`. Strings span all length bands — crucially the
//!   ≥ 128-byte band — so the corrected `SerString` length read is proven here
//!   directly.
//! - **Fixtures**: a ≥ 128-byte string attribute decodes to its exact text; an
//!   enum-typed argument decodes against a supplied [`EnumWidths`].
//! - **Refusal / fuzz**: unsupported element types and unknown enum widths are
//!   refused; arbitrary bytes never panic.

use super::attributes::{
    AttrError, DecodedAttribute, EnumWidths, FixedArg, IntegralParam, IntegralWidth, NamedArg,
    NamedArgKind, decode_blob,
};
use super::ids::TypeRefId;
use super::image::parse;
use super::model::TypeName;
use super::signature::{CustomMod, ModifiedType, NamedKind, Primitive, TypeScope, TypeSig};
use super::test_fixtures::all_dlls;
use proptest::prelude::*;

// ============================================================================
// Reference encoder (ECMA-335 II.23.3), test-only
// ============================================================================

/// Encode an unsigned compressed integer (II.23.2) — the same banding the
/// decoder reads, so a ≥ 128-byte `SerString` length emits the 2-byte form.
fn compress(n: u32, out: &mut Vec<u8>) {
    if n <= 0x7F {
        out.push(n as u8);
    } else if n <= 0x3FFF {
        out.push(0x80 | (n >> 8) as u8);
        out.push((n & 0xFF) as u8);
    } else {
        out.push(0xC0 | (n >> 24) as u8);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push((n & 0xFF) as u8);
    }
}

fn enc_ser_string(s: &Option<String>, out: &mut Vec<u8>) {
    match s {
        None => out.push(0xFF),
        Some(s) => {
            compress(s.len() as u32, out);
            out.extend_from_slice(s.as_bytes());
        }
    }
}

fn enc_integral(v: &IntegralParam, out: &mut Vec<u8>) {
    match v {
        IntegralParam::Int32(i) => out.extend_from_slice(&(*i as u32).to_le_bytes()),
        IntegralParam::UInt8(b) => out.push(*b),
        IntegralParam::UInt32(u) => out.extend_from_slice(&u.to_le_bytes()),
        IntegralParam::Int64(i) => out.extend_from_slice(&(*i as u64).to_le_bytes()),
    }
}

fn enc_value(v: &FixedArg, out: &mut Vec<u8>) {
    match v {
        FixedArg::Boolean(b) => out.push(*b as u8),
        FixedArg::Integral(i) => enc_integral(i, out),
        FixedArg::String(s) => enc_ser_string(s, out),
        FixedArg::Enum { underlying, .. } => enc_integral(underlying, out),
        FixedArg::Array(elems) => {
            out.extend_from_slice(&(elems.len() as u32).to_le_bytes());
            for e in elems {
                enc_value(e, out);
            }
        }
    }
}

/// Emit a named argument's self-describing `FieldOrPropType` (II.23.3).
fn enc_named_value_type(v: &FixedArg, out: &mut Vec<u8>) {
    match v {
        FixedArg::Boolean(_) => out.push(0x02),
        FixedArg::Integral(IntegralParam::Int32(_)) => out.push(0x08),
        FixedArg::Integral(IntegralParam::UInt8(_)) => out.push(0x05),
        FixedArg::Integral(IntegralParam::UInt32(_)) => out.push(0x09),
        FixedArg::Integral(IntegralParam::Int64(_)) => out.push(0x0a),
        FixedArg::String(_) => out.push(0x0e),
        FixedArg::Enum { type_name, .. } => {
            out.push(0x55);
            enc_ser_string(
                &Some(format!("{}.{}", type_name.namespace, type_name.name)),
                out,
            );
        }
        // Named-arg arrays are intentionally not generated (the array decode is
        // exercised through fixed args, whose element type comes from the ctor
        // signature and so admits empty arrays).
        FixedArg::Array(_) => unreachable!("round-trip does not generate named-arg arrays"),
    }
}

fn enc_named(n: &NamedArg, out: &mut Vec<u8>) {
    out.push(match n.kind {
        NamedArgKind::Field => 0x53,
        NamedArgKind::Property => 0x54,
    });
    enc_named_value_type(&n.value, out);
    enc_ser_string(&Some(n.name.clone()), out);
    enc_value(&n.value, out);
}

/// The two enum type names the round-trip uses, each tied to a fixed width so a
/// blob's enum width is unambiguous regardless of how many enum args appear.
fn roundtrip_widths() -> EnumWidths {
    let mut w = EnumWidths::new();
    w.insert(enum_name(IntegralWidth::Int32), IntegralWidth::Int32);
    w.insert(enum_name(IntegralWidth::UInt8), IntegralWidth::UInt8);
    w
}

fn enum_name(width: IntegralWidth) -> TypeName {
    TypeName {
        namespace: "Enum".to_string(),
        name: match width {
            IntegralWidth::Int32 => "Int32".to_string(),
            IntegralWidth::UInt8 => "UInt8".to_string(),
        },
    }
}

// ============================================================================
// Generators
// ============================================================================

/// A `SerString` value across all bands: null, arbitrary (incl. empty/short),
/// and ≥ 128 bytes (the multi-byte-prefix band a single-byte-prefix reader
/// mis-reads).
fn arb_ser_string() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        ".*".prop_map(Some),
        proptest::collection::vec(any::<char>(), 130..220)
            .prop_map(|cs| Some(cs.into_iter().collect())),
    ]
}

/// A supported fixed-argument type (a primitive leaf, or an `SZARRAY` of one),
/// as the unmodified position the decoder consumes.
fn arb_type() -> impl Strategy<Value = ModifiedType> {
    let leaf = prop_oneof![
        Just(TypeSig::Primitive(Primitive::Boolean)),
        Just(TypeSig::Primitive(Primitive::Int32)),
        Just(TypeSig::Primitive(Primitive::UInt8)),
        Just(TypeSig::Primitive(Primitive::UInt32)),
        Just(TypeSig::Primitive(Primitive::Int64)),
        Just(TypeSig::Primitive(Primitive::String)),
    ];
    leaf.prop_map(ModifiedType::plain)
        .prop_recursive(3, 8, 1, |inner| {
            inner.prop_map(|t| ModifiedType::plain(TypeSig::SzArray(Box::new(t))))
        })
}

/// A value consistent with `ty` (all array elements share the element type, so
/// even an empty array is well-typed).
fn value_for_type(mt: &ModifiedType) -> BoxedStrategy<FixedArg> {
    match &mt.ty {
        TypeSig::Primitive(Primitive::Boolean) => any::<bool>().prop_map(FixedArg::Boolean).boxed(),
        TypeSig::Primitive(Primitive::Int32) => any::<i32>()
            .prop_map(|i| FixedArg::Integral(IntegralParam::Int32(i)))
            .boxed(),
        TypeSig::Primitive(Primitive::UInt8) => any::<u8>()
            .prop_map(|b| FixedArg::Integral(IntegralParam::UInt8(b)))
            .boxed(),
        TypeSig::Primitive(Primitive::UInt32) => any::<u32>()
            .prop_map(|u| FixedArg::Integral(IntegralParam::UInt32(u)))
            .boxed(),
        TypeSig::Primitive(Primitive::Int64) => any::<i64>()
            .prop_map(|i| FixedArg::Integral(IntegralParam::Int64(i)))
            .boxed(),
        TypeSig::Primitive(Primitive::String) => {
            arb_ser_string().prop_map(FixedArg::String).boxed()
        }
        TypeSig::SzArray(inner) => {
            let inner = (**inner).clone();
            proptest::collection::vec(value_for_type(&inner), 0..4)
                .prop_map(FixedArg::Array)
                .boxed()
        }
        _ => unreachable!("arb_type only produces supported leaf/array types"),
    }
}

/// A consistent `(constructor parameter type, fixed argument value)` pair.
fn arb_fixed() -> impl Strategy<Value = (ModifiedType, FixedArg)> {
    arb_type().prop_flat_map(|ty| value_for_type(&ty).prop_map(move |v| (ty.clone(), v)))
}

fn arb_name() -> impl Strategy<Value = String> {
    "[A-Za-z][A-Za-z0-9]{0,8}".prop_map(|s| s.to_string())
}

fn arb_named_arg() -> impl Strategy<Value = NamedArg> {
    let kind = prop_oneof![Just(NamedArgKind::Field), Just(NamedArgKind::Property)];
    let value = prop_oneof![
        any::<bool>().prop_map(FixedArg::Boolean),
        any::<i32>().prop_map(|i| FixedArg::Integral(IntegralParam::Int32(i))),
        any::<u8>().prop_map(|b| FixedArg::Integral(IntegralParam::UInt8(b))),
        arb_ser_string().prop_map(FixedArg::String),
        any::<i32>().prop_map(|i| FixedArg::Enum {
            underlying: IntegralParam::Int32(i),
            type_name: enum_name(IntegralWidth::Int32),
        }),
        any::<u8>().prop_map(|b| FixedArg::Enum {
            underlying: IntegralParam::UInt8(b),
            type_name: enum_name(IntegralWidth::UInt8),
        }),
    ];
    (kind, arb_name(), value).prop_map(|(kind, name, value)| NamedArg { kind, name, value })
}

// ============================================================================
// Round-trip property
// ============================================================================

/// The owning type name is supplied to the decoder (it is not in the blob), so
/// the round-trip threads a fixed one through.
fn owning() -> TypeName {
    TypeName {
        namespace: "N".to_string(),
        name: "MyAttribute".to_string(),
    }
}

proptest! {
    /// `decode_blob(encode(fixed, named)) == (fixed, named)` for every generated
    /// supported attribute — fixed args typed by the ctor signature, named args
    /// self-describing, strings spanning all length bands.
    #[test]
    fn ca_blob_roundtrips(
        fixed in prop::collection::vec(arb_fixed(), 0..5),
        named in prop::collection::vec(arb_named_arg(), 0..5),
    ) {
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x0001u16.to_le_bytes()); // prolog
        for (_, v) in &fixed {
            enc_value(v, &mut blob);
        }
        blob.extend_from_slice(&(named.len() as u16).to_le_bytes());
        for n in &named {
            enc_named(n, &mut blob);
        }

        let ctor_param_types: Vec<ModifiedType> = fixed.iter().map(|(t, _)| t.clone()).collect();
        let expected_fixed: Vec<FixedArg> = fixed.iter().map(|(_, v)| v.clone()).collect();
        let resolve = |_: &TypeScope| -> TypeName {
            unreachable!("the round-trip emits no fixed enum arguments")
        };

        let decoded = decode_blob(&blob, &ctor_param_types, owning(), &resolve, &roundtrip_widths());
        prop_assert_eq!(
            decoded,
            Ok(DecodedAttribute {
                owning_type_name: owning(),
                fixed_args: expected_fixed,
                named_args: named,
                raw_blob: None,
            })
        );
    }

    /// ECMA-335 II.7.1.1 at the blob decoder: a custom modifier does not change
    /// how an argument is *encoded*, so an ignorable `modopt` on a constructor
    /// parameter must decode to exactly the same value as the bare parameter —
    /// and a `modreq` must be refused, since a required modifier here is by
    /// construction one we do not understand.
    ///
    /// Same blob, same expectation, modified parameter types: this is the
    /// reader-level half of the metamorphic property that
    /// `crate::modifier_metamorphic` runs over whole assemblies. It was a
    /// `modopt` here — on an attribute constructor — that made `[Obsolete]`
    /// undecodable and dropped the *type* carrying it.
    #[test]
    fn ca_blob_ignores_a_modopt_on_a_ctor_param(
        fixed in prop::collection::vec(arb_fixed(), 1..5),
    ) {
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x0001u16.to_le_bytes()); // prolog
        for (_, v) in &fixed {
            enc_value(v, &mut blob);
        }
        blob.extend_from_slice(&0u16.to_le_bytes()); // no named args

        let expected_fixed: Vec<FixedArg> = fixed.iter().map(|(_, v)| v.clone()).collect();
        let resolve = |_: &TypeScope| -> TypeName {
            unreachable!("the round-trip emits no fixed enum arguments")
        };
        // Any modifier type will do: the policy turns on the `required` bit, not
        // the name (the reader cannot resolve names — see `peel_ignorable_mods`).
        let modifier = |required| CustomMod {
            required,
            modifier: TypeScope::Reference(TypeRefId(0)),
        };
        let wrap = |required| -> Vec<ModifiedType> {
            fixed
                .iter()
                .map(|(t, _)| ModifiedType {
                    mods: vec![modifier(required)],
                    ty: t.ty.clone(),
                })
                .collect()
        };

        prop_assert_eq!(
            decode_blob(&blob, &wrap(false), owning(), &resolve, &roundtrip_widths()),
            Ok(DecodedAttribute {
                owning_type_name: owning(),
                fixed_args: expected_fixed,
                named_args: Vec::new(),
                raw_blob: None,
            })
        );
        prop_assert_eq!(
            decode_blob(&blob, &wrap(true), owning(), &resolve, &roundtrip_widths()),
            Err(AttrError::UnsupportedElementType(0x1f)) // CMOD_REQD
        );
    }

    /// Arbitrary blob bytes never panic the decoder, for a range of constructor
    /// parameter types (supported, enum, and refused).
    #[test]
    fn decode_blob_never_panics(
        bytes in prop::collection::vec(any::<u8>(), 0..512),
        param_types in prop::collection::vec(arb_fuzz_param_type(), 0..6),
    ) {
        let resolve = |_: &TypeScope| enum_name(IntegralWidth::Int32);
        let _ = decode_blob(&bytes, &param_types, owning(), &resolve, &roundtrip_widths());
    }
}

/// Constructor parameter types for the fuzz property — the supported set plus an
/// enum (exercising the width lookup) and refused types.
fn arb_fuzz_param_type() -> impl Strategy<Value = ModifiedType> {
    prop_oneof![
        Just(TypeSig::Primitive(Primitive::Boolean)),
        Just(TypeSig::Primitive(Primitive::Int32)),
        Just(TypeSig::Primitive(Primitive::UInt8)),
        Just(TypeSig::Primitive(Primitive::String)),
        Just(TypeSig::SzArray(Box::new(ModifiedType::plain(
            TypeSig::Primitive(Primitive::UInt8)
        )))),
        Just(TypeSig::SzArray(Box::new(ModifiedType::plain(
            TypeSig::Primitive(Primitive::String)
        )))),
        Just(TypeSig::Named {
            kind: Some(NamedKind::ValueType),
            scope: TypeScope::Reference(TypeRefId(0)),
        }),
        Just(TypeSig::Primitive(Primitive::Int64)),
        Just(TypeSig::Primitive(Primitive::Object)),
    ]
    .prop_map(ModifiedType::plain)
}

// ============================================================================
// Refusals
// ============================================================================

fn dummy_resolve(name: TypeName) -> impl Fn(&TypeScope) -> TypeName {
    move |_: &TypeScope| name.clone()
}

#[test]
fn refuses_unknown_enum_width() {
    // A fixed enum parameter whose type is absent from `EnumWidths`.
    let name = TypeName {
        namespace: "Some".to_string(),
        name: "Flags".to_string(),
    };
    let param = ModifiedType::plain(TypeSig::Named {
        kind: Some(NamedKind::ValueType),
        scope: TypeScope::Reference(TypeRefId(0)),
    });
    let mut blob = vec![0x01, 0x00];
    blob.extend_from_slice(&7u32.to_le_bytes());
    let r = decode_blob(
        &blob,
        std::slice::from_ref(&param),
        owning(),
        &dummy_resolve(name.clone()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::UnknownEnumWidth(name)));
}

#[test]
fn refuses_unsupported_fixed_arg_type() {
    // A `UInt64` constructor parameter is outside the supported integral subset
    // (`Int64` is the widest supported, for `[DateTimeConstant]`).
    let param = ModifiedType::plain(TypeSig::Primitive(Primitive::UInt64));
    let mut blob = vec![0x01, 0x00];
    blob.extend_from_slice(&0u64.to_le_bytes());
    let r = decode_blob(
        &blob,
        std::slice::from_ref(&param),
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::UnsupportedElementType(0x0b))); // U8 (UInt64)
}

#[test]
fn refuses_unsupported_named_arg_type() {
    // num_fixed = 0, num_named = 1, FIELD, FieldOrPropType = I8 (0x0a).
    let blob = vec![0x01, 0x00, 0x01, 0x00, 0x53, 0x0a];
    let r = decode_blob(
        &blob,
        &[],
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::UnsupportedElementType(0x0a)));
}

#[test]
fn refuses_null_array() {
    // A fixed `UInt8[]` parameter with the null-array count (0xFFFFFFFF).
    let param = ModifiedType::plain(TypeSig::SzArray(Box::new(ModifiedType::plain(
        TypeSig::Primitive(Primitive::UInt8),
    ))));
    let mut blob = vec![0x01, 0x00];
    blob.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let r = decode_blob(
        &blob,
        std::slice::from_ref(&param),
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::UnsupportedElementType(0x1d))); // SZARRAY
}

#[test]
fn refuses_empty_array_of_unsupported_element() {
    // An empty `UInt64[]` (count 0) must still be refused for its element type —
    // it would otherwise decode to a bogus `Array([])`.
    let param = ModifiedType::plain(TypeSig::SzArray(Box::new(ModifiedType::plain(
        TypeSig::Primitive(Primitive::UInt64),
    ))));
    let mut blob = vec![0x01, 0x00];
    blob.extend_from_slice(&0u32.to_le_bytes()); // count = 0
    let r = decode_blob(
        &blob,
        std::slice::from_ref(&param),
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::UnsupportedElementType(0x0b))); // U8 (UInt64)
}

#[test]
fn refuses_empty_array_of_unknown_width_enum() {
    // An empty enum array whose element type's width is unknown is refused, not
    // decoded to `Array([])`.
    let name = TypeName {
        namespace: "Some".to_string(),
        name: "Flags".to_string(),
    };
    let param = ModifiedType::plain(TypeSig::SzArray(Box::new(ModifiedType::plain(
        TypeSig::Named {
            kind: Some(NamedKind::ValueType),
            scope: TypeScope::Reference(TypeRefId(0)),
        },
    ))));
    let mut blob = vec![0x01, 0x00];
    blob.extend_from_slice(&0u32.to_le_bytes()); // count = 0
    let r = decode_blob(
        &blob,
        std::slice::from_ref(&param),
        owning(),
        &dummy_resolve(name.clone()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::UnknownEnumWidth(name)));
}

#[test]
fn refuses_bad_prolog() {
    let r = decode_blob(
        &[0x00, 0x00],
        &[],
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::Malformed));
}

#[test]
fn refuses_truncated_prolog() {
    let r = decode_blob(
        &[0x01],
        &[],
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::Truncated));
}

#[test]
fn refuses_trailing_bytes() {
    // A complete no-argument payload (prolog + num_named = 0) followed by a stray
    // byte: the length-delimited blob must be consumed exactly.
    let r = decode_blob(
        &[0x01, 0x00, 0x00, 0x00, 0xFF],
        &[],
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(r, Err(AttrError::Malformed));
}

#[test]
fn decodes_long_string_across_the_byte_boundary() {
    // A 200-byte ASCII string: the length prefix is the 2-byte compressed form
    // (a single-byte-prefix reader would mis-decode here). The decoder reads it
    // faithfully.
    let s = "z".repeat(200);
    let param = ModifiedType::plain(TypeSig::Primitive(Primitive::String));
    let mut blob = vec![0x01, 0x00];
    compress(s.len() as u32, &mut blob);
    blob.extend_from_slice(s.as_bytes());
    blob.extend_from_slice(&0u16.to_le_bytes()); // num_named = 0
    let r = decode_blob(
        &blob,
        std::slice::from_ref(&param),
        owning(),
        &dummy_resolve(owning()),
        &EnumWidths::new(),
    );
    assert_eq!(
        r,
        Ok(DecodedAttribute {
            owning_type_name: owning(),
            fixed_args: vec![FixedArg::String(Some(s))],
            named_args: vec![],
            raw_blob: None,
        })
    );
}

// ============================================================================
// Corpus fixtures: the bands the differential cannot cover
// ============================================================================

/// Decode every type-level attribute in the corpus, returning the first that
/// decodes to `Ok` and satisfies `pick`. `widths` covers any enum encountered.
fn find_corpus_attribute(
    widths: &EnumWidths,
    pick: impl Fn(&DecodedAttribute) -> bool,
) -> Option<DecodedAttribute> {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let image = parse(&bytes).expect("parse");
        for td in &image.type_defs {
            for raw in &td.attributes {
                if let Ok(d) = image.decode_attribute(raw, widths)
                    && pick(&d)
                {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// A real `[Obsolete("xxxx…")]` (the `MiniLib.ObsoleteLongMessage` fixture)
/// carries a 160-byte message — a 2-byte compressed length. The decoder reads
/// the exact text, so the fcs-dump differential compares the full message on
/// both sides (no degradation).
#[test]
fn decodes_long_string_attribute_in_corpus() {
    let found = find_corpus_attribute(&EnumWidths::new(), |d| {
        d.owning_type_name.name == "ObsoleteAttribute"
            && matches!(d.fixed_args.first(), Some(FixedArg::String(Some(s))) if s.len() >= 128)
    });
    let d = found.expect("corpus has an Obsolete attribute with a >=128-byte message");
    let Some(FixedArg::String(Some(message))) = d.fixed_args.first() else {
        unreachable!("pick guaranteed a string first arg");
    };
    // `MiniLib.ObsoleteLongMessage` is a run of ASCII 'x's past the 0x7F
    // single-byte boundary; the decoder must read every byte faithfully (a
    // single-byte-prefix reader would truncate/garble here). The exact length
    // is the fixture's business — pin only that it crossed the boundary and is
    // intact.
    assert!(
        message.len() >= 128,
        "message length {} below the band",
        message.len()
    );
    assert!(
        message.bytes().all(|b| b == b'x'),
        "message corrupted: {message:?}"
    );
}

/// A real `[CompilationMapping(SourceConstructFlags.…)]` (every `MiniLibFs`
/// type) has an enum-typed first argument, decoded against a supplied
/// `EnumWidths` — exercising the fixed-enum path (scope → type name → width)
/// end-to-end on a real blob.
#[test]
fn decodes_enum_attribute_in_corpus_with_supplied_width() {
    let mut widths = EnumWidths::new();
    let flags = TypeName {
        namespace: "Microsoft.FSharp.Core".to_string(),
        name: "SourceConstructFlags".to_string(),
    };
    widths.insert(flags.clone(), IntegralWidth::Int32);

    let found = find_corpus_attribute(&widths, |d| {
        d.owning_type_name.name == "CompilationMappingAttribute"
            && matches!(d.fixed_args.first(), Some(FixedArg::Enum { .. }))
    });
    let d = found.expect("MiniLibFs has CompilationMappingAttribute with an enum argument");
    match d.fixed_args.first() {
        Some(FixedArg::Enum {
            underlying: IntegralParam::Int32(_),
            type_name,
        }) => assert_eq!(*type_name, flags),
        other => panic!("expected an int32-backed enum first arg, got {other:?}"),
    }
}
