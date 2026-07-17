//! A bounds-checked little-endian byte cursor and the ECMA-335 compressed
//! integer primitive (II.23.2).
//!
//! Every read is fallible: out-of-range returns `None` so callers can map the
//! shortfall to a structural [`super::Error`] variant at the point it matters,
//! rather than panicking. This keeps the whole container reader total over
//! arbitrary input.

/// A forward-only reader over a borrowed byte slice.
pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    /// A cursor positioned at `pos` (which may be past the end; reads then
    /// simply fail).
    pub(crate) fn at(data: &'a [u8], pos: usize) -> Self {
        Cursor { data, pos }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    pub(crate) fn read_u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    /// The next byte without advancing, or `None` at end of input.
    pub(crate) fn peek_u8(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub(crate) fn read_u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn read_u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn read_u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub(crate) fn read_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        self.take(n)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    /// Read an ECMA-335 II.23.2 unsigned compressed integer at the cursor,
    /// advancing past the 1/2/4 bytes it occupies.
    pub(crate) fn read_compressed_u32(&mut self) -> Option<u32> {
        let (value, consumed) = read_compressed_u32(self.data.get(self.pos..)?)?;
        self.pos += consumed;
        Some(value)
    }

    /// Read an ECMA-335 II.23.2 *signed* compressed integer at the cursor,
    /// advancing past the 1/2/4 bytes it occupies.
    ///
    /// The signed form rotates the value so the sign bit is the least-
    /// significant bit of the compressed field, then sign-extends from the
    /// field width (7, 14, or 29 bits). Used for array `LoBound`s
    /// (II.23.2.13), which may be negative. (E.g. `-1` is encoded `0x7F`.)
    pub(crate) fn read_compressed_i32(&mut self) -> Option<i32> {
        let b0 = *self.data.get(self.pos)?;
        // `(raw, consumed, field_bits)` — `raw` is the still-rotated value and
        // `field_bits` the width of the compressed field (7 / 14 / 29 bits: the
        // lead byte contributes 7, 6, or 5 bits respectively), used to
        // sign-extend after the un-rotate.
        let (raw, consumed, field_bits) = if b0 & 0x80 == 0 {
            (u32::from(b0), 1usize, 7u32)
        } else if b0 & 0xC0 == 0x80 {
            let b1 = *self.data.get(self.pos + 1)?;
            ((u32::from(b0 & 0x3F) << 8) | u32::from(b1), 2, 14)
        } else if b0 & 0xE0 == 0xC0 {
            let b1 = *self.data.get(self.pos + 1)?;
            let b2 = *self.data.get(self.pos + 2)?;
            let b3 = *self.data.get(self.pos + 3)?;
            (
                (u32::from(b0 & 0x1F) << 24)
                    | (u32::from(b1) << 16)
                    | (u32::from(b2) << 8)
                    | u32::from(b3),
                4,
                29,
            )
        } else {
            return None;
        };
        self.pos += consumed;
        // Un-rotate: the field's LSB is the sign; the magnitude is the rest.
        let magnitude = (raw >> 1) as i32;
        let value = if raw & 1 == 0 {
            magnitude
        } else {
            // Negative: sign-extend from the field width minus the sign bit.
            magnitude | !(((1i32) << (field_bits - 1)) - 1)
        };
        Some(value)
    }
}

/// Decode an ECMA-335 II.23.2 unsigned compressed integer from the front of
/// `data`, returning the value and the number of bytes consumed (1, 2, or 4).
///
/// Trailing bytes beyond the encoded integer are ignored. Returns `None` if the
/// slice is too short for the indicated width, or the lead byte uses the
/// reserved `0b111…` prefix.
pub(crate) fn read_compressed_u32(data: &[u8]) -> Option<(u32, usize)> {
    let b0 = *data.first()?;
    if b0 & 0x80 == 0 {
        Some((u32::from(b0), 1))
    } else if b0 & 0xC0 == 0x80 {
        let b1 = *data.get(1)?;
        let value = (u32::from(b0 & 0x3F) << 8) | u32::from(b1);
        Some((value, 2))
    } else if b0 & 0xE0 == 0xC0 {
        let b1 = *data.get(1)?;
        let b2 = *data.get(2)?;
        let b3 = *data.get(3)?;
        let value = (u32::from(b0 & 0x1F) << 24)
            | (u32::from(b1) << 16)
            | (u32::from(b2) << 8)
            | u32::from(b3);
        Some((value, 4))
    } else {
        None
    }
}

/// Encode `n` as an ECMA-335 II.23.2 unsigned compressed integer.
///
/// Test-only reference encoder: it is the inverse of [`read_compressed_u32`]
/// over the representable range (`n <= 0x1FFF_FFFF`), and exists so the
/// round-trip property has something to round-trip against. Panics on values
/// the encoding cannot represent.
#[cfg(test)]
pub(crate) fn compress_u32(n: u32) -> Vec<u8> {
    if n <= 0x7F {
        vec![n as u8]
    } else if n <= 0x3FFF {
        vec![0x80 | (n >> 8) as u8, (n & 0xFF) as u8]
    } else if n <= 0x1FFF_FFFF {
        vec![
            0xC0 | (n >> 24) as u8,
            (n >> 16) as u8,
            (n >> 8) as u8,
            (n & 0xFF) as u8,
        ]
    } else {
        panic!("value {n} is too large for an ECMA-335 compressed integer");
    }
}

#[cfg(test)]
mod tests {
    use super::Cursor;

    #[test]
    fn read_compressed_i32_canonical_examples() {
        // ECMA-335 II.23.2 worked examples for signed compression.
        for (bytes, expected) in [
            (vec![0x06u8], 3),
            (vec![0x7B], -3),
            (vec![0x80, 0x80], 64),
            (vec![0x01], -64),
            (vec![0xDF, 0xFF, 0xFF, 0xFE], 268_435_455), // 0x0FFF_FFFF
            (vec![0xC0, 0x00, 0x00, 0x01], -268_435_456), // -0x1000_0000
            (vec![0x7F], -1),
            (vec![0x00], 0),
        ] {
            let mut c = Cursor::new(&bytes);
            assert_eq!(
                c.read_compressed_i32(),
                Some(expected),
                "bytes={bytes:02x?}"
            );
            assert_eq!(
                c.position(),
                bytes.len(),
                "consumed all bytes for {expected}"
            );
        }
    }

    #[test]
    fn read_compressed_i32_truncated_is_none() {
        // A 2-byte lead with only one byte available fails rather than panics.
        let mut c = Cursor::new(&[0x80u8]);
        assert_eq!(c.read_compressed_i32(), None);
    }
}
