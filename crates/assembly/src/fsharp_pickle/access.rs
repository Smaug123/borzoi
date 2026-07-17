//! Access-path leaves: ranges, public paths, composition paths, access
//! lists.
//!
//! All of these are tiny FCS helpers that string `u_int` / `u_string` /
//! `u_byte` together with a fixed shape. Grouping them here keeps the
//! per-decoder modules small and gives the test file a single home for
//! the table-attachment fixtures.
//!
//! ### FCS source map
//!
//! - `u_pos`         — `TypedTreePickle.fs:1899-1902` (two `u_int`s).
//! - `u_range`       — `:1904-1908` (`u_string` + two `u_pos`).
//! - `u_dummy_range` — `:1911` (reads no bytes).
//! - `u_istype`      — `:2643-2650` (single byte → enum).
//! - `u_pubpath`     — `:861` (compressed-int index into pubpaths
//!   table).
//! - `u_cpath`       — `:2652-2654` (`u_ILScopeRef` + list of
//!   `(u_string, u_istype)` pairs).
//! - `u_access`      — `:3088-3091` (`u_list u_cpath`; empty list ≡
//!   `taccessPublic`).

use crate::error::ImportError;
use crate::fsharp_pickle::il::read_il_scope_ref;
use crate::fsharp_pickle::model::{IsType, PickledAccess, PickledCPath, PickledPos, PickledRange};
use crate::fsharp_pickle::reader::PickleReader;

/// `u_pos` (`TypedTreePickle.fs:1899-1902`): two compressed ints =
/// (line, column). FCS uses 1-based positions; we pass them through
/// untouched.
pub(crate) fn read_pos(reader: &mut PickleReader<'_>) -> Result<PickledPos, ImportError> {
    let line = reader.read_uint32("u_pos line")?;
    let column = reader.read_uint32("u_pos column")?;
    Ok(PickledPos { line, column })
}

/// `u_range` (`:1904-1908`): `u_string` (file name → string-index)
/// followed by two `u_pos` values.
pub(crate) fn read_range(reader: &mut PickleReader<'_>) -> Result<PickledRange, ImportError> {
    let file = reader.read_string_index("u_range file")?;
    let start = read_pos(reader)?;
    let end = read_pos(reader)?;
    Ok(PickledRange { file, start, end })
}

/// `u_dummy_range` (`:1911`): reads zero bytes. Provided as a function
/// so call sites can document where the FCS source would have written
/// `range0` even though we ignore the value.
pub(crate) fn read_dummy_range(_reader: &mut PickleReader<'_>) -> Result<(), ImportError> {
    Ok(())
}

