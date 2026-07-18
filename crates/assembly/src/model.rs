//! The F#-flavoured entity data model — the shape the LSP queries.
//!
//! Mirrors FCS's `Entity`/`Tycon`/`Val` in spirit, not in field-for-field
//! detail (this is *our* type, not the backend's). Phase 1 only carries the
//! fields needed for the hand-built `System.Object`-shaped fixture in
//! `tests/all/assembly_diff.rs`; phases 2–3 will grow generics and the rest
//! of the surface.
//!
//! Everything here is plain owned data — no lifetimes back to a backend's
//! buffer. The plan's D4 calls for "lazy by entity, eager within an
//! entity": that laziness lives at the boundary that *produces* an
//! [`Entity`]; once you have one, every field is materialised.

/// `Major.Minor.Build.Revision` — the .NET assembly version quadruple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

/// Identity of a managed assembly. The triple `(name, version,
/// public_key_token)` uniquely names an assembly in the .NET ecosystem.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssemblyIdentity {
    pub name: String,
    pub version: Version,
    /// `None` for unsigned assemblies.
    pub public_key_token: Option<[u8; 8]>,
}

/// A reference to another type, symbolic until a resolver attaches.
///
/// `Named { assembly: None, .. }` is a same-assembly reference (an ECMA-335
/// `TypeDef`); `Some(...)` is a cross-assembly reference (`TypeRef`).
/// `Var { is_method: true, .. }` is a method-typar; `false` is a type-typar.
///
/// Generic args and array elements are wrapped in [`NullableType`] so each
/// inner position can carry its own nullable-reference-type annotation per
/// phase 4m.3. The byref wrapper does not carry nullability itself — the
/// referent's outer-position byte lives on the enclosing position
/// (`Parameter::nullability`, `MethodSignature::return_nullability`),
/// mirroring how 4m.2 strips the byref before consulting the attribute.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TypeRef {
    Primitive(Primitive),
    Named {
        assembly: Option<AssemblyIdentity>,
        namespace: Vec<String>,
        name: String,
        type_args: Vec<NullableType>,
        /// Per-segment generic *delta* arity — one entry per enclosing-chain
        /// segment of [`Self::Named::name`], outermost first: `[0]` for a
        /// non-generic type, `[2]` for `Dictionary`2`, `[2, 0]` for
        /// `Dictionary`2/Enumerator` (the generic encloser, then the non-generic
        /// nested type), `[1, 1]` for `Outer`1/Inner`1`. ECMA-335 mangles each
        /// nested segment with the number of type parameters *that segment
        /// introduces* (the backtick number — a delta, not a cumulative total),
        /// which the projector otherwise discards via `strip_arity`.
        ///
        /// Built parallel to the `/`-joined segments, so for **well-formed**
        /// metadata it is `name.split('/').count()` long and (for a closed
        /// instantiation) sums to `type_args.len()` — letting a consumer place
        /// each generic argument on its declaring segment
        /// (`Dictionary<int, string>.Enumerator`, not
        /// `Dictionary.Enumerator<int, string>`); the args distribute across
        /// segments left-to-right by these counts. An *open* (uninstantiated)
        /// reference carries empty `type_args` with non-zero arities here.
        ///
        /// Neither relationship is *enforced*: corrupt metadata (a backtick
        /// arity disagreeing with the GENERICINST arg count, or a segment name
        /// containing a literal `/`) can violate either, and the projector
        /// records what it read rather than panicking (the fail-loud contract
        /// forbids a panic mid-walk). Consumers must tolerate a mismatch — e.g.
        /// the hover formatter falls back to a naive rendering when the arities
        /// do not sum to `type_args.len()`.
        segment_arities: Vec<usize>,
    },
    Var {
        index: u16,
        is_method: bool,
    },
    Array {
        element: Box<NullableType>,
        rank: u8,
        /// ECMA-335 II.23.2.13 `Size*` — per-dimension fixed sizes, outermost
        /// first. Empty for the common forms (a vector `T[]` or an unbounded
        /// multidim array `T[,]`); a *bounded* array (`T[2..5, *]`) carries the
        /// declared sizes verbatim. May be shorter than `rank` (trailing
        /// dimensions have no declared size). Carried in full so a consumer is
        /// never silently handed an approximation — see [`TypeRef::Array`].
        sizes: Vec<u32>,
        /// ECMA-335 II.23.2.13 `LoBound*` — per-dimension lower bounds (signed),
        /// outermost first. Empty when every dimension is zero-based (the common
        /// case). May be shorter than `rank` (trailing dimensions are
        /// zero-based).
        lower_bounds: Vec<i32>,
    },
    /// `T*` — an unmanaged pointer (`ELEMENT_TYPE_PTR`). `Some(pointee)` for a
    /// typed pointer (F#'s `nativeptr<'T>`, C#'s `T*`); `None` for `void*`
    /// (F#'s `voidptr`) — the one place a `void` pointee is legal, modelled
    /// without reintroducing a general `void` type. The pointee is an unmanaged
    /// type, so — like [`Self::ByRef`] — it is a plain `TypeRef` with no
    /// nullability.
    Ptr(Option<Box<TypeRef>>),
    /// `T&` — a managed reference (`ELEMENT_TYPE_BYREF`). The referent is an
    /// unannotable position of its own, so — like [`Self::Ptr`] — it is a plain
    /// `TypeRef`; the referent's nullability rides on the enclosing position
    /// (`Field::nullability`, `MethodSignature::return_nullability`, …).
    ///
    /// `readonly` says the referent may be read but not written through — F#'s
    /// `inref<'T>` against a plain `byref<'T>`. C# spells it `ref readonly`
    /// (a return, field, or indexer) or `in` (a parameter; a parameter's byref
    /// is a flag rather than part of its type, so *there* the bit is
    /// [`Parameter::is_readonly_ref`]).
    ///
    /// It has two encodings in metadata and this bit is their union:
    /// `modreq(System.Runtime.InteropServices.InAttribute)` over the byref,
    /// which Roslyn emits only where the CLI must *match* on it (a byref return
    /// and the property type mirroring it; an `in` parameter of a
    /// virtual/abstract/interface member), and otherwise a
    /// `[System.Runtime.CompilerServices.IsReadOnly]` /
    /// `[…RequiresLocation]` attribute on the position. Reading only the
    /// modifier would make the same source construct read as `readonly` on a
    /// virtual member and writable on an ordinary one.
    ByRef {
        inner: Box<TypeRef>,
        readonly: bool,
    },
}

/// A type position paired with its nullable-reference-type annotation.
///
/// Phase 4m.3 walks Roslyn's `NullableAttribute(byte[])` payload in
/// pre-order DFS through the type tree, assigning one byte per annotable
/// position. Inner positions (generic args, array elements) carry the
/// resulting [`Nullability`] alongside the type via this wrapper. The
/// outermost position's byte is also held on the enclosing structural
/// field (`Parameter::nullability`, `Field::nullability`, etc., per
/// phase 4m.2) — the wrapper at the root of those positions' types
/// holds the same byte; the redundancy is intentional and the projector
/// keeps the two in sync.
///
/// `Oblivious` is the back-compat default for positions that pre-date
/// 4m.x or sit inside a type that Roslyn did not annotate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NullableType {
    pub ty: TypeRef,
    pub nullability: Nullability,
}

impl NullableType {
    /// Convenience: wrap a [`TypeRef`] with [`Nullability::Oblivious`].
    /// Most call sites (pre-4m.3 fixtures, value-typed inner positions,
    /// any TypeRef construction that doesn't need to pin nullability)
    /// should use this; the wrapper exists to make per-node nullability
    /// representable, not to force every constructor to spell it out.
    pub fn oblivious(ty: TypeRef) -> Self {
        Self {
            ty,
            nullability: Nullability::Oblivious,
        }
    }
}

/// A referenced/same-assembly F# type-abbreviation target, decoded from the
/// host signature pickle's `type_abbrev` into a *logical* reference the sema
/// layer resolves. Deliberately **not** a [`TypeRef`]: the ECMA namespace/nested
/// split and the assembly identity a `TypeRef::Named` needs are absent from the
/// pickle and cannot be reconstructed by the single-assembly reader (it does not
/// have the referenced assembly loaded). We store what the pickle knows — a CCU
/// *logical name* and an unsplit dotted path — and defer the split + identity to
/// the sema layer, which has every referenced assembly in scope. This mirrors
/// FCS's own lazy-nleref architecture; see
/// `docs/abbreviation-target-projection-plan.md` §3.1.
///
/// Hangs off the abbreviation marker as [`Entity::abbreviation_target`]; `None`
/// there on any marker whose target the decoder cannot yet faithfully model (a
/// structural/generic shape), which keeps the consumer deferring exactly as
/// before — an absent target can never turn a defer into a *wrong* resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AbbreviationTarget {
    /// A tycon application. `ccu = None` only when the pickle referenced the
    /// target as a `Local` (same-CCU) tcref — the one encoding that *proves*
    /// same-assembly membership. `Some(name)` carries the CCU **logical name**
    /// verbatim (`"FSharp.Core"`) for a non-local ref, and the loader resolves the
    /// full identity: note fsc pickles a reference to the host's *own* type
    /// non-locally too (a public signature is read from elsewhere), so a
    /// `Some(host-name)` is common and legitimate — it is **not** folded to
    /// `None`, because a `CcuRef` carries only a name (no version/PKT) and an
    /// assembly can reference a same-named other assembly, which only the sema
    /// layer can tell apart. `path` is the
    /// unsplit dotted *logical* path the pickle carries
    /// (`["System", "String"]`, `["Microsoft", "FSharp", "Core", "int"]`), and
    /// canonical-renders as `path.join(".")` — the ccu is stored for sema but is
    /// **not** part of the rendered string (a same-assembly target renders
    /// path-only, exactly as FCS does).
    Named {
        ccu: Option<String>,
        path: Vec<String>,
        /// Type arguments of the application, outermost-first. Empty for a nullary
        /// head (`int`); populated for a generic instantiation. **Arrays and other
        /// intrinsics are generic apps too** — `int[]` is the array tycon applied
        /// to `int` (`Named { path: [.., "[]"], args: [int] }`), `int list` is the
        /// list tycon applied to `int` — so there is no separate `Array` variant.
        /// Canonical-renders as `path.join(".")` + `` `N `` (the arity) + `<args>`
        /// (`Microsoft.FSharp.Collections.list``1<Microsoft.FSharp.Core.int>`).
        args: Vec<AbbreviationTarget>,
    },
    /// The abbreviation's own generic parameter, by position into the marker's
    /// [`Entity::generic_parameters`] (`type MyList<'T> = 'T list` ⇒ the `'T`
    /// target is `Var(0)`). Canonical-renders as `!T<pos>`.
    Var(u16),
    /// A function type `domain -> range` (F#'s `TType_fun`), right-associative.
    /// Canonical-renders as `<domain> -> <range>`, parenthesising the domain when
    /// it is itself a function so `(a -> b) -> c` stays distinct from
    /// `a -> b -> c`.
    Fun(Box<AbbreviationTarget>, Box<AbbreviationTarget>),
    /// A tuple type (F#'s `TType_tuple`). `struct_kind` is `true` for a
    /// value-tuple (`struct (a * b)`), `false` for a reference tuple (`a * b`).
    /// Canonical-renders parenthesised — `(a * b)` / `struct (a * b)` — so a tuple
    /// element never runs into a neighbouring operator.
    Tuple {
        struct_kind: bool,
        elems: Vec<AbbreviationTarget>,
    },
}

