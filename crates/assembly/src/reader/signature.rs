//! The signature decoder — the orthogonal core.
//!
//! A single recursive function, [`decode_type`], turns an ECMA-335 `Type`
//! production (II.23.2.12) into the owned [`TypeSig`] DU. Everything that can be
//! a `TypeDefOrRef` or `TypeSpec` — a field type, a parameter, a generic
//! argument, `extends`/`implements`, a generic constraint — funnels through it.
//!
//! It is pure: given the blob bytes and a table context for resolving coded
//! tokens into handles, it returns a `TypeSig` or a typed [`SigError`]. No IO,
//! no callbacks. Anything outside the supported subset is refused loudly rather
//! than fabricated.

use super::cursor::Cursor;
use super::ids::{TypeDefId, TypeRefId};

/// A primitive (built-in) type. `Void` and `TypedByRef` are deliberately absent:
/// `Void` lives only at the return-type boundary (a later stage's `RetType`),
/// and `ELEMENT_TYPE_TYPEDBYREF` decodes to the dedicated [`TypeSig::TypedByRef`]
/// (FCS's `ILType.Value(System.TypedReference)`), not a primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Primitive {
    Boolean,
    Char,
    Int8,
    UInt8,
    Int16,
    UInt16,
    Int32,
    UInt32,
    Int64,
    UInt64,
    Float32,
    Float64,
    IntPtr,
    UIntPtr,
    String,
    Object,
}

/// Whether a named type was tagged as a class or a value type. Only the
/// signature forms (`ELEMENT_TYPE_CLASS`/`_VALUETYPE`) record this; bare
/// `TypeDefOrRef` tokens do not (see [`TypeSig::Named`]'s `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedKind {
    Class,
    ValueType,
}

/// A resolved named-type reference: into this image's `TypeDef`s, or its
/// `TypeRef`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeScope {
    Definition(TypeDefId),
    Reference(TypeRefId),
}

/// One ECMA-335 `CustomMod` (II.23.2.7): the `CMOD_REQD`/`CMOD_OPT` byte plus
/// the modifier type's `TypeDefOrRef`.
///
/// `required` is the whole difference between the two, and it is a *policy*
/// difference, not a structural one (II.7.1.1): an optional modifier may be
/// ignored by a tool that does not understand it, a required one may not. The
/// decoder keeps both faithfully and lets the projector act on that rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CustomMod {
    pub required: bool,
    pub modifier: TypeScope,
}

/// A `Type` *in a position* — ECMA-335's own shape for every place a type can
/// appear in a signature: `CustomMod* Type` (II.23.2.7 and the `Param` /
/// `RetType` / `FieldSig` / `PTR` / `SZARRAY` productions).
///
/// The modifiers are a **prefix run on the position**, not a type former, and
/// this type says so. That is the whole point of it. The obvious alternative —
/// a `TypeSig::Modified { modifier, inner }` wrapper variant — makes a modifier
/// a *node*, and a node in front of a type hides that type's head: every guard
/// written as `matches!(sig, TypeSig::ByRef(_))` silently stops firing when a
/// modifier is present, while remaining perfectly well-typed. That is not a
/// hypothetical: it shipped five times (four found in review, one by the
/// metamorphic probe). With the run beside the type instead of in front of it,
/// `mt.ty` *is* the head — a modifier cannot get between a guard and the thing
/// it guards, because there is nowhere for it to sit.
///
/// The policy on the run (II.7.1.1 — drop an unrecognised `modopt`, refuse an
/// unrecognised `modreq`) needs type *names*, so it lives in the projector; see
/// `Ecma335Assembly::classify_mods`. `mods` is in signature order, outermost
/// first, and is empty for the overwhelming majority of positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModifiedType {
    pub mods: Vec<CustomMod>,
    pub ty: TypeSig,
}

impl ModifiedType {
    /// A position with no modifiers — the common case, and what every
    /// hand-constructed test signature wants.
    pub(crate) fn plain(ty: TypeSig) -> Self {
        Self {
            mods: Vec::new(),
            ty,
        }
    }
}

impl From<TypeSig> for ModifiedType {
    fn from(ty: TypeSig) -> Self {
        Self::plain(ty)
    }
}

