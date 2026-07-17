//! The **debug** side of a managed PE: the embedded portable PDB.
//!
//! ECMA-335 metadata (the `reader` module) describes an assembly's *types and
//! members*; a *portable PDB* describes its *source* — which file each method
//! came from, the line/column of each IL offset, and (optionally) the source
//! text itself, embedded. A deterministic .NET build can carry that PDB
//! **inside** the DLL, as an `IMAGE_DEBUG_TYPE_EMBEDDEDPORTABLEPDB` entry in the
//! PE debug directory (a `DeflateStream`-compressed metadata image behind an
//! `MPDB` magic). The shipped `FSharp.Core.dll` does exactly this, with no
//! sidecar `.pdb` anywhere — so reading the embedded image is what makes
//! offline go-to-definition into FSharp.Core source possible.
//!
//! This module locates and inflates that embedded image, returning the raw
//! portable-PDB *metadata image* (a `BSJB`-rooted ECMA-335 container with
//! PDB-specific streams/tables); parsing its tables — `Document`,
//! `MethodDebugInformation` sequence points, embedded source — is built on top
//! ([`PortablePdb`]).
//!
//! Most assemblies (NuGet packages like FsUnit) instead ship a *sidecar*
//! `.pdb`: the DLL's debug directory carries only a CodeView (`RSDS`) pointer to
//! it. [`codeview_pdb_reference`] parses that pointer — the sidecar's file name
//! and the 20-byte id it must match ([`PortablePdb::id`]) — so a caller can find
//! the separate `.pdb` on disk and confirm it belongs to the DLL.

mod metadata;
pub use metadata::{PortablePdb, SequencePoint};

use crate::fsharp_resource::{MAX_DEFLATE_OUTPUT_BYTES, decompress_deflate_limited};
use crate::reader::{Cursor, Section, rva_to_slice};

/// A structural failure reading the PE debug directory or the embedded PDB it
/// points at. Absence of an embedded PDB is **not** an error — a perfectly
/// valid PE simply has no such entry — so the locating API returns
/// `Ok(None)` for that case and reserves these variants for malformed input.
#[derive(Debug)]
pub enum PdbError {
    /// The bytes are not a PE image (no `MZ`/`PE` header, or an optional-header
    /// magic this reader doesn't recognise). Distinct from "a valid PE with no
    /// embedded PDB", which is `Ok(None)`.
    NotPortableExecutable,
    /// The PE's debug data directory, or one of its entries, is truncated or
    /// points outside the file.
    MalformedDebugDirectory,
    /// An `EmbeddedPortablePdb` entry was found but its payload is not a
    /// well-formed `MPDB` blob (bad magic, truncated, or the inflated length
    /// disagrees with the declared uncompressed size).
    BadEmbeddedPdb,
    /// The embedded PDB's deflate stream failed to decompress.
    Decompress(std::io::Error),
    /// The portable-PDB metadata image is malformed: a bad `BSJB` root, a
    /// truncated stream header, or a `#~` table-stream header this reader can't
    /// model (e.g. the `ExtraData` flag).
    BadMetadataImage,
    /// A required stream or heap is absent from the portable-PDB image (the
    /// `&'static str` names it, e.g. `"#~"` or `"#Blob"`).
    MissingPdbStream(&'static str),
    /// The portable-PDB `#~` stream marks a non-PDB table present (a portable
    /// PDB carries only tables `0x30..=0x37`); the offending index is carried.
    UnsupportedPdbTable(usize),
    /// A table row index, column span, or heap index falls outside the data it
    /// addresses.
    TableIndexOutOfRange,
    /// A `Document` row's name blob doesn't decode under the portable-PDB
    /// path-compression codec (truncated parts, or non-UTF-8 content).
    MalformedDocumentName,
    /// A method's sequence-points blob is truncated or otherwise doesn't decode
    /// under the portable-PDB sequence-points delta codec.
    MalformedSequencePoints,
    /// A document's `Embedded Source` `CustomDebugInformation` value is
    /// truncated, declares a bad compression format/length, or isn't UTF-8.
    MalformedEmbeddedSource,
    /// The module's `SourceLink` `CustomDebugInformation` value isn't UTF-8 (its
    /// JSON content is otherwise returned verbatim for the caller to parse).
    MalformedSourceLink,
}

impl std::fmt::Display for PdbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdbError::NotPortableExecutable => write!(f, "not a portable executable"),
            PdbError::MalformedDebugDirectory => write!(f, "malformed PE debug directory"),
            PdbError::BadEmbeddedPdb => write!(f, "malformed embedded portable PDB"),
            PdbError::Decompress(e) => write!(f, "embedded PDB decompression failed: {e}"),
            PdbError::BadMetadataImage => write!(f, "malformed portable-PDB metadata image"),
            PdbError::MissingPdbStream(s) => write!(f, "portable PDB is missing the {s} stream"),
            PdbError::UnsupportedPdbTable(t) => {
                write!(f, "unexpected non-PDB table {t:#x} in the portable PDB")
            }
            PdbError::TableIndexOutOfRange => write!(f, "portable-PDB table index out of range"),
            PdbError::MalformedDocumentName => write!(f, "malformed portable-PDB document name"),
            PdbError::MalformedSequencePoints => {
                write!(f, "malformed portable-PDB sequence-points blob")
            }
            PdbError::MalformedEmbeddedSource => {
                write!(f, "malformed portable-PDB embedded source")
            }
            PdbError::MalformedSourceLink => write!(f, "malformed portable-PDB SourceLink"),
        }
    }
}