/// One of the ECMA-335 `ELEMENT_TYPE_*` primitive codes. Phase 1 carries
/// only the variants reachable from the hand-built fixture; new variants
/// land alongside the test that motivates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Primitive {
    Void,
    Bool,
    Char,
    I1,
    U1,
    I2,
    U2,
    I4,
    U4,
    I8,
    U8,
    R4,
    R8,
    IntPtr,
    UIntPtr,
    Object,
    String,
}

/// What flavour of type an [`Entity`] is. Combines the ECMA-335 type flags
/// (class/interface/value-type/enum/delegate) with the F#-specific kinds
/// derived from `CompilationMappingAttribute` (`Module`, `Union`,
/// `Record`, `Abbreviation`, `Exception`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EntityKind {
    Class,
    Struct,
    Interface,
    Enum,
    Delegate,
    Module,
    Union,
    Record,
    Abbreviation,
    Exception,
    /// `[<Measure>] type T` — an F# unit-of-measure. fsc emits the
    /// underlying TypeDef row as `extends System.Object` with the
    /// `CompilationMappingAttribute(SourceConstructFlags.Measure = 4)`
    /// marker; the ECMA-only projector lacks the F# signature pickle
    /// and so reads it as [`Self::Class`]. Phase 6c1's projector merge
    /// upgrades the kind to `Measure` by walking the pickle for
    /// entities with `typar_kind = TyparKind::Measure` and matching
    /// each one's FQN against the ECMA-projected tree.
    Measure,
}

/// Accessibility, projected from ECMA-335 visibility flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Access {
    Public,
    Internal,
    Private,
    Protected,
    ProtectedOrInternal,
    ProtectedAndInternal,
}

/// Variance of a generic parameter, as encoded in ECMA-335 II.9.3. Only
/// meaningful on interface and delegate type parameters; class and struct
/// typars are always [`Variance::Invariant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Variance {
    Invariant,
    /// `out T` in C#, `+T` in ILAsm.
    Covariant,
    /// `in T` in C#, `-T` in ILAsm.
    Contravariant,
}

/// Nullable-reference-type state for a single type position, as encoded in
/// Roslyn's `[NullableAttribute]` byte payload (C# 8+).
///
/// The IL encoding is one byte per *annotable position* in a depth-first
/// pre-order walk of the signature tree. Phase 4m.1 carried this on
/// [`TypeParameter`] only; phase 4m.2 lifted it to outer positions
/// (parameters, fields, properties, events, return types); phase 4m.3
/// added inner positions via [`NullableType`] wrappers on generic args
/// and array elements. The shared walker mirrors the F# compiler's
/// `Nullness.ImportILTypeWithNullness` in
/// `dotnet/fsharp/src/Compiler/Checking/import.fs`.
///
/// Roslyn also emits `[NullableContextAttribute]` on the enclosing method
/// or type to supply a default byte for positions whose own attribute is
/// omitted; the projector consults that context when a position carries
/// no direct attribute. A position with neither a direct nor a context
/// attribute reads as [`Nullability::Oblivious`] — Roslyn's signal for
/// "compiled without the nullable feature," which is also what the BCL
/// and pre-C#8 assemblies emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Nullability {
    /// Byte 0 — no nullable annotation context. Pre-C#8 assemblies, the
    /// BCL, and references compiled with `#nullable disable` all surface
    /// this state. Cannot be distinguished from "attribute absent" at the
    /// projection level and the projector deliberately collapses the two.
    Oblivious,
    /// Byte 1 — non-nullable reference. The C# 8+ `where T : notnull`
    /// constraint and the default state in a `#nullable enable` scope.
    NotAnnotated,
    /// Byte 2 — nullable reference. C# 8+ `T?` syntax and the
    /// `where T : class?` constraint shape.
    Annotated,
}

/// One generic type parameter, declared either on a type or a method. The
/// index of the parameter in its parent's `generic_parameters` list is its
/// ECMA-335 typar number — i.e. the value that appears in `TypeRef::Var`.
///
/// Variance is preserved as encoded; ECMA-335 permits it only on interfaces
/// and delegates, but the projector does not enforce that — corrupt or
/// hand-rolled IL that emits `Covariant` on a class typar will be reported
/// faithfully so divergence with FCS is visible.
///
/// Special constraints are kept as separate bool flags rather than a bag
/// of strings to make exhaustive matching cheap. `type_constraints` carries
/// the named base classes and interfaces in the order ECMA-335 stores them.
/// C# emits an explicit `System.ValueType` row in `type_constraints` for
/// every `struct` typar; the projection preserves it so the diff against
/// fcs-dump stays honest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TypeParameter {
    pub name: String,
    pub variance: Variance,
    /// `class` constraint (the type argument must be a reference type).
    pub reference_type_constraint: bool,
    /// `struct` constraint (the type argument must be a non-nullable value
    /// type). Implies an inherited `System.ValueType` type constraint that
    /// C# emits explicitly into [`Self::type_constraints`].
    pub value_type_constraint: bool,
    /// `new()` constraint — the type argument must expose a public
    /// parameterless constructor.
    pub default_constructor_constraint: bool,
    /// `unmanaged` constraint — the type argument must be a non-nullable
    /// value type that transitively contains no managed references (a
    /// "blittable" type). Encoded in IL as
    /// `[System.Runtime.CompilerServices.IsUnmanagedAttribute]` on the
    /// typar declaration *in addition to* the `struct` constraint bit;
    /// both bits live alongside each other (unmanaged is additive, never
    /// a replacement). C#'s `where T : unmanaged` and F#'s
    /// `when 'T : unmanaged` both produce this shape.
    pub is_unmanaged: bool,
    /// `allows ref struct` anti-constraint — the type argument is *permitted*
    /// to be a `ref struct` (byref-like) type. Encoded as the
    /// `GenericParameterAttributes.AllowByRefLike` bit (`0x0020`) on the
    /// `GenericParam` row (ECMA-335 §II.23.1.7) — the same bit FCS reads as
    /// `ILGenericParameterDef.HasAllowsRefStruct`. C#'s
    /// `where T : allows ref struct` (C# 13) and F#'s
    /// `when 'T : (allows ref struct)` (F# 9) both produce it. Unlike the
    /// special constraints above this is an *anti*-constraint — it widens
    /// rather than narrows the admissible arguments — and is an independent
    /// bit (orthogonal to the value-type constraint), so it is reported on
    /// its own with no additivity guard. The F#-pickle reader models the
    /// same notion separately as `FSharpTyparConstraint::AllowsRefStruct`
    /// (B-stream tag 2); this is the IL-projection half.
    pub allows_ref_struct: bool,
    /// Nullable-reference-type state of the typar itself (phase 4m.1).
    /// Decoded from `[NullableAttribute(byte)]` directly on the
    /// GenericParam row when present, falling back to
    /// `[NullableContextAttribute(byte)]` on the enclosing method or type
    /// when the direct attribute is omitted. Reads as
    /// [`Nullability::Oblivious`] when neither is present — i.e. for
    /// pre-C#8 assemblies, the BCL, and any reference compiled with
    /// `#nullable disable`.
    pub nullability: Nullability,
    /// Base classes and interfaces the type argument must satisfy. Order
    /// matches the GenericParamConstraint table row order.
    pub type_constraints: Vec<TypeRef>,
}

/// A member the projector dropped because it could not decode it, paired with
/// the reason. See [`Entity::skipped_members`].
///
/// The failure is *localized, named, and stored* rather than propagated:
/// per the reader plan's "bound uncertainty", one unreadable member must never
/// sink a whole type or assembly. `reason` is the `Display` of the underlying
/// [`crate::ImportError`] — a human-readable diagnostic string, not a
/// machine-reparsed value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkippedMember {
    /// The dropped member's IL name.
    pub name: String,
    /// Why it was dropped — the `Display` rendering of the [`crate::ImportError`]
    /// the projector raised for it (e.g. `"unsupported signature element:
    /// property \`Item\` returns byref"`).
    pub reason: String,
}

/// A top-level or nested projection item the assembly enumerator dropped because
/// it could not decode that item's own shape. Today this records whole-type
/// drops from [`crate::EcmaView::enumerate_type_defs_with_skips`].
///
/// This is separate from [`SkippedMember`] because an assembly-level drop is
/// not a member of an enclosing projected entity. Keeping the records distinct
/// lets consumers report "type dropped" and "member dropped" without inferring
/// meaning from where a reused struct happened to be stored.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkippedProjectionItem {
    /// The dropped item's fully-qualified name.
    pub name: String,
    /// Why it was dropped — the `Display` rendering of the [`crate::ImportError`]
    /// the projector raised for it.
    pub reason: String,
}

impl SkippedProjectionItem {
    /// The **enclosing namespace segments** of the dropped item's fully-qualified
    /// [`name`](Self::name): the dotted prefix before the simple name, with the
    /// generic-arity suffix (`` `n ``) and any nested-type tail (`/Inner`) stripped
    /// — a nested type shares its top-level's namespace. A root-namespace type
    /// (`Foo`) yields the empty namespace. Consumers that must treat a dropped
    /// type's namespace as possibly-extension-bearing (an undecodable type may be a
    /// C#-style `[<Extension>]` class) use this; an over-approximation is fine.
    pub fn enclosing_namespace(&self) -> Vec<String> {
        let top_level = self.name.split('/').next().unwrap_or(&self.name);
        let base = top_level.split('`').next().unwrap_or(top_level);
        match base.rsplit_once('.') {
            Some((namespace, _simple)) => namespace.split('.').map(str::to_owned).collect(),
            None => Vec::new(),
        }
    }
}

/// Which F# signature-pickle overlay could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FsharpOverlayKind {
    /// Entity and module-member source names from the host CCU pickle.
    SourceName,
    /// F#-native extension-member flags from the host CCU pickle.
    Extension,
    /// Unit-of-measure entity kinds from the host CCU pickle.
    Measure,
    /// Name-only marker entities for metadata-invisible type abbreviations
    /// from the host CCU pickle.
    AbbreviationMarkers,
    /// Union case names ([`Entity::union_case_names`]) from the host CCU
    /// pickle — the module-open fold's pattern surface.
    UnionCases,
}

