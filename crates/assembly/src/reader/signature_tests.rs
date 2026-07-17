//! Stage 2 correctness oracles for the `TypeSig` signature decoder.
//!
//! - Reference round-trip: a naive encoder emits the spec's bytes for the
//!   supported `TypeSig` subset; `decode_type(encode(t)) == Ok(t)` for every
//!   generated `t`. This is the prime property.
//! - Refusal: every unsupported element byte yields the matching `SigError`.
//!   (A custom modifier is *not* refused here — both kinds decode; whether a
//!   `modreq` is understood is the projector's call, per ECMA-335 II.7.1.1.)
//! - Fuzz: arbitrary blob bytes are always `Ok` or `Err(SigError)`, never a
//!   panic.

use super::ids::{TypeDefId, TypeRefId};
use super::signature::{
    CallConv, CustomMod, DecodedMethodSig, ImageTables, ModifiedType, NamedKind, Primitive,
    RetType, SigError, TypeScope, TypeSig, decode_field_sig, decode_method_sig,
    decode_property_sig, decode_type,
};
use proptest::prelude::*;

// --- ECMA-335 II.23.1.16 element-type bytes (mirrored for the encoder) ---
const E_BOOLEAN: u8 = 0x02;
const E_CHAR: u8 = 0x03;
const E_I1: u8 = 0x04;
const E_U1: u8 = 0x05;
const E_I2: u8 = 0x06;
const E_U2: u8 = 0x07;
const E_I4: u8 = 0x08;
const E_U4: u8 = 0x09;
const E_I8: u8 = 0x0a;
const E_U8: u8 = 0x0b;
const E_R4: u8 = 0x0c;
const E_R8: u8 = 0x0d;
const E_STRING: u8 = 0x0e;
const E_PTR: u8 = 0x0f;
const E_BYREF: u8 = 0x10;
const E_VALUETYPE: u8 = 0x11;
const E_CLASS: u8 = 0x12;
const E_VAR: u8 = 0x13;
const E_ARRAY: u8 = 0x14;
const E_GENERICINST: u8 = 0x15;
const E_TYPEDBYREF: u8 = 0x16;
const E_I: u8 = 0x18;
const E_U: u8 = 0x19;
const E_FNPTR: u8 = 0x1b;
const E_OBJECT: u8 = 0x1c;
const E_SZARRAY: u8 = 0x1d;
const E_MVAR: u8 = 0x1e;
const E_CMOD_REQD: u8 = 0x1f;
const E_CMOD_OPT: u8 = 0x20;
const E_SENTINEL: u8 = 0x41;
const E_PINNED: u8 = 0x45;
const E_VOID: u8 = 0x01;

/// Row count used for both tables in the round-trip context; generated scope
/// indices stay in `0..ROWS` so every coded token resolves.
const ROWS: u32 = 64;

fn tables() -> ImageTables {
    ImageTables {
        type_def_rows: ROWS,
        type_ref_rows: ROWS,
    }
}

// --- Reference encoder (II.23.2) ---

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

/// Inverse of `Cursor::read_compressed_i32` (ECMA-335 II.23.2 signed form):
/// rotate the sign into the LSB of a 7/14/29-bit field.
fn compress_signed(n: i32, out: &mut Vec<u8>) {
    let sign = u32::from(n < 0);
    if (-64..=63).contains(&n) {
        out.push(((((n as u32) & 0x3F) << 1) | sign) as u8);
    } else if (-8192..=8191).contains(&n) {
        let raw = (((n as u32) & 0x1FFF) << 1) | sign;
        out.push(0x80 | (raw >> 8) as u8);
        out.push((raw & 0xFF) as u8);
    } else {
        let raw = (((n as u32) & 0x0FFF_FFFF) << 1) | sign;
        out.push(0xC0 | (raw >> 24) as u8);
        out.push((raw >> 16) as u8);
        out.push((raw >> 8) as u8);
        out.push((raw & 0xFF) as u8);
    }
}

fn encode_token(scope: TypeScope, out: &mut Vec<u8>) {
    let token = match scope {
        TypeScope::Definition(TypeDefId(i)) => (i + 1) << 2,
        TypeScope::Reference(TypeRefId(i)) => ((i + 1) << 2) | 1,
    };
    compress(token, out);
}

