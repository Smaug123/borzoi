//! The structural ECMA-335 container: PE/COFF → CLI header → metadata root →
//! streams → the `#~` table directory.
//!
//! [`MetadataFile::read`] is total over arbitrary input: every read is
//! bounds-checked and maps shortfall to a structural [`Error`]. It produces a
//! borrowed view (heap slices + per-table row counts + heap-index widths) that
//! later stages walk to build the owned `Image`. No table rows are decoded
//! here.

use super::Error;
use super::cursor::{Cursor, read_compressed_u32};

/// `#~` heap-index width flags (ECMA-335 II.24.2.6 `HeapSizes`).
pub(crate) struct HeapSizes {
    /// `#Strings` indices are 4 bytes wide (else 2).
    pub wide_strings: bool,
    /// `#GUID` indices are 4 bytes wide (else 2).
    pub wide_guid: bool,
    /// `#Blob` indices are 4 bytes wide (else 2).
    pub wide_blob: bool,
}

/// One PE section header, reduced to what RVA→file-offset resolution needs.
/// Shared with the sibling `pdb` reader (which walks the same PE container to
/// reach the debug directory), like [`super::Cursor`].
pub(crate) struct Section {
    pub(crate) virtual_address: u32,
    pub(crate) virtual_size: u32,
    pub(crate) raw_data_ptr: u32,
    pub(crate) raw_data_size: u32,
}

/// A parsed metadata container, borrowing the input image.
pub(crate) struct MetadataFile<'a> {
    /// The whole PE image, retained for later RVA-addressed reads (resources).
    pub image: &'a [u8],
    pub strings: &'a [u8],
    pub blobs: &'a [u8],
    /// Storing the slice is what enforces the required-heap refusal
    /// (`MissingHeap("#GUID")`) in `read`; only the container-shape tests
    /// read it back (via [`Self::guid_at`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub guids: &'a [u8],
    /// The `#~` stream region following its header — the raw table rows that
    /// later stages decode.
    pub tables: &'a [u8],
    /// Row count per ECMA-335 table index (0 for tables absent from `Valid`).
    pub rows: [u32; 64],
    pub heap_sizes: HeapSizes,
    /// PE sections, for resolving RVAs (CLI resource blobs) in later stages.
    sections: Vec<Section>,
    /// CLI-header `Resources` data-directory RVA and size (II.25.3.3).
    pub resources_rva: u32,
    pub resources_size: u32,
}

/// CLI (COM descriptor) data directory index within the PE optional header.
const CLI_HEADER_DIRECTORY: usize = 14;
/// `BSJB`, the metadata-root signature (II.24.2.1), little-endian.
const METADATA_SIGNATURE: u32 = 0x424A_5342;

/// The file-backed bytes an RVA maps to, ending at the section's raw data, or
/// `None` when the RVA is unbacked. Only a section's raw data — `[0,
/// SizeOfRawData)` — exists on disk; an RVA in the zero-filled virtual tail
/// (`delta >= SizeOfRawData`) has no file bytes, so mapping it would read
/// padding or a neighbouring section's bytes. Such an RVA is refused, and the
/// returned slice stops at the section's raw data so every read taken through
/// it stays within that section.
pub(crate) fn rva_to_slice<'a>(
    image: &'a [u8],
    sections: &[Section],
    rva: u32,
) -> Option<&'a [u8]> {
    for s in sections {
        let span = if s.virtual_size == 0 {
            s.raw_data_size
        } else {
            s.virtual_size
        };
        let end = s.virtual_address.checked_add(span)?;
        if rva >= s.virtual_address && rva < end {
            let delta = rva - s.virtual_address;
            if delta >= s.raw_data_size {
                return None;
            }
            let start = s.raw_data_ptr.checked_add(delta)? as usize;
            let raw_end = s.raw_data_ptr.checked_add(s.raw_data_size)? as usize;
            return image.get(start..raw_end);
        }
    }
    None
}

/// Read a stream-header name: ASCII, NUL-terminated, padded with NULs to a
/// 4-byte boundary (so reading 4 bytes at a time stays aligned). Max 32 bytes.
fn read_stream_name(c: &mut Cursor) -> Option<Vec<u8>> {
    let mut name = Vec::new();
    loop {
        let chunk = c.read_bytes(4)?;
        if let Some(nul) = chunk.iter().position(|&b| b == 0) {
            name.extend_from_slice(&chunk[..nul]);
            return Some(name);
        }
        name.extend_from_slice(chunk);
        if name.len() > 32 {
            return None;
        }
    }
}