/// A host F# signature-pickle overlay the projector skipped, paired with the
/// reason. Populated when the host pickle cannot be decoded, so callers can
/// distinguish "base ECMA projection succeeded, F# enrichment absent" from a
/// fully enriched assembly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkippedFsharpOverlay {
    /// The manifest-resource name whose pickle would have driven the overlay.
    pub resource_name: String,
    /// The overlay passes that depended on this resource.
    pub overlays: Vec<FsharpOverlayKind>,
    /// Why the overlay was skipped.
    pub reason: String,
}

/// Assembly-level projection degradations. Whole-type drops and F# overlay
/// skips are both assembly-level facts, but they have different semantics and
/// should be reported separately by consumers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssemblyProjectionSkips {
    pub dropped_types: Vec<SkippedProjectionItem>,
    pub skipped_fsharp_overlays: Vec<SkippedFsharpOverlay>,
    /// `true` when this (non-FSharp.Core) F#-authored assembly may export
    /// type abbreviations the projection cannot see: its host signature
    /// pickle failed to decode (so the abbreviation-marker overlay could not
    /// run — the skip is also recorded in
    /// [`Self::skipped_fsharp_overlays`]), or it embeds *foreign* CCU pickles
    /// (an `fsc --standalone` build), whose abbreviations we never decode.
    /// fsc emits no ECMA TypeDef for a plain abbreviation, so nothing else in
    /// the projection witnesses them. A name-resolution consumer should treat
    /// every namespace this assembly declares into as possibly shadowed;
    /// `false` means the projected tree (including its synthesised
    /// abbreviation markers) is abbreviation-complete for this assembly.
    /// Always `false` for FSharp.Core — its primitive-alias abbreviations are
    /// the semantics consumers hard-code, never a shadow risk (see
    /// `apply_abbreviation_markers`).
    pub fsharp_abbreviations_unknowable: bool,
    /// `true` when this F#-authored assembly's **F#-native extension-member
    /// index** cannot be trusted complete — its host signature pickle is not
    /// *authoritative* (absent, undecodable, or a `--standalone` image that also
    /// embeds foreign dependency CCUs). `apply_extension_member_index` reads only
    /// the host CCU, so in every such case some module's
    /// [`Entity::extension_member_names`] / [`Entity::static_extension_member_names`]
    /// is empty *because it is unread*, not because the assembly declares none.
    ///
    /// Distinct from [`Self::fsharp_abbreviations_unknowable`] on purpose: that
    /// flag exempts FSharp.Core (its abbreviations are hard-coded), but the
    /// exemption is about *abbreviations only* — FSharp.Core's extension members
    /// are ordinary pickle data, so an undecodable FSharp.Core pickle leaves the
    /// index just as blind. A name-keyed extension gate must treat such an
    /// assembly's extension queries as **unknowable** rather than proving a name
    /// absent from an unread list. `false` for a C#/BCL image (no F#-native
    /// extension members exist to miss) and for any F# assembly whose pickle
    /// decoded.
    pub fsharp_extension_index_unknowable: bool,
    /// `true` when the host F# signature pickle was **not authoritative** for this
    /// image — absent, undecodable, or a `--standalone` build that embeds foreign
    /// dependency CCUs. When so, the pickle overlay never ran, and the projected
    /// F# facts that depend on it — most visibly a module's kind
    /// (`EntityKind::Module`, otherwise set from the IL `CompilationMappingAttribute`)
    /// and its value/function member split — are IL heuristics, *not* what FCS
    /// sees: FCS imports such an assembly through IL, where a module reads as a
    /// plain type and its `let`s as ordinary members (verified against real
    /// `--standalone` output). A semantic-token classifier must therefore not trust
    /// `EntityKind::Module` here.
    ///
    /// This is plain `!authoritative` — deliberately *not* gated on the assembly
    /// being *detected* as F# (unlike [`Self::fsharp_extension_index_unknowable`]
    /// and [`Self::fsharp_abbreviations_unknowable`]). A `--standalone` image can
    /// lose its assembly-level `FSharpInterfaceDataVersionAttribute` while keeping
    /// per-type `CompilationMappingAttribute` module markers, so an F#-detection
    /// gate misses it; keying only on pickle authority catches it. Harmless for a
    /// C#/BCL image, which is non-authoritative but declares no modules to gate.
    pub fsharp_signature_non_authoritative: bool,
}

