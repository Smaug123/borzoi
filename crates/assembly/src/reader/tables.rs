//! The ECMA-335 metadata-table layout engine.
//!
//! Stage 1 ([`super::metadata`]) located the `#~` table stream and read the
//! per-table row counts, but left the rows themselves opaque. This module
//! turns those row counts plus the heap-index widths into a *layout*: the byte
//! stride of every table's row and the offset of each table within the stream,
//! so an arbitrary `(table, row, column)` can be addressed and read.
//!
//! It is pure and total: every column width is computed from the row counts and
//! the `HeapSizes` byte per ECMA-335 II.24.2.6, and every row/column read is
//! bounds-checked against the stream, mapping shortfall to [`Error`]. No table
//! row is interpreted here — that is each consuming stage's job.

use super::Error;
use super::metadata::MetadataFile;

/// Number of metadata table indices (the `Valid`/row-count bitmap is 64 wide).
const TABLE_COUNT: usize = 64;

/// ECMA-335 II.22 table indices this reader knows how to lay out. The contiguous
/// block `0x00..=0x2C` is the entire standard set; indices above it are
/// undefined, and an image that marks one present is refused (see [`Tables::new`]).
pub(crate) mod table {
    pub(crate) const MODULE: usize = 0x00;
    pub(crate) const TYPE_REF: usize = 0x01;
    pub(crate) const TYPE_DEF: usize = 0x02;
    pub(crate) const FIELD_PTR: usize = 0x03;
    pub(crate) const FIELD: usize = 0x04;
    pub(crate) const METHOD_PTR: usize = 0x05;
    pub(crate) const METHOD_DEF: usize = 0x06;
    pub(crate) const PARAM_PTR: usize = 0x07;
    pub(crate) const PARAM: usize = 0x08;
    pub(crate) const INTERFACE_IMPL: usize = 0x09;
    pub(crate) const MEMBER_REF: usize = 0x0A;
    pub(crate) const CONSTANT: usize = 0x0B;
    pub(crate) const CUSTOM_ATTRIBUTE: usize = 0x0C;
    pub(crate) const FIELD_MARSHAL: usize = 0x0D;
    pub(crate) const DECL_SECURITY: usize = 0x0E;
    pub(crate) const CLASS_LAYOUT: usize = 0x0F;
    pub(crate) const FIELD_LAYOUT: usize = 0x10;
    pub(crate) const STANDALONE_SIG: usize = 0x11;
    pub(crate) const EVENT_MAP: usize = 0x12;
    pub(crate) const EVENT_PTR: usize = 0x13;
    pub(crate) const EVENT: usize = 0x14;
    pub(crate) const PROPERTY_MAP: usize = 0x15;
    pub(crate) const PROPERTY_PTR: usize = 0x16;
    pub(crate) const PROPERTY: usize = 0x17;
    pub(crate) const METHOD_SEMANTICS: usize = 0x18;
    pub(crate) const METHOD_IMPL: usize = 0x19;
    pub(crate) const MODULE_REF: usize = 0x1A;
    pub(crate) const TYPE_SPEC: usize = 0x1B;
    pub(crate) const IMPL_MAP: usize = 0x1C;
    pub(crate) const FIELD_RVA: usize = 0x1D;
    pub(crate) const ENC_LOG: usize = 0x1E;
    pub(crate) const ENC_MAP: usize = 0x1F;
    pub(crate) const ASSEMBLY: usize = 0x20;
    pub(crate) const ASSEMBLY_PROCESSOR: usize = 0x21;
    pub(crate) const ASSEMBLY_OS: usize = 0x22;
    pub(crate) const ASSEMBLY_REF: usize = 0x23;
    pub(crate) const ASSEMBLY_REF_PROCESSOR: usize = 0x24;
    pub(crate) const ASSEMBLY_REF_OS: usize = 0x25;
    pub(crate) const FILE: usize = 0x26;
    pub(crate) const EXPORTED_TYPE: usize = 0x27;
    pub(crate) const MANIFEST_RESOURCE: usize = 0x28;
    pub(crate) const NESTED_CLASS: usize = 0x29;
    pub(crate) const GENERIC_PARAM: usize = 0x2A;
    pub(crate) const METHOD_SPEC: usize = 0x2B;
    pub(crate) const GENERIC_PARAM_CONSTRAINT: usize = 0x2C;
}

