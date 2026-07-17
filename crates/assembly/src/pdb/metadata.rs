//! The portable-PDB **metadata image**: a `BSJB`-rooted ECMA-335 container
//! (the inflated embedded PDB from [`super::embedded_portable_pdb`]).
//!
//! Structurally this is the same container as an assembly's metadata — the same
//! root header, the same `#Strings`/`#Blob`/`#GUID` heaps, the same `#~`
//! table-stream layout (II.24.2) — but it carries a different set of tables: the
//! portable-PDB debug tables `0x30..=0x37` (`Document`, `MethodDebugInformation`,
//! `LocalScope`, …) instead of the type-system tables. The PE-coupled
//! [`crate::reader::metadata`]/[`crate::reader::tables`] can't be pointed at it
//! directly (it has no PE, and a portable PDB sizes some of its indexes from
//! *referenced* type-system tables that live in the partner assembly, not in the
//! PDB — their row counts come from the `#Pdb` stream), so this is a small
//! dedicated reader over the same `Cursor`.
//!
//! It reads:
//! - the **`Document` table** (0x30) — source file names, in the portable-PDB
//!   path-compression encoding;
//! - the **`MethodDebugInformation` table** (0x31) — parallel to the partner
//!   assembly's `MethodDef` table, whose `SequencePoints` blob maps IL offsets to
//!   source `(document, line, column)` via a delta codec
//!   ([`PortablePdb::method_first_sequence_point`]);
//! - the **`CustomDebugInformation` table** (0x37) — from which
//!   [`PortablePdb::document_embedded_source`] recovers a document's *embedded
//!   source text* when the build embeds it (offline go-to-definition). Not every
//!   document embeds its source: FSharp.Core embeds only generated files and
//!   SourceLinks the rest, so a `None` here means "fetch via SourceLink instead".

use super::PdbError;
use crate::fsharp_resource::decompress_deflate_exact;
use crate::reader::Cursor;

/// `BSJB`, the metadata-root signature (II.24.2.1), little-endian.
const METADATA_SIGNATURE: u32 = 0x424A_5342;

// Portable-PDB table indices.
const TABLE_DOCUMENT: usize = 0x30;
const TABLE_METHOD_DEBUG_INFO: usize = 0x31;
const TABLE_LOCAL_SCOPE: usize = 0x32;
const TABLE_LOCAL_VARIABLE: usize = 0x33;
const TABLE_LOCAL_CONSTANT: usize = 0x34;
const TABLE_IMPORT_SCOPE: usize = 0x35;
const TABLE_STATE_MACHINE_METHOD: usize = 0x36;
const TABLE_CUSTOM_DEBUG_INFO: usize = 0x37;
/// `MethodDef` — a *type-system* table the PDB references (rows from `#Pdb`).
const TABLE_METHOD_DEF: usize = 0x06;

/// Tag-bit count of the `HasCustomDebugInformation` coded index (27 encodable
/// tables ⇒ 5 bits).
const HCDI_TAG_BITS: u32 = 5;
/// `HasCustomDebugInformation` tag selecting the `Document` table.
const HCDI_TAG_DOCUMENT: u32 = 22;
/// Tables a `HasCustomDebugInformation` coded index can reference — the
/// type-system `HasCustomAttribute` set plus the portable-PDB tables. Only used
/// to size the index (the largest member's row count decides 2 vs 4 bytes).
const HCDI_TABLES: [usize; 27] = [
    0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x00, 0x0E, 0x17, 0x14, 0x11, 0x1A, 0x1B, 0x20, 0x23,
    0x26, 0x27, 0x28, 0x2A, 0x2C, 0x2B, 0x30, 0x32, 0x33, 0x34, 0x35,
];

/// `Embedded Source` `CustomDebugInformation` kind GUID
/// `0E8A571B-6926-466E-B4AD-8AB04611F5FE`, in metadata `#GUID` byte order.
const EMBEDDED_SOURCE_GUID: [u8; 16] = [
    0x1b, 0x57, 0x8a, 0x0e, 0x26, 0x69, 0x6e, 0x46, 0xb4, 0xad, 0x8a, 0xb0, 0x46, 0x11, 0xf5, 0xfe,
];

/// `SourceLink` `CustomDebugInformation` kind GUID
/// `CC110556-A091-4D38-9FEC-25AB9A351A6A`, in metadata `#GUID` byte order. This
/// record is module-scoped and its value is the SourceLink JSON document.
const SOURCE_LINK_GUID: [u8; 16] = [
    0x56, 0x05, 0x11, 0xcc, 0x91, 0xa0, 0x38, 0x4d, 0x9f, 0xec, 0x25, 0xab, 0x9a, 0x35, 0x1a, 0x6a,
];

/// Heap-index byte widths (2 or 4) from the `#~` `HeapSizes` byte.
#[derive(Clone, Copy)]
struct HeapWidths {
    string: usize,
    guid: usize,
    blob: usize,
}