/// A type, module, or other top-level definition in an assembly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Entity {
    pub assembly: AssemblyIdentity,
    /// Namespace as a list of segments. `[]` is the global namespace.
    pub namespace: Vec<String>,
    pub name: String,
    pub kind: EntityKind,
    pub access: Access,
    /// `sealed` (`TypeAttributes` 0x0100): the type cannot be inherited from.
    /// Overload resolution (OV-2) uses this: a sealed, non-interface parameter
    /// type has no proper subtype, so `may_apply`'s subsumption channel reduces
    /// to type-identity — the honest way its refuter eliminates a candidate.
    /// A value type is always sealed in the CLR; an interface never is.
    pub is_sealed: bool,
    /// Formal type parameters declared on this entity. Empty for
    /// non-generic types. ECMA-335 stores generic arity in the type name
    /// (e.g. ``List`1``) but the canonical projection strips the
    /// backtick-arity suffix — the arity is recovered from this list.
    pub generic_parameters: Vec<TypeParameter>,
    /// `None` for `System.Object`, interfaces, and anything else that
    /// inherits from nothing.
    pub base_type: Option<TypeRef>,
    pub interfaces: Vec<TypeRef>,
    pub members: Vec<Member>,
    /// Members of this entity the projector could **not** decode and therefore
    /// dropped from [`Self::members`], each paired with the reason. Empty in the
    /// overwhelmingly common case.
    ///
    /// This is the reader plan's "bound uncertainty" made concrete: a single
    /// member whose signature uses a construct we don't model yet (a function
    /// pointer, a `modreq` custom modifier, a byref-returning property, a `where
    /// T : allows ref struct` method typar, …) is dropped *individually* rather
    /// than sinking the whole enclosing type — and the rest of the type stays
    /// usable. The drop is *recorded* here (not silently swallowed) so a
    /// consumer can surface it as a diagnostic. Whole *types* the projector
    /// cannot decode are dropped analogously, but — having no enclosing
    /// `Entity` to hang a record on — are reported through
    /// [`AssemblyProjectionSkips::dropped_types`] instead of here.
    pub skipped_members: Vec<SkippedMember>,
    /// Every `MethodDef` token (`0x0600_0000 | rid`) physically declared on
    /// this type, in metadata order — the type's *own* methods only (a nested
    /// type's methods belong to that nested [`Entity`], not this one).
    ///
    /// This is the **physical** method set, a superset of the tokens reachable
    /// through [`Self::members`]: that list is the *resolution* view, which for
    /// F# kinds (unions, records, modules) deliberately drops accessor and
    /// compiler-synthesised methods FCS does not surface. A union's
    /// user-written property getter (`member x.Get`) is one such drop — and it
    /// is exactly the method carrying the type's only source sequence point.
    /// Go-to-definition on the *type* therefore needs this physical set to find
    /// a navigable method (`docs/...` / the LSP's `goto_source`); name
    /// resolution and hover keep using [`Self::members`]. The differential
    /// normaliser ignores it (it names metadata rows, not logical surface, like
    /// [`MethodLike::metadata_token`]). `[]` for a synthesised/test entity.
    pub method_def_tokens: Vec<u32>,
    pub nested_types: Vec<Entity>,
    /// `true` when the entity carries
    /// `[System.Runtime.CompilerServices.IsReadOnlyAttribute]`. C# emits
    /// it on `readonly struct` declarations; F# emits it on `[<IsReadOnly>]`-
    /// annotated structs (and on records that opt in). The marker is
    /// orthogonal to [`EntityKind::Struct`] — set by the importer from
    /// the entity's custom-attribute list.
    pub is_readonly: bool,
    /// `true` when the entity carries
    /// `[System.Runtime.CompilerServices.IsByRefLikeAttribute]`. C# emits
    /// it on `ref struct` declarations; F# emits it on
    /// `[<IsByRefLike>]`-annotated structs. Like [`Self::is_readonly`],
    /// the marker is set by the importer from the CA list.
    pub is_byref_like: bool,
    /// `true` when the entity's IL row extends `System.ValueType` — i.e.
    /// the type is a CLR value type. Orthogonal to [`EntityKind`]: a
    /// `[<Struct>] type R = { ... }` projects as
    /// [`EntityKind::Record`] (F# kind wins) AND `is_struct = true`, so
    /// consumers can recover the struct-ness that the F# kind otherwise
    /// hides. For [`EntityKind::Struct`] and [`EntityKind::Enum`] the
    /// flag is necessarily redundant — they already imply value-type-
    /// ness — but the importer still sets it faithfully from the IL
    /// signal, and the renderer suppresses the redundant "struct "
    /// prefix in those cases.
    pub is_struct: bool,
    /// `true` when the entity carries `[Microsoft.FSharp.Core.AutoOpenAttribute]`.
    /// F# emits this on a module to mean "consumers don't need an explicit
    /// `open <module>`; opening the enclosing namespace is enough". Only
    /// the parameterless module-level form contributes — the
    /// `AutoOpenAttribute(path)` overload used at assembly-level
    /// (`[<assembly: AutoOpen("My.Namespace")>]`) is an assembly attribute
    /// and never appears on a TypeDef, so we don't need to discriminate
    /// (that list is surfaced separately, by
    /// [`EcmaView::assembly_auto_opens`](crate::EcmaView::assembly_auto_opens)).
    ///
    /// The flag is meaningful only on [`EntityKind::Module`] entities; the
    /// F# compiler ignores it elsewhere. The importer sets it whenever the
    /// attribute is present, leaving the "is this a module?" judgement to
    /// the consumer — same policy as [`Self::is_readonly`] vs
    /// [`EntityKind::Struct`].
    pub is_auto_open: bool,
    /// `true` when the entity carries
    /// `[Microsoft.FSharp.Core.RequireQualifiedAccessAttribute]`. F# emits
    /// it on a module or discriminated union to force callers to fully
    /// qualify member references (`Foo.bar`, `Color.Red`) instead of
    /// relying on `open Foo` / unqualified case names. Pure marker —
    /// presence alone is the signal; the attribute carries no payload.
    /// Set by the importer from the entity's CA list. Not in the F#
    /// compiler's `WellKnownILAttributes` catalogue (it's defined in
    /// `FSharp.Core` itself and consumed by name elsewhere in FCS), so
    /// the well-known-attribute sync test does not cover it.
    pub is_require_qualified_access: bool,
    /// `true` when the entity carries
    /// `[Microsoft.FSharp.Core.NoEqualityAttribute]`. F# emits this to
    /// suppress the auto-derived `Equals` / `GetHashCode` (and the
    /// `IEquatable<T>` implementation) on records and DUs. Carries no
    /// payload — presence alone is the signal.
    ///
    /// Lives alongside [`Self::is_no_comparison`], [`Self::is_structural_equality`],
    /// and [`Self::is_structural_comparison`] in the derived-impl policy
    /// cluster. F# requires `[<NoEquality>]` whenever `[<NoComparison>]`
    /// is used, but we don't enforce that here — the importer faithfully
    /// reports what the IL says. None of the four are in the F# compiler's
    /// `WellKnownILAttributes` catalogue (they live on the TypedTree
    /// `WellKnownEntityAttributes` enum instead), so the sync test does
    /// not cover them.
    pub is_no_equality: bool,
    /// `true` when the entity carries
    /// `[Microsoft.FSharp.Core.NoComparisonAttribute]`. Suppresses the
    /// auto-derived `IComparable<T>` / `IComparable` / `CompareTo` on
    /// records and DUs. Pure marker. See [`Self::is_no_equality`] for the
    /// cluster overview.
    pub is_no_comparison: bool,
    /// `true` when the entity carries
    /// `[Microsoft.FSharp.Core.StructuralEqualityAttribute]`. Explicit
    /// opt-in to F# structural equality — usually the default for
    /// records and DUs, but writing the attribute by hand is meaningful
    /// when combined with `[<NoComparison>]` (keep equality, drop
    /// comparison) or on a class that wouldn't otherwise get it. Pure
    /// marker. See [`Self::is_no_equality`] for the cluster overview.
    pub is_structural_equality: bool,
    /// `true` when the entity carries
    /// `[Microsoft.FSharp.Core.StructuralComparisonAttribute]`. Explicit
    /// opt-in to F# structural comparison — typically paired with
    /// `[<StructuralEquality>]`. Pure marker. See [`Self::is_no_equality`]
    /// for the cluster overview.
    pub is_structural_comparison: bool,
    /// `true` when the entity carries
    /// `[Microsoft.FSharp.Core.AllowNullLiteralAttribute]` *and* the bool
    /// ctor argument resolves to `true`. F#-only marker that opts a
    /// reference type out of F#'s default null-prohibition, making
    /// `null` a legal value at use sites. Valid on classes and
    /// interfaces; F# rejects it on records, DUs, and value types.
    ///
    /// The attribute has two constructor overloads:
    /// parameterless (`[<AllowNullLiteral>]`, equivalent to `(true)`) and
    /// `AllowNullLiteralAttribute(bool)`. The `(false)` form is the
    /// deliberate *disable* shape — F#'s own `WellKnownEntityAttributes`
    /// distinguishes `AllowNullLiteralAttribute` (presence) from
    /// `AllowNullLiteralAttribute_True` / `_False` to model the case
    /// where a derived class opts out of a base's `(true)`. We decode
    /// the bool so this field stays semantically honest;
    /// `fcs-dump` mirrors the same decode.
    pub is_allow_null_literal: bool,
    /// `Some` when the entity carries `[System.ObsoleteAttribute]`; the
    /// payload is the (optional) deprecation message + the
    /// warning-vs-error flag. The importer decodes ECMA-335 II.23.3 from
    /// the CA blob: constructor args + the `IsError` / `Message` named
    /// args (named args win on conflict, matching CLR runtime semantics).
    /// Other named args (`DiagnosticId`, `UrlFormat`) are dropped — they
    /// don't change "should I use this?" decisions.
    pub obsolete: Option<Obsolete>,
    /// `Some` when the entity carries
    /// `[System.Diagnostics.CodeAnalysis.ExperimentalAttribute]` (the
    /// .NET 8+ "this API may change without notice" marker). Decoding
    /// rules are documented on [`Experimental`]. Sibling of
    /// [`Self::obsolete`] — separate field rather than a shared
    /// `Diagnostic` union because the two attributes carry different
    /// payloads and consumers query them independently.
    pub experimental: Option<Experimental>,
    /// `Some` when the entity carries
    /// `[System.Reflection.DefaultMemberAttribute(string)]`. The payload
    /// is the member name nominated as the type's default — usually
    /// `"Item"` (C#'s implicit indexer marker, emitted automatically by
    /// the Roslyn compiler whenever a class declares an indexer), but
    /// any string is legal and user code may supply a different name
    /// explicitly. The attribute is the IL-level mechanism behind
    /// `Type.GetDefaultMembers()` and informs `IndexerNameAttribute`
    /// resolution.
    ///
    /// The DU has two variants. [`DefaultMember::Named`] is the only
    /// shape today's decoder produces — a clean single-positional
    /// `string` ctor arg. [`DefaultMember::Unknown`] is reserved for a
    /// future decoder relaxation that would *degrade* (rather than
    /// refuse) on payloads the model can't otherwise represent: a null
    /// ctor arg or named args. Until that relaxation lands the decoder
    /// still refuses loud on those shapes, so `Unknown` is currently
    /// unreachable from the projector; the model carries it now so the
    /// future change is a one-line edit at the decoder rather than a
    /// model migration.
    pub default_member: Option<DefaultMember>,
    /// Every `[System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute]`
    /// the type carries. The attribute is `AllowMultiple = true`, so a
    /// single type can legally carry several instances — see
    /// [`CompilerFeatureRequired`] for the per-instance shape and the
    /// decoding rules. Empty vec when the attribute is absent. Roslyn
    /// emits it on a type when the type uses a C# feature that requires
    /// compiler support (e.g. `where T : allows ref struct` lands
    /// `feature == "RefStructs"` on the type).
    pub compiler_feature_required: Vec<CompilerFeatureRequired>,
    /// The F# *source* name of this entity, when it differs from the IL
    /// [`Self::name`]. `Some` only for an F# **module** whose compiled class
    /// carries `[CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)]`:
    /// the F# compiler appends `"Module"` to the IL name when a module shares
    /// its name with a type in the same namespace (FSharp.Core's `List` type +
    /// `List` module ⇒ IL `ListModule`), and the source name is the IL name
    /// with that suffix stripped (`ListModule` ⇒ `List`).
    ///
    /// `None` for every other entity (the IL name *is* the source name). The
    /// differential normaliser renders the FQN from `source_name.unwrap_or(name)`
    /// because fcs-dump emits an entity's `DisplayName` (the source name) — so
    /// unlike the member case, the entity source name *is* part of that
    /// comparison. Consumers resolving F# qualified paths (the sema layer) match
    /// a module segment against `source_name.unwrap_or(name)`.
    pub source_name: Option<String>,
    /// The F# **source names of this module's instance extension members** —
    /// the members a `type T with member x.M …` augmentation declares on some
    /// *other* type `T`, keyed by the name a use site writes (`recv.M`). Empty
    /// for every non-module entity, and for a module that declares none.
    ///
    /// Read straight from the host signature pickle's
    /// `ValFlags.IsExtensionMember ∧ IsInstance` bit (each val's *logical* name),
    /// **before** any IL-method matching — so it carries no false negatives from
    /// the arity/name heuristics [`MethodLike::is_extension_method`] relies on
    /// (which drop generic-method, optional-parameter, and same-arity-collision
    /// extensions; see overload-resolution-plan §6.1(b)). The overload-resolution
    /// extension-*absence* gate reads this, never the per-method flag.
    ///
    /// **Completeness is bounded by
    /// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`]**: when that
    /// flag is set (the F# signature pickle failed to decode, or the image embeds
    /// foreign CCUs whose modules the host pickle does not describe), this list
    /// may be incomplete and a sound consumer must treat the whole assembly's
    /// extension set as unknowable rather than trust an empty/partial list. When
    /// the flag is clear the list is exhaustive for the module. C#-style
    /// `[<Extension>]` methods are **not** here — they are the trustworthy
    /// per-method [`MethodLike::is_extension_method`] channel.
    pub extension_member_names: Vec<String>,
    /// The F# **source names of this union's cross-assembly-accessible
    /// cases**, in declaration order. Populated only for
    /// [`EntityKind::Union`], and only from the host signature pickle
    /// (`PickledUnionCase.ident.name`): the ECMA-only projection cannot
    /// recover case names — the `NewCase` constructors are
    /// `[CompilerGenerated]` and dropped, the nullary-case getters are
    /// properties a union projection drops, and the per-case carrier nested
    /// types exist only for the class-per-case representation.
    ///
    /// `None` means **unknowable** (no host pickle described this union — a
    /// foreign CCU, a decode failure): name-resolution consumers folding an
    /// `open` must treat it as name-unknown residue (its hidden cases can
    /// shadow anything), exactly as they treat
    /// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`].
    /// `Some(names)` is the **complete accessible list** — possibly empty: a
    /// union with a private representation (`type U = private | Hidden`)
    /// knowably contributes no case to a cross-assembly `open`, and FCS
    /// resolves an earlier same-named binding where a hidden entry would
    /// wrongly shadow it (codex round 21). Only `TAccess []` (public) cases
    /// are listed.
    pub union_case_names: Option<Vec<String>>,
    /// The F# source names of this module's **static** extension members — what
    /// `type T with static member M …` declares. The static sibling of
    /// [`Self::extension_member_names`], read from the same pickled
    /// `ValFlags.IsExtensionMember` bit with `IsInstance` **clear**, and with the
    /// same `fsharp_abbreviations_unknowable` completeness bound.
    ///
    /// It is a *separate* list because the two feed different lookups and neither
    /// consumer should have to filter: a **value receiver**'s group takes only
    /// instance extensions (FCS filters by `MethInfo.IsInstance` —
    /// overload-resolution-plan §6.1(a)), while a **type-qualified static** call's
    /// group takes only static ones. Probed 2026-07-12 (`fcs-dump overloads`): a
    /// `static member Compare` extension on `System.String`, reached through an
    /// `open`, joins `System.String.Compare 1` as `call:extension` and competes
    /// flat with the intrinsic `Compare` overloads — so the static-call
    /// extension gate (OV-7) needs these names, and an instance-only index would
    /// let it commit an intrinsic FCS never chose.
    pub static_extension_member_names: Vec<String>,
    /// `true` when this type carries
    /// `[System.Runtime.CompilerServices.ExtensionAttribute]` — the *container*
    /// marker C# emits on a `static class` holding extension methods, and fsc on an
    /// `[<Extension>]` type. FCS's `IsTyconRefUsedForCSharpStyleExtensionMembers`.
    ///
    /// A name-resolution consumer needs this to reproduce FCS's C#-style extension
    /// predicate (`IsMethInfoPlainCSharpStyleExtensionMember`), which demands the
    /// attribute on the *enclosing type* as well as the method — the method flag
    /// alone over-fires on hand-written IL, and dropping such a method from an
    /// `open type`'s scope would let an earlier open's same-named member win.
    pub is_extension_container: bool,
    /// Custom attributes the importer did not classify into a typed field
    /// (see D6). Each one keeps its raw blob for hover; consumers should
    /// not try to interpret the bytes without going through the importer.
    pub custom_attrs: Vec<CustomAttr>,
    /// The decoded *target* of an [`EntityKind::Abbreviation`] marker
    /// (`type IntId = int` ⇒ `Named { path: ["Microsoft","FSharp","Core","int"],
    /// … }`), or `None` on every non-marker entity **and** on any marker whose
    /// target the decoder cannot yet faithfully model (a structural/generic
    /// shape). A resolvable target lets a consumer resolve *through* the alias
    /// instead of deferring; `None` keeps it deferring, so the field is strictly
    /// additive. See [`AbbreviationTarget`].
    pub abbreviation_target: Option<AbbreviationTarget>,
}

