//! Reads symbol information out of a managed assembly (`.dll`/`.exe`).
//!
//! An F#-flavoured view of "what's in a referenced DLL": types and their
//! members, presented through an owned data model rather than the raw
//! ECMA-335 tables that back it.
//!
//! The crate is intentionally standalone — its only runtime dependency is
//! `sha1` (for PublicKeyToken derivation); ECMA-335 parsing is done by an
//! in-crate reader. Consumers (the LSP, other tooling) take this crate as a
//! dependency rather than pulling in a Roslyn or FCS-shaped reader.
//!
//! ## Custom modifiers are checked as a property, not by inspection
//!
//! ECMA-335 II.7.1.1's `modopt`/`modreq` rule is a statement about two
//! projections of the same program, so `modifier_metamorphic` (test-support
//! feature) checks it as
//! one: decorate every signature node in a real assembly with an unrecognised
//! modifier and re-project. An ignorable `modopt` must move nothing; an
//! unrecognised `modreq` must leave no member standing. Anything in the
//! projector that inspects the *head* of a signature (`matches!(sig,
//! TypeSig::ByRef(_))`, …) stops firing when a modifier sits in front of it,
//! and that class of bug is invisible to the compiler — a new wrapper variant
//! leaves every such guard well-typed. Read that module before adding one.

pub mod display;
pub mod doc_id;
mod ecma335_assembly;
mod error;
pub mod fsharp_pickle;
mod fsharp_pickle_merge;
mod fsharp_resource;
mod model;
#[cfg(feature = "test-support")]
pub mod modifier_metamorphic;
pub mod pdb;
mod reader;
#[cfg(feature = "test-support")]
pub mod test_support;
mod view;

pub use display::{
    TyparScope, format_entity_header, format_member, format_nullable_type, format_type,
    fsharp_alias,
};
// The public ECMA-335 assembly view, backed by the in-crate reader.
pub use ecma335_assembly::Ecma335Assembly;
pub use error::ImportError;
pub use fsharp_pickle::{CcuRef, PickledCcu, PickledHeader, PickledNleRef, unpickle_signature};
pub use fsharp_pickle_merge::{ModuleMemberTarget, ModuleMemberVal, collect_module_member_targets};
pub use model::{
    AbbreviationTarget, Access, AssemblyIdentity, AssemblyProjectionSkips, Augmentation,
    CompilerFeatureRequired, ConstantValue, CustomAttr, DefaultMember, Entity, EntityKind, Event,
    Experimental, Field, FsharpOverlayKind, ImplementedMember, IndexParameter, InterfaceMemberImpl,
    Member, MethodLike, MethodSignature, ModuleValue, Nullability, NullableType, Obsolete,
    ParamDefault, Parameter, Primitive, Property, SkippedFsharpOverlay, SkippedMember,
    SkippedProjectionItem, TypeParameter, TypeRef, UnclassifiedMethodImpl, Variance, Version,
};
pub use view::{EcmaView, FSharpResource, ResourceKind};
