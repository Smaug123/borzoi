//! LSP positions (line, UTF-16 code unit) computed **independently** of the
//! LSP's own conversion (`borzoi::position`).
//!
//! The handler differential ([`crate::handler_diff`]) asks the real request
//! handlers questions at positions computed here and reads their answers back
//! through here. If it used the LSP's helpers instead, a bug in them would move
//! both sides of the comparison together and pass. So this is written to be
//! obviously correct rather than fast: a line is found by scanning for its
//! breaks, and a column is `str::encode_utf16` over the line's prefix.
//!
//! Line breaks are `\r\n`, `\n` and `\r`, the three the LSP specification
//! names.

/// The byte offset at which each line starts, the first always `0`.
pub fn line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                starts.push(i + 2);
                i += 2;
            }
            b'\r' | b'\n' => {
                starts.push(i + 1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    starts
}

/// The byte range of line `line`'s content, its break excluded.
fn line_content(text: &str, starts: &[usize], line: usize) -> Option<(usize, usize)> {
    let start = *starts.get(line)?;
    let end = match starts.get(line + 1) {
        Some(&next) => {
            let bytes = text.as_bytes();
            if next >= 2 && bytes[next - 2] == b'\r' && bytes[next - 1] == b'\n' {
                next - 2
            } else {
                next - 1
            }
        }
        None => text.len(),
    };
    Some((start, end))
}

/// The LSP position of byte `offset`, or `None` if `offset` is not a position
/// at all: past the end, inside a UTF-8 sequence, or inside a line break.
pub fn position_of(text: &str, starts: &[usize], offset: usize) -> Option<(u32, u32)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let line = starts.partition_point(|&start| start <= offset) - 1;
    let (start, end) = line_content(text, starts, line)?;
    if offset > end {
        return None;
    }
    let column = text[start..offset].encode_utf16().count();
    Some((u32::try_from(line).ok()?, u32::try_from(column).ok()?))
}

/// The byte offset of LSP position `(line, column)`, or `None` if it names no
/// byte: a line past the end, a column past the line's end, or a column that
/// splits a surrogate pair.
pub fn offset_of(text: &str, starts: &[usize], line: u32, column: u32) -> Option<usize> {
    let (start, end) = line_content(text, starts, usize::try_from(line).ok()?)?;
    let column = usize::try_from(column).ok()?;
    let mut units = 0;
    for (index, ch) in text[start..end].char_indices() {
        if units == column {
            return Some(start + index);
        }
        units += ch.len_utf16();
        if units > column {
            return None;
        }
    }
    (units == column).then_some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_utf16_code_units() {
        // `é` is 2 bytes and 1 unit; `🦀` is 4 bytes and 2 units.
        let text = "aé🦀b\r\nx\ry\nz";
        let starts = line_starts(text);
        assert_eq!(starts, vec![0, 10, 12, 14]);
        assert_eq!(position_of(text, &starts, 0), Some((0, 0)));
        assert_eq!(position_of(text, &starts, 1), Some((0, 1)));
        assert_eq!(position_of(text, &starts, 3), Some((0, 2)));
        assert_eq!(position_of(text, &starts, 7), Some((0, 4)));
        assert_eq!(position_of(text, &starts, 8), Some((0, 5)));
        // Inside `é`, inside `🦀`, and between `\r` and `\n`.
        assert_eq!(position_of(text, &starts, 2), None);
        assert_eq!(position_of(text, &starts, 5), None);
        assert_eq!(position_of(text, &starts, 9), None);
        assert_eq!(position_of(text, &starts, 10), Some((1, 0)));
        assert_eq!(position_of(text, &starts, 12), Some((2, 0)));
        assert_eq!(position_of(text, &starts, 15), Some((3, 1)));

        assert_eq!(offset_of(text, &starts, 0, 2), Some(3));
        assert_eq!(offset_of(text, &starts, 0, 4), Some(7));
        assert_eq!(offset_of(text, &starts, 0, 5), Some(8));
        // Splitting the surrogate pair, and past the line's end.
        assert_eq!(offset_of(text, &starts, 0, 3), None);
        assert_eq!(offset_of(text, &starts, 0, 6), None);
        assert_eq!(offset_of(text, &starts, 1, 1), Some(11));
        assert_eq!(offset_of(text, &starts, 4, 0), None);
    }

    proptest::proptest! {
        /// Every byte that is a position comes back from its position.
        #[test]
        fn a_position_names_the_byte_it_came_from(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec!["a", "é", "中", "🦀", "\n", "\r", "\r\n"]),
                0..24,
            ),
        ) {
            let text: String = pieces.concat();
            let starts = line_starts(&text);
            for offset in 0..=text.len() {
                if let Some((line, column)) = position_of(&text, &starts, offset) {
                    proptest::prop_assert_eq!(
                        offset_of(&text, &starts, line, column),
                        Some(offset)
                    );
                }
            }
        }
    }
}