/// A decoded ECMA-335 `Type`. Any *modifiers* on it belong to the position it
/// sits in — see [`ModifiedType`] — so every recursive slot below is a
/// `ModifiedType`, never a bare `TypeSig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeSig {
    Primitive(Primitive),
    /// `ELEMENT_TYPE_CLASS`/`_VALUETYPE` (`kind = Some`), or a bare
    /// `TypeDefOrRef` token from `extends`/`implements`/a constraint
    /// (`kind = None`, since the token carries no class-vs-valuetype bit).
    Named {
        kind: Option<NamedKind>,
        scope: TypeScope,
    },
    /// `ELEMENT_TYPE_GENERICINST` — an instantiated generic type.
    Generic {
        kind: Option<NamedKind>,
        scope: TypeScope,
        args: Vec<ModifiedType>,
    },
    /// `!n` — enclosing-type generic parameter.
    TypeVar(u32),
    /// `!!n` — enclosing-method generic parameter.
    MethodVar(u32),
    /// `T[]` — single-dimensional zero-based array.
    SzArray(Box<ModifiedType>),
    /// `ELEMENT_TYPE_ARRAY` — a general (multi-dimensional, or non-zero-based)
    /// array: `T[,]`, `T[*]`, `T[2..5, *]`, … The full `ArrayShape`
    /// (II.23.2.13) is decoded: `rank`, the per-dimension fixed `sizes`, and
    /// the per-dimension signed `lower_bounds`. The shape is carried verbatim
    /// (not consumed-and-discarded) so the projected model stays faithful for
    /// any consumer, not just the bounds-agnostic LSP. `sizes`/`lower_bounds`
    /// are each at most `rank` long.
    Array {
        element: Box<ModifiedType>,
        rank: u32,
        sizes: Vec<u32>,
        lower_bounds: Vec<i32>,
    },
    /// `T*` — unmanaged pointer (`ELEMENT_TYPE_PTR`). `Some(pointee)` for a
    /// typed pointer (F#'s `nativeptr<'T>`, C#'s `T*`); `None` for `void*`
    /// (`PTR VOID`, F#'s `voidptr`), the one position where a `void` pointee is
    /// legal. `PTR CustomMod* VOID` (a modified void pointer) is not modelled
    /// and still surfaces as [`SigError::UnexpectedVoid`].
    Ptr(Option<Box<ModifiedType>>),
    /// `T&` — managed reference.
    ByRef(Box<ModifiedType>),
    /// `ELEMENT_TYPE_TYPEDBYREF` — the built-in `System.TypedReference` bundle
    /// (`__makeref`/`__refvalue`, `ArgIterator.GetNextArg`). Unlike a named type
    /// it carries no `TypeDefOrRef` token; the single element byte *is* the
    /// type. FCS imports it as `ILType.Value(System.TypedReference)`
    /// (`ilread.fs:2671`), and the projector mirrors that, surfacing it as the
    /// `System.TypedReference` value type rather than a distinct model node.
    TypedByRef,
}

/// A signature element the decoder refuses rather than fabricating a value for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SigError {
    /// An `ELEMENT_TYPE_*` byte outside the supported subset (`FNPTR`,
    /// `PINNED`, `SENTINEL`, …), or a
    /// `MethodDefSig` calling convention this reader does not project (anything
    /// other than `DEFAULT`/`VARARG`/`GENERIC`, e.g. the native interop
    /// conventions). Carries the offending byte.
    UnsupportedElement(u8),
    /// `ELEMENT_TYPE_VOID` where a real type was expected.
    UnexpectedVoid,
    /// The blob ended mid-type.
    Truncated,
    /// A `TypeDefOrRef` coded token was malformed: RID 0, out of range, or a
    /// `TypeSpec`/reserved tag where a `TypeDef`/`TypeRef` was required. Also
    /// used when a `FieldSig`/`PropertySig` does not begin with its expected
    /// `FIELD`/`PROPERTY` sentinel byte.
    BadToken,
    /// The `Type` nested deeper than [`MAX_DEPTH`]. Each `SZARRAY`/`BYREF`/
    /// `modreq`/`GENERICINST` arg recurses, and a blob can prefix arbitrarily
    /// many such bytes; this bounds the recursion so a hostile blob cannot
    /// exhaust the stack.
    TooDeep,
}