/// How confidently a projected module member is known to be an **F#-native
/// augmentation** (`type T with member M …`) — the fact name resolution uses to
/// keep it out of scope, since an augmentation is reachable only through the dot
/// on its target type.
///
/// Three states rather than a bool because the fallback projection path cannot
/// tell an augmentation from an ordinary binding: fsc mangles an augmentation's
/// compiled name to `Type.Member`, but `[<CompiledName("A.B")>]` on a perfectly
/// ordinary `let` produces the same shape (legal, and bare-resolvable —
/// fsi-verified). Collapsing that ambiguity to `true` would *hide* a value FCS
/// resolves; collapsing it to `false` would *surface* an augmentation FCS hides.
/// Both are wrong resolutions, so the uncertainty is carried instead and the
/// consumer defers (D5: correctness over availability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Augmentation {
    /// Not an augmentation: an ordinary module `let`, or any member of a
    /// non-module type. A pickled val said so, or nothing suggests otherwise.
    #[default]
    No,
    /// The host signature pickle says so — the claiming val carries
    /// `ValFlags.IsExtensionMember` together with `MemberInfo` (FCS's
    /// `vref.IsMember`), instance or static. Authoritative: hide it.
    Certain,
    /// Only the IL name mangling (`Type.Member`) says so, on an image whose pickle
    /// did not decode or is not authoritative — indistinguishable from a dotted
    /// `[<CompiledName>]`. Defer: keep the name in scope (so it still shadows by
    /// position) but resolve it to nothing.
    Possible,
}

/// Deprecation marker, projected from `[System.ObsoleteAttribute]`. The
/// model deliberately captures only the two fields that inform
/// "should an LLM agent use this API?" decisions: the explanatory
/// `message` and the warning-vs-error `is_error` flag.
///
/// `ObsoleteAttribute` has three ctor overloads — `()`, `(string?)`,
/// `(string?, bool)` — plus the named `IsError` / `Message` properties.
/// The importer collapses all four shapes onto this struct. A bare
/// `[<Obsolete>]` projects to `Obsolete { message: None, is_error: false }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Obsolete {
    pub message: Option<String>,
    pub is_error: bool,
}

/// Experimental-API marker, projected from
/// `[System.Diagnostics.CodeAnalysis.ExperimentalAttribute]` (.NET 8+).
/// The model captures the three payload fields the .NET BCL itself
/// exposes:
///
/// - `diagnostic_id` — the required ctor arg, decoded verbatim.
/// - `url_format` — optional `UrlFormat` named property, a printf-style
///   string into which the diagnostic ID is substituted to produce a
///   help URL.
/// - `message` — optional `Message` named property, the human-readable
///   "what about this is experimental" note.
///
/// The reader decodes the CA blob faithfully regardless of string length, so
/// the payload fields reflect exactly what the attribute carries (the named
/// properties are `Option` because they are genuinely optional).
///
/// Why a separate field rather than a `Diagnostic` union with
/// [`Obsolete`]: the payloads are different shapes, consumers query
/// them independently, and there's no third sibling on the roadmap. A
/// shared union would be speculative generality.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Experimental {
    pub diagnostic_id: Option<String>,
    pub url_format: Option<String>,
    pub message: Option<String>,
}

/// Default-member marker, projected from
/// `[System.Reflection.DefaultMemberAttribute]` (see
/// [`Entity::default_member`]).
///
/// The attribute has a single ctor — `(string?)` — and no named
/// properties on its public surface, so the variant set is minimal: a
/// decoded name, or a sentinel for a present-but-undecodable payload.
///
/// Why a DU rather than the `Option<String>`-shaped struct that
/// [`Obsolete`] / [`Experimental`] use: a bare `[DefaultMember]` with no
/// name isn't a legal use of the attribute, whereas a bare `[<Obsolete>]`
/// *is*. Modelling this as a struct with an `Option<String>` field would
/// let callers construct a "marker present, no name" value that has no
/// counterpart in the metadata. The DU keeps the "name unknown" case
/// distinct from "name is `s`" so the two can't be confused.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DefaultMember {
    /// The ctor decoded cleanly to a member name. Almost always `"Item"`
    /// (Roslyn's implicit-indexer marker) but the attribute also surfaces
    /// under `[IndexerName("…")]` and hand-rolled
    /// `[<assembly: ... DefaultMember("…")>]`-style usage.
    Named(String),
    /// The attribute was present but its payload was undecodable or
    /// unrepresentable. Reserved for a future decoder relaxation; today's
    /// decoder refuses loud rather than producing it. See
    /// [`Entity::default_member`] for the policy.
    Unknown,
}

/// A single
/// `[System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute]`
/// instance, projected from the ECMA-335 custom-attribute blob. The
/// attribute carries a `string featureName` constructor argument plus an
/// optional `IsOptional: bool` named property; both are kept verbatim.
///
/// Roslyn's mechanism for telling a consuming compiler "the IL you're
/// about to read uses language feature X — refuse the load if you don't
/// understand X". A modern compiler that recognises the feature name
/// handles its semantics; an older compiler that does not is expected to
/// refuse (or, when `is_optional == true`, warn).
///
/// The feature-name string is kept as a free-form `String` rather than
/// enumerated — Roslyn's well-known set
/// (`WellKnownMemberNames.CompilerFeatureRequiredFeatures`: `"RefStructs"`,
/// `"RequiredMembers"`, `"VirtualStaticsInInterfaces"`, …) grows with
/// each C# release, and consumers match on the string.
///
/// The attribute is declared `AllowMultiple = true`, so each projection
/// site carries a `Vec<CompilerFeatureRequired>` (empty when absent)
/// rather than an `Option`. The vec preserves CA-emission order.
///
/// Decoding is **refuse-loud**: a corrupted feature name has no value
/// (the whole payload IS the feature name, so there's no degraded
/// "presence-only" fallback like [`Obsolete`] / [`Experimental`] have).
/// The decoder errors out on null/extra ctor args and unexpected named
/// args.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CompilerFeatureRequired {
    pub feature: String,
    pub is_optional: bool,
}

/// A member of an [`Entity`]. Phase 3a–d cover methods, fields,
/// properties, and events.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Member {
    Method(MethodLike),
    Field(Field),
    Property(Property),
    Event(Event),
}

/// Marks a [`MethodLike`] that is actually an F# module-level `let` **value**
/// rather than a function. A module value compiles to a static property whose
/// getter holds the value's signature; the projector rebrands that getter as a
/// 0-parameter method (so it matches FCS's IL view — see
/// `project_fsharp_members`). Without this marker, that rebranded value is
/// indistinguishable from a genuine 0-parameter function (`let f () = …`): both
/// are 0-parameter methods. `is_mutable` records `let mutable` (the rebranded
/// property had a setter, dropped in the rebrand).
///
/// This marker is also the signal that the member's *IL* form is a property:
/// [`crate::display`] renders it `val [mutable] x: T` (not `unit -> T`), and
/// [`crate::doc_id`] keys its documentation-comment ID `P:` (not the method's
/// natural `M:`), matching the F# compiler's own `FSharp.Core.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ModuleValue {
    pub is_mutable: bool,
}

/// The source location of an F# binding, as recorded by the host CCU's
/// signature pickle: FCS's `Val.DefinitionRange` — the *implementation*
/// range ("used by Visual Studio" for navigation), which for an
/// `.fsi`-constrained assembly names the `.fs` file rather than the
/// signature (`p_ValData` pickles the `(val_range, DefinitionRange)` pair;
/// this is the second component, falling back to the first when the pair is
/// collapsed).
///
/// Conventions are FCS's `pos`/`range`: **1-based lines, 0-based columns**,
/// spanning exactly the binder identifier. `file` is the compile-time path —
/// for a deterministic (SourceLink) build it matches the portable PDB's
/// document names byte-for-byte, which is what lets a consumer resolve it to
/// embedded source or a SourceLink URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FsharpSourceRange {
    pub file: String,
    /// 1-based.
    pub start_line: u32,
    /// 0-based.
    pub start_column: u32,
    /// 1-based.
    pub end_line: u32,
    /// 0-based (exclusive).
    pub end_column: u32,
}