impl std::error::Error for PdbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PdbError::Decompress(e) => Some(e),
            _ => None,
        }
    }
}

/// Index of the Debug data directory within the PE optional header (II/PE).
const DEBUG_DIRECTORY: usize = 6;
/// Size of one `IMAGE_DEBUG_DIRECTORY` entry.
const DEBUG_ENTRY_SIZE: usize = 28;
/// `IMAGE_DEBUG_TYPE_EMBEDDEDPORTABLEPDB`.
const EMBEDDED_PORTABLE_PDB: u32 = 17;
/// `IMAGE_DEBUG_TYPE_CODEVIEW` — the sidecar-PDB pointer (an `RSDS` record
/// naming the `.pdb` and carrying the GUID half of its id).
const CODEVIEW: u32 = 2;
/// Magic prefixing the (compressed) embedded portable-PDB blob.
const MPDB_MAGIC: &[u8] = b"MPDB";
/// Magic prefixing a portable-PDB CodeView (`RSDS`) record.
const RSDS_MAGIC: &[u8] = b"RSDS";

/// The PE pieces this module needs: the section map (for RVA resolution) and
/// the Debug data directory `(RVA, size)`.
struct PeDebug {
    sections: Vec<Section>,
    debug_rva: u32,
    debug_size: u32,
}