/// The deepest a `Type` may nest before [`decode`] refuses it. Each level
/// consumes at least one element-type byte, so a blob of length *n* could
/// otherwise drive *n* stack frames deep; this cap (far above anything a real
/// compiler emits, far below stack exhaustion) keeps the decoder total.
const MAX_DEPTH: u32 = 128;

/// The table context the decoder needs to resolve a `TypeDefOrRef` coded token
/// into a [`TypeScope`] handle: the row counts of the `TypeDef`/`TypeRef`
/// tables. Later stages pass the real counts from the parsed metadata.
pub(crate) struct ImageTables {
    pub type_def_rows: u32,
    pub type_ref_rows: u32,
}

/// ECMA-335 II.23.2.3 calling-convention byte flags (the leading byte of a
/// `MethodDefSig`/`FieldSig`/`PropertySig`).
mod conv {
    /// Low-nibble calling convention `DEFAULT`.
    pub(super) const DEFAULT: u8 = 0x00;
    /// Low-nibble calling convention `VARARG`.
    pub(super) const VARARG: u8 = 0x05;
    /// `FIELD` sentinel — the leading byte of a `FieldSig`.
    pub(super) const FIELD: u8 = 0x06;
    /// `PROPERTY` sentinel — the leading byte of a `PropertySig`.
    pub(super) const PROPERTY: u8 = 0x08;
    /// `GENERIC` calling convention — the method is generic; a generic-parameter
    /// count follows. This is a distinct value of the convention field, not a
    /// flag OR-ed onto another convention.
    pub(super) const GENERIC: u8 = 0x10;
    /// `HASTHIS` flag — the method has an implicit `this` parameter.
    pub(super) const HASTHIS: u8 = 0x20;
    /// `EXPLICITTHIS` flag — `this` is passed explicitly as the first signature
    /// parameter.
    pub(super) const EXPLICITTHIS: u8 = 0x40;
    /// The calling-convention field: the low 5 bits, isolating the convention
    /// value (`DEFAULT`/`VARARG`/`GENERIC`/the `FIELD`/`PROPERTY` sentinels) from
    /// the `HASTHIS`/`EXPLICITTHIS` flags above it.
    pub(super) const CONV_MASK: u8 = 0x1F;
}

/// ECMA-335 II.23.1.16 element-type bytes (the supported and refused subset).
mod elem {
    pub(super) const VOID: u8 = 0x01;
    pub(super) const BOOLEAN: u8 = 0x02;
    pub(super) const CHAR: u8 = 0x03;
    pub(super) const I1: u8 = 0x04;
    pub(super) const U1: u8 = 0x05;
    pub(super) const I2: u8 = 0x06;
    pub(super) const U2: u8 = 0x07;
    pub(super) const I4: u8 = 0x08;
    pub(super) const U4: u8 = 0x09;
    pub(super) const I8: u8 = 0x0a;
    pub(super) const U8: u8 = 0x0b;
    pub(super) const R4: u8 = 0x0c;
    pub(super) const R8: u8 = 0x0d;
    pub(super) const STRING: u8 = 0x0e;
    pub(super) const PTR: u8 = 0x0f;
    pub(super) const BYREF: u8 = 0x10;
    pub(super) const VALUETYPE: u8 = 0x11;
    pub(super) const CLASS: u8 = 0x12;
    pub(super) const VAR: u8 = 0x13;
    pub(super) const ARRAY: u8 = 0x14;
    pub(super) const GENERICINST: u8 = 0x15;
    pub(super) const TYPEDBYREF: u8 = 0x16;
    pub(super) const I: u8 = 0x18;
    pub(super) const U: u8 = 0x19;
    pub(super) const OBJECT: u8 = 0x1c;
    pub(super) const SZARRAY: u8 = 0x1d;
    pub(super) const MVAR: u8 = 0x1e;
    pub(super) const CMOD_REQD: u8 = 0x1f;
    pub(super) const CMOD_OPT: u8 = 0x20;
}