impl<'a> MetadataFile<'a> {
    pub(crate) fn read(image: &'a [u8]) -> Result<MetadataFile<'a>, Error> {
        let pe = Error::NotPortableExecutable;

        // --- DOS header: "MZ" magic, e_lfanew at 0x3C ---
        if image.get(0..2) != Some(b"MZ") {
            return Err(pe);
        }
        let e_lfanew = Cursor::at(image, 0x3C).read_u32().ok_or(pe)? as usize;

        // --- PE signature + COFF file header ---
        let mut c = Cursor::at(image, e_lfanew);
        if c.read_bytes(4) != Some(b"PE\0\0") {
            return Err(pe);
        }
        c.skip(2).ok_or(pe)?; // Machine
        let num_sections = c.read_u16().ok_or(pe)? as usize;
        c.skip(4 + 4 + 4).ok_or(pe)?; // TimeDateStamp, PtrToSymbols, NumSymbols
        let size_optional = c.read_u16().ok_or(pe)? as usize;
        c.skip(2).ok_or(pe)?; // Characteristics

        // --- Optional header: magic determines the data-directory offset ---
        let optional_start = c.position();
        let magic = c.read_u16().ok_or(pe)?;
        let data_dirs_at = match magic {
            0x10B => 96,  // PE32
            0x20B => 112, // PE32+
            _ => return Err(pe),
        };

        // CLI header data directory (RVA, Size).
        let cli_dir = optional_start + data_dirs_at + CLI_HEADER_DIRECTORY * 8;
        let mut dc = Cursor::at(image, cli_dir);
        let cli_rva = dc.read_u32().ok_or(Error::NoCliHeader)?;
        let _cli_size = dc.read_u32().ok_or(Error::NoCliHeader)?;
        if cli_rva == 0 {
            return Err(Error::NoCliHeader);
        }

        // --- Section headers (40 bytes each) ---
        let mut sc = Cursor::at(image, optional_start + size_optional);
        let mut sections = Vec::with_capacity(num_sections);
        for _ in 0..num_sections {
            sc.skip(8).ok_or(pe)?; // Name
            let virtual_size = sc.read_u32().ok_or(pe)?;
            let virtual_address = sc.read_u32().ok_or(pe)?;
            let raw_data_size = sc.read_u32().ok_or(pe)?;
            let raw_data_ptr = sc.read_u32().ok_or(pe)?;
            sc.skip(16).ok_or(pe)?; // relocs/linenums pointers + counts + characteristics
            sections.push(Section {
                virtual_address,
                virtual_size,
                raw_data_ptr,
                raw_data_size,
            });
        }

        // --- CLI (COR20) header, II.25.3.3 ---
        // Reads are bounded to the CLI header's own section raw data, so a
        // header straddling the end of a section is refused rather than read
        // out of the next section.
        let cli_region = rva_to_slice(image, &sections, cli_rva).ok_or(Error::NoCliHeader)?;
        let mut cli = Cursor::new(cli_region);
        cli.skip(4 + 2 + 2).ok_or(Error::NoCliHeader)?; // cb, Major/MinorRuntimeVersion
        let metadata_rva = cli.read_u32().ok_or(Error::NoCliHeader)?;
        let metadata_size = cli.read_u32().ok_or(Error::NoCliHeader)? as usize;
        cli.skip(4 + 4).ok_or(Error::NoCliHeader)?; // Flags, EntryPointToken
        let resources_rva = cli.read_u32().ok_or(Error::NoCliHeader)?;
        let resources_size = cli.read_u32().ok_or(Error::NoCliHeader)?;

        // --- Metadata root, II.24.2.1 ---
        let bad = Error::BadMetadataRoot;
        // The metadata root resolves to the file-backed bytes of its section,
        // and the declared `metadata_size` must fit within them — otherwise the
        // root spills past the section's raw data and is refused. *Every* read —
        // the root header as well as the heap regions — is sliced from this
        // declared region, never from arbitrary surrounding file bytes. Stream
        // offsets are relative to the root and bounded by `metadata_size`.
        let md_region = rva_to_slice(image, &sections, metadata_rva).ok_or(bad)?;
        let metadata = md_region.get(..metadata_size).ok_or(bad)?;
        let mut m = Cursor::new(metadata);
        if m.read_u32().ok_or(bad)? != METADATA_SIGNATURE {
            return Err(bad);
        }
        m.skip(2 + 2 + 4).ok_or(bad)?; // Major, Minor, Reserved
        let version_len = m.read_u32().ok_or(bad)? as usize;
        let version_padded = version_len.checked_add(3).ok_or(bad)? & !3;
        m.skip(version_padded).ok_or(bad)?;
        m.skip(2).ok_or(bad)?; // Flags
        let stream_count = m.read_u16().ok_or(bad)? as usize;

        // --- Stream headers ---
        let mut strings = None;
        let mut blobs = None;
        let mut guids = None;
        let mut tilde = None;
        for _ in 0..stream_count {
            let stream_offset = m.read_u32().ok_or(bad)? as usize;
            let stream_size = m.read_u32().ok_or(bad)? as usize;
            let name = read_stream_name(&mut m).ok_or(bad)?;
            // Stream offset/size are relative to the metadata root and must stay
            // within its declared size (II.24.2.2); slicing from `metadata`
            // enforces that.
            let stream_end = stream_offset.checked_add(stream_size).ok_or(bad)?;
            let region = metadata.get(stream_offset..stream_end).ok_or(bad)?;
            match name.as_slice() {
                b"#Strings" => strings = Some(region),
                // `#US` holds only IL `ldstr` operands, which nothing here
                // decodes: recognised (its region is still bounds-checked by
                // the slice above, like every stream's) but not retained.
                b"#US" => {}
                b"#Blob" => blobs = Some(region),
                b"#GUID" => guids = Some(region),
                b"#~" => tilde = Some(region),
                // "#-" (uncompressed tables) and unknown streams are unsupported.
                _ => {}
            }
        }
        let tilde = tilde.ok_or(Error::MissingHeap("#~"))?;

        // --- "#~" tables stream header, II.24.2.6 ---
        let mut t = Cursor::new(tilde);
        t.skip(4 + 1 + 1).ok_or(bad)?; // Reserved, Major, Minor
        let heap_sizes_byte = t.read_u8().ok_or(bad)?;
        // The `ExtraData` flag (0x40, EnC/unoptimized metadata) inserts a 4-byte
        // field between the row counts and the first table row. This reader does
        // not skip it, so the table rows would otherwise be located four bytes
        // out of alignment; refuse such an image rather than mis-decode it.
        if heap_sizes_byte & 0x40 != 0 {
            return Err(Error::UnsupportedTableStream);
        }
        t.skip(1).ok_or(bad)?; // Reserved (== 1)
        let valid = t.read_u64().ok_or(bad)?;
        t.skip(8).ok_or(bad)?; // Sorted
        let mut rows = [0u32; 64];
        for (i, row) in rows.iter_mut().enumerate() {
            if valid & (1u64 << i) != 0 {
                *row = t.read_u32().ok_or(bad)?;
            }
        }
        let tables = tilde.get(t.position()..).ok_or(bad)?;

        let heap_sizes = HeapSizes {
            wide_strings: heap_sizes_byte & 0x01 != 0,
            wide_guid: heap_sizes_byte & 0x02 != 0,
            wide_blob: heap_sizes_byte & 0x04 != 0,
        };

        // `#Strings`/`#Blob`/`#GUID` are required: the metadata tables index into
        // them, and ECMA-335 guarantees them (`Module.Name`, the non-null
        // `Module.Mvid`, member signatures). An absent one is a structural
        // failure, not an empty heap. `#US` holds only IL `ldstr` operands, which
        // this reader never decodes, so its absence is tolerated (and its
        // presence not retained).
        Ok(MetadataFile {
            image,
            strings: strings.ok_or(Error::MissingHeap("#Strings"))?,
            blobs: blobs.ok_or(Error::MissingHeap("#Blob"))?,
            guids: guids.ok_or(Error::MissingHeap("#GUID"))?,
            tables,
            rows,
            heap_sizes,
            sections,
            resources_rva,
            resources_size,
        })
    }

    /// Resolve an RVA to the file-backed bytes it maps to, bounded to the
    /// containing section's raw data. See [`rva_to_slice`].
    pub(crate) fn rva_to_slice(&self, rva: u32) -> Option<&'a [u8]> {
        rva_to_slice(self.image, &self.sections, rva)
    }