/// Parse just enough of the PE container to reach the debug directory: DOS
/// stub → PE signature → COFF header → optional header (data directory 6) →
/// section headers. Mirrors the prefix of [`crate::reader::metadata`] but stops
/// at the debug directory rather than the CLI header.
fn parse_pe_debug(image: &[u8]) -> Result<PeDebug, PdbError> {
    // `PdbError` isn't `Copy` (the `Decompress` variant wraps `io::Error`), so
    // each structural failure constructs a fresh unit variant rather than
    // reusing one binding.
    let pe = || PdbError::NotPortableExecutable;

    // DOS header: "MZ" magic, e_lfanew at 0x3C.
    if image.get(0..2) != Some(b"MZ") {
        return Err(pe());
    }
    let e_lfanew = Cursor::at(image, 0x3C).read_u32().ok_or_else(pe)? as usize;

    // PE signature + COFF file header.
    let mut c = Cursor::at(image, e_lfanew);
    if c.read_bytes(4) != Some(b"PE\0\0") {
        return Err(pe());
    }
    c.skip(2).ok_or_else(pe)?; // Machine
    let num_sections = c.read_u16().ok_or_else(pe)? as usize;
    c.skip(4 + 4 + 4).ok_or_else(pe)?; // TimeDateStamp, PtrToSymbols, NumSymbols
    let size_optional = c.read_u16().ok_or_else(pe)? as usize;
    c.skip(2).ok_or_else(pe)?; // Characteristics

    // Optional header: magic selects the data-directory offset (PE32 vs PE32+).
    let optional_start = c.position();
    let magic = c.read_u16().ok_or_else(pe)?;
    let data_dirs_at = match magic {
        0x10B => 96,  // PE32
        0x20B => 112, // PE32+
        _ => return Err(pe()),
    };

    // Debug data directory (index 6): RVA, Size. A short optional header that
    // doesn't reach data directory 6 is a malformed debug directory for our
    // purposes (a managed PE always carries all 16).
    let dbg_dir = optional_start + data_dirs_at + DEBUG_DIRECTORY * 8;
    let mut dc = Cursor::at(image, dbg_dir);
    let debug_rva = dc.read_u32().ok_or(PdbError::MalformedDebugDirectory)?;
    let debug_size = dc.read_u32().ok_or(PdbError::MalformedDebugDirectory)?;

    // Section headers (40 bytes each).
    let mut sc = Cursor::at(image, optional_start + size_optional);
    let mut sections = Vec::with_capacity(num_sections);
    for _ in 0..num_sections {
        sc.skip(8).ok_or_else(pe)?; // Name
        let virtual_size = sc.read_u32().ok_or_else(pe)?;
        let virtual_address = sc.read_u32().ok_or_else(pe)?;
        let raw_data_size = sc.read_u32().ok_or_else(pe)?;
        let raw_data_ptr = sc.read_u32().ok_or_else(pe)?;
        sc.skip(16).ok_or_else(pe)?; // relocs/linenums + counts + characteristics
        sections.push(Section {
            virtual_address,
            virtual_size,
            raw_data_ptr,
            raw_data_size,
        });
    }

    Ok(PeDebug {
        sections,
        debug_rva,
        debug_size,
    })
}

/// Locate and inflate the embedded portable PDB in a managed PE.
///
/// - `Ok(Some(image))` — the inflated portable-PDB *metadata image* (a
///   `BSJB`-rooted ECMA-335 container). This is the raw image; later slices
///   parse its tables.
/// - `Ok(None)` — the PE is well-formed but carries no `EmbeddedPortablePdb`
///   debug entry (e.g. a build with a *sidecar* `.pdb`, or none).
/// - `Err(PdbError)` — the input isn't a PE, or the debug directory / embedded
///   blob is malformed.
///
/// Reads only the file bytes already in hand: no sidecar `.pdb`, no symbol
/// server, no network.
pub fn embedded_portable_pdb(image: &[u8]) -> Result<Option<Vec<u8>>, PdbError> {
    let pe = parse_pe_debug(image)?;
    match find_embedded_pdb_payload(image, &pe.sections, pe.debug_rva, pe.debug_size)? {
        Some(payload) => inflate_embedded_pdb(payload).map(Some),
        None => Ok(None),
    }
}

/// The PE debug directory as a slice of whole 28-byte entries, bounds-checked
/// against both its declared `(rva, size)` span *and* the section that backs
/// it. `Ok(None)` when the PE declares no debug directory at all.
///
/// A `size` that isn't a whole number of 28-byte entries, or a directory that
/// runs past its section / off the file, is a malformed directory (`Err`) —
/// never silently floored to fewer entries or read past its end.
fn debug_directory<'a>(
    image: &'a [u8],
    sections: &[Section],
    debug_rva: u32,
    debug_size: u32,
) -> Result<Option<&'a [u8]>, PdbError> {
    if debug_rva == 0 || debug_size == 0 {
        return Ok(None); // No debug directory at all.
    }
    let dir_region =
        rva_to_slice(image, sections, debug_rva).ok_or(PdbError::MalformedDebugDirectory)?;
    // Bound to the *declared* size as well as the section: the directory is
    // exactly `debug_size` bytes, and that must be a whole number of entries.
    let dir = dir_region
        .get(..debug_size as usize)
        .ok_or(PdbError::MalformedDebugDirectory)?;
    if dir.len() % DEBUG_ENTRY_SIZE != 0 {
        return Err(PdbError::MalformedDebugDirectory);
    }
    Ok(Some(dir))
}