/// One column's on-disk kind, enough to compute its byte width.
#[derive(Clone, Copy)]
enum Col {
    /// 2-byte fixed integer.
    U16,
    /// 4-byte fixed integer.
    U32,
    /// `#Strings` heap index.
    Str,
    /// `#GUID` heap index.
    Guid,
    /// `#Blob` heap index.
    Blob,
    /// Simple index into the table with this index (2 or 4 bytes by its rows).
    Simple(usize),
    /// `HasCustomDebugInformation` coded index.
    HasCustomDebugInfo,
}

/// The columns of portable-PDB table `t`, in order, or `&[]` for a
/// non-PDB/undefined index.
fn pdb_schema(t: usize) -> &'static [Col] {
    use Col::{Blob, Guid, HasCustomDebugInfo, Simple, Str, U16, U32};
    match t {
        TABLE_DOCUMENT => &[Blob, Guid, Blob, Guid],
        TABLE_METHOD_DEBUG_INFO => &[Simple(TABLE_DOCUMENT), Blob],
        TABLE_LOCAL_SCOPE => &[
            Simple(TABLE_METHOD_DEF),
            Simple(TABLE_IMPORT_SCOPE),
            Simple(TABLE_LOCAL_VARIABLE),
            Simple(TABLE_LOCAL_CONSTANT),
            U32,
            U32,
        ],
        TABLE_LOCAL_VARIABLE => &[U16, U16, Str],
        TABLE_LOCAL_CONSTANT => &[Str, Blob],
        TABLE_IMPORT_SCOPE => &[Simple(TABLE_IMPORT_SCOPE), Blob],
        TABLE_STATE_MACHINE_METHOD => &[Simple(TABLE_METHOD_DEF), Simple(TABLE_METHOD_DEF)],
        TABLE_CUSTOM_DEBUG_INFO => &[HasCustomDebugInfo, Guid, Blob],
        _ => &[],
    }
}

/// Byte width of `col` given the heap widths and the (combined) row counts.
fn col_width(col: Col, heap: &HeapWidths, rows: &[u32; 64]) -> usize {
    match col {
        Col::U16 => 2,
        Col::U32 => 4,
        Col::Str => heap.string,
        Col::Guid => heap.guid,
        Col::Blob => heap.blob,
        Col::Simple(t) => simple_index_width(rows[t]),
        Col::HasCustomDebugInfo => hcdi_width(rows),
    }
}

/// Simple-index byte width into a table with `row_count` rows (II.24.2.6): 2
/// bytes when the count fits in 16 bits, else 4.
fn simple_index_width(row_count: u32) -> usize {
    if row_count < (1 << 16) { 2 } else { 4 }
}

/// `HasCustomDebugInformation` coded-index width: 2 bytes when the largest
/// member table's row count fits in `16 - tag_bits` bits, else 4.
fn hcdi_width(rows: &[u32; 64]) -> usize {
    let max = HCDI_TABLES.iter().map(|&t| rows[t]).max().unwrap_or(0);
    if u64::from(max) < (1u64 << (16 - HCDI_TAG_BITS)) {
        2
    } else {
        4
    }
}

/// A parsed portable-PDB metadata image, borrowing the inflated bytes.
pub struct PortablePdb<'a> {
    /// `#Blob` heap.
    blobs: &'a [u8],
    /// `#GUID` heap (`CustomDebugInformation.Kind`).
    guids: &'a [u8],
    /// The `#~` table region following its header — raw table rows.
    tables: &'a [u8],
    /// Combined row counts: the PDB tables `0x30..=0x37` (from `#~`) and the
    /// referenced type-system tables (from `#Pdb`), so coded/simple indexes size
    /// correctly.
    rows: [u32; 64],
    /// Heap-index widths.
    heap: HeapWidths,
    /// Byte stride of one row of each PDB table (`0x30..=0x37`).
    stride: [usize; 64],
    /// Byte offset of each PDB table within the `#~` region.
    offset: [usize; 64],
    /// The 20-byte PDB id from the head of the `#Pdb` stream: a 16-byte GUID
    /// followed by a 4-byte stamp. It uniquely identifies this PDB, and a
    /// sidecar `.pdb` matches its DLL exactly when this equals the DLL's
    /// CodeView debug-directory id ([`super::PdbReference::id`]).
    id: [u8; 20],
}

/// The source location of one sequence point: a row id into the [`Document`
/// table](PortablePdb::document_name) plus a 1-based line and column.
///
/// [`Document` table]: PortablePdb::document_name
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequencePoint {
    /// `Document` table row id (1-based) the point maps into. Resolve to a file
    /// name with [`PortablePdb::document_name`].
    pub document: u32,
    /// 1-based source line.
    pub start_line: u32,
    /// 1-based source column.
    pub start_column: u32,
}