fn primitive_byte(p: Primitive) -> u8 {
    match p {
        Primitive::Boolean => E_BOOLEAN,
        Primitive::Char => E_CHAR,
        Primitive::Int8 => E_I1,
        Primitive::UInt8 => E_U1,
        Primitive::Int16 => E_I2,
        Primitive::UInt16 => E_U2,
        Primitive::Int32 => E_I4,
        Primitive::UInt32 => E_U4,
        Primitive::Int64 => E_I8,
        Primitive::UInt64 => E_U8,
        Primitive::Float32 => E_R4,
        Primitive::Float64 => E_R8,
        Primitive::IntPtr => E_I,
        Primitive::UIntPtr => E_U,
        Primitive::String => E_STRING,
        Primitive::Object => E_OBJECT,
    }
}

fn named_kind_byte(k: NamedKind) -> u8 {
    match k {
        NamedKind::Class => E_CLASS,
        NamedKind::ValueType => E_VALUETYPE,
    }
}

/// An unmodified position — what almost every decode expectation below wants,
/// since the run is empty for all but the modifier tests.
fn p(ty: TypeSig) -> ModifiedType {
    ModifiedType::plain(ty)
}

/// Decode a position that is expected to carry *no* modifiers, yielding the type
/// itself. The run being empty is part of the expectation: these fixtures pin the
/// type, and a stray modifier appearing in one of them would be a decoder bug.
fn decode_unmodified(blob: &[u8], tables: &ImageTables) -> Result<TypeSig, SigError> {
    decode_type(blob, tables).map(|mt| {
        assert!(mt.mods.is_empty(), "expected an unmodified position");
        mt.ty
    })
}

/// [`p`], boxed, for a child slot.
fn b(ty: TypeSig) -> Box<ModifiedType> {
    Box::new(p(ty))
}

/// Encode a position: its `CustomMod*` run, then the type (II.23.2.7).
fn encode(mt: &ModifiedType, out: &mut Vec<u8>) {
    for m in &mt.mods {
        out.push(if m.required { E_CMOD_REQD } else { E_CMOD_OPT });
        encode_token(m.modifier, out);
    }
    encode_element(&mt.ty, out);
}

/// Encode the type proper; every child slot is a position, so it goes back
/// through [`encode`].
fn encode_element(ty: &TypeSig, out: &mut Vec<u8>) {
    match ty {
        TypeSig::Primitive(p) => out.push(primitive_byte(*p)),
        TypeSig::Named {
            kind: Some(k),
            scope,
        } => {
            out.push(named_kind_byte(*k));
            encode_token(*scope, out);
        }
        TypeSig::Named { kind: None, .. } => {
            unreachable!("kind:None is token-sourced, never produced by signature encoding")
        }
        TypeSig::Generic {
            kind: Some(k),
            scope,
            args,
        } => {
            out.push(E_GENERICINST);
            out.push(named_kind_byte(*k));
            encode_token(*scope, out);
            compress(args.len() as u32, out);
            for a in args {
                encode(a, out);
            }
        }
        TypeSig::Generic { kind: None, .. } => unreachable!("kind:None"),
        TypeSig::TypeVar(n) => {
            out.push(E_VAR);
            compress(*n, out);
        }
        TypeSig::MethodVar(n) => {
            out.push(E_MVAR);
            compress(*n, out);
        }
        TypeSig::SzArray(inner) => {
            out.push(E_SZARRAY);
            encode(inner, out);
        }
        TypeSig::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => {
            out.push(E_ARRAY);
            encode(element, out);
            compress(*rank, out);
            compress(sizes.len() as u32, out);
            for &s in sizes {
                compress(s, out);
            }
            compress(lower_bounds.len() as u32, out);
            for &lo in lower_bounds {
                compress_signed(lo, out);
            }
        }
        TypeSig::Ptr(inner) => {
            out.push(E_PTR);
            match inner {
                Some(p) => encode(p, out),
                None => out.push(E_VOID), // void*
            }
        }
        TypeSig::ByRef(inner) => {
            out.push(E_BYREF);
            encode(inner, out);
        }
        TypeSig::TypedByRef => out.push(E_TYPEDBYREF),
    }
}

// --- Generators ---

