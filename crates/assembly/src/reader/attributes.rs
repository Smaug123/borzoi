//! Stage 6: the custom-attribute blob decoder.
//!
//! A custom-attribute blob (ECMA-335 II.23.3) is *not* self-describing for its
//! fixed arguments — their types come from the constructor's parameter
//! signature, which this image already carries (a `MethodDef` defined here, or a
//! `MemberRef`). Named arguments *are* self-describing. The only datum that
//! lives outside this image is the underlying integral width of an enum-typed
//! argument, supplied by the caller as [`EnumWidths`].
//!
//! [`Image::decode_attribute`] is therefore a pure function of the blob, the
//! image (for the constructor signature and the owning type name), and the enum
//! widths. The wire walk itself ([`decode_blob`]) is a free function of the
//! blob plus the constructor's parameter types, so it is testable in isolation.
//!
//! **Correctness note.** `SerString` lengths are ECMA-335 II.23.2 compressed
//! integers (1/2/4 bytes), read here through the shared
//! [`Cursor::read_compressed_u32`] primitive. A reader that treats the prefix
//! as a single byte mis-decodes any string of 128+ bytes (whose prefix is 2 or
//! 4 bytes); this decoder is correct across all three length bands.

use std::collections::HashMap;

use super::cursor::Cursor;
use super::ids::{MemberRefId, MethodId, TypeDefId, TypeRefId};
use super::image::Image;
use super::model::{MemberHandle, MemberRefParent, RawAttribute, TypeName};
use super::signature::{ModifiedType, NamedKind, Primitive, SigError, TypeScope, TypeSig};

/// The underlying integral width of an enum used as an attribute argument. Only
/// the two widths this reader's consumers need are representable; an enum backed
/// by any other integer is outside the supported subset (the caller simply omits
/// it, yielding [`AttrError::UnknownEnumWidth`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegralWidth {
    Int32,
    /// Byte-backed. No production widths table registers one yet — every
    /// consumed enum today (`SourceConstructFlags`,
    /// `CompilationRepresentationFlags`) is `int32`-backed — but the CA-blob
    /// round-trip property (`attributes_tests`) constructs it, keeping the
    /// byte-enum decode path honest for the first real consumer.
    #[cfg_attr(not(test), allow(dead_code))]
    UInt8,
}

/// A decoded integral value, in one of the supported widths. `Int32`/`UInt8`
/// cover the bulk of attribute arguments; `UInt32`/`Int64` are carried for the
/// `[DecimalConstantAttribute]` (`byte, byte, uint, uint, uint`) and
/// `[DateTimeConstantAttribute]` (`long`) constructors behind `decimal`/
/// `DateTime` default-parameter values. (Enum-typed arguments still decode only
/// to `Int32`/`UInt8` — see [`IntegralWidth`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegralParam {
    Int32(i32),
    UInt8(u8),
    UInt32(u32),
    Int64(i64),
}

/// A decoded fixed or named argument value. The supported element types are
/// `String`, `Boolean`, `Int32`/`UInt8`/`UInt32`/`Int64` integrals, an enum
/// (recorded with its underlying value and type name), and a single-dimensional
/// array of those; everything else is refused (see
/// [`AttrError::UnsupportedElementType`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixedArg {
    /// A `SerString`; `None` is the ECMA-335 null-string encoding (`0xFF`).
    String(Option<String>),
    Boolean(bool),
    Integral(IntegralParam),
    Enum {
        underlying: IntegralParam,
        type_name: TypeName,
    },
    /// A non-null `SZARRAY`. The null-array encoding (`0xFFFFFFFF`) has no slot
    /// in this model and is refused.
    Array(Vec<FixedArg>),
}

/// Whether a named argument targets a field or a property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedArgKind {
    Field,
    Property,
}

/// A named (field or property) argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedArg {
    pub(crate) kind: NamedArgKind,
    pub(crate) name: String,
    pub(crate) value: FixedArg,
}

/// A fully decoded custom attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedAttribute {
    /// The attribute's own type (the constructor's declaring type).
    pub(crate) owning_type_name: TypeName,
    pub(crate) fixed_args: Vec<FixedArg>,
    pub(crate) named_args: Vec<NamedArg>,
    /// Reserved for a future catch-all that keeps the raw bytes alongside the
    /// decode; not populated yet.
    pub(crate) raw_blob: Option<Vec<u8>>,
}

