//! Stage 1 correctness oracles for the bespoke ECMA-335 container reader.
//!
//! Three families:
//! - the compressed-integer primitive (II.23.2) round-trips in every width band;
//! - `MetadataFile::read` never panics on arbitrary, truncated, or mutated input;
//! - real fixture assemblies satisfy the structural invariants the spec
//!   guarantees (Module row count, heap tiling, GUID alignment).

use super::Error;
use super::cursor::{compress_u32, read_compressed_u32};
use super::metadata::MetadataFile;
use proptest::prelude::*;
use std::path::PathBuf;

/// ECMA-335 II.22 table indices used below.
const TABLE_MODULE: usize = 0x00;
const TABLE_TYPEDEF: usize = 0x02;

/// Build the fixture assemblies once and return their `.dll` paths.
///
/// The `bin/` outputs are git-ignored, so a clean checkout has none.
/// `MetadataFile::read` is `pub(crate)`, so these tests must live in the lib
/// (an integration test can't reach it). The build itself is shared with the
/// stage-4 type-def tests (and `manifest_tests`) through [`super::test_fixtures`]
/// so every suite's `dotnet build`s funnel through a single `OnceLock`
/// initializer rather than racing on the same `obj/`/`bin/` outputs. Kept
/// `pub(super)` so `manifest_tests` can reach the project DLLs by name.
pub(super) fn fixtures() -> Vec<PathBuf> {
    super::test_fixtures::project_dlls()
}

fn expected_compressed_len(n: u32) -> usize {
    if n <= 0x7F {
        1
    } else if n <= 0x3FFF {
        2
    } else {
        4
    }
}

proptest! {
    /// `decompress(compress(n)) == n`, with the spec's 1/2/4-byte banding.
    #[test]
    fn compressed_u32_roundtrips(n in 0u32..=0x1FFF_FFFF) {
        let enc = compress_u32(n);
        prop_assert_eq!(enc.len(), expected_compressed_len(n));
        let (decoded, consumed) = read_compressed_u32(&enc).expect("decode");
        prop_assert_eq!(decoded, n);
        prop_assert_eq!(consumed, enc.len());
    }

    /// The reader stops after the encoded integer; trailing bytes are ignored
    /// and the consumed count is the band width, not the slice length. This is
    /// the property that, applied to `SerString` lengths, makes strings >= 128
    /// bytes (2-/4-byte prefixes) decode correctly where a single-byte-prefix
    /// reader would not.
    #[test]
    fn compressed_u32_ignores_trailing(
        n in 0u32..=0x1FFF_FFFF,
        tail in proptest::collection::vec(any::<u8>(), 0..8),
    ) {
        let mut enc = compress_u32(n);
        let used = enc.len();
        enc.extend_from_slice(&tail);
        let (decoded, consumed) = read_compressed_u32(&enc).expect("decode");
        prop_assert_eq!(decoded, n);
        prop_assert_eq!(consumed, used);
    }

    /// Arbitrary bytes never panic the container reader.
    #[test]
    fn read_never_panics_on_arbitrary(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let _ = MetadataFile::read(&bytes);
    }

    /// Mutating a real assembly at arbitrary offsets never panics the reader:
    /// the header usually survives, so this drives deep into the structural
    /// parse with corrupted offsets/lengths.
    #[test]
    fn read_never_panics_on_mutated_fixtures(
        which in 0usize..3,
        muts in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..64),
    ) {
        let files = fixtures();
        let mut bytes = std::fs::read(&files[which]).expect("fixture");
        for (off, val) in muts {
            if !bytes.is_empty() {
                let i = off % bytes.len();
                bytes[i] = val;
            }
        }
        let _ = MetadataFile::read(&bytes);
    }
}

/// Every prefix of a real assembly is parsed without panic (truncation fuzz).
#[test]
fn read_never_panics_on_truncated_fixtures() {
    for p in fixtures() {
        let bytes = std::fs::read(&p).expect("fixture");
        let step = (bytes.len() / 512).max(1);
        let mut len = 0;
        while len <= bytes.len() {
            let _ = MetadataFile::read(&bytes[..len]);
            len += step;
        }
    }
}