fn arb_primitive() -> impl Strategy<Value = Primitive> {
    prop_oneof![
        Just(Primitive::Boolean),
        Just(Primitive::Char),
        Just(Primitive::Int8),
        Just(Primitive::UInt8),
        Just(Primitive::Int16),
        Just(Primitive::UInt16),
        Just(Primitive::Int32),
        Just(Primitive::UInt32),
        Just(Primitive::Int64),
        Just(Primitive::UInt64),
        Just(Primitive::Float32),
        Just(Primitive::Float64),
        Just(Primitive::IntPtr),
        Just(Primitive::UIntPtr),
        Just(Primitive::String),
        Just(Primitive::Object),
    ]
}

fn arb_named_kind() -> impl Strategy<Value = NamedKind> {
    prop_oneof![Just(NamedKind::Class), Just(NamedKind::ValueType)]
}

fn arb_scope() -> impl Strategy<Value = TypeScope> {
    prop_oneof![
        (0..ROWS).prop_map(|i| TypeScope::Definition(TypeDefId(i))),
        (0..ROWS).prop_map(|i| TypeScope::Reference(TypeRefId(i))),
    ]
}

/// Both modifier kinds — `modreq` and `modopt` — so the round-trip covers the
/// `CMOD_OPT` byte the decoder used to refuse outright.
fn arb_custom_mod() -> impl Strategy<Value = CustomMod> {
    (any::<bool>(), arb_scope()).prop_map(|(required, modifier)| CustomMod { required, modifier })
}

/// An arbitrary *position*: a (usually empty, sometimes interleaved) modifier
/// run in front of an arbitrary type, recursively — so the round-trip exercises
/// modifiers at **every** slot the wire allows one, not just at a node the old
/// generator happened to wrap.
fn arb_type_sig() -> impl Strategy<Value = ModifiedType> {
    let leaf = prop_oneof![
        arb_primitive().prop_map(TypeSig::Primitive),
        (arb_named_kind(), arb_scope()).prop_map(|(k, scope)| TypeSig::Named {
            kind: Some(k),
            scope
        }),
        (0u32..0x1FFF_FFFF).prop_map(TypeSig::TypeVar),
        (0u32..0x1FFF_FFFF).prop_map(TypeSig::MethodVar),
        Just(TypeSig::TypedByRef),
    ]
    .prop_flat_map(arb_position);
    leaf.prop_recursive(5, 64, 4, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| TypeSig::SzArray(Box::new(t))),
            (1u32..=4, inner.clone()).prop_map(|(rank, t)| TypeSig::Array {
                element: Box::new(t),
                rank,
                // Bounded shapes are round-tripped by a dedicated test below;
                // the generator keeps the common unbounded form.
                sizes: Vec::new(),
                lower_bounds: Vec::new(),
            }),
            inner.clone().prop_map(|t| TypeSig::ByRef(Box::new(t))),
            (
                arb_named_kind(),
                arb_scope(),
                prop::collection::vec(inner, 0..4)
            )
                .prop_map(|(k, scope, args)| TypeSig::Generic {
                    kind: Some(k),
                    scope,
                    args
                }),
        ]
        .prop_flat_map(arb_position)
    })
}

/// Put an arbitrary modifier run in front of `ty`. Mostly empty — the common
/// case on the wire, and the one the rest of the decoder must stay fast for.
fn arb_position(ty: TypeSig) -> impl Strategy<Value = ModifiedType> {
    prop::collection::vec(arb_custom_mod(), 0..3).prop_map(move |mods| ModifiedType {
        mods,
        ty: ty.clone(),
    })
}

// --- Properties ---

proptest! {
    /// `decode_type(encode(t)) == Ok(t)` for every supported `TypeSig`.
    #[test]
    fn decode_roundtrips_encode(t in arb_type_sig()) {
        let mut bytes = Vec::new();
        encode(&t, &mut bytes);
        prop_assert_eq!(decode_type(&bytes, &tables()), Ok(t));
    }

    /// Arbitrary bytes never panic the decoder.
    #[test]
    fn decode_never_panics(
        bytes in proptest::collection::vec(any::<u8>(), 0..256),
        def_rows in 0u32..256,
        ref_rows in 0u32..256,
    ) {
        let ctx = ImageTables { type_def_rows: def_rows, type_ref_rows: ref_rows };
        let _ = decode_type(&bytes, &ctx);
    }
}

// --- Refusals ---

fn decode_byte(byte: u8) -> Result<TypeSig, SigError> {
    decode_unmodified(&[byte], &tables())
}