/// A failure decoding a custom-attribute blob. Distinct from the structural
/// [`super::Error`]: it is returned by the standalone decode step, never raised
/// during parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttrError {
    /// The blob ended mid-decode.
    Truncated,
    /// The blob is structurally invalid: a bad prolog, a `SerString` that is not
    /// valid UTF-8, a null field/property name, or a pathologically nested
    /// array type.
    Malformed,
    /// An element-type byte (a named-argument `FieldOrPropType`, or the element
    /// type implied by a constructor parameter) outside the supported subset.
    UnsupportedElementType(u8),
    /// An enum-typed argument whose type is absent from the supplied
    /// [`EnumWidths`], so its underlying width is unknown.
    UnknownEnumWidth(TypeName),
    /// The constructor's signature failed to decode, so the fixed arguments'
    /// types are unknown.
    BadCtorSignature(SigError),
    /// The attribute's constructor is a `MemberRef` whose parent is not a plain
    /// named type (a generic-instantiation `TypeSpec`, a `ModuleRef`, or a
    /// vararg `MethodDef`), so the owning type cannot be named.
    UnsupportedAttributeType,
}

/// The caller-supplied map from an enum's [`TypeName`] to its underlying
/// integral width — the one piece of attribute-decode context that lives in
/// another assembly.
#[derive(Debug, Clone, Default)]
pub(crate) struct EnumWidths {
    widths: HashMap<TypeName, IntegralWidth>,
}

impl EnumWidths {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, name: TypeName, width: IntegralWidth) {
        self.widths.insert(name, width);
    }

    fn get(&self, name: &TypeName) -> Option<IntegralWidth> {
        self.widths.get(name).copied()
    }
}

/// The maximum array-type nesting (`SZARRAY` of `SZARRAY` …) a named-argument
/// `FieldOrPropType` may declare before being refused — far above anything a
/// real compiler emits, bounding the recursion so a hostile blob cannot exhaust
/// the stack.
const MAX_ARRAY_DEPTH: u32 = 16;

// --- ECMA-335 II.23.3 / II.23.1.16 wire constants ---

/// The custom-attribute prolog (II.23.3): the little-endian `u16` `0x0001`.
const PROLOG: u16 = 0x0001;
/// `SerString` null encoding: a single `0xFF` length byte (II.23.3).
const NULL_STRING: u8 = 0xFF;
/// Null `SZARRAY` element count (II.23.3).
const NULL_ARRAY: u32 = 0xFFFF_FFFF;
/// Named-argument target tags (II.23.3).
const TAG_FIELD: u8 = 0x53;
const TAG_PROPERTY: u8 = 0x54;
/// `FieldOrPropType` element-type bytes this decoder reads (II.23.1.16).
const ELEM_BOOLEAN: u8 = 0x02;
const ELEM_U1: u8 = 0x05;
const ELEM_I4: u8 = 0x08;
const ELEM_STRING: u8 = 0x0e;
const ELEM_SZARRAY: u8 = 0x1d;
/// `CMOD_REQD` (II.23.1.16) — the byte a refused *required* custom modifier on
/// a constructor-parameter type is reported as. See [`peel_ignorable_mods`].
const ELEM_CMOD_REQD: u8 = 0x1f;
const ELEM_ENUM: u8 = 0x55;

impl Image {
    /// Decode a captured [`RawAttribute`] into its argument values. Resolves the
    /// constructor's parameter signature and the attribute's owning type from
    /// this image; only `widths` comes from outside.
    pub(crate) fn decode_attribute(
        &self,
        raw: &RawAttribute,
        widths: &EnumWidths,
    ) -> Result<DecodedAttribute, AttrError> {
        let owning_type_name = self.attribute_owning_type(raw)?;
        let param_types = self.ctor_param_types(&raw.ctor)?;
        let resolve = |scope: &TypeScope| self.scope_type_name(scope);
        decode_blob(&raw.blob, &param_types, owning_type_name, &resolve, widths)
    }

