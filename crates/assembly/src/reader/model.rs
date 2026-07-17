//! The owned type-definition and member model.
//!
//! Plain immutable data — no borrows of the input buffer, no lifetimes, every
//! cross-reference a resolved handle (see [`super::ids`]). Signature-typed
//! fields reuse the stage-2 [`TypeSig`]/[`SigError`] core and are stored as
//! `Result`, so one unreadable signature is localized rather than sinking the
//! whole type.
//!
//! Stage 4 built the type structure; stage 5 fills in the members
//! ([`Method`]/[`Field`]/[`Property`]/[`Event`]) and the per-position custom
//! attributes (parameter, return, and `GenericParam` rows) consumers later
//! classify. The handles minted here — `MethodId` via the `MethodList` ranges,
//! `MemberRefId`/`AssemblyRefId` as table-order indices — index arenas built in
//! table order.

use super::ids::{AssemblyRefId, MemberRefId, MethodId, TypeDefId, TypeRefId};
use super::signature::{CallConv, DecodedMethodSig, ModifiedType, RetType, SigError};

/// A type's namespace and (arity-suffixed) name, exactly as stored in
/// `#Strings`. The arity backtick (`Dictionary`2`) is preserved; callers strip
/// it if they want the source-level name.
///
/// `Hash` is derived so a `TypeName` can key the caller-supplied enum-width map
/// the custom-attribute decoder consumes (stage 6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TypeName {
    pub(crate) namespace: String,
    pub(crate) name: String,
}

/// Where a [`TypeRef`] resolves. `ModuleRef` (multi-module assemblies) and a
/// null scope (`ExportedType` lookup) are refused at parse time rather than
/// represented (see [`super::Error::UnsupportedTypeRefScope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefScope {
    /// Defined in another assembly (`AssemblyRef` resolution scope).
    AssemblyRef(AssemblyRefId),
    /// A `TypeRef` nested inside another `TypeRef` (the enclosing type is the
    /// resolution scope).
    Nested(TypeRefId),
    /// The current module (`Module` resolution scope) — an alias for a type
    /// defined in *this* image. F# emits these freely: a record/union's own
    /// `IComparable<T>`/`IEquatable<T>` interface arguments reference the type
    /// through a module-self `TypeRef` rather than its `TypeDef` token. It is
    /// recorded faithfully here (it carries no assembly attribution to get
    /// wrong); resolving it back to the aliased `TypeDefId` is a later
    /// name-resolution concern.
    Module,
}

/// A reference to a type defined elsewhere (the `TypeRef` table). `scope` walks
/// out to the assembly or enclosing type that anchors the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeRef {
    pub(crate) name: TypeName,
    pub(crate) scope: RefScope,
}

/// Folded ECMA-335 §II.23.1.15 type/member visibility. The seven-valued raw
/// field's `CompilerControlled`/privatescope (0) value has no variant — for a
/// *type* the value-0 visibility is `NotPublic`, which folds onto `Assembly`,
/// so this is total over the type-visibility field. (Member-level
/// `CompilerControlled` is stored per member as an [`AccessDefect`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Accessibility {
    Private,
    FamAndAssem,
    Assembly,
    Family,
    FamOrAssem,
    Public,
}

/// Generic-parameter variance (§II.23.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variance {
    Invariant,
    Covariant,
    Contravariant,
}

/// One formal generic parameter of a type or method (the `GenericParam` rows
/// the owner declares, in `Number` order).
///
/// `attributes` carries the per-`GenericParam` custom attributes consumers
/// classify (the nullable-reference and `unmanaged` markers), captured in
/// stage 5 alongside the parameter/return positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericParam {
    pub(crate) name: String,
    pub(crate) variance: Variance,
    /// `class` constraint (reference-type special constraint).
    pub(crate) reference_type: bool,
    /// `struct` constraint (non-nullable value-type special constraint).
    pub(crate) value_type: bool,
    /// `new()` constraint (default-constructor special constraint).
    pub(crate) default_ctor: bool,
    /// `allows ref struct` anti-constraint — the `AllowByRefLike` flag
    /// (`0x0020`) on the `GenericParam` row (ECMA-335 §II.23.1.7).
    pub(crate) allows_ref_struct: bool,
    /// Typed constraints (`GenericParamConstraint` rows), each a
    /// `TypeDefOrRef`-or-`TypeSpec` decoded through the stage-2 core plus the
    /// row's own custom attributes.
    pub(crate) constraints: Vec<TypeConstraint>,
    pub(crate) attributes: Vec<RawAttribute>,
}