#[test]
fn refuses_unsupported_elements() {
    // `E_ARRAY`, `E_PTR`, and `E_TYPEDBYREF` are now decoded, so they are no
    // longer here.
    for b in [E_FNPTR, E_SENTINEL, E_PINNED] {
        assert_eq!(
            decode_byte(b),
            Err(SigError::UnsupportedElement(b)),
            "byte {b:#x}"
        );
    }
}

#[test]
fn decodes_typed_reference() {
    // `ELEMENT_TYPE_TYPEDBYREF` (0x16) is a token-free built-in element type.
    // FCS imports it as `ILType.Value(System.TypedReference)`
    // (`ilread.fs:2671`); the decoder mirrors that with a dedicated nullary
    // `TypedByRef` the projector maps to the `System.TypedReference` value type.
    assert_eq!(decode_byte(E_TYPEDBYREF), Ok(TypeSig::TypedByRef));
}

#[test]
fn decodes_pointer() {
    // `PTR I4` — `int*`: the pointee survives as a nested type.
    let blob = [E_PTR, E_I4];
    assert_eq!(
        decode_unmodified(&blob, &tables()),
        Ok(TypeSig::Ptr(Some(b(TypeSig::Primitive(Primitive::Int32))))),
    );
    // `PTR VOID` — `void*` (F#'s `voidptr`): a void pointee, modelled as `None`.
    assert_eq!(
        decode_unmodified(&[E_PTR, E_VOID], &tables()),
        Ok(TypeSig::Ptr(None)),
    );
    // `nativeptr<!!0>` shape — `PTR MVAR 0` (`'T*` on a generic method): the
    // method type-var pointee round-trips through the encoder too.
    let original = p(TypeSig::Ptr(Some(b(TypeSig::MethodVar(0)))));
    let mut bytes = Vec::new();
    encode(&original, &mut bytes);
    assert_eq!(decode_type(&bytes, &tables()), Ok(original));
}

#[test]
fn refuses_void() {
    assert_eq!(decode_byte(E_VOID), Err(SigError::UnexpectedVoid));
}

#[test]
fn decodes_unbounded_multidim_array() {
    // `ARRAY I4 <rank=2> <NumSizes=0> <NumLoBounds=0>` — a plain `int[,]`: the
    // element type and rank survive with an empty shape.
    let blob = [E_ARRAY, E_I4, 2, 0, 0];
    assert_eq!(
        decode_unmodified(&blob, &tables()),
        Ok(TypeSig::Array {
            element: b(TypeSig::Primitive(Primitive::Int32)),
            rank: 2,
            sizes: vec![],
            lower_bounds: vec![],
        }),
    );
}

#[test]
fn decodes_bounded_array() {
    // `ARRAY I4 <rank=1> <NumSizes=1> <Size=5> <NumLoBounds=0>` — `int[5]`: the
    // declared size is carried, not refused or flattened away.
    let blob = [E_ARRAY, E_I4, 1, 1, 5, 0];
    assert_eq!(
        decode_unmodified(&blob, &tables()),
        Ok(TypeSig::Array {
            element: b(TypeSig::Primitive(Primitive::Int32)),
            rank: 1,
            sizes: vec![5],
            lower_bounds: vec![],
        }),
    );
    // A lower-bounded array with a *negative* bound: `ARRAY I4 <rank=1>
    // <NumSizes=0> <NumLoBounds=1> <LoBound=-1>` (`-1` compresses-signed to 0x7F).
    let blob = [E_ARRAY, E_I4, 1, 0, 1, 0x7F];
    assert_eq!(
        decode_unmodified(&blob, &tables()),
        Ok(TypeSig::Array {
            element: b(TypeSig::Primitive(Primitive::Int32)),
            rank: 1,
            sizes: vec![],
            lower_bounds: vec![-1],
        }),
    );
}

#[test]
fn rejects_array_shape_count_exceeding_rank() {
    // `NumSizes`/`NumLoBounds` may not exceed `Rank` (II.23.2.13).
    let blob = [E_ARRAY, E_I4, 1, 2, 3, 4]; // rank 1, NumSizes 2
    assert_eq!(decode_type(&blob, &tables()), Err(SigError::BadToken));
}

#[test]
fn bounded_array_round_trips_through_encoder() {
    // Build a rank-3 array with mixed sizes and signed bounds, encode it with
    // the reference encoder, and decode it back — exercising both directions of
    // the shape codec (incl. the signed lower-bound path).
    let original = p(TypeSig::Array {
        element: b(TypeSig::Primitive(Primitive::Int32)),
        rank: 3,
        sizes: vec![4, 9000],
        lower_bounds: vec![-100000, 0, 7],
    });
    let mut bytes = Vec::new();
    encode(&original, &mut bytes);
    assert_eq!(decode_type(&bytes, &tables()), Ok(original));
}