    /// The attribute's own type — the type that declares the constructor. Cheap
    /// (no blob decode), so a consumer can filter attributes by type before
    /// decoding the payload.
    pub(crate) fn attribute_owning_type(&self, raw: &RawAttribute) -> Result<TypeName, AttrError> {
        // The handles were range-checked when minted (stage 4/5).
        match raw.ctor {
            MemberHandle::MethodDef(TypeDefId(td), _) => {
                Ok(self.type_defs[td as usize].name.clone())
            }
            MemberHandle::MemberRef(MemberRefId(id)) => {
                match self.member_refs[id as usize].parent {
                    MemberRefParent::TypeDef(TypeDefId(d)) => {
                        Ok(self.type_defs[d as usize].name.clone())
                    }
                    MemberRefParent::TypeRef(TypeRefId(r)) => {
                        Ok(self.type_refs[r as usize].name.clone())
                    }
                    MemberRefParent::Other => Err(AttrError::UnsupportedAttributeType),
                }
            }
        }
    }

    /// The constructor's parameter types (the fixed arguments' types).
    fn ctor_param_types(&self, ctor: &MemberHandle) -> Result<Vec<ModifiedType>, AttrError> {
        match *ctor {
            MemberHandle::MethodDef(TypeDefId(td), MethodId(m)) => {
                let sig = self.type_defs[td as usize].methods[m as usize]
                    .signature
                    .as_ref()
                    .map_err(|e| AttrError::BadCtorSignature(*e))?;
                Ok(sig.parameters.iter().map(|p| p.ty.clone()).collect())
            }
            MemberHandle::MemberRef(MemberRefId(id)) => {
                let sig = self.member_refs[id as usize]
                    .signature
                    .as_ref()
                    .map_err(|e| AttrError::BadCtorSignature(*e))?;
                Ok(sig.param_types.clone())
            }
        }
    }

    /// Resolve a `TypeDefOrRef` scope to the referenced type's name (used to
    /// look up an enum-typed constructor parameter's width).
    fn scope_type_name(&self, scope: &TypeScope) -> TypeName {
        match *scope {
            TypeScope::Definition(TypeDefId(d)) => self.type_defs[d as usize].name.clone(),
            TypeScope::Reference(TypeRefId(r)) => self.type_refs[r as usize].name.clone(),
        }
    }
}

/// Decode a custom-attribute blob given the constructor's parameter types. The
/// `resolve` callback maps an enum-typed parameter's scope to its type name (for
/// the [`EnumWidths`] lookup). Pure and total over arbitrary bytes.
pub(super) fn decode_blob(
    blob: &[u8],
    ctor_param_types: &[ModifiedType],
    owning_type_name: TypeName,
    resolve: &dyn Fn(&TypeScope) -> TypeName,
    widths: &EnumWidths,
) -> Result<DecodedAttribute, AttrError> {
    let mut c = Cursor::new(blob);
    if c.read_u16().ok_or(AttrError::Truncated)? != PROLOG {
        return Err(AttrError::Malformed);
    }

    let mut fixed_args = Vec::with_capacity(ctor_param_types.len());
    for ty in ctor_param_types {
        fixed_args.push(read_fixed_arg(&mut c, ty, resolve, widths)?);
    }

    let num_named = c.read_u16().ok_or(AttrError::Truncated)?;
    // `num_named` is attacker-controlled; never pre-allocate from it. Each named
    // argument consumes at least its tag byte, so the loop is bounded by the
    // blob length.
    let mut named_args = Vec::new();
    for _ in 0..num_named {
        named_args.push(read_named_arg(&mut c, widths)?);
    }

    // The blob is length-delimited and the grammar consumes it exactly; leftover
    // bytes mean a malformed or misencoded attribute, refused rather than
    // silently dropped (which would make it look valid to later classifiers).
    if c.position() != blob.len() {
        return Err(AttrError::Malformed);
    }

    Ok(DecodedAttribute {
        owning_type_name,
        fixed_args,
        named_args,
        raw_blob: None,
    })
}

/// ECMA-335 II.7.1.1, applied to a constructor-parameter type.
///
/// A custom modifier does not change how the argument is *encoded* in the
/// attribute blob — the blob follows the underlying type — so an ignorable
/// `modopt` in front of a parameter type can and must be dropped here rather
/// than sinking the attribute (and with it, the type that carries it: an
/// undecodable `[Obsolete]` drops its owner).
///
/// A `modreq` is refused, which is both the rule and the only safe answer: the
/// projector's two recognised markers cannot occur in this position (`volatile`
/// is field-only; a read-only ref implies a byref parameter, which is not in the
/// attribute-argument subset at all), so a required modifier here is by
/// construction one we do not understand.
fn peel_ignorable_mods(mt: &ModifiedType) -> Result<&TypeSig, AttrError> {
    if mt.mods.iter().any(|m| m.required) {
        return Err(AttrError::UnsupportedElementType(ELEM_CMOD_REQD));
    }
    Ok(&mt.ty)
}

