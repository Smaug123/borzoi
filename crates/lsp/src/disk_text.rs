//! A file's text read from disk, as the document a client would hold for it.
//!
//! Editors decode a leading UTF-8 byte-order mark away: the document VS Code
//! or Neovim sends in `didOpen` has no U+FEFF, and FCS strips it too, so
//! neither counts it as a column. A file the client has not opened is read
//! here instead, and must have the same positions as when it is open, so the
//! mark is dropped. Text from `didOpen`/`didChange` is the client's own and is
//! never touched: whatever it holds is what its positions count.

use std::path::Path;

/// The UTF-8 byte-order mark.
const BOM: char = '\u{feff}';

/// `path`'s text without a leading byte-order mark.
pub fn read(path: &Path) -> std::io::Result<String> {
    let mut text = std::fs::read_to_string(path)?;
    if text.starts_with(BOM) {
        text.drain(..BOM.len_utf8());
    }
    Ok(text)
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
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("A.fs");
        std::fs::write(&path, "\u{feff}let x = \"\u{feff}\"\n").unwrap();
        assert_eq!(read(&path).unwrap(), "let x = \"\u{feff}\"\n");
        std::fs::write(&path, "let x = 1\n").unwrap();
        assert_eq!(read(&path).unwrap(), "let x = 1\n");
        assert_eq!(content("\u{feff}\u{feff}a"), "\u{feff}a");
    }
}