#[test]
fn refuses_zero_rank_array() {
    // `ARRAY I4 <rank=0> …` — ECMA-335 requires `Rank >= 1`; a zero rank is
    // malformed metadata, refused rather than projected as an impossible array.
    let blob = [E_ARRAY, E_I4, 0, 0, 0];
    assert_eq!(decode_type(&blob, &tables()), Err(SigError::BadToken));
}

#[test]
fn preserves_optional_modifier_token() {
    // CMOD_OPT <token> I4 — preserved, `required: false`. The decoder is total
    // over both modifier bytes; ECMA-335 II.7.1.1's "a `modopt` may be ignored"
    // is a *projector* policy, and the projector cannot apply it to a modifier
    // the decoder threw away.
    let mut bytes = vec![E_CMOD_OPT];
    encode_token(TypeScope::Reference(TypeRefId(0)), &mut bytes);
    bytes.push(E_I4);
    assert_eq!(
        decode_type(&bytes, &tables()),
        Ok(ModifiedType {
            mods: vec![CustomMod {
                required: false,
                modifier: TypeScope::Reference(TypeRefId(0)),
            }],
            ty: TypeSig::Primitive(Primitive::Int32),
        })
    );
}

#[test]
fn preserves_required_modifier_token() {
    // CMOD_REQD <token> I4 — preserved as a `required` entry in the position's
    // run, recording the modifier's scope beside the type it precedes.
    // Classifying the modifier (as the read-only-ref marker, the volatile
    // marker, or an unrecognised one) is the name-resolving projector's job.
    let mut bytes = vec![E_CMOD_REQD];
    encode_token(TypeScope::Reference(TypeRefId(3)), &mut bytes);
    bytes.push(E_I4);
    assert_eq!(
        decode_type(&bytes, &tables()),
        Ok(ModifiedType {
            mods: vec![CustomMod {
                required: true,
                modifier: TypeScope::Reference(TypeRefId(3)),
            }],
            ty: TypeSig::Primitive(Primitive::Int32),
        })
    );
}

#[test]
fn decodes_interleaved_modifier_run() {
    // `CMOD_REQD a CMOD_OPT b CMOD_REQD c I4` — II.23.2.7 allows the two kinds to
    // interleave freely; the run is kept in signature order, on the position.
    let mut bytes = vec![E_CMOD_REQD];
    encode_token(TypeScope::Reference(TypeRefId(1)), &mut bytes);
    bytes.push(E_CMOD_OPT);
    encode_token(TypeScope::Definition(TypeDefId(2)), &mut bytes);
    bytes.push(E_CMOD_REQD);
    encode_token(TypeScope::Reference(TypeRefId(3)), &mut bytes);
    bytes.push(E_I4);
    let reqd = |scope| CustomMod {
        required: true,
        modifier: scope,
    };
    let opt = |scope| CustomMod {
        required: false,
        modifier: scope,
    };
    assert_eq!(
        decode_type(&bytes, &tables()),
        Ok(ModifiedType {
            mods: vec![
                reqd(TypeScope::Reference(TypeRefId(1))),
                opt(TypeScope::Definition(TypeDefId(2))),
                reqd(TypeScope::Reference(TypeRefId(3))),
            ],
            ty: TypeSig::Primitive(Primitive::Int32),
        })
    );
}

#[test]
fn refuses_empty_blob() {
    assert_eq!(decode_type(&[], &tables()), Err(SigError::Truncated));
}

#[test]
fn refuses_overly_deep_nesting() {
    // A pathological SZARRAY chain must be refused with a bounded error rather
    // than recursing until the stack overflows.
    let mut bytes = vec![E_SZARRAY; 100_000];
    bytes.push(E_I4);
    assert_eq!(decode_type(&bytes, &tables()), Err(SigError::TooDeep));
}

#[test]
fn refuses_out_of_range_token() {
    // CLASS with a RID past the TypeRef table.
    let mut bytes = vec![E_CLASS];
    // token: tag=1 (TypeRef), RID = ROWS + 5 (out of range)
    let token = ((ROWS + 5) << 2) | 1;
    compress(token, &mut bytes);
    assert_eq!(decode_type(&bytes, &tables()), Err(SigError::BadToken));
}