/// Structural invariants the ECMA-335 container guarantees for any real,
/// well-formed assembly.
#[test]
fn fixture_structural_invariants() {
    for p in fixtures() {
        let bytes = std::fs::read(&p).expect("fixture");
        let md = MetadataFile::read(&bytes)
            .unwrap_or_else(|e| panic!("failed to read {}: {e:?}", p.display()));

        // Exactly one Module row (II.22.30).
        assert_eq!(
            md.rows[TABLE_MODULE],
            1,
            "Module row count for {}",
            p.display()
        );
        // At least the `<Module>` pseudo-type lives in TypeDef.
        assert!(
            md.rows[TABLE_TYPEDEF] >= 1,
            "TypeDef row count for {}",
            p.display()
        );

        // #Strings is a packed run of NUL-terminated UTF-8 strings: index 0 is
        // the empty string, it ends on a NUL, and every segment is valid UTF-8.
        let strings = md.strings;
        if !strings.is_empty() {
            assert_eq!(
                strings[0],
                0,
                "#Strings[0] must be empty in {}",
                p.display()
            );
            assert_eq!(
                *strings.last().unwrap(),
                0,
                "#Strings must end on NUL in {}",
                p.display()
            );
            for seg in strings.split(|&b| b == 0) {
                assert!(
                    std::str::from_utf8(seg).is_ok(),
                    "#Strings segment not UTF-8 in {}",
                    p.display()
                );
            }
        }

        // #Blob tiles exactly: each entry is <compressed-len><bytes>.
        let blobs = md.blobs;
        let mut pos = 0usize;
        while pos < blobs.len() {
            let (len, consumed) = read_compressed_u32(&blobs[pos..]).expect("blob length prefix");
            pos += consumed + len as usize;
            assert!(pos <= blobs.len(), "blob overran #Blob in {}", p.display());
        }
        assert_eq!(
            pos,
            blobs.len(),
            "#Blob did not tile exactly in {}",
            p.display()
        );

        // #GUID is a packed array of 16-byte GUIDs.
        assert_eq!(
            md.guids.len() % 16,
            0,
            "#GUID length not a multiple of 16 in {}",
            p.display()
        );
    }
}

/// Find the metadata-root (`BSJB`) offset in a real assembly image.
pub(super) fn metadata_root_offset(bytes: &[u8]) -> usize {
    const BSJB: [u8; 4] = [0x42, 0x53, 0x4A, 0x42];
    bytes
        .windows(4)
        .position(|w| w == BSJB)
        .expect("fixture has a metadata root")
}

/// Byte offset of the `Offset` and `Size` fields of the first stream header
/// whose name is not `#~`, walking the II.24.2.1 metadata root from `md_offset`.
fn first_heap_stream_header(bytes: &[u8], md_offset: usize) -> (usize, usize) {
    let mut pos = md_offset + 12; // signature, versions, reserved
    let version_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4 + ((version_len + 3) & !3); // length field + padded version string
    pos += 2; // flags
    let stream_count = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    for _ in 0..stream_count {
        let offset_field = pos;
        let size_field = pos + 4;
        pos += 8;
        let name_start = pos;
        let nul = bytes[name_start..]
            .iter()
            .position(|&b| b == 0)
            .expect("stream name NUL");
        let name = &bytes[name_start..name_start + nul];
        pos += (nul + 1 + 3) & !3; // NUL-terminated name padded to 4 bytes
        if name != b"#~" {
            return (offset_field, size_field);
        }
    }
    panic!("no heap stream header found");
}