/// One typed generic constraint: a `GenericParamConstraint` row's decoded
/// `Constraint` type plus the row's own custom attributes.
///
/// The attributes are carried (not dropped) so consumers can decide whether a
/// constraint they would otherwise consume — the synthetic `System.ValueType
/// modreq(UnmanagedType)` row the `unmanaged` refinement emits — is genuinely
/// the canonical marker or hand-authored metadata that must be refused. Real
/// compilers leave constraint rows un-attributed; surfacing them lets the
/// projector fail loud rather than silently discard an attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeConstraint {
    pub(crate) ty: Result<ModifiedType, SigError>,
    pub(crate) attributes: Vec<RawAttribute>,
}

/// A type definition (a `TypeDef` row) and its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeDef {
    pub(crate) name: TypeName,
    pub(crate) accessibility: Accessibility,
    pub(crate) is_interface: bool,
    /// `sealed` (`TypeAttributes` 0x0100): the type cannot be inherited from.
    /// Consumed by overload resolution (OV-2): a sealed non-interface parameter
    /// type admits no proper subtype, so the applicability `may_apply` refuter
    /// can decide subsumption channels for it.
    pub(crate) is_sealed: bool,
    /// The base type. `None` for `System.Object` and interfaces (a null
    /// `Extends`); otherwise the decoded `TypeDefOrRef`-or-`TypeSpec`.
    pub(crate) extends: Option<Result<ModifiedType, SigError>>,
    /// Implemented interfaces (`InterfaceImpl` rows for this type).
    pub(crate) implements: Vec<Result<ModifiedType, SigError>>,
    pub(crate) generic_params: Vec<GenericParam>,
    /// The enclosing type (`NestedClass` linkage), or `None` for a top-level
    /// type.
    pub(crate) enclosing: Option<TypeDefId>,
    /// Directly-nested types (the inverse of `enclosing`).
    pub(crate) nested: Vec<TypeDefId>,
    /// Methods owned by this type, in `MethodList` (RID) order — so a
    /// [`MethodId`] is a direct index into this `Vec`.
    pub(crate) methods: Vec<Method>,
    /// Fields owned by this type, in `FieldList` (RID) order.
    pub(crate) fields: Vec<Field>,
    /// Properties owned by this type (via `PropertyMap`), in `Property`-table
    /// order.
    pub(crate) properties: Vec<Property>,
    /// Events owned by this type (via `EventMap`), in `Event`-table order.
    pub(crate) events: Vec<Event>,
    /// Raw custom attributes on this `TypeDef` row, captured here so later
    /// stages need not revisit the `CustomAttribute` table walk.
    pub(crate) attributes: Vec<RawAttribute>,
}

/// Folded ECMA-335 §II.23.1.10 member visibility for a method or field. Unlike
/// the type-level fold, value 0 here is `CompilerControlled` (privatescope),
/// which has no variant: the fold stores an [`AccessDefect`] instead (see
/// [`Method::accessibility`]), like a per-member [`SigError`]; the six values
/// below are 1..=6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberAccess {
    Private,
    FamAndAssem,
    Assembly,
    Family,
    FamOrAssem,
    Public,
}

/// A §II.23.1.10 member-visibility value the model has no variant for. Stored
/// per member (mirroring [`SigError`] in [`Method::signature`]) rather than
/// aborting the image: the projector drops the one member and records it on
/// `Entity::skipped_members`, so a stray C++/CLI or hand-assembled member in a
/// big interop DLL costs that member, not the whole assembly. Mapping either
/// value onto [`MemberAccess::Private`] would fabricate an accessibility the
/// metadata does not state — refused instead (D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessDefect {
    /// Value 0 — `CompilerControlled` (privatescope): referenceable only from
    /// within the defining module, by token; emitted by C++/CLI and hand-written
    /// IL, never by C#/F#.
    CompilerControlled,
    /// Value 7 — reserved by ECMA-335.
    Reserved,
}

impl std::fmt::Display for AccessDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessDefect::CompilerControlled => {
                write!(f, "compilercontrolled (privatescope) member visibility")
            }
            AccessDefect::Reserved => write!(f, "reserved member-visibility value 7"),
        }
    }
}