/// A coded-index tag slot reserved by ECMA-335 that no real emitter uses (e.g.
/// `CustomAttributeType` tags 0/1/4). It contributes no rows to the width rule
/// and decoding a value that lands on it is refused as out of range.
const RESERVED: usize = usize::MAX;

/// An ECMA-335 II.24.2.6 coded-index family: a column whose value packs a small
/// tag selecting one of several tables with the row index. The width depends on
/// the largest member table's row count and the tag-bit count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coded {
    TypeDefOrRef,
    HasConstant,
    HasCustomAttribute,
    HasFieldMarshal,
    HasDeclSecurity,
    MemberRefParent,
    HasSemantics,
    MethodDefOrRef,
    MemberForwarded,
    Implementation,
    CustomAttributeType,
    ResolutionScope,
    TypeOrMethodDef,
}

impl Coded {
    /// Tag bits the family reserves in the low end of the coded value. This is
    /// `ceil(log2(number-of-encodable-slots))`, *including* the "not used" slots
    /// some families leave in the tag space (e.g. `CustomAttributeType`).
    fn tag_bits(self) -> u32 {
        match self {
            // 2 slots → 1 bit.
            Coded::HasFieldMarshal
            | Coded::HasSemantics
            | Coded::MethodDefOrRef
            | Coded::MemberForwarded
            | Coded::TypeOrMethodDef => 1,
            // 3–4 slots → 2 bits.
            Coded::TypeDefOrRef
            | Coded::HasConstant
            | Coded::HasDeclSecurity
            | Coded::Implementation
            | Coded::ResolutionScope => 2,
            // 5 slots → 3 bits (CustomAttributeType reserves 5, two unused).
            Coded::MemberRefParent | Coded::CustomAttributeType => 3,
            // 22 slots → 5 bits.
            Coded::HasCustomAttribute => 5,
        }
    }