/// Byte offset where the name of the stream called `target` begins, walking the
/// II.24.2.1 stream-header list from `md_offset`.
fn stream_name_offset(bytes: &[u8], md_offset: usize, target: &[u8]) -> usize {
    let mut pos = md_offset + 12; // signature, versions, reserved
    let version_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4 + ((version_len + 3) & !3); // length field + padded version string
    pos += 2; // flags
    let stream_count = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    for _ in 0..stream_count {
        pos += 8; // offset + size
        let name_start = pos;
        let nul = bytes[name_start..]
            .iter()
            .position(|&b| b == 0)
            .expect("stream name NUL");
        let name = &bytes[name_start..name_start + nul];
        if name == target {
            return name_start;
        }
        pos += (nul + 1 + 3) & !3; // NUL-terminated name padded to 4 bytes
    }
    panic!("stream {target:?} not found");
}

/// Byte offset of the CLI (COR20) header's `MetaData.Size` field, walking the
/// PE the same way the reader does (DOS → COFF → optional header → CLI data
/// directory → section table → RVA-to-offset).
fn cli_metadata_size_field(bytes: &[u8]) -> usize {
    let rd32 = |off: usize| u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
    let rd16 = |off: usize| u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap());

    let e_lfanew = rd32(0x3C) as usize;
    let coff = e_lfanew + 4; // skip "PE\0\0"
    let num_sections = rd16(coff + 2) as usize;
    let size_optional = rd16(coff + 16) as usize;
    let optional_start = coff + 20;
    let data_dirs_at = match rd16(optional_start) {
        0x10B => 96,  // PE32
        0x20B => 112, // PE32+
        m => panic!("unexpected optional-header magic {m:#x}"),
    };
    let cli_rva = rd32(optional_start + data_dirs_at + 14 * 8);

    let mut pos = optional_start + size_optional; // section headers
    for _ in 0..num_sections {
        let virtual_size = rd32(pos + 8);
        let virtual_address = rd32(pos + 12);
        let raw_data_size = rd32(pos + 16);
        let raw_data_ptr = rd32(pos + 20);
        let span = if virtual_size == 0 {
            raw_data_size
        } else {
            virtual_size
        };
        if cli_rva >= virtual_address && cli_rva < virtual_address + span {
            let cli_offset = (raw_data_ptr + (cli_rva - virtual_address)) as usize;
            // COR20: cb(4) + Major(2) + Minor(2) + MetaData.RVA(4) => Size at +12.
            return cli_offset + 12;
        }
        pos += 40;
    }
    panic!("CLI header RVA not in any section");
}

/// `(file offset of the SizeOfRawData field, RVA delta within section)` for the
/// PE section containing `rva`.
fn section_raw_size_field_for_rva(bytes: &[u8], rva: u32) -> (usize, u32) {
    let rd32 = |off: usize| u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
    let rd16 = |off: usize| u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap());

    let e_lfanew = rd32(0x3C) as usize;
    let coff = e_lfanew + 4;
    let num_sections = rd16(coff + 2) as usize;
    let size_optional = rd16(coff + 16) as usize;
    let mut pos = coff + 20 + size_optional; // section headers

    for _ in 0..num_sections {
        let virtual_size = rd32(pos + 8);
        let virtual_address = rd32(pos + 12);
        let raw_data_size = rd32(pos + 16);
        let span = if virtual_size == 0 {
            raw_data_size
        } else {
            virtual_size
        };
        if rva >= virtual_address && rva < virtual_address + span {
            return (pos + 16, rva - virtual_address); // SizeOfRawData at +16
        }
        pos += 40;
    }
    panic!("RVA not in any section");
}