/// A method, constructor, or operator. The `is_constructor` flag
/// distinguishes the `.ctor` / `.cctor` cases from regular methods so
/// callers don't have to string-compare names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MethodLike {
    pub name: String,
    pub access: Access,
    pub signature: MethodSignature,
    /// The number of **F# argument groups** the method takes, when known —
    /// `Some(1)` for a provably single-group (tupled) method, `Some(n≥2)` for a
    /// curried one, `None` when unknown. A method compiled by a **non-F#
    /// assembly** (no host F# signature pickle) is always `Some(1)`: C# and VB
    /// cannot curry, so their flattened parameter list *is* the one argument
    /// group. An **F# assembly**'s methods are `None`, because the flattened
    /// ECMA MethodDef signature cannot distinguish a curried `member x.M a b`
    /// from a tupled `member x.M(a, b)` — both project to two parameters. The
    /// overload engine reads this: a call commits only when every candidate in
    /// the group is a provably single argument group (or takes ≤ 1 parameter),
    /// otherwise FCS's FS0816 curried-overload rule may type the call `obj`.
    /// See `docs/completed/ov-6.1-curry-detection-plan.md`.
    pub arg_group_count: Option<usize>,
    pub is_static: bool,
    pub is_virtual: bool,
    pub is_abstract: bool,
    /// `final` (`MethodAttributes` 0x0020): a virtual that cannot be overridden.
    /// With [`Self::is_newslot`] it exempts a subtype re-declaration from the
    /// F# override-dedup that collapses a virtual re-declaration into its base
    /// (overload method-group construction, OV-3).
    pub is_final: bool,
    /// `newslot` (`MethodAttributes` 0x0100): claims a fresh vtable slot rather
    /// than overriding a base slot — so a same-signature subtype method is a
    /// *new* virtual, not an override, and is not deduped against the base.
    pub is_newslot: bool,
    /// `hidebysig` (`MethodAttributes` 0x0080): name-hiding is by full signature
    /// (the C#/F# default), not by name alone. Informs the overload
    /// method-group hiding rule (OV-3).
    pub is_hide_by_sig: bool,
    pub is_constructor: bool,
    /// `true` when the method carries
    /// `[System.Runtime.CompilerServices.ExtensionAttribute]`. C# emits
    /// it on any `static` method whose first parameter is `this T`;
    /// F# emits it on `type Foo with member …` augmentations. Set by
    /// the importer alongside the typed kind bits; the raw attribute
    /// row is *not* duplicated into `custom_attrs`.
    pub is_extension_method: bool,
    /// Whether this member is an **F#-native extension member** — a
    /// `type T with member/static member M …` augmentation, which fsc compiles to a
    /// static of the declaring module — and *how well we know*
    /// ([`Augmentation`]).
    ///
    /// Distinct from [`Self::is_extension_method`], which means "instance-callable
    /// extension" — the CLR `[Extension]` attribute, or an F#-native *instance*
    /// augmentation (FCS's `IsInstanceMember` gate on the surface flag). A
    /// `type T with static member Create …` is an augmentation but not that:
    /// telling the overload engine it is instance-callable would be a lie, yet name
    /// resolution must still keep it out of scope (an augmentation is reachable only
    /// through the dot on its target — bare *and* module-qualified uses are FS0039,
    /// both fsi-verified). Hence two facts rather than one widened flag.
    pub augmentation: Augmentation,
    /// `Some` when this method is really an F# module-level `let` **value**, not
    /// a function — see [`ModuleValue`]. `None` for every genuine method
    /// (including a unit-taking module function `let f () = …`, which stays a
    /// 0-parameter method). Lets a consumer render `val x: T` for a value and
    /// `val f: unit -> T` for a unit-function, which their identical shapes
    /// otherwise can't distinguish.
    ///
    /// Its presence *also* asserts the **property emission shape** — a rebranded
    /// static property — which the XML doc-ID (`P:` vs `M:`) and the `val`
    /// rendering key off. That is why it does not cover the *generic* module
    /// values (`typeof<'T>`/`sizeof<'T>`): a CLR property cannot be generic, so
    /// fsc emits those as generic MethodDefs, and flagging them here would forge a
    /// `P:` doc-ID. [`Self::is_module_value_binding`] is the value-ness signal that
    /// spans both emission shapes.
    pub module_value: Option<ModuleValue>,
    /// `true` when the F# host pickle records this method as a **value** binding
    /// (zero F# argument groups), regardless of how fsc emitted it. In the pickle
    /// path it is a superset of [`Self::module_value`]`.is_some()`: it *also* covers a
    /// generic value like `typeof<'T>`/`sizeof<'T>`/`Unchecked.defaultof<'T>`,
    /// which — being generic — fsc emits as a generic MethodDef rather than the
    /// property [`Self::module_value`] marks. On the IL-heuristic path (no usable host
    /// pickle) it is always `false`, and [`Self::module_value`] alone carries the
    /// property-shaped values. `false` for genuine functions/members.
    ///
    /// Consumers classifying F# surface (the sema semantic-token layer) read this
    /// so a referenced generic module value is coloured a value, not a function;
    /// unlike [`Self::module_value`], it does not imply the property emission shape, so
    /// it is safe to set on a method without disturbing the doc-ID / `val` render.
    pub is_module_value_binding: bool,
    /// Where the F# binding this member came from is declared, from the host
    /// CCU's signature pickle ([`FsharpSourceRange`]). `Some` only on the
    /// authoritative-pickle path for module members whose claim group agreed
    /// on one range (the same unanimity posture as `source_name`); `None`
    /// everywhere else — non-F# assemblies, the IL-heuristic path, and
    /// members the pickle did not claim.
    ///
    /// Go-to-definition reads it when the PDB has no sequence point for the
    /// member's own MethodDef: an F# module *value* compiles to a static
    /// property whose getter merely reads the backing field (the initialiser
    /// lives in the module's `.cctor`), so the getter carries no sequence
    /// point and this pickled range is the only — and FCS's own — source
    /// location for the binding.
    /// Boxed: the range is consulted only at navigation time, and inline it
    /// would tip [`Member`] over clippy's large-variant threshold.
    pub definition_range: Option<Box<FsharpSourceRange>>,
    /// Formal method type parameters (the `M0`/`M1`/... typars referenced
    /// from the signature via `TypeRef::Var { is_method: true, .. }`).
    /// Empty for non-generic methods. The ECMA-335 generic-arity number
    /// equals this list's length.
    pub generic_parameters: Vec<TypeParameter>,
    /// `Some` when the method carries `[System.ObsoleteAttribute]`; same
    /// shape and decoding rules as [`Entity::obsolete`].
    pub obsolete: Option<Obsolete>,
    /// `Some` when the method carries
    /// `[System.Diagnostics.CodeAnalysis.ExperimentalAttribute]`; same
    /// shape and decoding rules as [`Entity::experimental`].
    pub experimental: Option<Experimental>,
    /// `true` when the method carries
    /// `[System.Diagnostics.CodeAnalysis.SetsRequiredMembersAttribute]`.
    /// C# 11 emits it on a constructor to declare that the body itself
    /// satisfies the type's `required` members, so callers don't have
    /// to do so via object-initialiser. Only meaningful when
    /// [`Self::is_constructor`] is `true` — the C# compiler enforces
    /// that target at the source level, and the importer surfaces the
    /// raw bit faithfully even if the IL is hand-rolled with the attr
    /// on a non-constructor method.
    pub sets_required_members: bool,
    /// Every `[System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute]`
    /// the method carries. Same shape and decoding rules as
    /// [`Entity::compiler_feature_required`]; see
    /// [`CompilerFeatureRequired`] for the per-instance shape.
    ///
    /// On constructors, the projected `Self::obsolete` is dropped
    /// whenever `feature == "RequiredMembers"` appears in this list:
    /// Roslyn pairs `[CompilerFeatureRequired("RequiredMembers")]` with a
    /// synthetic `[Obsolete(error: true)]` as a fallback signal for
    /// pre-C#-11 compilers, and surfacing the Obsolete would misclassify
    /// valid C# 11 callers as obsolete-error API users. The suppression
    /// reads from this already-decoded typed field rather than re-inspecting
    /// the raw CA list.
    pub compiler_feature_required: Vec<CompilerFeatureRequired>,
    /// The F# *source* name of this member, when it differs from the IL
    /// [`Self::name`]. `Some` exactly when the method carries
    /// `[Microsoft.FSharp.Core.CompilationSourceNameAttribute(string)]`, which
    /// the F# compiler emits whenever a value has `[<CompiledName>]` (see
    /// `dotnet/fsharp/src/Compiler/CodeGen/IlxGen.fs`, the `CompiledName` arm of
    /// the source-name-attribs computation): the IL method is renamed but the
    /// attribute preserves the F# identifier. For `printfn` the IL name is
    /// `PrintFormatLine` and `source_name` is `Some("printfn")`.
    ///
    /// `None` when the IL name *is* the source name (the common case — no
    /// `[<CompiledName>]`). Consumers that resolve F# source identifiers (the
    /// sema layer) match against `source_name.unwrap_or(name)`; the differential
    /// normaliser keeps comparing on the IL [`Self::name`] (fcs-dump emits each
    /// member's `CompiledName`), so this field is deliberately *not* part of
    /// that comparison. Operators (`+` ⇒ `op_Addition`) carry no such attribute
    /// — their source name is recovered by name-demangling, a separate concern.
    pub source_name: Option<String>,
    pub custom_attrs: Vec<CustomAttr>,
    /// The method's ECMA-335 metadata token: the `MethodDef` table tag
    /// (`0x06`) in the high byte and the 1-based row id in the low 24 bits
    /// (`0x0600_0000 | rid`). `0` for a method not backed by a `MethodDef` row
    /// (synthesised in tests). It correlates a member back to its metadata row —
    /// notably to index the portable PDB's parallel `MethodDebugInformation`
    /// table for source positions (go-to-definition). It names a *location in
    /// the metadata*, not a logical property, so the differential normaliser
    /// deliberately ignores it.
    pub metadata_token: u32,
    /// The interface members this method implements via ECMA-335 `MethodImpl`
    /// rows, classified by each row's *declaration* target — never parsed back
    /// out of [`Self::name`]. Each entry carries the implemented interface as
    /// a [`TypeRef`] (so type parameters are [`TypeRef::Var`], not bare
    /// identifiers) plus the interface member with its declared kind
    /// ([`ImplementedMember`]).
    ///
    /// For an *instance* method this is exactly the *explicit* interface
    /// implementations (an implicit instance impl binds by vtable slot
    /// matching, with no `MethodImpl` row). The IL [`Self::name`] of such a
    /// method is *conventionally* the constructed-interface-qualified string
    /// (`System.Collections.Generic.IDictionary<TKey,TValue>.get_Keys`), but
    /// that is a C#/F# compiler convention, not a CLR requirement — VB emits
    /// plain-named bodies via `Implements` — and is lossy for
    /// documentation-comment IDs (the interface's generic arguments must be
    /// rendered in a distinct dialect). A *static* interface member has no
    /// vtable slot, so its implementation is always wired through
    /// `MethodImpl`: a plain-named public static method here (C#11 generic
    /// math — `NFloat` satisfying `INumberBase<NFloat>.Parse`) is an
    /// *implicit* implementation, faithfully included.
    ///
    /// One body may satisfy several interface members (one `MethodImpl` row
    /// each — VB's `Implements IFoo.M, IBar.M`), hence a list, in
    /// `MethodImpl`-table order. Empty for an ordinary method (the
    /// overwhelming majority).
    pub implements: Vec<InterfaceMemberImpl>,
    /// `MethodImpl` rows naming this method as their body whose declaration
    /// reaches into another assembly and is *undecidable from this assembly
    /// alone*: the parent is neither in the implementing type's in-module
    /// interface closure nor a provable ancestor. Ordinary F#/VB output lands
    /// here — a member of an *inherited external* interface implemented
    /// through the derived interface's clause (F#'s `interface IDerived with
    /// member _.M()`, VB's `Implements IDerived` + `Sub Body() Implements
    /// IBase.M`) lists only `IDerived` in `InterfaceImpl` while declaring
    /// against `IBase::M` — and so does a C# covariant-return override
    /// targeting a *non-direct* external ancestor (Roslyn points the
    /// declaration at the original declarer). The two are indistinguishable
    /// in this assembly; a consumer holding the referenced assembly (is the
    /// parent an interface?) can finish the classification. Kept separate
    /// from [`Self::implements`] so that field stays *proven-only*.
    pub unclassified_impls: Vec<UnclassifiedMethodImpl>,
}