#[test]
fn refuses_zero_rid_token() {
    // CLASS with RID 0 is never valid (tables are 1-based).
    let mut bytes = vec![E_CLASS];
    compress(0, &mut bytes); // token 0 => tag 0, RID 0
    assert_eq!(decode_type(&bytes, &tables()), Err(SigError::BadToken));
}

#[test]
fn resolves_class_and_valuetype_tokens() {
    let mut class = vec![E_CLASS];
    encode_token(TypeScope::Reference(TypeRefId(7)), &mut class);
    assert_eq!(
        decode_unmodified(&class, &tables()),
        Ok(TypeSig::Named {
            kind: Some(NamedKind::Class),
            scope: TypeScope::Reference(TypeRefId(7))
        })
    );

    let mut vt = vec![E_VALUETYPE];
    encode_token(TypeScope::Definition(TypeDefId(2)), &mut vt);
    assert_eq!(
        decode_unmodified(&vt, &tables()),
        Ok(TypeSig::Named {
            kind: Some(NamedKind::ValueType),
            scope: TypeScope::Definition(TypeDefId(2))
        })
    );
}

// ============================================================================
// Member signatures (method / field / property)
// ============================================================================
//
// Same reference-round-trip strategy as the `TypeSig` core: a naive encoder
// emits the spec's bytes, and `decode_*(encode(x)) == Ok(x)` for every
// generated `x`. The `SerString`/compressed-int correctness this shares with
// the `TypeSig` encoder carries straight over.

// ECMA-335 II.23.2.3 calling-convention byte flags (mirrored for the encoder).
const CC_VARARG: u8 = 0x05;
const CC_FIELD: u8 = 0x06;
const CC_PROPERTY: u8 = 0x08;
const CC_GENERIC: u8 = 0x10;
const CC_HASTHIS: u8 = 0x20;
const CC_EXPLICITTHIS: u8 = 0x40;

fn encode_method_sig(sig: &DecodedMethodSig, out: &mut Vec<u8>) {
    let mut first = 0u8;
    if sig.has_this {
        first |= CC_HASTHIS;
    }
    if sig.explicit_this {
        first |= CC_EXPLICITTHIS;
    }
    match sig.calling_convention {
        CallConv::Default => {}
        CallConv::VarArg => first |= CC_VARARG,
        CallConv::Generic { .. } => first |= CC_GENERIC,
    }
    out.push(first);
    if let CallConv::Generic { count } = sig.calling_convention {
        compress(count, out);
    }
    compress(sig.param_types.len() as u32, out);
    match &sig.return_type {
        RetType::Void(modifiers) => {
            for m in modifiers {
                out.push(if m.required { E_CMOD_REQD } else { E_CMOD_OPT });
                encode_token(m.modifier, out);
            }
            out.push(E_VOID);
        }
        RetType::Type(t) => encode(t, out),
    }
    for p in &sig.param_types {
        encode(p, out);
    }
}

fn encode_field_sig(t: &ModifiedType, out: &mut Vec<u8>) {
    out.push(CC_FIELD);
    encode(t, out);
}

/// `index_count` and the index parameters are not projected by
/// [`decode_property_sig`]; the encoder emits the count (which the decoder
/// consumes and ignores) and stops at the property type, so the round-trip
/// pins exactly what the decoder reads.
fn encode_property_sig(has_this: bool, index_count: u32, t: &ModifiedType, out: &mut Vec<u8>) {
    let mut first = CC_PROPERTY;
    if has_this {
        first |= CC_HASTHIS;
    }
    out.push(first);
    compress(index_count, out);
    encode(t, out);
}

fn arb_call_conv() -> impl Strategy<Value = CallConv> {
    prop_oneof![
        Just(CallConv::Default),
        Just(CallConv::VarArg),
        (0u32..0x1FFF_FFFF).prop_map(|count| CallConv::Generic { count }),
    ]
}

fn arb_ret_type() -> impl Strategy<Value = RetType> {
    prop_oneof![
        Just(RetType::Void(Vec::new())),
        // `CustomMod+ VOID` — a modified void return (the `init`-setter shape).
        proptest::collection::vec(arb_custom_mod(), 1..=3).prop_map(RetType::Void),
        arb_type_sig().prop_map(RetType::Type),
    ]
}