/// The bytes a debug-directory entry's data points at, bounded to `size_of_data`
/// and the file/section that backs it. The data is reachable both by file
/// offset (`PointerToRawData`) and by RVA (`AddressOfRawData`); prefer the
/// direct file offset, falling back to mapping the RVA when a linker left the
/// pointer zero.
fn debug_entry_data<'a>(
    image: &'a [u8],
    sections: &[Section],
    addr_raw: u32,
    ptr_raw: usize,
    size_of_data: usize,
) -> Result<&'a [u8], PdbError> {
    let payload = if ptr_raw != 0 {
        let end = ptr_raw
            .checked_add(size_of_data)
            .ok_or(PdbError::MalformedDebugDirectory)?;
        image.get(ptr_raw..end)
    } else {
        rva_to_slice(image, sections, addr_raw).and_then(|r| r.get(..size_of_data))
    };
    payload.ok_or(PdbError::MalformedDebugDirectory)
}

/// Scan the PE debug directory for the `EmbeddedPortablePdb` entry and return
/// its (still-compressed) payload bytes, or `None` if there is no such entry
/// (the API reserves `Ok(None)` for a *well-formed* directory with no
/// embedded-PDB entry — e.g. a build with a sidecar `.pdb`).
fn find_embedded_pdb_payload<'a>(
    image: &'a [u8],
    sections: &[Section],
    debug_rva: u32,
    debug_size: u32,
) -> Result<Option<&'a [u8]>, PdbError> {
    let Some(dir) = debug_directory(image, sections, debug_rva, debug_size)? else {
        return Ok(None);
    };

    for entry in dir.chunks_exact(DEBUG_ENTRY_SIZE) {
        let mut ec = Cursor::new(entry);
        // Characteristics(4) TimeDateStamp(4) MajorVersion(2) MinorVersion(2).
        ec.skip(12).ok_or(PdbError::MalformedDebugDirectory)?;
        let kind = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)?;
        let size_of_data = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)? as usize;
        let addr_raw = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)?;
        let ptr_raw = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)? as usize;

        if kind != EMBEDDED_PORTABLE_PDB {
            continue;
        }
        let payload = debug_entry_data(image, sections, addr_raw, ptr_raw, size_of_data)?;
        return Ok(Some(payload));
    }

    Ok(None)
}

/// Decode one `EmbeddedPortablePdb` entry payload: `MPDB` magic, a 4-byte
/// uncompressed size, then a raw-deflate stream of the metadata image.
fn inflate_embedded_pdb(data: &[u8]) -> Result<Vec<u8>, PdbError> {
    let mut c = Cursor::new(data);
    if c.read_bytes(4) != Some(MPDB_MAGIC) {
        return Err(PdbError::BadEmbeddedPdb);
    }
    let uncompressed = c.read_u32().ok_or(PdbError::BadEmbeddedPdb)? as usize;
    if uncompressed > MAX_DEFLATE_OUTPUT_BYTES {
        return Err(PdbError::BadEmbeddedPdb);
    }
    // The deflate stream is the remainder of the entry data (its length is the
    // entry's `SizeOfData`, so there is no trailing padding to trip the reader).
    let deflated = data.get(8..).ok_or(PdbError::BadEmbeddedPdb)?;
    let out = match decompress_deflate_limited(deflated, uncompressed) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Err(PdbError::BadEmbeddedPdb);
        }
        Err(e) => return Err(PdbError::Decompress(e)),
    };
    // The declared size is the authoritative length; a mismatch means a
    // corrupt or truncated stream.
    if out.len() != uncompressed {
        return Err(PdbError::BadEmbeddedPdb);
    }
    Ok(out)
}

/// A managed PE's pointer to its *sidecar* portable PDB, parsed from the
/// CodeView (`RSDS`) debug-directory entry. Present on any build that emits a
/// separate `.pdb` (most NuGet packages); a build that *embeds* its PDB also
/// carries one, pointing at the would-be sidecar name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdbReference {
    /// The PDB path the compiler recorded — typically an absolute build-machine
    /// path with the build OS's separators (e.g. `D:\repo\obj\Foo.pdb`). Only
    /// its file name ([`PdbReference::file_name`]) is meaningful for locating a
    /// sidecar next to the DLL.
    pub path: String,
    /// The 20-byte PDB id this DLL expects: the CodeView GUID (16 bytes)
    /// followed by the debug entry's 4-byte stamp. A sidecar `.pdb` belongs to
    /// this DLL exactly when its [`PortablePdb::id`] equals this — comparing
    /// them rejects a stale sidecar left next to a rebuilt assembly.
    pub id: [u8; 20],
}