/// `u_istype` (`:2643-2650`): single byte tag.
///
/// FCS's tag 2 carries a `true` payload (`Namespace true`) but the
/// boolean is invariant — the pickler always writes `Namespace true`,
/// and the unpickler unconditionally reconstructs it. We collapse the
/// representation to a payload-free `Namespace`.
pub(crate) fn read_istype(reader: &mut PickleReader<'_>) -> Result<IsType, ImportError> {
    let tag = reader.read_byte("u_istype tag")?;
    match tag {
        0 => Ok(IsType::FSharpModuleWithSuffix),
        1 => Ok(IsType::ModuleOrType),
        2 => Ok(IsType::Namespace),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_istype tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_pubpath` (`:861`): compressed-int index into the phase-2
/// pubpaths table, returning the pre-resolved path of string indices.
/// Thin wrapper over `PickleReader::read_pubpath` to keep the FCS-side
/// naming visible in the call graph.
pub(crate) fn read_pubpath(reader: &mut PickleReader<'_>) -> Result<Vec<u32>, ImportError> {
    reader.read_pubpath("u_pubpath")
}

/// `u_cpath` (`:2652-2654`): `u_ILScopeRef` + `u_list (u_string,
/// u_istype)`.
pub(crate) fn read_cpath(reader: &mut PickleReader<'_>) -> Result<PickledCPath, ImportError> {
    let scope = read_il_scope_ref(reader)?;
    let path = reader.read_list("u_cpath path entry", |r| {
        let name = r.read_string("u_cpath segment name")?;
        let kind = read_istype(r)?;
        Ok((name, kind))
    })?;
    Ok(PickledCPath { scope, path })
}

/// `u_access` (`:3088-3091`): `u_list u_cpath`. An empty list maps to
/// `taccessPublic` in FCS; we preserve the same convention (an empty
/// `Vec` ≡ public access).
pub(crate) fn read_access(reader: &mut PickleReader<'_>) -> Result<PickledAccess, ImportError> {
    reader.read_list("u_access cpath", read_cpath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::PickledILScopeRef;

    fn enc_str_idx(idx: u32) -> Vec<u8> {
        assert!(idx < 0x80, "test fixture string-index out of literal range");
        vec![idx as u8]
    }

    #[test]
    fn pos_reads_two_compressed_ints() {
        let bytes = [3u8, 5];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(read_pos(&mut r).unwrap(), PickledPos { line: 3, column: 5 });
        assert!(r.is_eof());
    }

    #[test]
    fn range_reads_file_index_then_two_positions() {
        let strings = vec!["src/Foo.fs".to_string()];
        let bytes = [0u8, 1, 2, 3, 4];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: Vec<Vec<u32>> = vec![];
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(
            read_range(&mut r).unwrap(),
            PickledRange {
                file: 0,
                start: PickledPos { line: 1, column: 2 },
                end: PickledPos { line: 3, column: 4 },
            }
        );
        assert!(r.is_eof());
    }

    #[test]
    fn dummy_range_reads_zero_bytes() {
        let bytes = [0xAAu8];
        let mut r = PickleReader::new(&bytes);
        read_dummy_range(&mut r).unwrap();
        // Cursor untouched.
        assert_eq!(r.read_byte("t").unwrap(), 0xAA);
    }

    #[test]
    fn istype_covers_each_tag() {
        for (tag, want) in [
            (0u8, IsType::FSharpModuleWithSuffix),
            (1, IsType::ModuleOrType),
            (2, IsType::Namespace),
        ] {
            let bytes = [tag];
            let mut r = PickleReader::new(&bytes);
            assert_eq!(read_istype(&mut r).unwrap(), want);
        }
    }

    #[test]
    fn istype_rejects_unknown_tag() {
        let bytes = [9u8];
        let mut r = PickleReader::new(&bytes);
        match read_istype(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_istype tag",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn pubpath_resolves_through_attached_table() {
        let strings: Vec<String> = vec![];
        let pubpaths = vec![vec![1u32, 2], vec![5u32]];
        let bytes = [1u8]; // index 1
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(read_pubpath(&mut r).unwrap(), vec![5]);
    }

    #[test]
    fn cpath_local_scope_empty_path() {
        let strings: Vec<String> = vec![];
        let pubpaths: Vec<Vec<u32>> = vec![];
        // scope = Local (tag 0), then list-length 0.
        let bytes = [0u8, 0u8];
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(
            read_cpath(&mut r).unwrap(),
            PickledCPath {
                scope: PickledILScopeRef::Local,
                path: vec![],
            }
        );
    }

    #[test]
    fn cpath_local_scope_with_path() {
        let strings = vec![
            "Microsoft".to_string(),
            "FSharp".to_string(),
            "Core".to_string(),
        ];
        let pubpaths: Vec<Vec<u32>> = vec![];
        // scope = Local, then list of 3: (str0, Namespace), (str1, Namespace), (str2, ModuleOrType)
        let mut bytes = vec![0u8, 3u8];
        bytes.extend(enc_str_idx(0));
        bytes.push(2u8); // Namespace
        bytes.extend(enc_str_idx(1));
        bytes.push(2u8); // Namespace
        bytes.extend(enc_str_idx(2));
        bytes.push(1u8); // ModuleOrType
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        let cp = read_cpath(&mut r).unwrap();
        assert_eq!(
            cp,
            PickledCPath {
                scope: PickledILScopeRef::Local,
                path: vec![
                    ("Microsoft".to_string(), IsType::Namespace),
                    ("FSharp".to_string(), IsType::Namespace),
                    ("Core".to_string(), IsType::ModuleOrType),
                ],
            }
        );
        assert!(r.is_eof());
    }

    #[test]
    fn access_empty_list_is_public() {
        let strings: Vec<String> = vec![];
        let pubpaths: Vec<Vec<u32>> = vec![];
        let bytes = [0u8];
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        let a = read_access(&mut r).unwrap();
        assert!(a.is_empty());
    }

    #[test]
    fn access_carries_one_cpath() {
        let strings: Vec<String> = vec![];
        let pubpaths: Vec<Vec<u32>> = vec![];
        // list-length 1, scope = Local, empty path.
        let bytes = [1u8, 0u8, 0u8];
        let mut r = PickleReader::new(&bytes);
        r.attach_tables(&strings, &pubpaths);
        let a = read_access(&mut r).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].scope, PickledILScopeRef::Local);
    }
}