/// One method parameter: its signature type paired with the `Param`-table
/// metadata consumers depend on (names for hover, `in`/`out`/optional/default
/// for signature rendering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Param {
    /// The declared name, or `None` for a parameter with no `Param` row (or an
    /// unnamed one).
    pub(crate) name: Option<String>,
    /// The parameter type, with the custom-modifier run its position carries; a
    /// byref parameter is preserved as [`TypeSig::ByRef`].
    pub(crate) ty: ModifiedType,
    /// `[In]` (`Param` flag 0x0001).
    pub(crate) is_in: bool,
    /// `[Out]` (`Param` flag 0x0002).
    pub(crate) is_out: bool,
    /// `[Optional]` (`Param` flag 0x0010).
    pub(crate) optional: bool,
    /// The decoded `Constant` default value for this parameter, when it owns a
    /// `Constant` row (`Some`) — a C# `x = <value>`.
    pub(crate) default_value: Option<Constant>,
    /// Per-parameter custom attributes, raw. Consumers derive e.g.
    /// `[ParamArray]` and the nullable-reference bytes from these.
    pub(crate) attributes: Vec<RawAttribute>,
}

/// A decoded ECMA-335 `Constant` value (II.22.9) — a default-parameter value.
/// Integer widths collapse to [`Self::Int`] / [`Self::UInt`]; floats are kept as
/// raw bits so the type is `Eq`. `char`/string values are kept as raw UTF-16
/// code units — they may legally hold unpaired surrogates (valid CLI metadata,
/// but not a Rust `char`/`String`), so the faithful reader preserves them
/// losslessly and the projector renders them down. Mirrors the projected
/// [`crate::model::ConstantValue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Constant {
    Bool(bool),
    /// A UTF-16 code unit, kept verbatim (may be an unpaired surrogate).
    Char(u16),
    Int(i64),
    UInt(u64),
    /// `R4` as IEEE-754 bits.
    F32(u32),
    /// `R8` as IEEE-754 bits.
    F64(u64),
    /// Raw UTF-16 code units, kept verbatim (may contain unpaired surrogates).
    Str(Vec<u16>),
    Null,
}

/// A method's decoded signature: the `MethodDefSig` blob ([`CallConv`],
/// [`RetType`], `has_this`) augmented with the `Param`-table metadata for each
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MethodSig {
    pub(crate) has_this: bool,
    pub(crate) explicit_this: bool,
    pub(crate) calling_convention: CallConv,
    pub(crate) return_type: RetType,
    /// Return-position (`Param` sequence 0) custom attributes, raw — the home of
    /// the return type's nullable-reference byte, among others.
    pub(crate) return_attributes: Vec<RawAttribute>,
    pub(crate) parameters: Vec<Param>,
}

/// A method definition (a `MethodDef` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Method {
    /// The `MethodDef` metadata token (`0x0600_0000 | rid`), carried so the
    /// projection can correlate the method back to its metadata row (e.g. to
    /// index a portable PDB's `MethodDebugInformation`).
    pub(crate) token: u32,
    pub(crate) name: String,
    /// The folded member visibility, or a stored [`AccessDefect`] for the two
    /// §II.23.1.10 values (privatescope / reserved) the model cannot express —
    /// per-member, like [`Self::signature`], so one such member never sinks the
    /// image.
    pub(crate) accessibility: Result<MemberAccess, AccessDefect>,
    pub(crate) is_static: bool,
    pub(crate) is_abstract: bool,
    pub(crate) is_virtual: bool,
    pub(crate) is_final: bool,
    /// `newslot` (`MethodAttributes` 0x0100): the method gets a *new* vtable
    /// slot rather than reusing (overriding) a base slot. Distinguishes a fresh
    /// virtual from an override — the F# override-dedup rule (OV-3) exempts a
    /// newslot re-declaration from being treated as an override of a base
    /// virtual.
    pub(crate) is_new_slot: bool,
    /// `hidebysig` (`MethodAttributes` 0x0080): name-hiding is by full signature
    /// (the C#/F# default) rather than by name alone. Part of the overload
    /// method-group hiding semantics (OV-3).
    pub(crate) is_hide_by_sig: bool,
    /// `rtspecialname` (`MethodAttributes` 0x1000). ECMA-335 §II.22.26 requires
    /// it on a constructor (a `.ctor`/`.cctor` name alone is reserved but not
    /// sufficient), so consumers pair it with the name to recognise one.
    pub(crate) is_rt_special_name: bool,
    /// Method-owned generic parameters (the `GenericParam` rows whose owner is
    /// this `MethodDef`), in `Number` order.
    pub(crate) generic_params: Vec<GenericParam>,
    /// The decoded signature, or a stored [`SigError`] if it falls outside the
    /// supported subset (a function-pointer parameter, `modopt`, …).
    pub(crate) signature: Result<MethodSig, SigError>,
    pub(crate) attributes: Vec<RawAttribute>,
    /// The interface members this method implements: one entry per ECMA-335
    /// `MethodImpl` row that names this method as its `MethodBody` and
    /// declares an *interface* member (an explicit impl for an instance
    /// method; explicit *or* implicit for a static interface member, which is
    /// always wired through `MethodImpl`) — expanded per declaration *owner*
    /// when the declaration is an accessor several properties/events claim.
    /// Each entry carries the implemented interface (the `MethodDeclaration`'s
    /// parent, decoded to a [`TypeSig`], or a stored [`SigError`] if its
    /// `TypeSpec` blob is unreadable) and the bare interface-member name. One
    /// body may satisfy several interface members (one `MethodImpl` row each —
    /// VB's `Implements IFoo.M, IBar.M`), hence a list, in `MethodImpl`-table
    /// order. Filled by a post-pass over the `MethodImpl` table after the
    /// per-type member runs are built; empty for an ordinary method.
    pub(crate) implements: Vec<InterfaceMemberImpl>,
    /// `MethodImpl` rows naming this method as their body whose declaration
    /// parent is *undecidable from this image alone*: a `Reference`-scoped
    /// type that is neither in the implementing type's in-module interface
    /// closure nor a provable ancestor (an `Extends` target along the
    /// walkable chain). Ordinary F#/VB output lands here — a member of an
    /// *inherited external* interface implemented through the derived
    /// interface's clause lists only the derived interface in
    /// `InterfaceImpl` while declaring against the base — and so does a C#
    /// covariant-return override targeting a *non-direct* external ancestor
    /// (Roslyn points the declaration at the original declarer). The two are
    /// in-image identical; deciding needs the referenced assembly, so the
    /// rows are surfaced raw rather than dropped or guessed at.
    pub(crate) unclassified_impls: Vec<UnclassifiedImpl>,
}