/// One in-assembly-undecidable `MethodImpl` row: the declaration's parent
/// type and the declaration method's raw name. See
/// [`MethodLike::unclassified_impls`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UnclassifiedMethodImpl {
    /// The declaration's parent type — external, so it carries the assembly
    /// identity a resolving consumer needs.
    pub parent: TypeRef,
    /// The declaration method's raw `Name`, verbatim (`M`, `get_Q`): its
    /// `MethodSemantics` is as unreachable as its parent's kind, exactly as
    /// for [`ImplementedMember::Unresolved`].
    pub member: String,
}

/// The structured form of one implemented interface member: which interface
/// member a [`MethodLike`]/[`Property`]/[`Event`] implements. See
/// [`MethodLike::implements`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct InterfaceMemberImpl {
    /// The implemented interface as a constructed type. For a generic interface
    /// the type arguments are the *implementing* type's parameters
    /// ([`TypeRef::Var`] with `is_method: false`) or concrete types, exactly as
    /// the `MethodImpl` row records them.
    pub interface: TypeRef,
    /// The member on the *interface* (never derived from the implementing
    /// member's own name, which need not match), with its declared kind.
    pub member: ImplementedMember,
}

/// The interface-side member a `MethodImpl` declaration names, classified by
/// what the declaration *is* per the interface's `MethodSemantics` table —
/// ECMA-335's only authority on which methods are accessors and of what.
/// Accessor naming (`get_P`) is a CLS convention, not a CLR rule, and it
/// misleads in both directions: an interface property `P` may have a getter
/// named `Read`, and an interface may declare an ordinary *method* named
/// `get_Q`. Carrying the kind here is what lets a consumer decide, e.g.,
/// whether the implemented member's documentation-comment ID is `M:`, `P:`,
/// or `E:`.
///
/// A `MethodImpl` declaration is always a *method* token; the property/event
/// variants arise because an accessor's `MethodImpl` is the only place an
/// explicitly-implemented interface property/event is wired, and the member a
/// consumer wants to name is then the owning property/event, not the accessor.
/// Every `MethodSemantics` role counts as an accessor — get/set/add/remove/
/// fire and the open-ended `Other` alike — and since the table does not make
/// `Method` unique, a declaration claimed by several properties/events yields
/// one [`InterfaceMemberImpl`] entry per owner. The kinds may legitimately
/// cross the implementing member's own kind: a class property's getter can
/// satisfy an ordinary interface method ([`ImplementedMember::Method`] inside
/// [`Property::implements`]), and a plain method can satisfy an interface
/// property's accessor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ImplementedMember {
    /// An ordinary interface method; the payload is its name (`Store`) —
    /// which may *look* like an accessor's (`get_Q`) without being one.
    Method(String),
    /// An accessor of an interface property; the payload is the owning
    /// property's own name (`Count` — or even `get_Value`, kept verbatim),
    /// resolved through the interface's `MethodSemantics`.
    Property(String),
    /// An accessor of an interface event; the payload is the owning event's
    /// own name (`Changed`), resolved through the interface's
    /// `MethodSemantics`.
    Event(String),
    /// A declaration that could not be followed to a `MethodDef` in the
    /// module being read — typically a `MemberRef` into another assembly,
    /// whose `MethodSemantics` is not locally readable. The payload is the
    /// declaration method's raw `Name`, verbatim (`get_Count`, `Dispose`):
    /// whether it is an accessor — and so which property/event it would name —
    /// is *not knowable* from this module alone. A consumer wanting the
    /// CLS-conventional reading (`get_Count` → property `Count`) must apply
    /// that guess itself, knowingly; this model never presents a guess as
    /// resolved data.
    Unresolved(String),
}

impl ImplementedMember {
    /// The payload name, whatever the kind: the method's name for
    /// [`Self::Method`], the owning member's name for [`Self::Property`] /
    /// [`Self::Event`], and the raw declaration name for
    /// [`Self::Unresolved`]. For anything kind-sensitive (doc-comment IDs,
    /// member lookup), match on the variant instead.
    pub fn name(&self) -> &str {
        match self {
            ImplementedMember::Method(n)
            | ImplementedMember::Property(n)
            | ImplementedMember::Event(n)
            | ImplementedMember::Unresolved(n) => n,
        }
    }
}

/// The "type" of a method — parameters and return — separated from method
/// identity so it can be reused by function-pointer types and delegate
/// invoke methods in later phases.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MethodSignature {
    pub parameters: Vec<Parameter>,
    pub return_type: TypeRef,
    /// Nullable-reference-type state of the return type (phase 4m.2).
    /// Decoded from `[NullableAttribute(byte)]` on the return
    /// `ParameterMetadata` row (sequence 0) when present, falling back to
    /// the enclosing method's `[NullableContextAttribute(byte)]`, then the
    /// enclosing type's. Reads as [`Nullability::Oblivious`] when the
    /// return type is a value type (which cannot carry nullable
    /// annotations) or when no attribute is in scope.
    pub return_nullability: Nullability,
}

/// The value of a compile-time constant default-parameter value. Most come
/// from an ECMA-335 `Constant` row; `decimal`/`DateTime` defaults cannot
/// (neither is a primitive `ELEMENT_TYPE`) and instead arrive as the
/// `[DecimalConstantAttribute]` / `[DateTimeConstantAttribute]` the compiler
/// emits alongside the `Optional` flag. Integer widths are collapsed to
/// [`Self::Int`] / [`Self::UInt`]; floats are stored as raw IEEE-754 bits so the
/// enclosing model stays `Eq`/`Hash` (recover the value with `f32::from_bits` /
/// `f64::from_bits`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ConstantValue {
    Bool(bool),
    Char(char),
    /// A signed integer (`I1`..`I8`, sign-extended), or an enum default's
    /// underlying integral value.
    Int(i64),
    /// An unsigned integer (`U1`..`U8`).
    UInt(u64),
    /// `R4`, as IEEE-754 bits (`f32::from_bits`).
    F32(u32),
    /// `R8`, as IEEE-754 bits (`f64::from_bits`).
    F64(u64),
    String(String),
    /// A `null` reference default (`ELEMENT_TYPE_CLASS` with a zero value).
    Null,
    /// A `System.Decimal` default, from `[DecimalConstantAttribute]`. The value
    /// is `(-1)^negative · mantissa · 10^-scale`: `mantissa` is the 96-bit
    /// integer (the attribute's `hi`/`mid`/`low` words combined), `scale` the
    /// number of fractional digits (0..=28 in valid metadata), `negative` the
    /// sign. The declared `scale` is preserved verbatim, so `1.50m` (scale 2)
    /// stays distinct from `1.5m` (scale 1).
    Decimal {
        negative: bool,
        scale: u8,
        mantissa: u128,
    },
    /// A `System.DateTime` default, from `[DateTimeConstantAttribute]`: the
    /// raw tick count (100 ns since 0001-01-01), exactly as the attribute
    /// records it.
    DateTime(i64),
}