    /// The tag→table slots for this family, indexed by the coded value's tag
    /// (II.24.2.6). [`RESERVED`] marks a slot no real emitter uses; it pins the
    /// tag space so later tags decode at the right index, but contributes no
    /// rows to the width rule and is refused on decode.
    fn slots(self) -> &'static [usize] {
        use table::*;
        match self {
            Coded::TypeDefOrRef => &[TYPE_DEF, TYPE_REF, TYPE_SPEC],
            Coded::HasConstant => &[FIELD, PARAM, PROPERTY],
            Coded::HasCustomAttribute => &[
                METHOD_DEF,
                FIELD,
                TYPE_REF,
                TYPE_DEF,
                PARAM,
                INTERFACE_IMPL,
                MEMBER_REF,
                MODULE,
                DECL_SECURITY,
                PROPERTY,
                EVENT,
                STANDALONE_SIG,
                MODULE_REF,
                TYPE_SPEC,
                ASSEMBLY,
                ASSEMBLY_REF,
                FILE,
                EXPORTED_TYPE,
                MANIFEST_RESOURCE,
                GENERIC_PARAM,
                GENERIC_PARAM_CONSTRAINT,
                METHOD_SPEC,
            ],
            Coded::HasFieldMarshal => &[FIELD, PARAM],
            Coded::HasDeclSecurity => &[TYPE_DEF, METHOD_DEF, ASSEMBLY],
            Coded::MemberRefParent => &[TYPE_DEF, TYPE_REF, MODULE_REF, METHOD_DEF, TYPE_SPEC],
            Coded::HasSemantics => &[EVENT, PROPERTY],
            Coded::MethodDefOrRef => &[METHOD_DEF, MEMBER_REF],
            Coded::MemberForwarded => &[FIELD, METHOD_DEF],
            Coded::Implementation => &[FILE, ASSEMBLY_REF, EXPORTED_TYPE],
            // Tags 0/1/4 are reserved (TypeRef/TypeDef/string forms no real
            // emitter uses); only 2 (MethodDef) and 3 (MemberRef) appear.
            Coded::CustomAttributeType => &[RESERVED, RESERVED, METHOD_DEF, MEMBER_REF, RESERVED],
            Coded::ResolutionScope => &[MODULE, MODULE_REF, ASSEMBLY_REF, TYPE_REF],
            Coded::TypeOrMethodDef => &[TYPE_DEF, METHOD_DEF],
        }
    }

    /// Byte width of this coded index given the table row counts: 2 bytes when
    /// the largest member table fits in `16 - tag_bits` bits, else 4.
    fn width(self, rows: &[u32; TABLE_COUNT]) -> usize {
        let max = self
            .slots()
            .iter()
            .filter(|&&t| t != RESERVED)
            .map(|&t| rows[t])
            .max()
            .unwrap_or(0);
        if u64::from(max) < (1u64 << (16 - self.tag_bits())) {
            2
        } else {
            4
        }
    }

    /// Decode an on-disk coded value into the table it points at and the 1-based
    /// row id, or `None` for the null coded index. ECMA-335 II.24.2.6 encodes
    /// the null reference as the single all-zero value; a nonzero value whose
    /// RID part is 0 points at the nonexistent row 0, and a tag that selects a
    /// [`RESERVED`] slot or no slot at all is likewise refused with
    /// [`Error::TableIndexOutOfRange`] rather than mistaken for an absent
    /// reference.
    pub(crate) fn decode(self, value: u32) -> Result<Option<CodedToken>, Error> {
        if value == 0 {
            return Ok(None);
        }
        let bits = self.tag_bits();
        let tag = (value & ((1u32 << bits) - 1)) as usize;
        let rid = value >> bits;
        match self.slots().get(tag) {
            Some(&table) if table != RESERVED && rid != 0 => Ok(Some(CodedToken { table, rid })),
            _ => Err(Error::TableIndexOutOfRange),
        }
    }
}

/// A decoded coded index: which table it points into and the 1-based row id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodedToken {
    pub(crate) table: usize,
    pub(crate) rid: u32,
}

/// One column of a table row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    /// A 2-byte constant. `Constant.Type` (a 1-byte type tag plus a 1-byte
    /// padding zero) is also modelled here — only its width matters for layout.
    U16,
    /// A 4-byte constant.
    U32,
    /// `#Strings` heap index (2 or 4 bytes).
    Str,
    /// `#GUID` heap index (2 or 4 bytes).
    Guid,
    /// `#Blob` heap index (2 or 4 bytes).
    Blob,
    /// A simple index into the table with this index (2 or 4 bytes).
    Simple(usize),
    /// A coded index (II.24.2.6).
    Coded(Coded),
}

impl Col {
    fn width(self, w: &Widths, rows: &[u32; TABLE_COUNT]) -> usize {
        match self {
            Col::U16 => 2,
            Col::U32 => 4,
            Col::Str => w.str,
            Col::Guid => w.guid,
            Col::Blob => w.blob,
            Col::Simple(t) => {
                if rows[t] < (1 << 16) {
                    2
                } else {
                    4
                }
            }
            Col::Coded(c) => c.width(rows),
        }
    }
}