/// Resolve an ECMA-335 II.23.2.8 `TypeDefOrRef` coded token (2-bit tag + 1-based
/// RID) into a [`TypeScope`]. Only `TypeDef` (tag 0) and `TypeRef` (tag 1) are
/// accepted; `TypeSpec` (tag 2) and the reserved tag are refused.
pub(crate) fn resolve_token(token: u32, tables: &ImageTables) -> Result<TypeScope, SigError> {
    let tag = token & 0b11;
    let rid = token >> 2;
    if rid == 0 {
        return Err(SigError::BadToken);
    }
    let index = rid - 1;
    match tag {
        0 if index < tables.type_def_rows => Ok(TypeScope::Definition(TypeDefId(index))),
        1 if index < tables.type_ref_rows => Ok(TypeScope::Reference(TypeRefId(index))),
        0 | 1 => Err(SigError::BadToken), // in-range tag, out-of-range RID
        _ => Err(SigError::BadToken),     // TypeSpec / reserved
    }
}

/// Decode a single position (`CustomMod* Type`) from the front of `blob`.
/// Trailing bytes are ignored; callers that need to decode a sequence use the
/// multi-type machinery in a later stage.
pub(crate) fn decode_type(blob: &[u8], tables: &ImageTables) -> Result<ModifiedType, SigError> {
    let mut c = Cursor::new(blob);
    decode(&mut c, tables, 0)
}

/// Decode `CustomMod* Type` — a type together with the modifier run in front of
/// it. Every recursive child slot goes through here, so a modifier is accepted
/// wherever a `Type` may start (II.23.2.12 spells that out for `PTR`/`SZARRAY`;
/// C++/CLI puts them in generic arguments too) and always lands *beside* the
/// type it modifies rather than in front of its head.
fn decode(c: &mut Cursor, tables: &ImageTables, depth: u32) -> Result<ModifiedType, SigError> {
    let mods = read_mods(c, tables, depth)?;
    // A modifier costs one nesting level, exactly as it did when each one was a
    // wrapper node: the bound that stops a hostile run of `SZARRAY`/`BYREF`
    // bytes must equally stop a hostile run of `CMOD_REQD`/`CMOD_OPT`, refusing
    // before scanning or allocating across a huge blob.
    let ty = decode_element(c, tables, depth + mods.len() as u32)?;
    Ok(ModifiedType { mods, ty })
}

/// Read the (usually empty) `CustomMod*` run at the cursor. Which modifier each
/// one *is* needs type names, so classification is deferred to the projector;
/// here the token is preserved, never discarded.
fn read_mods(c: &mut Cursor, tables: &ImageTables, depth: u32) -> Result<Vec<CustomMod>, SigError> {
    let mut mods = Vec::new();
    while matches!(c.peek_u8(), Some(elem::CMOD_REQD | elem::CMOD_OPT)) {
        if depth + mods.len() as u32 >= MAX_DEPTH {
            return Err(SigError::TooDeep);
        }
        let required = c.read_u8() == Some(elem::CMOD_REQD);
        mods.push(CustomMod {
            required,
            modifier: read_scope(c, tables)?,
        });
    }
    Ok(mods)
}