/// Read one fixed argument whose type is given by the constructor parameter
/// `ty`.
fn read_fixed_arg(
    c: &mut Cursor,
    ty: &ModifiedType,
    resolve: &dyn Fn(&TypeScope) -> TypeName,
    widths: &EnumWidths,
) -> Result<FixedArg, AttrError> {
    match peel_ignorable_mods(ty)? {
        TypeSig::Primitive(Primitive::Boolean) => Ok(FixedArg::Boolean(read_bool(c)?)),
        TypeSig::Primitive(Primitive::Int32) => {
            Ok(FixedArg::Integral(IntegralParam::Int32(read_i32(c)?)))
        }
        TypeSig::Primitive(Primitive::UInt8) => {
            Ok(FixedArg::Integral(IntegralParam::UInt8(read_byte(c)?)))
        }
        TypeSig::Primitive(Primitive::UInt32) => {
            Ok(FixedArg::Integral(IntegralParam::UInt32(read_u32(c)?)))
        }
        TypeSig::Primitive(Primitive::Int64) => {
            Ok(FixedArg::Integral(IntegralParam::Int64(read_i64(c)?)))
        }
        TypeSig::Primitive(Primitive::String) => Ok(FixedArg::String(read_ser_string(c)?)),
        TypeSig::SzArray(inner) => read_fixed_array(c, inner, resolve, widths),
        // A value-type named type in a constructor signature is an enum (a
        // class — `System.Type` and friends — and `System.Object` boxing are
        // outside the subset). The width comes from `widths`.
        TypeSig::Named {
            kind: Some(NamedKind::ValueType) | None,
            scope,
        } => {
            let type_name = resolve(scope);
            let width = widths
                .get(&type_name)
                .ok_or_else(|| AttrError::UnknownEnumWidth(type_name.clone()))?;
            Ok(FixedArg::Enum {
                underlying: read_integral(c, width)?,
                type_name,
            })
        }
        other => Err(AttrError::UnsupportedElementType(element_byte(other))),
    }
}

/// Read a non-null `SZARRAY` of `inner`-typed elements (II.23.3).
fn read_fixed_array(
    c: &mut Cursor,
    inner: &ModifiedType,
    resolve: &dyn Fn(&TypeScope) -> TypeName,
    widths: &EnumWidths,
) -> Result<FixedArg, AttrError> {
    let count = c.read_u32().ok_or(AttrError::Truncated)?;
    if count == NULL_ARRAY {
        return Err(AttrError::UnsupportedElementType(ELEM_SZARRAY));
    }
    // Validate the element type even when the array is empty — otherwise an
    // unsupported element (e.g. `Int64[]`) or an unknown-width enum array would
    // decode to a bogus empty `Array`, making an unsupported attribute look
    // valid. The model does not carry the element type, so it cannot be recovered
    // afterwards.
    validate_element_type(inner, resolve, widths)?;
    // `count` is attacker-controlled; never pre-allocate. Each element consumes
    // >= 1 byte, so the loop is bounded by the blob length.
    let mut elems = Vec::new();
    for _ in 0..count {
        elems.push(read_fixed_arg(c, inner, resolve, widths)?);
    }
    Ok(FixedArg::Array(elems))
}

/// Whether `ty` is a supported fixed-argument element type, without reading any
/// bytes. Mirrors the type acceptance of [`read_fixed_arg`] so an empty array's
/// element type is still validated (an enum's width must be known).
fn validate_element_type(
    ty: &ModifiedType,
    resolve: &dyn Fn(&TypeScope) -> TypeName,
    widths: &EnumWidths,
) -> Result<(), AttrError> {
    match peel_ignorable_mods(ty)? {
        TypeSig::Primitive(
            Primitive::Boolean
            | Primitive::Int32
            | Primitive::UInt8
            | Primitive::UInt32
            | Primitive::Int64
            | Primitive::String,
        ) => Ok(()),
        TypeSig::SzArray(inner) => validate_element_type(inner, resolve, widths),
        TypeSig::Named {
            kind: Some(NamedKind::ValueType) | None,
            scope,
        } => {
            let name = resolve(scope);
            widths
                .get(&name)
                .map(|_| ())
                .ok_or(AttrError::UnknownEnumWidth(name))
        }
        other => Err(AttrError::UnsupportedElementType(element_byte(other))),
    }
}