impl<'a> PortablePdb<'a> {
    /// Parse the portable-PDB metadata image (the inflated embedded PDB).
    ///
    /// Fails (`BadMetadataImage` / `MissingPdbStream` / `UnsupportedPdbTable` /
    /// `TableIndexOutOfRange`) only on a structurally malformed image; it
    /// interprets no row here beyond laying out the tables.
    pub fn read(image: &'a [u8]) -> Result<PortablePdb<'a>, PdbError> {
        let bad = || PdbError::BadMetadataImage;

        // --- Metadata root (II.24.2.1) — the image *is* the BSJB blob. ---
        let mut m = Cursor::new(image);
        if m.read_u32().ok_or_else(bad)? != METADATA_SIGNATURE {
            return Err(PdbError::BadMetadataImage);
        }
        m.skip(2 + 2 + 4).ok_or_else(bad)?; // Major, Minor, Reserved
        let version_len = m.read_u32().ok_or_else(bad)? as usize;
        let version_padded = version_len.checked_add(3).ok_or_else(bad)? & !3;
        m.skip(version_padded).ok_or_else(bad)?;
        m.skip(2).ok_or_else(bad)?; // Flags
        let stream_count = m.read_u16().ok_or_else(bad)? as usize;

        // --- Stream headers. ---
        let mut blobs: Option<&[u8]> = None;
        let mut guids: Option<&[u8]> = None;
        let mut tilde: Option<&[u8]> = None;
        let mut pdb: Option<&[u8]> = None;
        for _ in 0..stream_count {
            let off = m.read_u32().ok_or_else(bad)? as usize;
            let size = m.read_u32().ok_or_else(bad)? as usize;
            let name = read_stream_name(&mut m).ok_or_else(bad)?;
            let end = off.checked_add(size).ok_or_else(bad)?;
            let region = image.get(off..end).ok_or_else(bad)?;
            match name.as_slice() {
                b"#Blob" => blobs = Some(region),
                b"#GUID" => guids = Some(region),
                b"#~" => tilde = Some(region),
                b"#Pdb" => pdb = Some(region),
                _ => {} // #Strings / #US: not read by the current accessors.
            }
        }
        let tilde = tilde.ok_or(PdbError::MissingPdbStream("#~"))?;
        let blobs = blobs.ok_or(PdbError::MissingPdbStream("#Blob"))?;
        let pdb = pdb.ok_or(PdbError::MissingPdbStream("#Pdb"))?;

        // --- "#~" table-stream header (II.24.2.6): heap widths + PDB-table rows.
        let mut t = Cursor::new(tilde);
        t.skip(4 + 1 + 1).ok_or_else(bad)?; // Reserved, Major, Minor
        let heap_sizes_byte = t.read_u8().ok_or_else(bad)?;
        // The `ExtraData` flag (0x40) inserts a field this reader doesn't skip.
        if heap_sizes_byte & 0x40 != 0 {
            return Err(PdbError::BadMetadataImage);
        }
        t.skip(1).ok_or_else(bad)?; // Reserved (== 1)
        let valid = t.read_u64().ok_or_else(bad)?;
        t.skip(8).ok_or_else(bad)?; // Sorted
        let mut rows = [0u32; 64];
        for (i, row) in rows.iter_mut().enumerate() {
            if valid & (1u64 << i) != 0 {
                // A portable PDB's `#~` carries only the PDB-specific tables.
                if !(TABLE_DOCUMENT..=TABLE_CUSTOM_DEBUG_INFO).contains(&i) {
                    return Err(PdbError::UnsupportedPdbTable(i));
                }
                *row = t.read_u32().ok_or_else(bad)?;
            }
        }
        let tables = tilde.get(t.position()..).ok_or_else(bad)?;

        // --- "#Pdb" stream: referenced type-system table row counts. These size
        // simple/coded indexes that point into the partner assembly's tables. ---
        let mut p = Cursor::new(pdb);
        let id: [u8; 20] = p
            .read_bytes(20)
            .ok_or_else(bad)?
            .try_into()
            .expect("read_bytes(20) yields exactly 20 bytes");
        p.skip(4).ok_or_else(bad)?; // EntryPoint token
        let referenced = p.read_u64().ok_or_else(bad)?;
        for (i, row) in rows.iter_mut().enumerate() {
            if referenced & (1u64 << i) != 0 {
                let count = p.read_u32().ok_or_else(bad)?;
                // Referenced tables are type-system tables; never the PDB tables
                // (whose rows came from `#~`). Read every referenced count to
                // stay aligned, but never clobber a PDB row count.
                if i < TABLE_DOCUMENT {
                    *row = count;
                }
            }
        }

        let heap = HeapWidths {
            string: if heap_sizes_byte & 0x01 != 0 { 4 } else { 2 },
            guid: if heap_sizes_byte & 0x02 != 0 { 4 } else { 2 },
            blob: if heap_sizes_byte & 0x04 != 0 { 4 } else { 2 },
        };

        // --- Lay out the PDB tables consecutively in index order. ---
        let mut stride = [0usize; 64];
        let mut offset = [0usize; 64];
        let mut running = 0usize;
        for table in TABLE_DOCUMENT..=TABLE_CUSTOM_DEBUG_INFO {
            offset[table] = running;
            let s: usize = pdb_schema(table)
                .iter()
                .map(|&c| col_width(c, &heap, &rows))
                .sum();
            stride[table] = s;
            let bytes = (rows[table] as usize)
                .checked_mul(s)
                .ok_or(PdbError::TableIndexOutOfRange)?;
            running = running
                .checked_add(bytes)
                .ok_or(PdbError::TableIndexOutOfRange)?;
        }
        if running > tables.len() {
            return Err(PdbError::TableIndexOutOfRange);
        }

        Ok(PortablePdb {
            blobs,
            guids: guids.unwrap_or(&[]),
            tables,
            rows,
            heap,
            stride,
            offset,
            id,
        })
    }

    /// The 20-byte PDB id (16-byte GUID + 4-byte stamp) from the head of the
    /// `#Pdb` stream. A sidecar `.pdb` belongs to a given DLL exactly when this
    /// equals the DLL's CodeView debug-directory id
    /// ([`super::PdbReference::id`]); comparing them rejects a stale sidecar
    /// left next to a rebuilt assembly.
    pub fn id(&self) -> [u8; 20] {
        self.id
    }

    /// The little-endian value of column `col` (0-based) in row `row` (0-based)
    /// of PDB `table` — a fixed integer, heap index, simple index, or coded
    /// index. The row/column are bounds-checked against the `#~` region.
    fn col_value(&self, table: usize, row: u32, col: usize) -> Result<u32, PdbError> {
        let schema = pdb_schema(table);
        let row_off = self.offset[table]
            .checked_add(
                (row as usize)
                    .checked_mul(self.stride[table])
                    .ok_or(PdbError::TableIndexOutOfRange)?,
            )
            .ok_or(PdbError::TableIndexOutOfRange)?;
        let col_off: usize = schema[..col]
            .iter()
            .map(|&c| col_width(c, &self.heap, &self.rows))
            .sum();
        let width = col_width(schema[col], &self.heap, &self.rows);
        let start = row_off
            .checked_add(col_off)
            .ok_or(PdbError::TableIndexOutOfRange)?;
        let bytes = self
            .tables
            .get(start..start + width)
            .ok_or(PdbError::TableIndexOutOfRange)?;
        Ok(read_heap_index(bytes))
    }

    /// Number of rows in the `Document` table (1-based RIDs run `1..=count`).
    pub fn document_count(&self) -> u32 {
        self.rows[TABLE_DOCUMENT]
    }

    /// The source file name of `Document` row `rid` (1-based), decoded from its
    /// path-compressed `Name` blob. The string is the document path as the
    /// compiler recorded it (often an absolute or SourceLink-mapped path).
    pub fn document_name(&self, rid: u32) -> Result<String, PdbError> {
        if rid == 0 || rid > self.rows[TABLE_DOCUMENT] {
            return Err(PdbError::TableIndexOutOfRange);
        }
        // `Name` is column 0 (a `#Blob` index).
        let name_idx = self.col_value(TABLE_DOCUMENT, rid - 1, 0)?;
        let name_blob = blob_at(self.blobs, name_idx)?;
        decode_document_name(self.blobs, name_blob)
    }

    /// Number of rows in the `MethodDebugInformation` table — equal to the
    /// partner assembly's `MethodDef` count, since the tables are parallel.
    /// A method's `MethodDef` row id is its row id here.
    pub fn method_debug_info_count(&self) -> u32 {
        self.rows[TABLE_METHOD_DEBUG_INFO]
    }

    /// The first non-hidden sequence point of the method whose `MethodDef` row
    /// id is `method_rid` (1-based) — the source location go-to-definition jumps
    /// to. `Ok(None)` when the method carries no sequence points (no source
    /// mapping: compiler-generated, abstract, or all-hidden).
    pub fn method_first_sequence_point(
        &self,
        method_rid: u32,
    ) -> Result<Option<SequencePoint>, PdbError> {
        if method_rid == 0 || method_rid > self.rows[TABLE_METHOD_DEBUG_INFO] {
            return Err(PdbError::TableIndexOutOfRange);
        }
        // Column 0: Document (simple index). Column 1: SequencePoints (#Blob).
        let document_column = self.col_value(TABLE_METHOD_DEBUG_INFO, method_rid - 1, 0)?;
        let sp_index = self.col_value(TABLE_METHOD_DEBUG_INFO, method_rid - 1, 1)?;
        if sp_index == 0 {
            return Ok(None); // No sequence points for this method.
        }
        let blob = blob_at(self.blobs, sp_index).map_err(|_| PdbError::MalformedSequencePoints)?;
        decode_first_sequence_point(blob, document_column, self.rows[TABLE_DOCUMENT])
    }

    /// The *embedded source text* of `Document` row `rid` (1-based), if the PDB
    /// embeds it (an `Embedded Source` `CustomDebugInformation` whose parent is
    /// that document). `Ok(None)` when the document carries no embedded source —
    /// then the source must come from SourceLink / the filesystem instead.
    ///
    /// This is what lets go-to-definition into a referenced assembly work fully
    /// offline: the source bytes travel inside the PDB inside the DLL.
    pub fn document_embedded_source(&self, rid: u32) -> Result<Option<String>, PdbError> {
        if rid == 0 || rid > self.rows[TABLE_DOCUMENT] {
            return Err(PdbError::TableIndexOutOfRange);
        }
        for row in 0..self.rows[TABLE_CUSTOM_DEBUG_INFO] {
            // Parent is a `HasCustomDebugInformation` coded index: low tag bits
            // select the table, the rest is the 1-based row id.
            let parent = self.col_value(TABLE_CUSTOM_DEBUG_INFO, row, 0)?;
            if parent & ((1 << HCDI_TAG_BITS) - 1) != HCDI_TAG_DOCUMENT
                || parent >> HCDI_TAG_BITS != rid
            {
                continue;
            }
            let kind_idx = self.col_value(TABLE_CUSTOM_DEBUG_INFO, row, 1)?;
            if guid_at(self.guids, kind_idx)? != EMBEDDED_SOURCE_GUID {
                continue;
            }
            let value_idx = self.col_value(TABLE_CUSTOM_DEBUG_INFO, row, 2)?;
            let value =
                blob_at(self.blobs, value_idx).map_err(|_| PdbError::MalformedEmbeddedSource)?;
            return Ok(Some(decode_embedded_source(value)?));
        }
        Ok(None)
    }

    /// The module's **SourceLink** JSON document, if present — a
    /// `{ "documents": { "<path-prefix>*": "<url-prefix>*" } }` map from build
    /// source paths to fetchable URLs. Returned verbatim (UTF-8); parsing the map
    /// and resolving a document name to a URL is left to the caller (so this
    /// crate stays free of a JSON dependency).
    ///
    /// This is how a SourceLink-only document (one with no embedded source —
    /// most of FSharp.Core, including `printf.fs`) is turned into a fetchable
    /// URL for go-to-definition.
    pub fn sourcelink_json(&self) -> Result<Option<String>, PdbError> {
        for row in 0..self.rows[TABLE_CUSTOM_DEBUG_INFO] {
            let kind_idx = self.col_value(TABLE_CUSTOM_DEBUG_INFO, row, 1)?;
            if guid_at(self.guids, kind_idx)? != SOURCE_LINK_GUID {
                continue;
            }
            let value_idx = self.col_value(TABLE_CUSTOM_DEBUG_INFO, row, 2)?;
            let value =
                blob_at(self.blobs, value_idx).map_err(|_| PdbError::MalformedSourceLink)?;
            return Ok(Some(
                String::from_utf8(value.to_vec()).map_err(|_| PdbError::MalformedSourceLink)?,
            ));
        }
        Ok(None)
    }
}

/// Read a stream-header name: ASCII, NUL-terminated, padded with NULs to a
/// 4-byte boundary. Mirrors `reader::metadata::read_stream_name` (kept local so
/// the PDB reader doesn't reach into the PE-coupled reader's internals).
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

/// A little-endian heap index from its 2 or 4 on-disk bytes.
fn read_heap_index(bytes: &[u8]) -> u32 {
    let mut value = 0u32;
    for (i, &b) in bytes.iter().enumerate() {
        value |= u32::from(b) << (8 * i);
    }
    value
}

/// The length-prefixed blob at `index` in `#Blob` (II.24.2.4). Index 0 is the
/// empty blob.
fn blob_at(blobs: &[u8], index: u32) -> Result<&[u8], PdbError> {
    let rest = blobs
        .get(index as usize..)
        .ok_or(PdbError::TableIndexOutOfRange)?;
    let mut c = Cursor::new(rest);
    let len = c
        .read_compressed_u32()
        .ok_or(PdbError::TableIndexOutOfRange)? as usize;
    let start = c.position();
    let end = start
        .checked_add(len)
        .ok_or(PdbError::TableIndexOutOfRange)?;
    rest.get(start..end).ok_or(PdbError::TableIndexOutOfRange)
}

/// The 16-byte GUID at 1-based `index` in `#GUID` (II.24.2.5). Index 0 means
/// "no GUID" and is refused here.
fn guid_at(guids: &[u8], index: u32) -> Result<[u8; 16], PdbError> {
    if index == 0 {
        return Err(PdbError::TableIndexOutOfRange);
    }
    let start = (index as usize - 1)
        .checked_mul(16)
        .ok_or(PdbError::TableIndexOutOfRange)?;
    let end = start
        .checked_add(16)
        .ok_or(PdbError::TableIndexOutOfRange)?;
    let bytes = guids
        .get(start..end)
        .ok_or(PdbError::TableIndexOutOfRange)?;
    let mut g = [0u8; 16];
    g.copy_from_slice(bytes);
    Ok(g)
}

/// Decode a portable-PDB *document-name blob*: a single separator byte followed
/// by a sequence of compressed `#Blob` heap indices ("parts"). The name is the
/// parts joined by the separator char; a part index of 0 is an empty string,
/// and a 0 separator joins the parts with nothing between them.
fn decode_document_name(blobs: &[u8], name_blob: &[u8]) -> Result<String, PdbError> {
    if name_blob.is_empty() {
        return Ok(String::new());
    }
    let separator = name_blob[0];
    let parts = &name_blob[1..];
    let mut out: Vec<u8> = Vec::new();
    let mut c = Cursor::new(parts);
    let mut first = true;
    while c.position() < parts.len() {
        let idx = c
            .read_compressed_u32()
            .ok_or(PdbError::MalformedDocumentName)?;
        if !first && separator != 0 {
            out.push(separator);
        }
        first = false;
        if idx != 0 {
            let part = blob_at(blobs, idx).map_err(|_| PdbError::MalformedDocumentName)?;
            out.extend_from_slice(part);
        }
    }
    String::from_utf8(out).map_err(|_| PdbError::MalformedDocumentName)
}

/// Decode a method's sequence-points blob far enough to return its **first
/// non-hidden** sequence point — the method's first mapped source location —
/// or `None` if every point is hidden / the blob holds no points.
///
/// `document_column` is the `MethodDebugInformation.Document` value: nonzero
/// names the method's single document; 0 means the document comes from the
/// blob header's `InitialDocument` and may change mid-stream via document
/// records. Only the fields up to the first non-hidden point are decoded — its
/// `StartLine`/`StartColumn` are absolute (unsigned), so no signed-delta state
/// is needed.
///
/// `document_count` is the `Document` table's row count. The document RID that
/// ends up on the returned point is a 1-based row id into that table, so any
/// value outside `1..=document_count` is a malformed reference and is rejected
/// loudly (rather than surfacing later as an apparently-valid point whose name
/// happens not to resolve) — every returned [`SequencePoint`] names a real
/// document by construction.
fn decode_first_sequence_point(
    blob: &[u8],
    document_column: u32,
    document_count: u32,
) -> Result<Option<SequencePoint>, PdbError> {
    let err = || PdbError::MalformedSequencePoints;
    let mut c = Cursor::new(blob);

    // Header: LocalSignature (always), then InitialDocument iff Document == 0.
    let _local_signature = c.read_compressed_u32().ok_or_else(err)?;
    let mut current_document = if document_column == 0 {
        c.read_compressed_u32().ok_or_else(err)?
    } else {
        document_column
    };

    let mut first_record = true;
    while c.position() < blob.len() {
        let delta_il = c.read_compressed_u32().ok_or_else(err)?;
        // A δILOffset of 0 on a *non-first* record is a document record (a
        // document switch), not a sequence point. The first record's δIL is the
        // absolute IL offset and may legitimately be 0.
        if delta_il == 0 && !first_record {
            current_document = c.read_compressed_u32().ok_or_else(err)?;
            continue;
        }
        first_record = false;

        let delta_lines = c.read_compressed_u32().ok_or_else(err)?;
        // ΔColumns is unsigned when ΔLines == 0, signed otherwise. A point is
        // hidden iff ΔLines == 0 and ΔColumns == 0 (and then carries no start
        // line/column); a ΔLines > 0 point is never hidden.
        let is_hidden = if delta_lines == 0 {
            c.read_compressed_u32().ok_or_else(err)? == 0
        } else {
            c.read_compressed_i32().ok_or_else(err)?;
            false
        };
        if is_hidden {
            continue;
        }

        // First non-hidden point: StartLine/StartColumn are absolute (unsigned).
        let start_line = c.read_compressed_u32().ok_or_else(err)?;
        let start_column = c.read_compressed_u32().ok_or_else(err)?;
        // The document RID is 1-based into the `Document` table; a value of 0 or
        // past the last row is a malformed reference, not a usable location.
        if current_document == 0 || current_document > document_count {
            return Err(PdbError::MalformedSequencePoints);
        }
        return Ok(Some(SequencePoint {
            document: current_document,
            start_line,
            start_column,
        }));
    }

    Ok(None)
}

/// Decode an `Embedded Source` `CustomDebugInformation` value blob: a 4-byte
/// little-endian `format` followed by content. `format == 0` means the content
/// is the raw UTF-8 source; `format > 0` means it is raw-deflate-compressed and
/// `format` is the decompressed byte length. A leading UTF-8 BOM is stripped.
fn decode_embedded_source(value: &[u8]) -> Result<String, PdbError> {
    let err = || PdbError::MalformedEmbeddedSource;
    let format_bytes: [u8; 4] = value.get(0..4).ok_or_else(err)?.try_into().unwrap();
    let format = i32::from_le_bytes(format_bytes);
    let content = &value[4..];
    let bytes = if format == 0 {
        content.to_vec()
    } else if format > 0 {
        decompress_deflate_exact(content, format as usize)
            .map_err(|_| PdbError::MalformedEmbeddedSource)?
    } else {
        return Err(err());
    };
    let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    String::from_utf8(text.to_vec()).map_err(|_| PdbError::MalformedEmbeddedSource)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `#Blob` heap with the empty blob at index 0 and `"src"` / `"FSharp.Core"`
    /// / `"printf.fs"` at indices 1 / 5 / 17 (each blob is a 1-byte length prefix
    /// — all parts are < 128 bytes — followed by its UTF-8 bytes).
    fn blob_heap() -> Vec<u8> {
        let mut h = vec![0u8]; // index 0 = the empty blob
        for s in ["src", "FSharp.Core", "printf.fs"] {
            h.push(s.len() as u8);
            h.extend_from_slice(s.as_bytes());
        }
        h
    }

    /// A document-name blob: a separator byte followed by the compressed part
    /// indices (all < 128, so each is one byte).
    fn name_blob(separator: u8, parts: &[u8]) -> Vec<u8> {
        let mut v = vec![separator];
        v.extend_from_slice(parts);
        v
    }

    #[test]
    fn document_name_joins_parts_with_separator() {
        let heap = blob_heap();
        let name = decode_document_name(&heap, &name_blob(b'/', &[1, 5, 17])).unwrap();
        assert_eq!(name, "src/FSharp.Core/printf.fs");
    }

    #[test]
    fn empty_leading_part_yields_leading_separator() {
        let heap = blob_heap();
        // Part 0 (empty), then "src" → "/src" (the empty first part gives the
        // leading separator).
        let name = decode_document_name(&heap, &name_blob(b'/', &[0, 1])).unwrap();
        assert_eq!(name, "/src");
    }

    #[test]
    fn zero_separator_concatenates_parts() {
        let heap = blob_heap();
        // "src" then "FSharp.Core", joined by nothing.
        let name = decode_document_name(&heap, &name_blob(0, &[1, 5])).unwrap();
        assert_eq!(name, "srcFSharp.Core");
    }

    #[test]
    fn empty_name_blob_is_empty_string() {
        assert_eq!(decode_document_name(&[0u8], &[]).unwrap(), "");
        // A lone separator with no parts is also empty.
        assert_eq!(
            decode_document_name(&[0u8], &name_blob(b'/', &[])).unwrap(),
            ""
        );
    }

    #[test]
    fn part_index_past_the_heap_is_malformed() {
        let heap = blob_heap();
        // Index 99 isn't a blob in this heap.
        assert!(matches!(
            decode_document_name(&heap, &name_blob(b'/', &[99])),
            Err(PdbError::MalformedDocumentName)
        ));
    }

    // --- Sequence-points codec ---------------------------------------------
    //
    // Every field in these synthetic blobs is < 128, so each compressed integer
    // is a single byte and the blob can be written as a plain byte slice.
    // Records used here keep ΔLines == 0, so ΔColumns is the unsigned form
    // throughout (a ΔLines > 0 record would encode ΔColumns as a signed value).

    #[test]
    fn first_non_hidden_point_after_a_hidden_one() {
        // Document column = 5 (nonzero ⇒ no InitialDocument in the header).
        // Header: LocalSig=0.
        // Record 1 (first, hidden): δIL=0, ΔLines=0, ΔColumns=0.
        // Record 2 (non-hidden): δIL=5, ΔLines=0, ΔColumns=10, StartLine=42, StartColumn=7.
        let blob = [0, /*hidden*/ 0, 0, 0, /*point*/ 5, 0, 10, 42, 7];
        let sp = decode_first_sequence_point(&blob, 5, 8).unwrap().unwrap();
        assert_eq!(
            sp,
            SequencePoint {
                document: 5,
                start_line: 42,
                start_column: 7
            }
        );
    }

    #[test]
    fn document_record_switches_the_current_document() {
        // Document column = 0 ⇒ header carries InitialDocument=3.
        // Record 1 (first, hidden): δIL=0, ΔLines=0, ΔColumns=0.
        // Record 2 (document record): δIL=0 (non-first), Document=8.
        // Record 3 (non-hidden): δIL=5, ΔLines=0, ΔColumns=10, StartLine=42, StartColumn=7.
        let blob = [
            0, 3, /*hidden*/ 0, 0, 0, /*doc-record*/ 0, 8, /*point*/ 5, 0, 10, 42, 7,
        ];
        let sp = decode_first_sequence_point(&blob, 0, 8).unwrap().unwrap();
        assert_eq!(
            sp,
            SequencePoint {
                document: 8, // the document-record's document, not InitialDocument
                start_line: 42,
                start_column: 7
            }
        );
    }

    #[test]
    fn first_record_non_hidden_is_absolute() {
        // The very first record being non-hidden: its δIL=3 is the absolute IL
        // offset (not a document record), and Start{Line,Column} are absolute.
        let blob = [0, /*point*/ 3, 0, 4, 100, 12];
        let sp = decode_first_sequence_point(&blob, 9, 9).unwrap().unwrap();
        assert_eq!(
            sp,
            SequencePoint {
                document: 9,
                start_line: 100,
                start_column: 12
            }
        );
    }

    #[test]
    fn no_records_or_all_hidden_is_none() {
        // Header only (LocalSig=0), no records.
        assert_eq!(decode_first_sequence_point(&[0], 5, 8).unwrap(), None);
        // Header + a single hidden record → still no source location.
        assert_eq!(
            decode_first_sequence_point(&[0, 0, 0, 0], 5, 8).unwrap(),
            None
        );
    }

    #[test]
    fn truncated_blob_is_malformed() {
        // Claims a point (δIL=5, ΔLines=0, ΔColumns=10) but stops before
        // StartLine/StartColumn.
        assert!(matches!(
            decode_first_sequence_point(&[0, 5, 0, 10], 5, 8),
            Err(PdbError::MalformedSequencePoints)
        ));
    }

    #[test]
    fn document_rid_zero_is_malformed() {
        // A point whose document RID is 0 (no valid 1-based row) must fail
        // loudly rather than return an apparently-valid point. Column = 0 with
        // an InitialDocument of 0 in the header.
        let blob = [0, 0, /*point*/ 3, 0, 4, 100, 12];
        assert!(matches!(
            decode_first_sequence_point(&blob, 0, 8),
            Err(PdbError::MalformedSequencePoints)
        ));
    }

    #[test]
    fn document_rid_past_the_table_is_malformed() {
        // Document column = 9, but the Document table has only 8 rows: an
        // out-of-range reference, so no usable location.
        let blob = [0, /*point*/ 3, 0, 4, 100, 12];
        assert!(matches!(
            decode_first_sequence_point(&blob, 9, 8),
            Err(PdbError::MalformedSequencePoints)
        ));
    }

    #[test]
    fn document_switch_to_out_of_range_rid_is_malformed() {
        // Header InitialDocument=3 (valid), then a document-switch record moves
        // to RID 8 which is past the 5-row table before the first real point.
        let blob = [
            0, 3, /*hidden*/ 0, 0, 0, /*doc-record*/ 0, 8, /*point*/ 5, 0, 10, 42, 7,
        ];
        assert!(matches!(
            decode_first_sequence_point(&blob, 0, 5),
            Err(PdbError::MalformedSequencePoints)
        ));
    }

    // --- Embedded-source value codec ---------------------------------------

    /// `format` (4-byte LE) followed by `content`.
    fn embedded_value(format: i32, content: &[u8]) -> Vec<u8> {
        let mut v = format.to_le_bytes().to_vec();
        v.extend_from_slice(content);
        v
    }

    fn deflate(bytes: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::DeflateEncoder};
        use std::io::Write;
        let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn uncompressed_embedded_source_is_returned_verbatim() {
        let src = "let printfn fmt = ...\n";
        let value = embedded_value(0, src.as_bytes());
        assert_eq!(decode_embedded_source(&value).unwrap(), src);
    }

    #[test]
    fn compressed_embedded_source_inflates() {
        let src = "module Microsoft.FSharp.Core.Printf\nlet printfn = ...\n";
        let value = embedded_value(src.len() as i32, &deflate(src.as_bytes()));
        assert_eq!(decode_embedded_source(&value).unwrap(), src);
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let mut content = vec![0xEF, 0xBB, 0xBF];
        content.extend_from_slice(b"namespace N\n");
        let value = embedded_value(0, &content);
        assert_eq!(decode_embedded_source(&value).unwrap(), "namespace N\n");
    }

    #[test]
    fn embedded_source_too_short_or_negative_is_malformed() {
        // Fewer than 4 bytes for the format field.
        assert!(matches!(
            decode_embedded_source(&[0, 0]),
            Err(PdbError::MalformedEmbeddedSource)
        ));
        // A negative format is invalid.
        let value = embedded_value(-1, b"junk");
        assert!(matches!(
            decode_embedded_source(&value),
            Err(PdbError::MalformedEmbeddedSource)
        ));
    }

    #[test]
    fn compressed_embedded_source_size_mismatch_is_malformed() {
        let src = b"hello";
        // Declare the wrong decompressed length.
        let value = embedded_value(999, &deflate(src));
        assert!(matches!(
            decode_embedded_source(&value),
            Err(PdbError::MalformedEmbeddedSource)
        ));
    }
}