/// An RVA that lands in a section's zero-filled virtual tail (at or beyond
/// `SizeOfRawData`) has no file bytes; mapping it to `raw_data_ptr + delta`
/// would consume padding or the next section. Shrinking the metadata section's
/// on-disk size so the metadata RVA falls in that tail must be refused, not read
/// from the now-unbacked bytes.
#[test]
fn rejects_rva_in_unbacked_section_tail() {
    for p in fixtures() {
        let original = std::fs::read(&p).expect("fixture");
        MetadataFile::read(&original).expect("fixture parses");

        let md_size_field = cli_metadata_size_field(&original);
        let metadata_rva = u32::from_le_bytes(
            original[md_size_field - 4..md_size_field]
                .try_into()
                .unwrap(),
        );
        let (raw_size_field, delta) = section_raw_size_field_for_rva(&original, metadata_rva);

        // SizeOfRawData == delta makes the metadata RVA the first byte of the
        // unbacked tail (delta >= SizeOfRawData). The CLI header sits earlier in
        // the section (smaller delta), so it still resolves.
        let mut corrupted = original.clone();
        corrupted[raw_size_field..raw_size_field + 4].copy_from_slice(&delta.to_le_bytes());

        assert_eq!(
            MetadataFile::read(&corrupted).err(),
            Some(Error::BadMetadataRoot),
            "RVA in unbacked section tail must be refused in {}",
            p.display()
        );
    }
}

/// The metadata-root header is parsed only from within the declared metadata
/// region: if the CLI `MetaData.Size` is too small to contain the root header,
/// the reader refuses it rather than reading the header from arbitrary
/// surrounding file bytes.
#[test]
fn bounds_header_reads_to_declared_size() {
    for p in fixtures() {
        let original = std::fs::read(&p).expect("fixture");
        MetadataFile::read(&original).expect("fixture parses");

        let size_field = cli_metadata_size_field(&original);
        let mut corrupted = original.clone();
        // 8 bytes cannot hold the root header (signature + versions + version
        // length + flags + stream count), so the root must be refused.
        corrupted[size_field..size_field + 4].copy_from_slice(&8u32.to_le_bytes());

        assert_eq!(
            MetadataFile::read(&corrupted).err(),
            Some(Error::BadMetadataRoot),
            "undersized metadata region must be refused in {}",
            p.display()
        );
    }
}

/// A required heap (`#Strings`/`#Blob`/`#GUID`) that is absent must be refused
/// with `MissingHeap`, not silently substituted with an empty slice — otherwise
/// a structurally broken image would decode tables against the wrong heap state.
#[test]
fn rejects_missing_required_heap() {
    for name in [
        b"#Strings".as_slice(),
        b"#Blob".as_slice(),
        b"#GUID".as_slice(),
    ] {
        for p in fixtures() {
            let original = std::fs::read(&p).expect("fixture");
            MetadataFile::read(&original).expect("fixture parses");
            let md_offset = metadata_root_offset(&original);

            // Corrupt the heap's name so the reader no longer recognises it; the
            // heap is then absent (same byte length, so later headers are intact).
            let name_off = stream_name_offset(&original, md_offset, name);
            let mut corrupted = original.clone();
            corrupted[name_off] = b'Z';

            let expected = std::str::from_utf8(name).unwrap();
            assert_eq!(
                MetadataFile::read(&corrupted).err(),
                Some(Error::MissingHeap(expected)),
                "missing {expected} must be refused in {}",
                p.display()
            );
        }
    }
}

/// `#US` (user strings) is *not* required: it holds only IL `ldstr` operands,
/// which this reader never decodes, and real reference assemblies omit it. An
/// absent `#US` must still parse, treated as an empty heap.
#[test]
fn tolerates_missing_user_strings() {
    for p in fixtures() {
        let original = std::fs::read(&p).expect("fixture");
        let md_offset = metadata_root_offset(&original);
        let name_off = stream_name_offset(&original, md_offset, b"#US");
        let mut corrupted = original.clone();
        corrupted[name_off] = b'Z';
        assert!(
            MetadataFile::read(&corrupted).is_ok(),
            "absent #US must be tolerated in {}",
            p.display()
        );
    }
}

