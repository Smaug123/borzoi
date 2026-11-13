//! A file's text read from disk, as the document a client would hold for it.
//!
//! Editors decode a leading UTF-8 byte-order mark away: the document VS Code
//! or Neovim sends in `didOpen` has no U+FEFF, and FCS strips it too, so
//! neither counts it as a column. A file the client has not opened is read
//! from disk instead ([`crate::observed::Sources::read_disk`]), and must have
//! the same positions as when it is open, so the mark is dropped. Text from
//! `didOpen`/`didChange` is the client's own and is never touched: whatever it
//! holds is what its positions count.

/// The UTF-8 byte-order mark.
const BOM: char = '\u{feff}';

/// Text read from disk, without a leading byte-order mark.
pub fn without_bom(mut text: String) -> String {
    if text.starts_with(BOM) {
        text.drain(..BOM.len_utf8());
    }
    text
}

/// `text` without a leading byte-order mark: the content two spellings of one
/// document share, for comparing a buffer with what is on disk.
pub fn content(text: &str) -> &str {
    text.strip_prefix(BOM).unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_leading_mark_is_dropped() {
        assert_eq!(
            without_bom("\u{feff}let x = \"\u{feff}\"\n".to_string()),
            "let x = \"\u{feff}\"\n"
        );
        assert_eq!(without_bom("let x = 1\n".to_string()), "let x = 1\n");
        assert_eq!(content("\u{feff}\u{feff}a"), "\u{feff}a");
    }
}