/// The columns of table `t`, in order, per ECMA-335 II.22. `None` for an
/// undefined table index (`0x2D..0x3F`).
fn schema(t: usize) -> Option<&'static [Col]> {
    use self::Coded::*;
    use Col::Coded;
    use Col::{Blob, Guid, Simple, Str, U16, U32};
    use table::*;
    Some(match t {
        MODULE => &[U16, Str, Guid, Guid, Guid],
        TYPE_REF => &[Coded(ResolutionScope), Str, Str],
        TYPE_DEF => &[
            U32,
            Str,
            Str,
            Coded(TypeDefOrRef),
            Simple(FIELD),
            Simple(METHOD_DEF),
        ],
        FIELD_PTR => &[Simple(FIELD)],
        FIELD => &[U16, Str, Blob],
        METHOD_PTR => &[Simple(METHOD_DEF)],
        METHOD_DEF => &[U32, U16, U16, Str, Blob, Simple(PARAM)],
        PARAM_PTR => &[Simple(PARAM)],
        PARAM => &[U16, U16, Str],
        INTERFACE_IMPL => &[Simple(TYPE_DEF), Coded(TypeDefOrRef)],
        MEMBER_REF => &[Coded(MemberRefParent), Str, Blob],
        CONSTANT => &[U16, Coded(HasConstant), Blob],
        CUSTOM_ATTRIBUTE => &[Coded(HasCustomAttribute), Coded(CustomAttributeType), Blob],
        FIELD_MARSHAL => &[Coded(HasFieldMarshal), Blob],
        DECL_SECURITY => &[U16, Coded(HasDeclSecurity), Blob],
        CLASS_LAYOUT => &[U16, U32, Simple(TYPE_DEF)],
        FIELD_LAYOUT => &[U32, Simple(FIELD)],
        STANDALONE_SIG => &[Blob],
        EVENT_MAP => &[Simple(TYPE_DEF), Simple(EVENT)],
        EVENT_PTR => &[Simple(EVENT)],
        EVENT => &[U16, Str, Coded(TypeDefOrRef)],
        PROPERTY_MAP => &[Simple(TYPE_DEF), Simple(PROPERTY)],
        PROPERTY_PTR => &[Simple(PROPERTY)],
        PROPERTY => &[U16, Str, Blob],
        METHOD_SEMANTICS => &[U16, Simple(METHOD_DEF), Coded(HasSemantics)],
        METHOD_IMPL => &[
            Simple(TYPE_DEF),
            Coded(MethodDefOrRef),
            Coded(MethodDefOrRef),
        ],
        MODULE_REF => &[Str],
        TYPE_SPEC => &[Blob],
        IMPL_MAP => &[U16, Coded(MemberForwarded), Str, Simple(MODULE_REF)],
        FIELD_RVA => &[U32, Simple(FIELD)],
        ENC_LOG => &[U32, U32],
        ENC_MAP => &[U32],
        ASSEMBLY => &[U32, U16, U16, U16, U16, U32, Blob, Str, Str],
        ASSEMBLY_PROCESSOR => &[U32],
        ASSEMBLY_OS => &[U32, U32, U32],
        ASSEMBLY_REF => &[U16, U16, U16, U16, U32, Blob, Str, Str, Blob],
        ASSEMBLY_REF_PROCESSOR => &[U32, Simple(ASSEMBLY_REF)],
        ASSEMBLY_REF_OS => &[U32, U32, U32, Simple(ASSEMBLY_REF)],
        FILE => &[U32, Str, Blob],
        EXPORTED_TYPE => &[U32, U32, Str, Str, Coded(Implementation)],
        MANIFEST_RESOURCE => &[U32, U32, Str, Coded(Implementation)],
        NESTED_CLASS => &[Simple(TYPE_DEF), Simple(TYPE_DEF)],
        GENERIC_PARAM => &[U16, U16, Coded(TypeOrMethodDef), Str],
        METHOD_SPEC => &[Coded(MethodDefOrRef), Blob],
        GENERIC_PARAM_CONSTRAINT => &[Simple(GENERIC_PARAM), Coded(TypeDefOrRef)],
        _ => return None,
    })
}

/// Heap-index byte widths (2 or 4) from the `#~` `HeapSizes` byte.
struct Widths {
    str: usize,
    guid: usize,
    blob: usize,
}

/// The computed layout of every table in a parsed metadata image: row strides
/// and stream offsets, with the heap-index widths needed to read heap columns.
pub(crate) struct Tables<'a> {
    md: &'a MetadataFile<'a>,
    widths: Widths,
    /// Byte size of one row of each table (0 for absent tables).
    stride: [usize; TABLE_COUNT],
    /// Byte offset of each table's first row within `md.tables`.
    offset: [usize; TABLE_COUNT],
}