impl PdbReference {
    /// The final component of the recorded [`path`](PdbReference::path),
    /// splitting on both `/` and `\` (the path is usually a *Windows* absolute
    /// path, so `std::path` on a Unix host would not split it). `None` for an
    /// empty path or one ending in a separator.
    pub fn file_name(&self) -> Option<&str> {
        self.path
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
    }
}

/// Locate the sidecar-PDB pointer in a managed PE's CodeView debug entry.
///
/// - `Ok(Some(reference))` — the PE carries an `RSDS` CodeView entry (its path +
///   the 20-byte id a matching sidecar must have).
/// - `Ok(None)` — a well-formed PE with no CodeView entry, or a CodeView entry
///   that isn't the portable-PDB `RSDS` form (an old NB10 record). Nothing to
///   follow.
/// - `Err(PdbError)` — the input isn't a PE, or the debug directory / entry is
///   malformed.
///
/// Reads only the bytes in hand: it does *not* open the sidecar `.pdb` (that
/// IO, and the id match against [`PortablePdb::id`], are the caller's).
pub fn codeview_pdb_reference(image: &[u8]) -> Result<Option<PdbReference>, PdbError> {
    let pe = parse_pe_debug(image)?;
    find_codeview_reference(image, &pe.sections, pe.debug_rva, pe.debug_size)
}