/// A stream header pointing outside the declared metadata region is refused,
/// even when the bytes it names lie within the file. Without the
/// metadata-size bound the reader would accept arbitrary non-metadata bytes as
/// heap data; this pins that structural guarantee.
#[test]
fn rejects_stream_outside_metadata_region() {
    for p in fixtures() {
        let original = std::fs::read(&p).expect("fixture");
        // Sanity: the unmodified fixture parses.
        MetadataFile::read(&original).expect("fixture parses");

        let md_offset = metadata_root_offset(&original);
        let (offset_field, size_field) = first_heap_stream_header(&original, md_offset);

        // Point the heap stream at appended padding: its bytes are inside the
        // (grown) file but the offset is at/after the original end, hence past
        // the metadata region.
        let original_len = original.len();
        let mut corrupted = original.clone();
        corrupted.extend(std::iter::repeat_n(0u8, 64));
        let bogus_offset = (original_len - md_offset) as u32;
        corrupted[offset_field..offset_field + 4].copy_from_slice(&bogus_offset.to_le_bytes());
        corrupted[size_field..size_field + 4].copy_from_slice(&16u32.to_le_bytes());

        assert_eq!(
            MetadataFile::read(&corrupted).err(),
            Some(Error::BadMetadataRoot),
            "stream outside metadata region must be refused in {}",
            p.display()
        );
    }
}

/// The bounds-checked heap accessors honour the index-0 conventions and refuse
/// out-of-range offsets rather than panicking.
#[test]
fn heap_accessors_bounds_checked() {
    for p in fixtures() {
        let bytes = std::fs::read(&p).expect("fixture");
        let md = MetadataFile::read(&bytes).unwrap();

        if !md.strings.is_empty() {
            assert_eq!(md.string_at(0).unwrap(), "");
        }
        if !md.blobs.is_empty() {
            assert_eq!(md.blob_at(0).unwrap(), &[] as &[u8]);
        }
        assert_eq!(
            md.string_at(md.strings.len() as u32 + 1),
            Err(Error::HeapOffsetOutOfRange)
        );
        assert_eq!(
            md.blob_at(md.blobs.len() as u32 + 1),
            Err(Error::HeapOffsetOutOfRange)
        );
        // GUID index is 1-based; index 0 means "no GUID".
        assert_eq!(md.guid_at(0), Err(Error::HeapOffsetOutOfRange));
        if md.guids.len() >= 16 {
            assert!(md.guid_at(1).is_ok());
        }
    }
}

/// File offset of the `#~` stream's `HeapSizes` byte (II.24.2.6): the stream
/// data starts `Reserved[4] + Major[1] + Minor[1]` ahead of it.
pub(super) fn tilde_heap_sizes_offset(bytes: &[u8], md_offset: usize) -> usize {
    let mut pos = md_offset + 12; // signature, versions, reserved
    let version_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4 + ((version_len + 3) & !3);
    pos += 2; // flags
    let stream_count = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    for _ in 0..stream_count {
        let stream_offset = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 8; // offset + size
        let name_start = pos;
        let nul = bytes[name_start..]
            .iter()
            .position(|&b| b == 0)
            .expect("stream name NUL");
        let name = &bytes[name_start..name_start + nul];
        pos += (nul + 1 + 3) & !3; // NUL-terminated name padded to 4 bytes
        if name == b"#~" {
            return md_offset + stream_offset + 6;
        }
    }
    panic!("#~ stream not found");
}

/// The `#~` `HeapSizes` `ExtraData` flag (`0x40`) inserts a 4-byte field after
/// the row-count array, displacing the table rows. This reader does not skip
/// that field, so an image setting the flag must be refused loudly rather than
/// decoding every table row four bytes out of alignment.
#[test]
fn rejects_extra_data_table_stream() {
    for p in fixtures() {
        let original = std::fs::read(&p).expect("fixture");
        MetadataFile::read(&original).expect("fixture parses");
        let md_offset = metadata_root_offset(&original);
        let heap_sizes_at = tilde_heap_sizes_offset(&original, md_offset);
        assert_eq!(
            original[heap_sizes_at] & 0x40,
            0,
            "fixture unexpectedly already sets ExtraData in {}",
            p.display()
        );
        let mut corrupted = original.clone();
        corrupted[heap_sizes_at] |= 0x40;
        assert_eq!(
            MetadataFile::read(&corrupted).err(),
            Some(Error::UnsupportedTableStream),
            "the #~ ExtraData flag must be refused in {}",
            p.display()
        );
    }
}