/// Read one named argument (II.23.3): a field/property tag, a self-describing
/// `FieldOrPropType`, the member name, and the value.
fn read_named_arg(c: &mut Cursor, widths: &EnumWidths) -> Result<NamedArg, AttrError> {
    let kind = match c.read_u8().ok_or(AttrError::Truncated)? {
        TAG_FIELD => NamedArgKind::Field,
        TAG_PROPERTY => NamedArgKind::Property,
        other => return Err(AttrError::UnsupportedElementType(other)),
    };
    let declared = read_declared_type(c, 0)?;
    let name = read_ser_string(c)?.ok_or(AttrError::Malformed)?;
    let value = read_declared_value(c, &declared, widths)?;
    Ok(NamedArg { kind, name, value })
}

/// A named argument's self-describing type (II.23.3 `FieldOrPropType`).
enum DeclaredType {
    Boolean,
    Int32,
    UInt8,
    String,
    Enum(TypeName),
    Array(Box<DeclaredType>),
}

/// Read a `FieldOrPropType`. `depth` bounds the `SZARRAY` nesting.
fn read_declared_type(c: &mut Cursor, depth: u32) -> Result<DeclaredType, AttrError> {
    if depth >= MAX_ARRAY_DEPTH {
        return Err(AttrError::Malformed);
    }
    match c.read_u8().ok_or(AttrError::Truncated)? {
        ELEM_BOOLEAN => Ok(DeclaredType::Boolean),
        ELEM_I4 => Ok(DeclaredType::Int32),
        ELEM_U1 => Ok(DeclaredType::UInt8),
        ELEM_STRING => Ok(DeclaredType::String),
        ELEM_ENUM => {
            let name = read_ser_string(c)?.ok_or(AttrError::Malformed)?;
            Ok(DeclaredType::Enum(parse_type_name(&name)))
        }
        ELEM_SZARRAY => Ok(DeclaredType::Array(Box::new(read_declared_type(
            c,
            depth + 1,
        )?))),
        other => Err(AttrError::UnsupportedElementType(other)),
    }
}

/// Read a value of a (self-describing) [`DeclaredType`].
fn read_declared_value(
    c: &mut Cursor,
    declared: &DeclaredType,
    widths: &EnumWidths,
) -> Result<FixedArg, AttrError> {
    match declared {
        DeclaredType::Boolean => Ok(FixedArg::Boolean(read_bool(c)?)),
        DeclaredType::Int32 => Ok(FixedArg::Integral(IntegralParam::Int32(read_i32(c)?))),
        DeclaredType::UInt8 => Ok(FixedArg::Integral(IntegralParam::UInt8(read_byte(c)?))),
        DeclaredType::String => Ok(FixedArg::String(read_ser_string(c)?)),
        DeclaredType::Enum(type_name) => {
            let width = widths
                .get(type_name)
                .ok_or_else(|| AttrError::UnknownEnumWidth(type_name.clone()))?;
            Ok(FixedArg::Enum {
                underlying: read_integral(c, width)?,
                type_name: type_name.clone(),
            })
        }
        DeclaredType::Array(inner) => {
            let count = c.read_u32().ok_or(AttrError::Truncated)?;
            if count == NULL_ARRAY {
                return Err(AttrError::UnsupportedElementType(ELEM_SZARRAY));
            }
            // As for fixed arrays: an empty array of an unknown-width enum must
            // be refused rather than decoded to a bogus `Array([])` (the element
            // type is otherwise unrecoverable). The element-type *byte* was
            // already validated by `read_declared_type`; only the enum width
            // remains to check here.
            check_declared_enum_widths(inner, widths)?;
            let mut elems = Vec::new();
            for _ in 0..count {
                elems.push(read_declared_value(c, inner, widths)?);
            }
            Ok(FixedArg::Array(elems))
        }
    }
}

/// Verify any enum element's width is known, recursing through nested arrays —
/// so an empty array of an unknown-width enum is refused before the (skipped)
/// element loop.
fn check_declared_enum_widths(
    declared: &DeclaredType,
    widths: &EnumWidths,
) -> Result<(), AttrError> {
    match declared {
        DeclaredType::Enum(type_name) => widths
            .get(type_name)
            .map(|_| ())
            .ok_or_else(|| AttrError::UnknownEnumWidth(type_name.clone())),
        DeclaredType::Array(inner) => check_declared_enum_widths(inner, widths),
        _ => Ok(()),
    }
}