/// Scan the PE debug directory for the `RSDS` CodeView entry and parse it into a
/// [`PdbReference`]. Split from [`codeview_pdb_reference`] (which parses the PE
/// to reach the directory) so it can be tested against a synthetic directory +
/// section map, exactly as [`find_embedded_pdb_payload`] is.
fn find_codeview_reference(
    image: &[u8],
    sections: &[Section],
    debug_rva: u32,
    debug_size: u32,
) -> Result<Option<PdbReference>, PdbError> {
    let Some(dir) = debug_directory(image, sections, debug_rva, debug_size)? else {
        return Ok(None);
    };

    for entry in dir.chunks_exact(DEBUG_ENTRY_SIZE) {
        let mut ec = Cursor::new(entry);
        ec.skip(4).ok_or(PdbError::MalformedDebugDirectory)?; // Characteristics
        let stamp = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)?; // TimeDateStamp
        ec.skip(4).ok_or(PdbError::MalformedDebugDirectory)?; // Major/Minor version
        let kind = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)?;
        let size_of_data = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)? as usize;
        let addr_raw = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)?;
        let ptr_raw = ec.read_u32().ok_or(PdbError::MalformedDebugDirectory)? as usize;

        if kind != CODEVIEW {
            continue;
        }
        let data = debug_entry_data(image, sections, addr_raw, ptr_raw, size_of_data)?;
        // Portable-PDB CodeView record: "RSDS"(4) + Guid(16) + Age(4) + path
        // (NUL-terminated UTF-8). A non-`RSDS` (e.g. legacy NB10) record carries
        // no portable-PDB id, so there is nothing to follow.
        let mut rc = Cursor::new(data);
        if rc.read_bytes(4) != Some(RSDS_MAGIC) {
            return Ok(None);
        }
        let guid = rc.read_bytes(16).ok_or(PdbError::MalformedDebugDirectory)?;
        rc.skip(4).ok_or(PdbError::MalformedDebugDirectory)?; // Age
        let path_bytes = data.get(24..).ok_or(PdbError::MalformedDebugDirectory)?;
        // The path is NUL-terminated; trailing bytes after the NUL are padding.
        let path_bytes = match path_bytes.iter().position(|&b| b == 0) {
            Some(nul) => &path_bytes[..nul],
            None => path_bytes,
        };
        let path =
            std::str::from_utf8(path_bytes).map_err(|_| PdbError::MalformedDebugDirectory)?;

        let mut id = [0u8; 20];
        id[..16].copy_from_slice(guid);
        id[16..].copy_from_slice(&stamp.to_le_bytes());
        return Ok(Some(PdbReference {
            path: path.to_string(),
            id,
        }));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One section mapping RVA `0x2000` to file offset `0x200`, backing the
    /// half-open file range `[0x200, 0x400)`.
    fn one_section() -> Vec<Section> {
        vec![Section {
            virtual_address: 0x2000,
            virtual_size: 0x200,
            raw_data_ptr: 0x200,
            raw_data_size: 0x200,
        }]
    }

    /// A 28-byte `IMAGE_DEBUG_DIRECTORY` entry with the fields this reader uses.
    fn debug_entry(kind: u32, size_of_data: u32, addr_raw: u32, ptr_raw: u32) -> [u8; 28] {
        let mut e = [0u8; 28];
        e[12..16].copy_from_slice(&kind.to_le_bytes()); // Type
        e[16..20].copy_from_slice(&size_of_data.to_le_bytes()); // SizeOfData
        e[20..24].copy_from_slice(&addr_raw.to_le_bytes()); // AddressOfRawData
        e[24..28].copy_from_slice(&ptr_raw.to_le_bytes()); // PointerToRawData
        e
    }

    /// A 0x400-byte image with `entry_bytes` laid down at the debug directory's
    /// file offset (`0x200`, where RVA `0x2000` maps).
    fn image_with_directory(entry_bytes: &[u8]) -> Vec<u8> {
        let mut img = vec![0u8; 0x400];
        img[0x200..0x200 + entry_bytes.len()].copy_from_slice(entry_bytes);
        img
    }

    #[test]
    fn size_not_a_multiple_of_entry_is_malformed() {
        let img = image_with_directory(&[0u8; 28]);
        // 30 is within the section but not a whole number of 28-byte entries.
        let err = find_embedded_pdb_payload(&img, &one_section(), 0x2000, 30).unwrap_err();
        assert!(matches!(err, PdbError::MalformedDebugDirectory));
    }

    #[test]
    fn directory_overrunning_its_section_is_malformed() {
        let img = image_with_directory(&[0u8; 28]);
        // The section backs only 0x200 bytes from RVA 0x2000; a 0x300-byte
        // directory runs past it (and would otherwise read the next section).
        let err = find_embedded_pdb_payload(&img, &one_section(), 0x2000, 0x300).unwrap_err();
        assert!(matches!(err, PdbError::MalformedDebugDirectory));
    }

    #[test]
    fn well_formed_directory_without_embedded_entry_is_none() {
        // A lone CodeView (type 2) entry — the sidecar-PDB pointer — is not an
        // embedded PDB, so a well-formed directory yields `Ok(None)`.
        let entry = debug_entry(2, 0, 0, 0);
        let img = image_with_directory(&entry);
        let found = find_embedded_pdb_payload(&img, &one_section(), 0x2000, 28).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn embedded_entry_payload_is_returned_by_file_offset() {
        // An embedded-PDB (type 17) entry whose payload sits at file offset
        // 0x100 (reached via PointerToRawData, outside the directory's section).
        let payload = [0xAB, 0xCD, 0xEF, 0x01, 0x02];
        let mut img = image_with_directory(&debug_entry(
            EMBEDDED_PORTABLE_PDB,
            payload.len() as u32,
            0,
            0x100,
        ));
        img[0x100..0x100 + payload.len()].copy_from_slice(&payload);
        let found = find_embedded_pdb_payload(&img, &one_section(), 0x2000, 28)
            .unwrap()
            .expect("embedded entry found");
        assert_eq!(found, &payload);
    }

    #[test]
    fn embedded_entry_payload_overrunning_the_file_is_malformed() {
        // PointerToRawData + SizeOfData runs off the end of the image.
        let img = image_with_directory(&debug_entry(EMBEDDED_PORTABLE_PDB, 0x100, 0, 0x3FF));
        let err = find_embedded_pdb_payload(&img, &one_section(), 0x2000, 28).unwrap_err();
        assert!(matches!(err, PdbError::MalformedDebugDirectory));
    }

    /// A 28-byte CodeView (`type 2`) debug entry with the given TimeDateStamp,
    /// data size, and PointerToRawData (file offset of its `RSDS` payload).
    fn codeview_entry(stamp: u32, size_of_data: u32, ptr_raw: u32) -> [u8; 28] {
        let mut e = [0u8; 28];
        e[4..8].copy_from_slice(&stamp.to_le_bytes()); // TimeDateStamp
        e[12..16].copy_from_slice(&CODEVIEW.to_le_bytes()); // Type
        e[16..20].copy_from_slice(&size_of_data.to_le_bytes()); // SizeOfData
        e[24..28].copy_from_slice(&ptr_raw.to_le_bytes()); // PointerToRawData
        e
    }

    /// An `RSDS` CodeView payload: magic + 16-byte GUID + 4-byte age + a
    /// NUL-terminated path.
    fn rsds_payload(guid: [u8; 16], age: u32, path: &str) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(RSDS_MAGIC);
        p.extend_from_slice(&guid);
        p.extend_from_slice(&age.to_le_bytes());
        p.extend_from_slice(path.as_bytes());
        p.push(0);
        p
    }

    #[test]
    fn codeview_rsds_reference_is_parsed() {
        // The 20-byte id is GUID ++ stamp (little-endian), matching the head of
        // the partner `.pdb`'s `#Pdb` stream.
        let guid = [
            0x53, 0xd3, 0x48, 0x77, 0x89, 0x52, 0xc9, 0x45, 0xbf, 0x36, 0x29, 0x1a, 0x91, 0x57,
            0xe7, 0xfc,
        ];
        let stamp = 0x93c5_22ad;
        let payload = rsds_payload(guid, 1, r"D:\repo\obj\Release\net6.0\FsUnit.NUnit.pdb");
        let mut img = image_with_directory(&codeview_entry(stamp, payload.len() as u32, 0x100));
        img[0x100..0x100 + payload.len()].copy_from_slice(&payload);

        let reference = find_codeview_reference(&img, &one_section(), 0x2000, 28)
            .unwrap()
            .expect("CodeView reference found");
        assert_eq!(
            reference.path,
            r"D:\repo\obj\Release\net6.0\FsUnit.NUnit.pdb"
        );
        assert_eq!(reference.file_name(), Some("FsUnit.NUnit.pdb"));
        let mut expected = [0u8; 20];
        expected[..16].copy_from_slice(&guid);
        expected[16..].copy_from_slice(&stamp.to_le_bytes());
        assert_eq!(reference.id, expected);
    }

    #[test]
    fn file_name_splits_on_either_separator() {
        let win = PdbReference {
            path: r"C:\a\b\Foo.pdb".to_string(),
            id: [0u8; 20],
        };
        assert_eq!(win.file_name(), Some("Foo.pdb"));
        let unix = PdbReference {
            path: "/x/y/Foo.pdb".to_string(),
            id: [0u8; 20],
        };
        assert_eq!(unix.file_name(), Some("Foo.pdb"));
        let bare = PdbReference {
            path: "Foo.pdb".to_string(),
            id: [0u8; 20],
        };
        assert_eq!(bare.file_name(), Some("Foo.pdb"));
        let trailing = PdbReference {
            path: r"C:\a\".to_string(),
            id: [0u8; 20],
        };
        assert_eq!(trailing.file_name(), None);
    }

    #[test]
    fn non_rsds_codeview_entry_is_none() {
        // A legacy `NB10` CodeView record carries no portable-PDB id.
        let mut payload = b"NB10".to_vec();
        payload.extend_from_slice(&[0u8; 16]);
        let mut img = image_with_directory(&codeview_entry(1, payload.len() as u32, 0x100));
        img[0x100..0x100 + payload.len()].copy_from_slice(&payload);
        let found = find_codeview_reference(&img, &one_section(), 0x2000, 28).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn directory_without_codeview_entry_is_none() {
        // Only an embedded-PDB (type 17) entry: no sidecar pointer to follow.
        let entry = debug_entry(EMBEDDED_PORTABLE_PDB, 0, 0, 0);
        let img = image_with_directory(&entry);
        let found = find_codeview_reference(&img, &one_section(), 0x2000, 28).unwrap();
        assert!(found.is_none());
    }
}
