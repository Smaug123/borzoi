//! Stage 7.0: the owned [`Image`] and the [`parse`] entry point.
//!
//! [`parse`] is the whole new reader assembled into one value: it runs the
//! stage-1 container read, the stage-3 manifest reads (assembly identity,
//! references, resources), and the stage-4–6 type/member/attribute walk, then
//! gathers their owned results into an [`Image`]. It borrows nothing of the
//! input buffer.
//!
//! It is **total over structurally-valid metadata**: it fails only when the
//! PE/CLI/metadata structure itself is malformed (the structural [`Error`]
//! variants). Per-item signature and custom-attribute failures are stored as
//! `Result`s inside the data, never raised here.
//!
//! `Image`/`parse` are `pub(crate)` for now: the crate's consumer-facing API is
//! the `EcmaView`/`Entity` projection, which later stages re-home onto this
//! `Image`. Promoting them to the public API is a deliberate later choice.

use super::Error;
use super::ids::TypeDefId;
use super::manifest::{
    AssemblyIdentity, ManifestResource, RawTypeForwarder, read_assembly, read_assembly_refs,
    read_resources, read_type_forwarders,
};
use super::metadata::MetadataFile;
use super::model::{MemberRef, RawAttribute, TypeDef, TypeRef};
use super::tables::Tables;
use super::typedefs::read_types;

/// The fully-owned result of reading an assembly: identity, external references,
/// the type/member arena, the `MemberRef` arena, and embedded resources. Every
/// cross-reference is a resolved handle into one of these flat `Vec`s (see
/// [`super::ids`]); nothing borrows the input bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Image {
    /// The `Assembly` row, or `None` for a module with no assembly manifest.
    pub(crate) assembly: Option<AssemblyIdentity>,
    /// Raw custom attributes on the `Assembly` row (assembly-scoped markers).
    pub(crate) assembly_attributes: Vec<RawAttribute>,
    /// `AssemblyRef` rows, in table order (an [`super::ids::AssemblyRefId`]
    /// indexes this directly).
    pub(crate) references: Vec<AssemblyIdentity>,
    /// The flat `TypeDef` arena, in table order (a [`TypeDefId`] is its index),
    /// including nested types and the `<Module>` pseudo-type.
    pub(crate) type_defs: Vec<TypeDef>,
    /// The top-level types (those with no enclosing type) — the iterable a
    /// consumer walks.
    pub(crate) top_level: Vec<TypeDefId>,
    /// The `TypeRef` arena, in table order.
    pub(crate) type_refs: Vec<TypeRef>,
    /// The `MemberRef` arena, in table order (needed to decode custom-attribute
    /// constructor signatures).
    pub(crate) member_refs: Vec<MemberRef>,
    /// Manifest resources embedded in this file (`CurrentFile` implementation).
    pub(crate) resources: Vec<ManifestResource>,
    /// Forwarder `ExportedType` rows — the facade-assembly redirects.
    pub(crate) type_forwarders: Vec<RawTypeForwarder>,
}

/// Read an assembly from its PE bytes into an owned [`Image`].
pub(crate) fn parse(bytes: &[u8]) -> Result<Image, Error> {
    let md = MetadataFile::read(bytes)?;
    let tables = Tables::new(&md)?;

    let assembly = read_assembly(&tables)?;
    let references = read_assembly_refs(&tables)?;
    let resources = read_resources(&tables, &md)?;
    let type_forwarders = read_type_forwarders(&tables)?;
    // `read_types` builds its own `Tables` (cheap — strides/offsets only) and
    // owns the type/member/attribute walk, returning the assembly-row attributes
    // alongside the type slice.
    let types = read_types(&md)?;

    Ok(Image {
        assembly,
        assembly_attributes: types.assembly_attributes,
        references,
        type_defs: types.type_defs,
        top_level: types.top_level,
        type_refs: types.type_refs,
        member_refs: types.member_refs,
        resources,
        type_forwarders,
    })
}