fn arb_method_sig() -> impl Strategy<Value = DecodedMethodSig> {
    (
        any::<bool>(),
        any::<bool>(),
        arb_call_conv(),
        arb_ret_type(),
        prop::collection::vec(arb_type_sig(), 0..6),
    )
        .prop_map(
            |(has_this, explicit_this, calling_convention, return_type, param_types)| {
                DecodedMethodSig {
                    has_this,
                    explicit_this,
                    calling_convention,
                    return_type,
                    param_types,
                }
            },
        )
}

proptest! {
    /// `decode_method_sig(encode(sig)) == Ok(sig)` for every supported method
    /// signature: the calling convention, `this` flags, return type, and the
    /// parameter type vector all round-trip.
    #[test]
    fn method_sig_roundtrips(sig in arb_method_sig()) {
        let mut bytes = Vec::new();
        encode_method_sig(&sig, &mut bytes);
        prop_assert_eq!(decode_method_sig(&bytes, &tables()), Ok(sig));
    }

    /// `decode_field_sig(encode(t)) == Ok(t)` for every supported field type.
    #[test]
    fn field_sig_roundtrips(t in arb_type_sig()) {
        let mut bytes = Vec::new();
        encode_field_sig(&t, &mut bytes);
        prop_assert_eq!(decode_field_sig(&bytes, &tables()), Ok(t));
    }

    /// `decode_property_sig(encode(.., t)) == Ok(t)` regardless of the `HASTHIS`
    /// bit or the (ignored) index-parameter count.
    #[test]
    fn property_sig_roundtrips(
        has_this in any::<bool>(),
        index_count in 0u32..0x1FFF_FFFF,
        t in arb_type_sig(),
    ) {
        let mut bytes = Vec::new();
        encode_property_sig(has_this, index_count, &t, &mut bytes);
        prop_assert_eq!(decode_property_sig(&bytes, &tables()), Ok(t));
    }

    /// Arbitrary bytes never panic the three member-signature decoders.
    #[test]
    fn member_sigs_never_panic(
        bytes in proptest::collection::vec(any::<u8>(), 0..256),
        def_rows in 0u32..256,
        ref_rows in 0u32..256,
    ) {
        let ctx = ImageTables { type_def_rows: def_rows, type_ref_rows: ref_rows };
        let _ = decode_method_sig(&bytes, &ctx);
        let _ = decode_field_sig(&bytes, &ctx);
        let _ = decode_property_sig(&bytes, &ctx);
    }
}

#[test]
fn method_sig_decodes_void_return_and_generic_arity() {
    // `GENERIC` (0x10) + `HASTHIS` (0x20), arity 2, 1 param, void return, I4 param.
    let bytes = [CC_GENERIC | CC_HASTHIS, 0x02, 0x01, E_VOID, E_I4];
    assert_eq!(
        decode_method_sig(&bytes, &tables()),
        Ok(DecodedMethodSig {
            has_this: true,
            explicit_this: false,
            calling_convention: CallConv::Generic { count: 2 },
            return_type: RetType::Void(vec![]),
            param_types: vec![p(TypeSig::Primitive(Primitive::Int32))],
        })
    );
}

#[test]
fn method_sig_decodes_init_setter_modified_void_return() {
    // A C# 9 `init` setter `set_P(int)`: `HASTHIS`, 1 param, and a `void` return
    // under `modreq(<token>)` — `CMOD_REQD <token> VOID`. The decoder recognises
    // the modified `void` as `RetType::Void` carrying the modifier run
    // (the projector decides it is the accepted `IsExternalInit` marker); the
    // trailing `I4` is the value parameter.
    let mut bytes = vec![CC_HASTHIS, 0x01, E_CMOD_REQD];
    encode_token(TypeScope::Definition(TypeDefId(0)), &mut bytes);
    bytes.push(E_VOID);
    bytes.push(E_I4);
    assert_eq!(
        decode_method_sig(&bytes, &tables()),
        Ok(DecodedMethodSig {
            has_this: true,
            explicit_this: false,
            calling_convention: CallConv::Default,
            return_type: RetType::Void(vec![CustomMod {
                required: true,
                modifier: TypeScope::Definition(TypeDefId(0)),
            }]),
            param_types: vec![p(TypeSig::Primitive(Primitive::Int32))],
        })
    );
}