/// One undecidable `MethodImpl` row: the declaration's parent type and the
/// declaration method's raw `Name`. See [`Method::unclassified_impls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnclassifiedImpl {
    /// The declaration parent, decoded (it decoded successfully, or the row
    /// would have been structurally skipped).
    pub(crate) parent: ModifiedType,
    /// The declaration method's raw `Name`, verbatim (its `MethodSemantics`
    /// is as unreachable as its parent's kind).
    pub(crate) member: String,
}

/// One implemented interface member recovered from a `MethodImpl` row: the
/// implemented interface, the declaration method's name, and what that method
/// *is*. See [`Method::implements`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterfaceMemberImpl {
    /// The implemented interface (the `MethodDeclaration` parent), or a stored
    /// [`SigError`] when its `TypeSpec` blob falls outside the supported subset.
    pub(crate) interface: Result<ModifiedType, SigError>,
    /// The declaration method's raw `Name` (`get_Keys`, `Store`), from the
    /// `MethodDeclaration`'s `MethodDef`/`MemberRef` row, verbatim.
    pub(crate) member: String,
    /// What the declaration method is, per its `MethodSemantics` linkage.
    pub(crate) decl: DeclSemantics,
}

/// What a `MethodImpl`'s `MethodDeclaration` method *is*, per the
/// `MethodSemantics` table (§II.22.28) — the CLR's only authority on which
/// methods are accessors and of what. Accessor naming (`get_P`) is a CLS
/// convention, not a CLR rule, and it misleads in both directions: an interface
/// property `P` may have a getter named `Read`, and an interface property may
/// itself be named `get_Value`. So a consumer that wants to name the
/// *property/event* an implementation satisfies must read this, not the name
/// text of [`InterfaceMemberImpl::member`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclSemantics {
    /// The declaration is a property/event accessor — via *any* semantics
    /// role, the open-ended `Other` included; the payload says which kind owns
    /// it, and the owning property's/event's own name. A declaration claimed
    /// by several owners appears as several [`InterfaceMemberImpl`] entries,
    /// one per owner.
    Accessor(AccessorOwner, String),
    /// The declaration is an ordinary method: a resolved in-module `MethodDef`
    /// that no `Property`/`Event` claims as an accessor. Its `Name` is its
    /// surface name, however much that name may look like an accessor's.
    OrdinaryMethod,
    /// The declaration could not be followed to an in-module `MethodDef`: a
    /// `MemberRef` into another assembly (whose `MethodSemantics` this reader
    /// does not have), or one naming no method of the type it resolves to.
    /// Whether it is an accessor is not knowable here; the declaration's raw
    /// `Name` is all this module can truthfully say.
    Unresolved,
}

/// Which member kind owns an accessor declaration
/// ([`DeclSemantics::Accessor`]): the `MethodSemantics` row's association is a
/// `Property` or an `Event` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessorOwner {
    Property,
    Event,
}