impl<'a> Tables<'a> {
    /// Compute the table layout for a parsed [`MetadataFile`].
    ///
    /// Refuses (`UnsupportedTableStream`) an image that marks an undefined table
    /// index (`0x2D..0x3F`) present: its row stride is unknown, so every later
    /// table's offset would be wrong — better to fail loudly than mis-locate.
    ///
    /// Refuses (`TableIndexOutOfRange`) a layout whose declared rows overrun the
    /// `#~` table region: a malformed stream can claim a huge row count its
    /// bytes can't back, and accepting it would let consumers trust that count
    /// (e.g. `Vec::with_capacity`) before any per-row bounds check fires. The
    /// bound also caps every `row_count` at `tables.len() / 2` (the minimum
    /// stride is one 2-byte column), so the count is always allocation-safe.
    pub(crate) fn new(md: &'a MetadataFile<'a>) -> Result<Tables<'a>, Error> {
        let widths = Widths {
            str: if md.heap_sizes.wide_strings { 4 } else { 2 },
            guid: if md.heap_sizes.wide_guid { 4 } else { 2 },
            blob: if md.heap_sizes.wide_blob { 4 } else { 2 },
        };

        let mut stride = [0usize; TABLE_COUNT];
        for (t, s) in stride.iter_mut().enumerate() {
            if md.rows[t] == 0 {
                continue;
            }
            let cols = schema(t).ok_or(Error::UnsupportedTableStream)?;
            *s = cols.iter().map(|c| c.width(&widths, &md.rows)).sum();
        }

        // Tables are laid out consecutively in table-index order; a table's
        // offset is the total byte size of every lower-indexed table.
        let mut offset = [0usize; TABLE_COUNT];
        let mut running = 0usize;
        for t in 0..TABLE_COUNT {
            offset[t] = running;
            let bytes = (md.rows[t] as usize)
                .checked_mul(stride[t])
                .ok_or(Error::TableIndexOutOfRange)?;
            running = running
                .checked_add(bytes)
                .ok_or(Error::TableIndexOutOfRange)?;
        }

        // The declared rows must fit the `#~` table region; an inflated count
        // whose bytes aren't present is a structural inconsistency, refused
        // rather than trusted (and later over-allocated against).
        if running > md.tables.len() {
            return Err(Error::TableIndexOutOfRange);
        }

        Ok(Tables {
            md,
            widths,
            stride,
            offset,
        })
    }

    /// Row count of `table`.
    pub(crate) fn row_count(&self, table: usize) -> u32 {
        self.md.rows[table]
    }

    /// Decode a coded value read from a column into its `(table, rid)` token.
    /// See [`Coded::decode`].
    pub(crate) fn decode_coded(
        &self,
        kind: Coded,
        value: u32,
    ) -> Result<Option<CodedToken>, Error> {
        kind.decode(value)
    }

    /// A reader over row `row` (0-based) of `table`, or `TableIndexOutOfRange` if
    /// the row falls outside the table or the stream.
    pub(crate) fn row(&self, table: usize, row: u32) -> Result<Row<'_, 'a>, Error> {
        if row >= self.md.rows[table] {
            return Err(Error::TableIndexOutOfRange);
        }
        let stride = self.stride[table];
        let start = self.offset[table]
            .checked_add(
                (row as usize)
                    .checked_mul(stride)
                    .ok_or(Error::TableIndexOutOfRange)?,
            )
            .ok_or(Error::TableIndexOutOfRange)?;
        let end = start
            .checked_add(stride)
            .ok_or(Error::TableIndexOutOfRange)?;
        let bytes = self
            .md
            .tables
            .get(start..end)
            .ok_or(Error::TableIndexOutOfRange)?;
        Ok(Row {
            tables: self,
            table,
            bytes,
        })
    }

    /// `(byte offset, width)` of column `col` within a row of `table`.
    fn col_span(&self, table: usize, col: usize) -> (usize, usize) {
        let cols = schema(table).expect("row() only built for defined tables");
        let mut off = 0;
        for c in &cols[..col] {
            off += c.width(&self.widths, &self.md.rows);
        }
        (off, cols[col].width(&self.widths, &self.md.rows))
    }
}