/// Decode the `Type` proper — the element byte and its payload. The modifier run
/// that may precede it has already been consumed by [`decode`].
fn decode_element(c: &mut Cursor, tables: &ImageTables, depth: u32) -> Result<TypeSig, SigError> {
    if depth >= MAX_DEPTH {
        return Err(SigError::TooDeep);
    }
    let byte = c.read_u8().ok_or(SigError::Truncated)?;
    match byte {
        elem::BOOLEAN => Ok(TypeSig::Primitive(Primitive::Boolean)),
        elem::CHAR => Ok(TypeSig::Primitive(Primitive::Char)),
        elem::I1 => Ok(TypeSig::Primitive(Primitive::Int8)),
        elem::U1 => Ok(TypeSig::Primitive(Primitive::UInt8)),
        elem::I2 => Ok(TypeSig::Primitive(Primitive::Int16)),
        elem::U2 => Ok(TypeSig::Primitive(Primitive::UInt16)),
        elem::I4 => Ok(TypeSig::Primitive(Primitive::Int32)),
        elem::U4 => Ok(TypeSig::Primitive(Primitive::UInt32)),
        elem::I8 => Ok(TypeSig::Primitive(Primitive::Int64)),
        elem::U8 => Ok(TypeSig::Primitive(Primitive::UInt64)),
        elem::R4 => Ok(TypeSig::Primitive(Primitive::Float32)),
        elem::R8 => Ok(TypeSig::Primitive(Primitive::Float64)),
        elem::I => Ok(TypeSig::Primitive(Primitive::IntPtr)),
        elem::U => Ok(TypeSig::Primitive(Primitive::UIntPtr)),
        elem::STRING => Ok(TypeSig::Primitive(Primitive::String)),
        elem::OBJECT => Ok(TypeSig::Primitive(Primitive::Object)),

        // A token-free built-in: the single byte *is* the type. FCS imports it
        // as `ILType.Value(System.TypedReference)` (`ilread.fs:2671`).
        elem::TYPEDBYREF => Ok(TypeSig::TypedByRef),

        elem::VOID => Err(SigError::UnexpectedVoid),

        elem::CLASS => Ok(TypeSig::Named {
            kind: Some(NamedKind::Class),
            scope: read_scope(c, tables)?,
        }),
        elem::VALUETYPE => Ok(TypeSig::Named {
            kind: Some(NamedKind::ValueType),
            scope: read_scope(c, tables)?,
        }),

        elem::VAR => Ok(TypeSig::TypeVar(
            c.read_compressed_u32().ok_or(SigError::Truncated)?,
        )),
        elem::MVAR => Ok(TypeSig::MethodVar(
            c.read_compressed_u32().ok_or(SigError::Truncated)?,
        )),

        elem::SZARRAY => Ok(TypeSig::SzArray(Box::new(decode(c, tables, depth + 1)?))),
        elem::ARRAY => {
            // II.23.2.13: `ARRAY Type ArrayShape`, where
            // `ArrayShape = Rank NumSizes Size* NumLoBounds LoBound*`. The whole
            // shape is decoded and carried (`TypeSig::Array` →
            // [`crate::TypeRef::Array`]); a bounded array (`int[2..5, *]`) keeps
            // its sizes/lower-bounds rather than being flattened or refused, so
            // no consumer is silently handed an approximation.
            let element = Box::new(decode(c, tables, depth + 1)?);
            let rank = c.read_compressed_u32().ok_or(SigError::Truncated)?;
            // ECMA-335 II.23.2.13: `Rank` is a positive integer. A zero rank is
            // malformed metadata — refuse rather than fabricate an impossible
            // `Array { rank: 0 }` that would render like a plain `[]`.
            if rank == 0 {
                return Err(SigError::BadToken);
            }
            // `NumSizes`/`NumLoBounds` are each at most `rank` (II.23.2.13), but
            // both are attacker-controlled — never pre-allocate from them. Each
            // entry consumes >= 1 byte, so the push loop is bounded by the blob
            // length; the `> rank` check is spec-conformance, not a safety bound.
            let num_sizes = c.read_compressed_u32().ok_or(SigError::Truncated)?;
            if num_sizes > rank {
                return Err(SigError::BadToken);
            }
            let mut sizes = Vec::new();
            for _ in 0..num_sizes {
                sizes.push(c.read_compressed_u32().ok_or(SigError::Truncated)?);
            }
            let num_lo_bounds = c.read_compressed_u32().ok_or(SigError::Truncated)?;
            if num_lo_bounds > rank {
                return Err(SigError::BadToken);
            }
            let mut lower_bounds = Vec::new();
            for _ in 0..num_lo_bounds {
                lower_bounds.push(c.read_compressed_i32().ok_or(SigError::Truncated)?);
            }
            // `rank` stays the wire `u32` here; the owned model's `u8` narrowing
            // (and its fail-loud range check) happens at projection.
            Ok(TypeSig::Array {
                element,
                rank,
                sizes,
                lower_bounds,
            })
        }
        // II.23.2.12: `PTR CustomMod* Type` or `PTR CustomMod* VOID`. A bare
        // `PTR VOID` is `void*` (`Ptr(None)`); otherwise the recursive `decode`
        // consumes any leading custom modifiers and the (non-void) pointee. A
        // custom-modified void pointer (`PTR cmod* VOID`) is not modelled and
        // falls through to `UnexpectedVoid`.
        elem::PTR => {
            if c.peek_u8() == Some(elem::VOID) {
                c.read_u8();
                Ok(TypeSig::Ptr(None))
            } else {
                Ok(TypeSig::Ptr(Some(Box::new(decode(c, tables, depth + 1)?))))
            }
        }
        elem::BYREF => Ok(TypeSig::ByRef(Box::new(decode(c, tables, depth + 1)?))),

        elem::GENERICINST => {
            let kind = match c.read_u8().ok_or(SigError::Truncated)? {
                elem::CLASS => NamedKind::Class,
                elem::VALUETYPE => NamedKind::ValueType,
                // GENERICINST must be followed by CLASS or VALUETYPE.
                _ => return Err(SigError::BadToken),
            };
            let scope = read_scope(c, tables)?;
            let count = c.read_compressed_u32().ok_or(SigError::Truncated)?;
            // `count` is attacker-controlled; never pre-allocate from it. Each
            // arg consumes >= 1 byte, so the loop is bounded by the blob length.
            let mut args = Vec::new();
            for _ in 0..count {
                args.push(decode(c, tables, depth + 1)?);
            }
            Ok(TypeSig::Generic {
                kind: Some(kind),
                scope,
                args,
            })
        }

        // `CMOD_REQD`/`CMOD_OPT` cannot appear here: [`decode`] consumes the
        // whole run before calling this, and it loops until the next byte is not
        // a modifier. Reaching this arm would mean a modifier byte in a position
        // where no `Type` may start, which is exactly what `UnsupportedElement`
        // is for.
        other => Err(SigError::UnsupportedElement(other)),
    }
}

