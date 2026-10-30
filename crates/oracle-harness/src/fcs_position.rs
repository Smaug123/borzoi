//! FCS source positions to byte offsets in the text we hold.
//!
//! FCS reports a position as a 1-based line and a 0-based column counted in
//! **UTF-16 code units**, over the text as .NET decoded it: a leading UTF-8
//! byte-order mark (`EF BB BF`) is consumed by the decoder, so line 1, column 0
//! is the byte *after* the mark. The text we parse and resolve keeps the mark
//! (the lexer reads it as whitespace), so a mapping that walks line 1 from byte
//! 0 counts the mark as one column and lands every line-1 position one
//! character early.
//!
//! Lines end at `\n` (a `\r\n` pair ends at its `\n`); a lone `\r` does not end
//! one, which is FCS's lexer's own rule (`newline = ('\n' | '\r' '\n')`).
//!
//! Every differential that reads FCS's ranges maps them through [`LineIndex`],
//! so the rule is stated once.

/// The UTF-8 byte-order mark FCS strips before it counts columns.
const BOM: char = '\u{feff}';

/// Byte offsets for FCS `(line, col)` positions over one source text.
pub struct LineIndex<'a> {
    source: &'a str,
    /// Byte offset of the start of each line, 1-indexed (`starts[0]` is
    /// unused): `starts[1]` is past a leading byte-order mark.
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let line_one = if source.starts_with(BOM) {
            BOM.len_utf8()
        } else {
            0
        };
        let mut starts = vec![0, line_one];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        Self { source, starts }
    }

    /// The byte offset of FCS position `(line, col)`.
    ///
    /// Out of range clamps rather than panics, since FCS reports some end
    /// positions past the text: a line past the last is the end of the text
    /// (FCS ends a range at `lastLine + 1, 0`), line 0 is its start, and a
    /// column past the line's end walks no further than the start of the next
    /// line. A column inside a
    /// surrogate pair stops at the boundary before it; FCS itself only reports
    /// columns at character boundaries.
    pub fn offset(&self, line: u32, col: u32) -> usize {
        let line = line as usize;
        let col = col as usize;
        if line == 0 {
            return 0;
        }
        if line >= self.starts.len() {
            return self.source.len();
        }
        let base = self.starts[line];
        let line_end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());
        let mut units = 0usize;
        let mut byte = base;
        for ch in self.source[base..line_end].chars() {
            let next = units + ch.len_utf16();
            if next > col {
                break;
            }
            units = next;
            byte += ch.len_utf8();
        }
        byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Every character boundary of `text` with the FCS position that names it,
    /// computed the slow way: strip the mark, split the lines FCS splits, and
    /// count each line's prefix in UTF-16.
    fn positions(text: &str) -> Vec<(u32, u32, usize)> {
        let skipped = if text.starts_with(BOM) { 3 } else { 0 };
        let body = &text[skipped..];
        let mut out = Vec::new();
        let mut line_start = 0;
        for (n, line) in body.split('\n').enumerate() {
            for (i, _) in line.char_indices().chain([(line.len(), ' ')]) {
                let col = line[..i].encode_utf16().count();
                out.push((
                    u32::try_from(n + 1).unwrap(),
                    u32::try_from(col).unwrap(),
                    skipped + line_start + i,
                ));
            }
            line_start += line.len() + 1;
        }
        out
    }

    fn text() -> impl Strategy<Value = String> {
        (
            any::<bool>(),
            proptest::collection::vec(
                proptest::sample::select(vec![
                    "a", "é", "中", "🦀", "\n", "\r", "\r\n", "\u{feff}",
                ]),
                0..24,
            ),
        )
            .prop_map(|(bom, pieces)| {
                let body: String = pieces.concat();
                if bom { format!("{BOM}{body}") } else { body }
            })
    }

    #[test]
    fn line_one_starts_after_a_byte_order_mark() {
        let idx = LineIndex::new("\u{feff}module A\nlet x = 1\n");
        assert_eq!(idx.offset(1, 0), 3);
        assert_eq!(idx.offset(1, 7), 10);
        assert_eq!(idx.offset(2, 4), 16);
        // Only a leading mark is stripped.
        let idx = LineIndex::new("a\n\u{feff}b");
        assert_eq!(idx.offset(2, 1), 5);
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let text = "ab\ncd";
        let idx = LineIndex::new(text);
        assert_eq!(idx.offset(3, 0), text.len());
        assert_eq!(idx.offset(0, 5), 0);
        assert_eq!(idx.offset(2, 9), text.len());
        // Inside the surrogate pair of `🦀`.
        assert_eq!(LineIndex::new("🦀x").offset(1, 1), 0);
    }

    proptest! {
        /// The index agrees with the slow model at every position that names a
        /// character boundary.
        #[test]
        fn every_position_names_its_byte(text in text()) {
            let idx = LineIndex::new(&text);
            for (line, col, byte) in positions(&text) {
                prop_assert_eq!(idx.offset(line, col), byte, "{:?} at {}:{}", text, line, col);
            }
        }

        /// A leading byte-order mark moves every position three bytes and
        /// changes nothing else, out-of-range positions included.
        #[test]
        fn a_byte_order_mark_moves_every_position_three_bytes(
            text in text().prop_filter("no leading mark", |t| !t.starts_with(BOM)),
        ) {
            let marked = format!("{BOM}{text}");
            let (plain, marked) = (LineIndex::new(&text), LineIndex::new(&marked));
            let lines = u32::try_from(text.matches('\n').count()).unwrap() + 2;
            for line in 1..=lines {
                for col in 0..30 {
                    prop_assert_eq!(marked.offset(line, col), plain.offset(line, col) + 3);
                }
            }
        }
    }
}
