//! Assembly identity, external references, and manifest resources — the
//! manifest-level projection.
//!
//! Three reads on top of the [`Tables`] layout:
//! - [`read_assembly`] projects the single `Assembly` row (II.22.2);
//! - [`read_assembly_refs`] projects every `AssemblyRef` row (II.22.5);
//! - [`read_resources`] projects every `ManifestResource` row (II.22.24),
//!   extracting the bytes of file-embedded resources and refusing any resource
//!   that lives in another file or assembly.
//!
//! All three are pure functions of the parsed metadata; later stages assemble
//! their results onto the owned `Image`.

use super::Error;
use super::metadata::MetadataFile;
use super::tables::{Tables, table};

/// ECMA-335 `assemblyFlags` bit 0 (`afPublicKey`): the `PublicKey`/
/// `PublicKeyOrToken` blob is the full (unhashed) public key rather than an
/// 8-byte `PublicKeyToken`.
const FLAG_PUBLIC_KEY: u32 = 0x0001;

/// `Major.Minor.Build.Revision` — the .NET assembly version quadruple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Version {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

/// Identity of a managed assembly, as recorded by the `Assembly` or
/// `AssemblyRef` row it came from.
///
/// `public_key` is the raw blob verbatim — possibly empty (unsigned), the full
/// public key, or an 8-byte `PublicKeyToken`. `has_full_key` is the row's
/// `afPublicKey` flag, distinguishing the full-key case from a token (or none);
/// deriving the token from a full key is left to a consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssemblyIdentity {
    pub name: String,
    pub version: Version,
    pub public_key: Vec<u8>,
    pub has_full_key: bool,
}

/// A manifest resource embedded in this file (`CurrentFile` implementation).
/// Resources implemented in another file or assembly are refused, never
/// represented (see [`read_resources`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestResource {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Project the `Assembly` row (II.22.2), or `None` for a module with no
/// `Assembly` record. ECMA-335 permits at most one row; any beyond the first is
/// ignored.
pub(crate) fn read_assembly(tables: &Tables) -> Result<Option<AssemblyIdentity>, Error> {
    if tables.row_count(table::ASSEMBLY) == 0 {
        return Ok(None);
    }
    // Columns: HashAlgId(0), Major(1), Minor(2), Build(3), Revision(4),
    // Flags(5), PublicKey(6), Name(7), Culture(8).
    let row = tables.row(table::ASSEMBLY, 0)?;
    let version = Version {
        major: row.int(1) as u16,
        minor: row.int(2) as u16,
        build: row.int(3) as u16,
        revision: row.int(4) as u16,
    };
    let flags = row.int(5);
    let public_key = row.blob(6)?.to_vec();
    let name = row.string(7)?.to_string();
    Ok(Some(AssemblyIdentity {
        name,
        version,
        public_key,
        has_full_key: flags & FLAG_PUBLIC_KEY != 0,
    }))
}

/// Project every `AssemblyRef` row (II.22.5), in table order.
pub(crate) fn read_assembly_refs(tables: &Tables) -> Result<Vec<AssemblyIdentity>, Error> {
    let count = tables.row_count(table::ASSEMBLY_REF);
    let mut refs = Vec::with_capacity(count as usize);
    for r in 0..count {
        // Columns: Major(0), Minor(1), Build(2), Revision(3), Flags(4),
        // PublicKeyOrToken(5), Name(6), Culture(7), HashValue(8).
        let row = tables.row(table::ASSEMBLY_REF, r)?;
        let version = Version {
            major: row.int(0) as u16,
            minor: row.int(1) as u16,
            build: row.int(2) as u16,
            revision: row.int(3) as u16,
        };
        let flags = row.int(4);
        let public_key = row.blob(5)?.to_vec();
        let name = row.string(6)?.to_string();
        refs.push(AssemblyIdentity {
            name,
            version,
            public_key,
            has_full_key: flags & FLAG_PUBLIC_KEY != 0,
        });
    }
    Ok(refs)
}

/// Project every `ManifestResource` row (II.22.24).
///
/// A null `Implementation` coded index means the resource is embedded in this
/// file; its bytes are extracted from the CLI `Resources` blob at the row's
/// `Offset`. A non-null `Implementation` names another file or assembly, which
/// this reader refuses with [`Error::UnsupportedResourceImplementation`] rather
/// than fabricating bytes it does not have.
pub(crate) fn read_resources(
    tables: &Tables,
    md: &MetadataFile,
) -> Result<Vec<ManifestResource>, Error> {
    let count = tables.row_count(table::MANIFEST_RESOURCE);
    let mut out = Vec::with_capacity(count as usize);
    for r in 0..count {
        // Columns: Offset(0), Flags(1), Name(2), Implementation(3).
        let row = tables.row(table::MANIFEST_RESOURCE, r)?;
        let offset = row.int(0);
        let name = row.string(2)?.to_string();
        let implementation = row.coded(3);
        if implementation != 0 {
            return Err(Error::UnsupportedResourceImplementation);
        }
        let bytes = extract_embedded_resource(md, offset)?.to_vec();
        out.push(ManifestResource { name, bytes });
    }
    Ok(out)
}

/// The bytes of a file-embedded resource at `offset` within the CLI `Resources`
/// blob (II.25.3.3): the directory is `[resources_rva, +resources_size)`, each
/// entry is a 4-byte little-endian length followed by that many bytes, and the
/// `Offset` is relative to the directory start. Any escape from the declared
/// region — an unbacked RVA, a short section, or a length/offset past the
/// region — is a structural inconsistency, refused with
/// [`Error::ResourceDataOutOfRange`].
fn extract_embedded_resource<'a>(md: &MetadataFile<'a>, offset: u32) -> Result<&'a [u8], Error> {
    let oor = Error::ResourceDataOutOfRange;
    let region = md.rva_to_slice(md.resources_rva).ok_or(oor)?;
    let region = region.get(..md.resources_size as usize).ok_or(oor)?;
    let entry = region.get(offset as usize..).ok_or(oor)?;
    let len_bytes: [u8; 4] = entry.get(0..4).ok_or(oor)?.try_into().unwrap();
    let len = u32::from_le_bytes(len_bytes) as usize;
    let end = len.checked_add(4).ok_or(oor)?;
    entry.get(4..end).ok_or(oor)
}