fn read_scope(c: &mut Cursor, tables: &ImageTables) -> Result<TypeScope, SigError> {
    let token = c.read_compressed_u32().ok_or(SigError::Truncated)?;
    resolve_token(token, tables)
}

/// The calling convention recorded in a `MethodDefSig`'s leading byte. The
/// native-interop conventions (`C`/`STDCALL`/…) never appear on a `MethodDef`
/// and are refused (see [`SigError::UnsupportedElement`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallConv {
    Default,
    VarArg,
    /// A generic method; `count` is the generic-parameter arity declared in the
    /// signature (which the `GenericParam` table should corroborate).
    Generic {
        count: u32,
    },
}

/// A method's return type (ECMA-335 II.23.2.11). `Void` is legal only here —
/// the standalone [`decode_type`] refuses `ELEMENT_TYPE_VOID` with
/// [`SigError::UnexpectedVoid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetType {
    /// `CustomMod* VOID`. The modifier run is usually empty; the one shape a
    /// real compiler emits is a C# 9 `init` setter's
    /// `modreq(System.Runtime.CompilerServices.IsExternalInit) void`. As
    /// everywhere else, the run is carried and the projector applies II.7.1.1
    /// to it — there is no separate "modified void" shape, because a modifier
    /// run is not a shape.
    Void(Vec<CustomMod>),
    Type(ModifiedType),
}

/// The signature-blob half of a method: everything a `MethodDefSig` (II.23.2.1)
/// carries on its own. The `Param`-table metadata (names, `in`/`out`, defaults,
/// per-position attributes) is paired in by the member stage; this struct holds
/// only what the blob determines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedMethodSig {
    pub has_this: bool,
    pub explicit_this: bool,
    pub calling_convention: CallConv,
    pub return_type: RetType,
    /// Parameter types in declaration order, each with its own modifier run (a
    /// byref parameter is preserved as [`TypeSig::ByRef`]). The count comes from
    /// the signature, not the `Param` table, so it is authoritative for pairing.
    pub param_types: Vec<ModifiedType>,
}

