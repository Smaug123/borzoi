//! `u_const` — constant values at attribute-argument and `Expr.Const`
//! positions.
//!
//! Mirrors the complete FCS dispatcher at
//! `TypedTreePickle.fs:3394-3416`. Every reader primitive matches the FCS
//! definition at `:435-454`:
//!
//! | Tag | Variant | Wire payload                                       |
//! |-----|---------|----------------------------------------------------|
//! | 0   | Bool    | `u_bool` (1 byte)                                  |
//! | 1   | SByte   | `sbyte (u_int32)` (compressed, truncated)         |
//! | 2   | Byte    | `byte (u_byte)` — a *raw* byte, not `u_int32`      |
//! | 3   | Int16   | `int16 (u_int32)` (compressed, truncated)         |
//! | 4   | UInt16  | `uint16 (u_int32)` (compressed, truncated)        |
//! | 5   | Int32   | `u_int32` (compressed)                            |
//! | 6   | UInt32  | `uint32 (u_int32)` (compressed)                   |
//! | 7   | Int64   | `u_int64` (two compressed words, low then high)   |
//! | 8   | UInt64  | `uint64 (u_int64)`                                |
//! | 9   | IntPtr  | `u_int64`                                         |
//! | 10  | UIntPtr | `uint64 (u_int64)`                                |
//! | 11  | Single  | `float32_of_bits (u_int32)` — 32-bit pattern      |
//! | 12  | Double  | `float_of_bits (u_int64)` — 64-bit pattern        |
//! | 13  | Char    | `char (uint16 (u_int32))` — a UTF-16 code unit    |
//! | 14  | String  | `u_string` (compressed-int index)                 |
//! | 15  | Unit    | —                                                 |
//! | 16  | Zero    | —                                                 |
//! | 17  | Decimal | `u_array u_int32` — four `Decimal.GetBits` words  |
//!
//! Tags outside `0..=17` hard-error (`UnsupportedPickleTag`), matching
//! FCS's `ufailwith st "u_const"`. `read_const` backs not only
//! attribute-argument `Expr.Const` nodes but every `[<Literal>]` val and
//! record-field value, so a tag the decoder couldn't read previously
//! failed the whole CCU decode, causing the F# overlays to be recorded as
//! skipped and every `[<Measure>]` type in the assembly to remain `Class`.
//! Decoding the full set closes that failure mode.

use crate::error::ImportError;
use crate::fsharp_pickle::model::PickledConst;
use crate::fsharp_pickle::reader::PickleReader;

