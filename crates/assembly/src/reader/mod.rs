//! A bespoke ECMA-335 reader, built incrementally.
//!
//! Stage 1 lands the structural container: PE → CLI header → metadata root →
//! the `#~`/`#Strings`/`#US`/`#Blob`/`#GUID` heaps and the `#~` table directory
//! (row counts). No table rows are projected yet; later stages build the owned
//! `Image` on top of this foundation.

mod attributes;
mod cursor;
mod ids;
mod image;
mod manifest;
mod members;
mod metadata;
mod model;
mod signature;
mod tables;
mod typedefs;

#[cfg(test)]
mod attributes_tests;
#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod manifest_tests;
#[cfg(test)]
mod members_tests;
#[cfg(test)]
mod signature_tests;
#[cfg(test)]
mod test_fixtures;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod typedefs_tests;

// The new reader's public-in-crate surface: the owned `Image`, the `parse`
// entry point, and the raw manifest identity the projector (`Ecma335Assembly`) maps
// onto the `Entity`-level `model::AssemblyIdentity`.
pub(crate) use image::{Image, parse};
pub(crate) use manifest::AssemblyIdentity;
// The custom-attribute value model `Image::decode_attribute` produces, which the
// projector classifies into `Entity`-level facts.
pub(crate) use attributes::{DecodedAttribute, EnumWidths, FixedArg, IntegralParam, IntegralWidth};
// The raw type/member model the projector reads to build the `Entity` tree.
pub(crate) use ids::{AssemblyRefId, MethodId, TypeDefId, TypeRefId};
pub(crate) use model::{
    AccessDefect, Accessibility, AccessorOwner, Constant, DeclSemantics, Event, Field,
    GenericParam, MemberAccess, Method, MethodSig, Param, Property, RawAttribute, RefScope,
    TypeDef, TypeName, Variance,
};
// The signature-carrying model corners only the modifier metamorphic probe
// walks (`crate::modifier_metamorphic`): it must reach *every* `TypeSig` in the
// image, so it names every struct that holds one.
#[cfg(test)]
pub(crate) use model::MemberRefParent;
#[cfg(feature = "test-support")]
pub(crate) use model::{InterfaceMemberImpl, MemberRef, UnclassifiedImpl};
#[cfg(feature = "test-support")]
pub(crate) use signature::DecodedMethodSig;
pub(crate) use signature::{
    CallConv, CustomMod, ModifiedType, NamedKind, Primitive, RetType, SigError, TypeScope, TypeSig,
};
// The bounds-checked byte cursor, PE section map, and RVA resolver, reused by
// the sibling `pdb` reader (which walks the same PE container to reach the
// debug directory).
pub(crate) use cursor::Cursor;
pub(crate) use metadata::{Section, rva_to_slice};

// The fixture corpus is shared with the projector's differential tests.
#[cfg(test)]
pub(crate) use test_fixtures::all_dlls;
// Model pieces the projector's tests construct by hand to exercise paths the
// corpus cannot reach (e.g. a constraint row carrying a custom attribute).
#[cfg(test)]
pub(crate) use ids::MemberRefId;
#[cfg(test)]
pub(crate) use model::MemberHandle;
// `TypeConstraint` is also reached by the metamorphic probe (below), which is
// feature-gated rather than `cfg(test)`; one export serves both.
#[cfg(any(test, feature = "test-support"))]
pub(crate) use model::TypeConstraint;

use std::fmt;

/// A structural failure decoding the PE/CLI/metadata container.
///
/// These are the *only* hard errors `parse` raises: per-item signature and
/// custom-attribute decode failures are stored as `Result`s in the data model,
/// never propagated, so one unreadable member cannot sink a whole assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a PE/COFF image (missing `MZ`/`PE` signature or a malformed header).
    NotPortableExecutable,
    /// The image declares **no** CLI (COM descriptor) data directory: its RVA
    /// is zero, or the optional header is too short to carry the entry. Such a
    /// file is not a managed assembly at all — a native DLL or EXE — and every
    /// ECMA-335 reader refuses it, so this is a statement about the input
    /// rather than a limitation of this one. Distinct from
    /// [`Self::UnreadableCliHeader`] for exactly that reason.
    NoCliHeader,
    /// The image *does* declare a CLI header, but this reader could not follow
    /// it: the RVA maps into no section, or the header is truncated within its
    /// section. Unlike [`Self::NoCliHeader`] this says nothing about the input
    /// being unmanaged — a managed assembly whose RVA mapping we get wrong
    /// lands here, which is a reader gap and must never be mistaken for a
    /// native file.
    UnreadableCliHeader,
    /// The metadata root is malformed (bad `BSJB` signature, truncated stream
    /// headers, or an out-of-range stream region).
    BadMetadataRoot,
    /// A required metadata heap is absent. Carries the stream name (`#~`,
    /// `#Strings`, `#Blob`, `#US`, `#GUID`).
    MissingHeap(&'static str),
    /// A heap offset (string/blob/GUID) fell outside its heap.
    HeapOffsetOutOfRange,
    /// A metadata table index referred to a row outside the table, or the `#~`
    /// stream declared more table rows than its byte region can hold.
    TableIndexOutOfRange,
    /// The `#~` table stream uses a layout this reader does not handle. Two
    /// cases: the `HeapSizes` `ExtraData` flag (`0x40`, EnC/unoptimized metadata)
    /// inserts a 4-byte field between the row counts and the table rows, so the
    /// rows cannot be located without skipping it; or a `*Ptr` indirection table
    /// (`FieldPtr`/`MethodPtr`/`ParamPtr`/`PropertyPtr`/`EventPtr`, present only
    /// in unoptimized metadata) is populated, which breaks the assumption that a
    /// member's RID is its physical position. Either is refused rather than
    /// mis-decoded.
    UnsupportedTableStream,
    /// A manifest resource was not embedded in the current file.
    UnsupportedResourceImplementation,
    /// An embedded manifest resource's bytes fell outside the CLI `Resources`
    /// data directory (unbacked RVA, short section, or a length/offset past the
    /// declared region).
    ResourceDataOutOfRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotPortableExecutable => write!(f, "not a portable executable"),
            Error::NoCliHeader => write!(f, "no CLI header"),
            Error::UnreadableCliHeader => write!(f, "declared CLI header is unreadable"),
            Error::BadMetadataRoot => write!(f, "malformed metadata root"),
            Error::MissingHeap(name) => write!(f, "missing metadata heap {name}"),
            Error::HeapOffsetOutOfRange => write!(f, "heap offset out of range"),
            Error::TableIndexOutOfRange => write!(f, "metadata table index out of range"),
            Error::UnsupportedTableStream => write!(f, "unsupported #~ table stream layout"),
            Error::UnsupportedResourceImplementation => {
                write!(f, "manifest resource is not embedded in this file")
            }
            Error::ResourceDataOutOfRange => write!(f, "manifest resource data out of range"),
        }
    }
}

impl std::error::Error for Error {}