/// Whether a parameter is required, optional, and — if optional — in which
/// dialect's sense. F# `?x` and a C# default value are *different* calling
/// conventions, so the projector keeps them apart (a consumer can render `?x: T`
/// for the F# form and `name: T = <value>` / `[<Optional>] name: T` for the .NET
/// form).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParamDefault {
    /// A required parameter.
    None,
    /// F# `?x: T` — the parameter carries
    /// `[Microsoft.FSharp.Core.OptionalArgumentAttribute]` and is typed
    /// `FSharpOption<T>`. The caller may omit it (passing `None`).
    FSharpOptional,
    /// A .NET optional parameter: the ECMA-335 `Optional` flag (`0x0010`).
    /// `Some` carries the `Constant` default value (a C# `x = <value>`); `None`
    /// is an `[Optional]` / COM optional with no value.
    Optional(Option<ConstantValue>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Parameter {
    /// Some assemblies (or stripped pdbs) emit unnamed parameters; in those
    /// cases the importer leaves `name` as `None` rather than fabricate
    /// `arg0`/`arg1`. The LSP can surface them as positional.
    pub name: Option<String>,
    pub ty: TypeRef,
    pub is_byref: bool,
    pub is_out: bool,
    /// `true` when the byref is *read-only* — C#'s `in` / `ref readonly`
    /// parameter, F#'s `inref<'T>`: the callee may read through the reference
    /// but not write. Set from either metadata encoding of that fact (the
    /// `modreq(InAttribute)` over the byref, or an `[IsReadOnly]` /
    /// `[RequiresLocation]` attribute on the parameter — see
    /// [`TypeRef::ByRef`]'s `readonly`, which is the same bit for a
    /// field/property/return). Never set without [`Self::is_byref`]; an `out`
    /// parameter is a plain (writable) byref, so this and [`Self::is_out`] never
    /// both hold for compiler-emitted metadata.
    ///
    /// It lives here rather than on the type because a parameter's byref
    /// wrapper is itself a flag ([`Self::is_byref`]) and not part of
    /// [`Self::ty`].
    pub is_readonly_ref: bool,
    /// Whether the parameter is required, an F# `?optional`, or a .NET optional
    /// (C# default). Replaces the older `has_default: bool`, which conflated the
    /// F# and .NET forms.
    pub default: ParamDefault,
    /// `true` when the parameter carries
    /// `[System.ParamArrayAttribute]`. C# emits it on the final
    /// `params T[]` parameter of a method; F# emits it on a `[<ParamArray>]`-
    /// annotated value. The trailing array type is unchanged — only the
    /// flag bit distinguishes a `params int[]` call site from a plain
    /// `int[]` one. Set by the importer alongside the byref/out flags.
    pub is_param_array: bool,
    /// Nullable-reference-type state of the parameter type (phase 4m.2).
    /// Decoded from `[NullableAttribute(byte)]` on the parameter row when
    /// present, falling back to the enclosing method's
    /// `[NullableContextAttribute(byte)]`, then the enclosing type's.
    /// Reads as [`Nullability::Oblivious`] when the parameter type is a
    /// value type (which cannot carry nullable annotations) or when no
    /// attribute is in scope.
    pub nullability: Nullability,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Field {
    pub name: String,
    pub access: Access,
    pub ty: TypeRef,
    pub is_static: bool,
    pub is_init_only: bool,
    /// `true` when the field's type carried
    /// `modreq(System.Runtime.CompilerServices.IsVolatile)` — a C# `volatile`
    /// field. The modifier is *the* encoding of volatility (there is no flag
    /// bit), it is `required`, and it changes the memory model the field must
    /// be accessed under, so it is projected rather than dropped. The projected
    /// [`Self::ty`] is the underlying type with the modifier peeled off.
    pub is_volatile: bool,
    /// `true` when the field is a compile-time constant — the ECMA-335 `Literal`
    /// flag (`0x0040`), which C# `const` and F# `[<Literal>]` emit, and which
    /// backs every enum case. A literal is **not** assignable, yet — unlike a
    /// `readonly`/`initonly` field — it does not set [`Self::is_init_only`]
    /// (the CLR uses `Literal` instead), so a consumer must check this flag to
    /// avoid treating a constant as mutable.
    pub is_literal: bool,
    /// `true` when the field carries
    /// `[System.Runtime.CompilerServices.RequiredMemberAttribute]`. C# 11
    /// emits it on every field declared with the `required` keyword;
    /// callers must populate the field through an object initialiser
    /// (unless the constructor opts out via
    /// [`MethodLike::sets_required_members`]). F# has no equivalent
    /// keyword, so this never fires on F#-emitted fields.
    pub is_required: bool,
    /// Every `[System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute]`
    /// the field carries. Same shape and decoding rules as
    /// [`Entity::compiler_feature_required`]; see
    /// [`CompilerFeatureRequired`] for the per-instance shape.
    pub compiler_feature_required: Vec<CompilerFeatureRequired>,
    /// Nullable-reference-type state of the field type (phase 4m.2).
    /// Decoded from `[NullableAttribute(byte)]` on the field row when
    /// present, falling back to the enclosing type's
    /// `[NullableContextAttribute(byte)]`. Reads as
    /// [`Nullability::Oblivious`] when the field type is a value type
    /// (which cannot carry nullable annotations) or when no attribute is
    /// in scope.
    pub nullability: Nullability,
    pub custom_attrs: Vec<CustomAttr>,
}

/// One index parameter of an indexer property (the `i` in `this[int i]`).
///
/// Recovered from the property's **getter** parameter (fallback: the setter's
/// parameters minus the trailing `value`), since ECMA-335 keeps the parameter
/// *name* on the accessor method, not the property signature. `name` is `None`
/// when the accessor row carries no name (stripped metadata); `ty` bundles the
/// index type with its [`Nullability`] exactly as for any other position.
///
/// An index parameter is never `byref`/`out` and has no default value — the
/// projector rejects a byref index parameter — so, unlike a method
/// [`Parameter`], only the name and type are modelled.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct IndexParameter {
    pub name: Option<String>,
    pub ty: NullableType,
    /// `true` when the index parameter carries `[System.ParamArrayAttribute]` — a
    /// `params`/`[<ParamArray>]` indexer (`this[params int[] xs]`). Threaded from
    /// the accessor's [`Parameter::is_param_array`] so the indexer hover can
    /// surface the marker; see [`Parameter::is_param_array`].
    pub is_param_array: bool,
}

/// A property, projected to the LSP-shaped surface rather than the raw
/// `PropertyDef` + `MethodSemantics` linkage ECMA-335 carries.
///
/// `access` is the least-restrictive of the getter/setter visibilities (a
/// `protected internal` getter and `protected` setter together produce a
/// `protected internal` property), matching how C# tooling surfaces
/// property visibility.
///
/// [`Self::parameters`] carries the index dimension for indexers
/// (properties with parameters — the IL marker behind C#'s `this[...]`);
/// it is empty for an ordinary property.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Property {
    pub name: String,
    pub access: Access,
    pub ty: TypeRef,
    /// Index parameters (name + type + nullable-reference-type state), in
    /// declaration order. Empty for a non-indexer property. ECMA-335 keeps
    /// parameter names, `out`/`in`, and default values on the accessor
    /// methods, not on the property signature — and the per-parameter
    /// nullness lives there too (phase B3): the property signature carries
    /// neither the outer annotation nor composite inner annotations, so the
    /// authoritative source of the index name, type, and nullability is
    /// the **getter** parameter (fallback: the setter's parameters minus the
    /// trailing value). Both projectors read the index dimension from that
    /// accessor parameter to stay in lockstep. See [`IndexParameter`].
    pub parameters: Vec<IndexParameter>,
    pub is_static: bool,
    pub has_getter: bool,
    pub has_setter: bool,
    /// The **getter accessor's own** accessibility, `None` for a write-only
    /// property (no getter). Distinct from [`Self::access`], which is the
    /// *least-restrictive* of the two accessors (`max_access(getter, setter)`):
    /// a `public T P { public set; private get; }` reports `access == Public`
    /// (from the setter) yet has a `Private` getter. A consumer that types a
    /// **read** (`recv.P`) must gate on *this* field — the read goes through the
    /// getter — not on the property-level `access`, which the setter can inflate.
    pub getter_access: Option<Access>,
    /// `true` when the property carries
    /// `[System.Runtime.CompilerServices.RequiredMemberAttribute]`. Same
    /// rule as [`Field::is_required`]: C# 11 emits it on every
    /// auto-property declared with the `required` keyword. F# has no
    /// equivalent keyword.
    pub is_required: bool,
    /// Every `[System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute]`
    /// the property carries. Same shape and decoding rules as
    /// [`Entity::compiler_feature_required`]; see
    /// [`CompilerFeatureRequired`] for the per-instance shape.
    pub compiler_feature_required: Vec<CompilerFeatureRequired>,
    /// Nullable-reference-type state of the property type (phase 4m.2).
    /// Decoded from `[NullableAttribute(byte)]` on the property row when
    /// present, falling back to the enclosing type's
    /// `[NullableContextAttribute(byte)]`. Reads as
    /// [`Nullability::Oblivious`] when the property type is a value type
    /// (which cannot carry nullable annotations) or when no attribute is
    /// in scope.
    pub nullability: Nullability,
    pub custom_attrs: Vec<CustomAttr>,
    /// The interface members this property's accessors implement — the same
    /// notion as [`MethodLike::implements`] (explicit impls for instance
    /// properties; explicit *and* implicit impls for static interface
    /// properties), unioned over the `MethodImpl` rows on both accessors and
    /// deduplicated (a get+set interface property resolved through
    /// `MethodSemantics` contributes one row per accessor that collapses to a
    /// single [`ImplementedMember::Property`]; the getter and setter may also
    /// satisfy *different* interfaces — VB's `Property P … Implements IRead.P,
    /// IWrite.P`). Usually [`ImplementedMember::Property`], but a cross-kind
    /// mapping ([`ImplementedMember::Method`]: this property's getter
    /// satisfying an ordinary interface method) is representable IL, and an
    /// external interface's accessors stay [`ImplementedMember::Unresolved`] —
    /// one entry per accessor, *never* deduplicated even when the raw names
    /// coincide (two same-named entries are two distinct overloads: §II.22.27
    /// forbids duplicate rows), since without the referenced assembly's
    /// `MethodSemantics` they cannot be proven to name one property. Empty for
    /// an ordinary property.
    pub implements: Vec<InterfaceMemberImpl>,
    /// The accessors' in-assembly-undecidable `MethodImpl` rows, unioned
    /// *without* dedup — identical entries always denote distinct
    /// declarations (§II.22.27 forbids duplicate rows, and name equality
    /// cannot prove two external declarations are one member). See
    /// [`MethodLike::unclassified_impls`].
    pub unclassified_impls: Vec<UnclassifiedMethodImpl>,
}

/// An event, projected to the LSP-shaped surface rather than the raw
/// `EventDef` + `MethodSemantics` linkage ECMA-335 carries.
///
/// Events have no accessibility bit of their own — ECMA-335 II.18 stores
/// the access on each accessor (`add` / `remove` / `fire`) as a regular
/// method-access bit. We surface the least-restrictive of `add` and
/// `remove` (`fire` is observed-only, not part of the public surface),
/// matching how C# tooling renders an event's visibility and parallel to
/// how `Property` collapses getter/setter access.
///
/// `delegate_type` is the type of a handler — typically a
/// `System.EventHandler` instantiation. ECMA-335 II.22.13 permits the
/// `EventType` slot to be null; we reject that case at projection time
/// because no real-world compiler emits one and the model has no slot
/// for a "typeless" event.
///
/// `has_fire` records whether the metadata carries an explicit `fire`
/// (raise) accessor. C# never emits one; ILAsm and managed C++ can.
/// `add` and `remove` are mandatory per ECMA-335 and are therefore not
/// flagged — their presence is implied by the event existing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Event {
    pub name: String,
    pub access: Access,
    pub delegate_type: TypeRef,
    pub is_static: bool,
    pub has_fire: bool,
    /// Nullable-reference-type state of the delegate type (phase 4m.2).
    /// Decoded from `[NullableAttribute(byte)]` on the event row when
    /// present, falling back to the enclosing type's
    /// `[NullableContextAttribute(byte)]`. Reads as
    /// [`Nullability::Oblivious`] when no attribute is in scope. Delegate
    /// types are always reference types, so the value-type gate is moot.
    pub nullability: Nullability,
    pub custom_attrs: Vec<CustomAttr>,
    /// The interface members this event's accessors implement — the same
    /// notion as [`MethodLike::implements`], unioned over the `MethodImpl`
    /// rows on the `add`, `remove`, *and* `fire` accessors and deduplicated
    /// (see [`Property::implements`] for the kind and dedup story). Empty for
    /// an ordinary event.
    pub implements: Vec<InterfaceMemberImpl>,
    /// The accessors' in-assembly-undecidable `MethodImpl` rows, unioned
    /// *without* dedup — identical entries always denote distinct
    /// declarations (§II.22.27 forbids duplicate rows, and name equality
    /// cannot prove two external declarations are one member). See
    /// [`MethodLike::unclassified_impls`].
    pub unclassified_impls: Vec<UnclassifiedMethodImpl>,
}

/// A custom attribute the importer didn't classify into a typed field on
/// its containing entity/member. `blob` is the raw ECMA-335 `CustomAttribute`
/// blob (Partition II §23.3); consumers must not parse it themselves —
/// future phases of the importer will decode more attributes and shrink
/// this bag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CustomAttr {
    pub attribute_type: TypeRef,
    pub blob: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::SkippedProjectionItem;

    fn ns(name: &str) -> Vec<String> {
        SkippedProjectionItem {
            name: name.to_string(),
            reason: String::new(),
        }
        .enclosing_namespace()
    }

    #[test]
    fn enclosing_namespace_parses_the_fqn() {
        // A namespaced type → its namespace segments.
        assert_eq!(ns("System.String"), vec!["System".to_string()]);
        assert_eq!(
            ns("System.Collections.Generic.List"),
            vec![
                "System".to_string(),
                "Collections".to_string(),
                "Generic".to_string()
            ]
        );
        // The generic-arity suffix is stripped before splitting.
        assert_eq!(ns("System.IParsable`1"), vec!["System".to_string()]);
        assert_eq!(
            ns("System.Numerics.IAdditionOperators`3"),
            vec!["System".to_string(), "Numerics".to_string()]
        );
        // A nested type shares its top-level's namespace (the `/Inner` tail is dropped).
        assert_eq!(ns("System.Outer/Inner"), vec!["System".to_string()]);
        // A root-namespace type → the empty namespace.
        assert_eq!(ns("Foo"), Vec::<String>::new());
        assert_eq!(ns("Foo`1"), Vec::<String>::new());
    }
}