    /// The NUL-terminated UTF-8 string at `offset` in `#Strings`.
    pub(crate) fn string_at(&self, offset: u32) -> Result<&'a str, Error> {
        let rest = self
            .strings
            .get(offset as usize..)
            .ok_or(Error::HeapOffsetOutOfRange)?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::HeapOffsetOutOfRange)?;
        std::str::from_utf8(&rest[..end]).map_err(|_| Error::HeapOffsetOutOfRange)
    }

    /// The length-prefixed blob at `offset` in `#Blob` (II.24.2.4).
    pub(crate) fn blob_at(&self, offset: u32) -> Result<&'a [u8], Error> {
        let rest = self
            .blobs
            .get(offset as usize..)
            .ok_or(Error::HeapOffsetOutOfRange)?;
        let (len, consumed) = read_compressed_u32(rest).ok_or(Error::HeapOffsetOutOfRange)?;
        let end = consumed
            .checked_add(len as usize)
            .ok_or(Error::HeapOffsetOutOfRange)?;
        rest.get(consumed..end).ok_or(Error::HeapOffsetOutOfRange)
    }

    /// The 16-byte GUID at 1-based `index` in `#GUID` (II.24.2.5). Index 0 means
    /// "no GUID" and is rejected here; callers treat it as absent. No
    /// production consumer yet — the container-shape tests pin the heap's
    /// 1-based indexing and bounds behaviour through it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn guid_at(&self, index: u32) -> Result<[u8; 16], Error> {
        if index == 0 {
            return Err(Error::HeapOffsetOutOfRange);
        }
        let start = (index as usize - 1)
            .checked_mul(16)
            .ok_or(Error::HeapOffsetOutOfRange)?;
        let end = start.checked_add(16).ok_or(Error::HeapOffsetOutOfRange)?;
        let bytes = self
            .guids
            .get(start..end)
            .ok_or(Error::HeapOffsetOutOfRange)?;
        let mut g = [0u8; 16];
        g.copy_from_slice(bytes);
        Ok(g)
    }
}