/// A borrowed view of one metadata-table row, addressable by column index.
pub(crate) struct Row<'t, 'a> {
    tables: &'t Tables<'a>,
    table: usize,
    bytes: &'a [u8],
}

impl<'a> Row<'_, 'a> {
    /// The unsigned little-endian value of column `col` (2- or 4-byte). Used for
    /// fixed-width constants, simple indices, coded indices, and raw heap
    /// indices. The row slice was already bounds-checked to the full stride, so
    /// the column always lies within it.
    fn uint(&self, col: usize) -> u32 {
        let (off, width) = self.tables.col_span(self.table, col);
        let mut value = 0u32;
        for (i, &b) in self.bytes[off..off + width].iter().enumerate() {
            value |= u32::from(b) << (8 * i);
        }
        value
    }

    /// Column `col` as a fixed-width integer (`U16`/`U32`).
    pub(crate) fn int(&self, col: usize) -> u32 {
        self.uint(col)
    }

    /// Column `col` as a raw coded-index value (tag in the low bits, RID above).
    /// A value of 0 is the null coded index.
    pub(crate) fn coded(&self, col: usize) -> u32 {
        self.uint(col)
    }

    /// Column `col` resolved through `#Strings`.
    pub(crate) fn string(&self, col: usize) -> Result<&'a str, Error> {
        self.tables.md.string_at(self.uint(col))
    }

    /// Column `col` resolved through `#Blob`. An index of 0 yields the empty blob.
    pub(crate) fn blob(&self, col: usize) -> Result<&'a [u8], Error> {
        self.tables.md.blob_at(self.uint(col))
    }
}

#[cfg(test)]
mod decode_tests {
    use super::{Coded, CodedToken, table};
    use crate::reader::Error;

    /// Only the canonical all-zero value is the null coded index (II.24.2.6).
    #[test]
    fn zero_is_null() {
        assert_eq!(Coded::HasCustomAttribute.decode(0), Ok(None));
        assert_eq!(Coded::ResolutionScope.decode(0), Ok(None));
        assert_eq!(Coded::CustomAttributeType.decode(0), Ok(None));
    }

    /// A nonzero value whose RID part is 0 points at row 0, which never exists
    /// (RIDs are 1-based). It is not the null encoding, so it is refused rather
    /// than silently treated as an absent reference.
    #[test]
    fn nonzero_with_zero_rid_is_refused() {
        // HasCustomAttribute has 5 tag bits; value 2 → tag 2, rid 0.
        assert_eq!(
            Coded::HasCustomAttribute.decode(2),
            Err(Error::TableIndexOutOfRange)
        );
        // ResolutionScope has 2 tag bits; value 1 → tag 1 (ModuleRef), rid 0.
        assert_eq!(
            Coded::ResolutionScope.decode(1),
            Err(Error::TableIndexOutOfRange)
        );
    }

    /// A well-formed value decodes to its table and 1-based RID.
    #[test]
    fn valid_token_decodes() {
        // HasCustomAttribute tag 3 = TypeDef; rid 1 → (1 << 5) | 3.
        assert_eq!(
            Coded::HasCustomAttribute.decode((1 << 5) | 3),
            Ok(Some(CodedToken {
                table: table::TYPE_DEF,
                rid: 1,
            }))
        );
    }

    /// A tag selecting a reserved slot (CustomAttributeType tags 0/1/4) is
    /// refused even with a nonzero RID.
    #[test]
    fn reserved_tag_is_refused() {
        // CustomAttributeType has 3 tag bits; value (1 << 3) | 0 → tag 0
        // (reserved), rid 1.
        assert_eq!(
            Coded::CustomAttributeType.decode(1 << 3),
            Err(Error::TableIndexOutOfRange)
        );
    }
}