#[test]
fn method_sig_keeps_modreq_on_a_non_void_return() {
    // A `modreq(<token>) byref I4` return (`ref readonly` shape): the run belongs
    // to the return *position*, so it is simply recorded there, in front of the
    // byref it qualifies. The projector reads it as the read-only marker.
    //
    // Under the old encoding the decoder had to peel the leading modifiers to
    // check for a `void`, then *re-attach* them as wrapper nodes over the decoded
    // type — a dance this test used to pin. There is nothing to re-attach now.
    let mut bytes = vec![0x00u8, 0x00, E_CMOD_REQD];
    encode_token(TypeScope::Definition(TypeDefId(1)), &mut bytes);
    bytes.push(E_BYREF);
    bytes.push(E_I4);
    assert_eq!(
        decode_method_sig(&bytes, &tables()),
        Ok(DecodedMethodSig {
            has_this: false,
            explicit_this: false,
            calling_convention: CallConv::Default,
            return_type: RetType::Type(ModifiedType {
                mods: vec![CustomMod {
                    required: true,
                    modifier: TypeScope::Definition(TypeDefId(1)),
                }],
                ty: TypeSig::ByRef(b(TypeSig::Primitive(Primitive::Int32))),
            }),
            param_types: vec![],
        })
    );
}

#[test]
fn method_sig_bounds_return_modreq_nesting_depth() {
    // A hostile run of leading `modreq`s before a (non-void) return must not
    // escape the recursion cap: `decode_ret_type` peels them but seeds the inner
    // decode at the peeled depth, so a run past the cap is refused (`TooDeep`)
    // rather than folding into a `TypeSig` deeper than the stack-safety bound.
    // 300 is well past `MAX_DEPTH` (128) whatever its exact value.
    let mut bytes = vec![0x00u8, 0x00]; // DEFAULT, 0 params
    for _ in 0..300 {
        bytes.push(E_CMOD_REQD);
        encode_token(TypeScope::Definition(TypeDefId(0)), &mut bytes);
    }
    bytes.push(E_I4);
    assert_eq!(decode_method_sig(&bytes, &tables()), Err(SigError::TooDeep));
}

#[test]
fn method_sig_refuses_native_calling_convention() {
    // 0x03 = STDCALL, which never appears on a MethodDef.
    let bytes = [0x03u8, 0x00, E_VOID];
    assert_eq!(
        decode_method_sig(&bytes, &tables()),
        Err(SigError::UnsupportedElement(0x03))
    );
}

#[test]
fn method_sig_refuses_generic_combined_with_other_convention() {
    // 0x15 = GENERIC (0x10) | VARARG (0x05): `GENERIC` is a distinct convention
    // value, not a flag, so a byte that also sets low convention bits is
    // malformed and must be refused — not read as a plain generic method (which
    // would then consume the next byte as a generic-parameter count).
    let bytes = [0x15u8, 0x01, 0x00, E_VOID];
    assert_eq!(
        decode_method_sig(&bytes, &tables()),
        Err(SigError::UnsupportedElement(0x15))
    );
}

#[test]
fn method_sig_refuses_void_parameter() {
    // DEFAULT, 1 param, I4 return, VOID param — a void parameter is illegal.
    let bytes = [0x00u8, 0x01, E_I4, E_VOID];
    assert_eq!(
        decode_method_sig(&bytes, &tables()),
        Err(SigError::UnexpectedVoid)
    );
}

#[test]
fn field_sig_requires_field_sentinel() {
    // A blob that does not begin with FIELD (0x06) is refused.
    assert_eq!(
        decode_field_sig(&[E_I4], &tables()),
        Err(SigError::BadToken)
    );
    // The well-formed shape decodes.
    assert_eq!(
        decode_field_sig(&[CC_FIELD, E_STRING], &tables()),
        Ok(p(TypeSig::Primitive(Primitive::String)))
    );
}

#[test]
fn property_sig_requires_property_sentinel() {
    // Not a PROPERTY sentinel in the low 5 bits.
    assert_eq!(
        decode_property_sig(&[CC_FIELD, 0x00, E_I4], &tables()),
        Err(SigError::BadToken)
    );
    // PROPERTY | HASTHIS, zero index params, Object type.
    assert_eq!(
        decode_property_sig(&[CC_PROPERTY | CC_HASTHIS, 0x00, E_OBJECT], &tables()),
        Ok(p(TypeSig::Primitive(Primitive::Object)))
    );
}
