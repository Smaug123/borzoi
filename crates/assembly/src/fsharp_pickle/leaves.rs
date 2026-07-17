//! Tiny leaf decoders that didn't deserve their own module.
//!
//! - `u_kind`   — typar binder kind (Type vs Measure), single byte.
//!   `TypedTreePickle.fs:2060-2064`.
//! - `u_xmldoc` — `u_array u_string`, returning the string-table
//!   indices verbatim (the `range0` is discarded — cross-CCU views do
//!   not carry source positions). `:1918`.

use crate::error::ImportError;
use crate::fsharp_pickle::model::{PickledXmlDoc, TyparKind};
use crate::fsharp_pickle::reader::PickleReader;

/// `u_kind` (`TypedTreePickle.fs:2060-2064`): single byte tag.
pub(crate) fn read_kind(reader: &mut PickleReader<'_>) -> Result<TyparKind, ImportError> {
    let tag = reader.read_byte("u_kind tag")?;
    match tag {
        0 => Ok(TyparKind::Type),
        1 => Ok(TyparKind::Measure),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_kind tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_xmldoc` (`:1918`): `u_array u_string`. Stores the string-table
/// indices rather than resolved strings — callers that need the actual
/// lines look them up against the pickled header's strings table.
pub(crate) fn read_xmldoc(reader: &mut PickleReader<'_>) -> Result<PickledXmlDoc, ImportError> {
    let lines = reader.read_array("u_xmldoc lines", |r| {
        r.read_string_index("u_xmldoc line index")
    })?;
    Ok(PickledXmlDoc { lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_type_and_measure() {
        let mut r = PickleReader::new(&[0u8]);
        assert_eq!(read_kind(&mut r).unwrap(), TyparKind::Type);

        let mut r = PickleReader::new(&[1u8]);
        assert_eq!(read_kind(&mut r).unwrap(), TyparKind::Measure);
    }

    #[test]
    fn kind_rejects_unknown_tag() {
        let mut r = PickleReader::new(&[2u8]);
        match read_kind(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_kind tag",
                tag: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn xmldoc_empty() {
        let strings: Vec<String> = vec![];
        let pubpaths: Vec<Vec<u32>> = vec![];
        let mut r = PickleReader::new(&[0u8]);
        r.attach_tables(&strings, &pubpaths);
        let doc = read_xmldoc(&mut r).unwrap();
        assert!(doc.lines.is_empty());
    }

    #[test]
    fn xmldoc_carries_indices() {
        let strings = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];
        // length 2, then indices 0 and 2.
        let bytes = [2u8, 0, 2];
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        let doc = read_xmldoc(&mut r).unwrap();
        assert_eq!(doc.lines, vec![0, 2]);
    }

    #[test]
    fn xmldoc_errors_on_out_of_range_index() {
        let strings = vec!["a".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];
        let bytes = [1u8, 5]; // index 5, table has 1 entry
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        match read_xmldoc(&mut r) {
            Err(ImportError::DanglingPickleRef {
                kind: "string",
                index: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