/// A field definition (a `Field` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub(crate) name: String,
    /// See [`Method::accessibility`] — the privatescope/reserved visibility
    /// values are stored per member, not propagated.
    pub(crate) accessibility: Result<MemberAccess, AccessDefect>,
    pub(crate) is_static: bool,
    /// `const` (`Field` flag `Literal`, 0x0040).
    pub(crate) is_literal: bool,
    /// `readonly` (`Field` flag `InitOnly`, 0x0020).
    pub(crate) is_init_only: bool,
    pub(crate) signature: Result<ModifiedType, SigError>,
    pub(crate) attributes: Vec<RawAttribute>,
}

/// A property definition (a `Property` row), with its accessors resolved through
/// `MethodSemantics`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Property {
    pub(crate) name: String,
    /// The property's type (the `PropertySig` type; index parameters are not
    /// projected).
    pub(crate) signature: Result<ModifiedType, SigError>,
    pub(crate) getter: Option<MethodId>,
    pub(crate) setter: Option<MethodId>,
    /// Accessors linked by a `MethodSemantics` row carrying the `Other` (0x4)
    /// semantics — non-standard extras beyond get/set that the projected model
    /// has no slot for. One entry per row: `Some` resolves the accessor within
    /// the owning type's method run (so it can be excluded from the plain
    /// method list); `None` records a row whose method RID fell outside that
    /// run (malformed metadata) — kept so the *presence* of the row still
    /// forces the projector to drop (and record) the whole property rather
    /// than surface it while silently ignoring an accessor. Usually empty
    /// (emitted by some F# pickle output and a little interop, never by C#).
    pub(crate) other_accessors: Vec<Option<MethodId>>,
    pub(crate) attributes: Vec<RawAttribute>,
}

/// An event definition (an `Event` row), with its accessors resolved through
/// `MethodSemantics`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) name: String,
    /// The event's delegate type (the `EventType` `TypeDefOrRef`). A null
    /// `EventType` (ECMA-335 permits it; no real compiler emits one) is surfaced
    /// as `Err` since the model has no slot for a typeless event.
    pub(crate) event_type: Result<ModifiedType, SigError>,
    pub(crate) add: Option<MethodId>,
    pub(crate) remove: Option<MethodId>,
    pub(crate) raise: Option<MethodId>,
    /// See [`Property::other_accessors`] — `Other`-semantics accessors beyond
    /// add/remove/fire, recorded so the projector can drop-and-record the event.
    pub(crate) other_accessors: Vec<Option<MethodId>>,
    pub(crate) attributes: Vec<RawAttribute>,
}

/// The constructor a custom attribute names (the `CustomAttribute.Type` coded
/// index): either a `MethodDef` defined in this image or a `MemberRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberHandle {
    /// A constructor defined in this image, resolved to its owning type and the
    /// method's position within that type's `MethodList` range.
    MethodDef(TypeDefId, MethodId),
    /// A referenced constructor; its signature is carried in this image's
    /// `MemberRef` arena.
    MemberRef(MemberRefId),
}

/// An undecoded custom attribute: the constructor it names plus the raw blob.
/// Decoding is a separate, later step (stage 6) that needs only this plus
/// enum widths from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawAttribute {
    pub(crate) ctor: MemberHandle,
    pub(crate) blob: Vec<u8>,
}

/// The parent of a [`MemberRef`] — the scope that declares the referenced
/// member (`MemberRef.Class`, §II.24.2.6). Only the type parents an
/// attribute-constructor reference uses are distinguished; the exotic scopes
/// (`ModuleRef`, a vararg `MethodDef`, a generic-instantiation `TypeSpec`) fold
/// into `Other`, which the attribute decoder refuses because it cannot name a
/// single owning type for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberRefParent {
    TypeDef(TypeDefId),
    TypeRef(TypeRefId),
    Other,
}

/// A `MemberRef` row (§II.22.25): a member referenced from this image but
/// defined elsewhere. Captured so stage 6 can decode a custom attribute whose
/// constructor is a `MemberRef` — the constructor's parameter signature is
/// carried here, and `parent` names the attribute's own type.
///
/// `signature` is decoded as a *method* reference signature (the form an
/// attribute constructor uses). A `MemberRef` to a field therefore stores an
/// `Err` — its `FieldSig` is not a method signature — which is harmless: only
/// constructor references are ever consumed by the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemberRef {
    pub(crate) name: String,
    pub(crate) parent: MemberRefParent,
    pub(crate) signature: Result<DecodedMethodSig, SigError>,
}
