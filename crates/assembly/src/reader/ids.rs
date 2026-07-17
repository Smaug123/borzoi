//! Resolved-handle newtypes: indices into the `Image`'s flat arenas.
//!
//! Every cross-reference in the owned model is one of these — never a raw
//! ECMA-335 coded token, never a borrow. The compiler tracks which arena a
//! handle indexes, so a `TypeDefId` can never be mistaken for a `TypeRefId`.

/// Index into `Image.type_defs`. Built in TypeDef-table order, so a
/// `TypeDefOrRef` coded token with RID `r` resolves to `TypeDefId(r - 1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TypeDefId(pub(crate) u32);

/// Index into `Image.type_refs`, in TypeRef-table order (RID `r` → `r - 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TypeRefId(pub(crate) u32);

/// Index into `Image.references` (the `AssemblyRef` rows), in AssemblyRef-table
/// order (RID `r` → `r - 1`). The referenced identity is filled in by the
/// assembly-identity stage; this handle is valid as soon as that arena is built
/// in table order, so the type-def stage can mint it without that data present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AssemblyRefId(pub(crate) u32);

/// Index into the owning `TypeDef`'s `methods`, in `MethodList` order (the same
/// order the member stage materialises them). Derived here from the `MethodDef`
/// RID via the `TypeDef.MethodList` ranges, so an attribute whose constructor is
/// a `MethodDef` defined in this image resolves to `(TypeDefId, MethodId)`
/// without the method bodies being projected yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MethodId(pub(crate) u32);

/// Index into `Image.member_refs` (the `MemberRef` rows), in MemberRef-table
/// order (RID `r` → `r - 1`). Like [`AssemblyRefId`], the handle is valid as
/// soon as that arena is built in table order; the custom-attribute stage fills
/// in the referenced member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MemberRefId(pub(crate) u32);