fn read_integral(c: &mut Cursor, width: IntegralWidth) -> Result<IntegralParam, AttrError> {
    match width {
        IntegralWidth::Int32 => Ok(IntegralParam::Int32(read_i32(c)?)),
        IntegralWidth::UInt8 => Ok(IntegralParam::UInt8(read_byte(c)?)),
    }
}

fn read_bool(c: &mut Cursor) -> Result<bool, AttrError> {
    Ok(c.read_u8().ok_or(AttrError::Truncated)? != 0)
}

fn read_byte(c: &mut Cursor) -> Result<u8, AttrError> {
    c.read_u8().ok_or(AttrError::Truncated)
}

fn read_i32(c: &mut Cursor) -> Result<i32, AttrError> {
    Ok(c.read_u32().ok_or(AttrError::Truncated)? as i32)
}

fn read_u32(c: &mut Cursor) -> Result<u32, AttrError> {
    c.read_u32().ok_or(AttrError::Truncated)
}

fn read_i64(c: &mut Cursor) -> Result<i64, AttrError> {
    Ok(c.read_u64().ok_or(AttrError::Truncated)? as i64)
}

/// Read a `SerString` (II.23.3): a compressed-integer length then that many
/// UTF-8 bytes, or the single-byte `0xFF` null encoding. Empty (length 0) is a
/// present-but-empty string, distinct from null.
fn read_ser_string(c: &mut Cursor) -> Result<Option<String>, AttrError> {
    if c.peek_u8() == Some(NULL_STRING) {
        c.read_u8();
        return Ok(None);
    }
    let len = c.read_compressed_u32().ok_or(AttrError::Truncated)? as usize;
    let bytes = c.read_bytes(len).ok_or(AttrError::Truncated)?;
    String::from_utf8(bytes.to_vec())
        .map(Some)
        .map_err(|_| AttrError::Malformed)
}

/// Parse a serialized enum type name (II.23.3) into a [`TypeName`]. The wire
/// form may be assembly-qualified (`Ns.Name, Assembly, …`); only the
/// namespace-qualified head is kept, split on the final `.`.
fn parse_type_name(s: &str) -> TypeName {
    let head = s.split(',').next().unwrap_or(s).trim();
    match head.rsplit_once('.') {
        Some((namespace, name)) => TypeName {
            namespace: namespace.to_string(),
            name: name.to_string(),
        },
        None => TypeName {
            namespace: String::new(),
            name: head.to_string(),
        },
    }
}

/// A representative ECMA-335 II.23.1.16 element-type byte for an unsupported
/// constructor-parameter type, for the [`AttrError::UnsupportedElementType`]
/// diagnostic. (The named-argument path reports the actual wire byte instead.)
fn element_byte(ty: &TypeSig) -> u8 {
    match ty {
        TypeSig::Primitive(p) => match p {
            Primitive::Boolean => 0x02,
            Primitive::Char => 0x03,
            Primitive::Int8 => 0x04,
            Primitive::UInt8 => 0x05,
            Primitive::Int16 => 0x06,
            Primitive::UInt16 => 0x07,
            Primitive::Int32 => 0x08,
            Primitive::UInt32 => 0x09,
            Primitive::Int64 => 0x0a,
            Primitive::UInt64 => 0x0b,
            Primitive::Float32 => 0x0c,
            Primitive::Float64 => 0x0d,
            Primitive::IntPtr => 0x18,
            Primitive::UIntPtr => 0x19,
            Primitive::String => 0x0e,
            Primitive::Object => 0x1c,
        },
        TypeSig::Named { .. } => 0x12,   // CLASS (e.g. System.Type)
        TypeSig::Generic { .. } => 0x15, // GENERICINST
        TypeSig::TypeVar(_) => 0x13,     // VAR
        TypeSig::MethodVar(_) => 0x1e,   // MVAR
        TypeSig::SzArray(_) => 0x1d,     // SZARRAY
        TypeSig::Array { .. } => 0x14,   // ARRAY
        TypeSig::Ptr(_) => 0x0f,         // PTR
        TypeSig::ByRef(_) => 0x10,       // BYREF
        TypeSig::TypedByRef => 0x16,     // TYPEDBYREF
    }
}