/// `u_const` (`TypedTreePickle.fs:3394-3416`), the complete tag set.
pub(crate) fn read_const(reader: &mut PickleReader<'_>) -> Result<PickledConst, ImportError> {
    let tag = reader.read_byte("u_const tag")?;
    match tag {
        0 => Ok(PickledConst::Bool(reader.read_bool("u_const Bool body")?)),
        // `sbyte (u_int32)`: read the compressed word, narrow with the
        // same wrapping truncation F#'s `sbyte` cast applies.
        1 => Ok(PickledConst::SByte(
            reader.read_int32("u_const SByte body")? as i8,
        )),
        // `byte (u_byte)`: a *raw* byte, NOT the compressed `u_int32` —
        // the one arm whose primitive differs from its siblings.
        2 => Ok(PickledConst::Byte(reader.read_byte("u_const Byte body")?)),
        3 => Ok(PickledConst::Int16(
            reader.read_int32("u_const Int16 body")? as i16,
        )),
        4 => Ok(PickledConst::UInt16(
            reader.read_uint32("u_const UInt16 body")? as u16,
        )),
        5 => Ok(PickledConst::Int32(
            reader.read_int32("u_const Int32 body")?,
        )),
        6 => Ok(PickledConst::UInt32(
            reader.read_uint32("u_const UInt32 body")?,
        )),
        7 => Ok(PickledConst::Int64(
            reader.read_int64("u_const Int64 body")?,
        )),
        8 => Ok(PickledConst::UInt64(
            reader.read_int64("u_const UInt64 body")? as u64,
        )),
        // `IntPtr` / `UIntPtr` share `u_int64` with `Int64` / `UInt64`.
        9 => Ok(PickledConst::IntPtr(
            reader.read_int64("u_const IntPtr body")?,
        )),
        10 => Ok(PickledConst::UIntPtr(
            reader.read_int64("u_const UIntPtr body")? as u64,
        )),
        // `float32_of_bits (u_int32)`: the compressed word *is* the IEEE
        // bit pattern (FCS pickled `bits_of_float`); we keep it raw.
        11 => Ok(PickledConst::Single(
            reader.read_uint32("u_const Single body")?,
        )),
        // `float_of_bits (u_int64)`: likewise the 64-bit pattern.
        12 => Ok(PickledConst::Double(
            reader.read_int64("u_const Double body")? as u64,
        )),
        // `char (uint16 (u_int32))`: a UTF-16 code unit kept as a `u16`.
        13 => Ok(PickledConst::Char(
            reader.read_uint32("u_const Char body")? as u16
        )),
        14 => Ok(PickledConst::String(
            reader.read_string("u_const String body")?,
        )),
        15 => Ok(PickledConst::Unit),
        16 => Ok(PickledConst::Zero),
        // `System.Decimal (u_array u_int32)`: the four `GetBits` words.
        17 => {
            let words = reader.read_array("u_const Decimal word", |r| {
                r.read_int32("u_const Decimal word body")
            })?;
            let quad: [i32; 4] =
                words
                    .try_into()
                    .map_err(|words: Vec<i32>| ImportError::MalformedPickleHeader {
                        detail: format!(
                            "u_const Decimal expects a 4-word System.Decimal.GetBits array, got {}",
                            words.len()
                        ),
                    })?;
            Ok(PickledConst::Decimal(quad))
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_const tag (FCS dispatcher covers 0..=17)",
            tag: u32::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_reader<'a>(bytes: &'a [u8], strings: &'a [String]) -> PickleReader<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        r
    }

    #[test]
    fn const_bool_round_trip() {
        let strings: Vec<String> = vec![];
        let mut r = make_reader(&[0u8, 0u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Bool(false));

        let mut r = make_reader(&[0u8, 1u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Bool(true));
    }

    #[test]
    fn const_int32_round_trip() {
        let strings: Vec<String> = vec![];
        // Tag 5 + literal compressed 0.
        let mut r = make_reader(&[5u8, 0u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int32(0));

        // Tag 5 + 0x7F literal.
        let mut r = make_reader(&[5u8, 0x7F], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int32(0x7F));

        // Tag 5 + -1 (full marker form).
        let mut bytes = vec![5u8, 0xFF];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int32(-1));
    }

    #[test]
    fn const_string_resolves_index() {
        let strings = vec!["zero".to_string(), "one".to_string(), "two".to_string()];
        // Tag 14, then index 2.
        let mut r = make_reader(&[14u8, 2u8], &strings);
        assert_eq!(
            read_const(&mut r).unwrap(),
            PickledConst::String("two".to_string()),
        );
    }

    #[test]
    fn const_unit() {
        let strings: Vec<String> = vec![];
        let mut r = make_reader(&[15u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Unit);
    }

    #[test]
    fn const_zero() {
        let strings: Vec<String> = vec![];
        let mut r = make_reader(&[16u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Zero);
    }

    /// Tag 1 (`SByte`): `sbyte (u_int32)` — a compressed int truncated.
    #[test]
    fn const_sbyte() {
        let strings: Vec<String> = vec![];
        // Small positive: literal compressed byte 5.
        let mut r = make_reader(&[1u8, 5u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::SByte(5));

        // -1: u_int32 marker form decodes 0xFFFF_FFFF, `as i8` == -1.
        let mut bytes = vec![1u8, 0xFF];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::SByte(-1));
    }

    /// Tag 2 (`Byte`): the one asymmetric arm — `byte (u_byte)`, a *raw*
    /// single byte, NOT the compressed `u_int32`. `[2, 0xFF]` therefore
    /// decodes to `Byte(255)`; a (wrong) compressed read would see `0xFF`
    /// as the four-byte marker and run off the end of the stream.
    #[test]
    fn const_byte_is_raw_not_compressed() {
        let strings: Vec<String> = vec![];
        let mut r = make_reader(&[2u8, 0xFFu8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Byte(255));
        assert!(r.is_eof(), "raw byte consumes exactly one trailing byte");

        let mut r = make_reader(&[2u8, 0u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Byte(0));
    }

    /// Tag 3 (`Int16`): `int16 (u_int32)`.
    #[test]
    fn const_int16() {
        let strings: Vec<String> = vec![];
        // 1000 = 0x3E8 → two-byte compressed form (0x83, 0xE8).
        let mut r = make_reader(&[3u8, 0x83u8, 0xE8u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int16(1000));

        // -1 via the marker form.
        let mut bytes = vec![3u8, 0xFF];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int16(-1));
    }

    /// Tag 4 (`UInt16`): `uint16 (u_int32)`.
    #[test]
    fn const_uint16() {
        let strings: Vec<String> = vec![];
        let mut bytes = vec![4u8, 0xFF];
        bytes.extend_from_slice(&0x0000_FFFFu32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::UInt16(65535));
    }

    /// Tag 6 (`UInt32`): `uint32 (u_int32)`.
    #[test]
    fn const_uint32() {
        let strings: Vec<String> = vec![];
        let mut bytes = vec![6u8, 0xFF];
        bytes.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_const(&mut r).unwrap(),
            PickledConst::UInt32(0xDEAD_BEEF),
        );
    }

    /// Tag 7 (`Int64`): `u_int64` — two compressed words, low then high.
    #[test]
    fn const_int64() {
        let strings: Vec<String> = vec![];
        // 1 = (lo=1, hi=0), both literal bytes.
        let mut r = make_reader(&[7u8, 1u8, 0u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int64(1));

        // 0x1122_3344_5566_7788 — low word then high word, each marker form.
        let mut bytes = vec![7u8, 0xFF];
        bytes.extend_from_slice(&0x5566_7788u32.to_le_bytes());
        bytes.push(0xFF);
        bytes.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_const(&mut r).unwrap(),
            PickledConst::Int64(0x1122_3344_5566_7788),
        );

        // -1 — both words all-ones.
        let mut bytes = vec![7u8, 0xFF];
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        bytes.push(0xFF);
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Int64(-1));
    }

    /// Tags 8 / 9 / 10 (`UInt64` / `IntPtr` / `UIntPtr`): all read a
    /// `u_int64` word pair; only the variant label differs.
    #[test]
    fn const_uint64_intptr_uintptr() {
        let strings: Vec<String> = vec![];

        // UInt64 max: word pair all-ones → -1i64 → u64::MAX.
        let mut bytes = vec![8u8, 0xFF];
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        bytes.push(0xFF);
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::UInt64(u64::MAX));

        let mut r = make_reader(&[9u8, 7u8, 0u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::IntPtr(7));

        let mut r = make_reader(&[10u8, 7u8, 0u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::UIntPtr(7));
    }

    /// Tag 11 (`Single`): `float32_of_bits (u_int32)`. We keep the raw
    /// 32-bit pattern; `as_f32` decodes it.
    #[test]
    fn const_single() {
        let strings: Vec<String> = vec![];
        // 0.0f32 — bit pattern 0, one literal byte.
        let mut r = make_reader(&[11u8, 0u8], &strings);
        let c = read_const(&mut r).unwrap();
        assert_eq!(c, PickledConst::Single(0));
        assert_eq!(c.as_f32(), Some(0.0f32));

        // 1.0f32 — bit pattern 0x3F80_0000, marker form.
        let mut bytes = vec![11u8, 0xFF];
        bytes.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        let c = read_const(&mut r).unwrap();
        assert_eq!(c, PickledConst::Single(1.0f32.to_bits()));
        assert_eq!(c.as_f32(), Some(1.0f32));
    }

    /// Tag 12 (`Double`): `float_of_bits (u_int64)`. Raw 64-bit pattern,
    /// decoded via `as_f64`.
    #[test]
    fn const_double() {
        let strings: Vec<String> = vec![];
        // 1.0f64 — bit pattern 0x3FF0_0000_0000_0000 = (lo=0, hi=0x3FF0_0000).
        let mut bytes = vec![12u8, 0u8, 0xFF];
        bytes.extend_from_slice(&0x3FF0_0000u32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        let c = read_const(&mut r).unwrap();
        assert_eq!(c, PickledConst::Double(1.0f64.to_bits()));
        assert_eq!(c.as_f64(), Some(1.0f64));
    }

    /// Tag 13 (`Char`): `char (uint16 (u_int32))`, kept as a raw `u16` so a
    /// lone UTF-16 surrogate (not a valid Rust `char`) still decodes.
    #[test]
    fn const_char_keeps_lone_surrogate() {
        let strings: Vec<String> = vec![];
        // 'A' = 0x41 — literal byte.
        let mut r = make_reader(&[13u8, 0x41u8], &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Char(0x41));

        // 0xD800 — a high-surrogate code unit; marker form.
        let mut bytes = vec![13u8, 0xFF];
        bytes.extend_from_slice(&0x0000_D800u32.to_le_bytes());
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_const(&mut r).unwrap(), PickledConst::Char(0xD800));
    }

    /// Tag 17 (`Decimal`): `u_array u_int32`, the four `Decimal.GetBits`
    /// words. We keep the raw quadruple.
    #[test]
    fn const_decimal_quadruple() {
        let strings: Vec<String> = vec![];
        // length 4, then [1, 0, 0, 0] (the bits of decimal `1m`).
        let mut r = make_reader(&[17u8, 4u8, 1u8, 0u8, 0u8, 0u8], &strings);
        assert_eq!(
            read_const(&mut r).unwrap(),
            PickledConst::Decimal([1, 0, 0, 0]),
        );
    }

    /// A `Decimal` array whose length is not exactly four is malformed —
    /// `System.Decimal` is always pickled from a 4-element `GetBits`.
    #[test]
    fn const_decimal_wrong_arity_errors() {
        let strings: Vec<String> = vec![];
        // length 3 → not a valid GetBits quadruple.
        let mut r = make_reader(&[17u8, 3u8, 1u8, 0u8, 0u8], &strings);
        match read_const(&mut r) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("Decimal"), "detail: {detail}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Tag 18 is past the end of FCS's `u_const` dispatcher (tags 0–17), so
    /// it still hard-errors — the loud-failure backstop for a genuinely
    /// unknown tag, matching FCS's `ufailwith`.
    #[test]
    fn const_unknown_tag_errors() {
        let strings: Vec<String> = vec![];
        for tag in [18u8, 99u8] {
            let bytes = [tag];
            let mut r = make_reader(&bytes, &strings);
            match read_const(&mut r) {
                Err(ImportError::UnsupportedPickleTag { context, tag: got }) => {
                    assert_eq!(u32::from(tag), got);
                    assert!(context.contains("u_const"), "context: {context}");
                }
                other => panic!("unexpected for tag {tag}: {other:?}"),
            }
        }
    }
}