/// Decode a `MethodDefSig` (II.23.2.1): the calling-convention byte, an optional
/// generic-arity, the parameter count, the return type, and that many parameter
/// types. Pure; a refused element anywhere surfaces as the matching
/// [`SigError`] for the whole signature (members store it as a `Result`, so one
/// bad method never sinks the type).
pub(crate) fn decode_method_sig(
    blob: &[u8],
    tables: &ImageTables,
) -> Result<DecodedMethodSig, SigError> {
    let mut c = Cursor::new(blob);
    let first = c.read_u8().ok_or(SigError::Truncated)?;
    let has_this = first & conv::HASTHIS != 0;
    let explicit_this = first & conv::EXPLICITTHIS != 0;
    // The convention is the whole low-5-bit field, matched exactly: `GENERIC` is
    // a distinct value, not a flag, so a byte that sets the generic bit *and*
    // low convention bits (e.g. `0x15` = generic+vararg) is malformed and refused
    // rather than mistaken for a plain generic method. Native-interop conventions
    // are likewise refused loudly, the offending byte preserved for diagnosis.
    let calling_convention = match first & conv::CONV_MASK {
        conv::DEFAULT => CallConv::Default,
        conv::VARARG => CallConv::VarArg,
        conv::GENERIC => {
            let count = c.read_compressed_u32().ok_or(SigError::Truncated)?;
            CallConv::Generic { count }
        }
        _ => return Err(SigError::UnsupportedElement(first)),
    };
    let param_count = c.read_compressed_u32().ok_or(SigError::Truncated)?;
    let return_type = decode_ret_type(&mut c, tables)?;
    // `param_count` is attacker-controlled; never pre-allocate from it. Each
    // parameter consumes >= 1 byte, so the loop is bounded by the blob length.
    let mut param_types = Vec::new();
    for _ in 0..param_count {
        param_types.push(decode(&mut c, tables, 0)?);
    }
    Ok(DecodedMethodSig {
        has_this,
        explicit_this,
        calling_convention,
        return_type,
        param_types,
    })
}

/// Decode a return type (II.23.2.11): `CustomMod* (VOID | Type)`.
///
/// The only thing that distinguishes this from any other position is that `VOID`
/// is legal here — [`decode_element`] refuses `ELEMENT_TYPE_VOID` with
/// [`SigError::UnexpectedVoid`] everywhere else. The modifier run is read by the
/// same [`read_mods`] as everywhere else and, for a `void` return, simply has no
/// type to sit beside (`RetType::Void`). A `modreq(IsExternalInit) void` — a C# 9
/// `init` setter — is exactly that shape.
fn decode_ret_type(c: &mut Cursor, tables: &ImageTables) -> Result<RetType, SigError> {
    let mods = read_mods(c, tables, 0)?;
    if c.peek_u8() == Some(elem::VOID) {
        c.read_u8();
        return Ok(RetType::Void(mods));
    }
    let ty = decode_element(c, tables, mods.len() as u32)?;
    Ok(RetType::Type(ModifiedType { mods, ty }))
}

/// Decode a `FieldSig` (II.23.2.4): the `FIELD` sentinel, then the field's type
/// (with any leading custom modifiers handled by [`decode`]). A blob that does
/// not begin with `FIELD` is refused with [`SigError::BadToken`].
pub(crate) fn decode_field_sig(
    blob: &[u8],
    tables: &ImageTables,
) -> Result<ModifiedType, SigError> {
    let mut c = Cursor::new(blob);
    let tag = c.read_u8().ok_or(SigError::Truncated)?;
    if tag != conv::FIELD {
        return Err(SigError::BadToken);
    }
    decode(&mut c, tables, 0)
}

/// Decode the *type* of a `PropertySig` (II.23.2.5): the `PROPERTY` sentinel
/// (with optional `HASTHIS`), the index-parameter count, then the property's
/// own type. The index parameters that follow are not projected — the property
/// model carries only the type. A blob that does not begin with the `PROPERTY`
/// sentinel is refused with [`SigError::BadToken`].
pub(crate) fn decode_property_sig(
    blob: &[u8],
    tables: &ImageTables,
) -> Result<ModifiedType, SigError> {
    let mut c = Cursor::new(blob);
    let first = c.read_u8().ok_or(SigError::Truncated)?;
    if first & conv::CONV_MASK != conv::PROPERTY {
        return Err(SigError::BadToken);
    }
    c.read_compressed_u32().ok_or(SigError::Truncated)?; // index-parameter count
    decode(&mut c, tables, 0)
}
