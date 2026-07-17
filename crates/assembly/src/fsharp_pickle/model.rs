//! Typed result values produced by the F# pickle unpickler.
//!
//! Phase 6a populates only the phase-2 header tables; later sub-phases
//! attach the phase-1 entity / typar / val bodies. The header types are
//! kept narrow: indices into the interning tables are stored raw (`u32`)
//! so that resolution stays explicit and so the wire format is preserved
//! for diagnostic dumps.

/// Top-level result of `unpickle_signature`.
///
/// Phase 6b4 walks the phase-1 body inline; the result includes the
/// dense OSGN tables (`tycons`, `typars`, `vals`), the root entity
/// stamp, the CCU's mangled name, the `usesQuotations` flag, and the
/// original phase-2 header (kept for caller-side string-table lookup).
#[derive(Debug, Clone, PartialEq)]
pub struct PickledCcu {
    pub header: PickledHeader,
    /// The root `ModuleOrNamespace` — an osgn-decl index into
    /// `tables.tycons`. FCS calls this `mspec`.
    pub root_entity: u32,
    /// FCS's `compileTimeWorkingDir`. Preserved for diagnostic
    /// fidelity.
    pub compile_time_working_dir: String,
    /// FCS's `usesQuotations`.
    pub uses_quotations: bool,
    /// The finalised OSGN tables. Every slot is populated; the walker
    /// hard-errors if any slot is left `NewUnlinked`.
    pub tables: PickledOsgnTables,
}

/// The three OSGN stamp tables emitted by phase-1 decode, dense and
/// finalised. Produced by `PhaseOneState::finalize` after every slot
/// has been linked.
#[derive(Debug, Clone, PartialEq)]
pub struct PickledOsgnTables {
    pub tycons: Vec<PickledEntity>,
    pub typars: Vec<PickledTyparSpecData>,
    pub vals: Vec<PickledVal>,
}

/// The phase-2 metadata block.
///
/// Mirrors the tuple read by `unpickleObjWithDanglingCcus` at
/// `dotnet/fsharp/src/Compiler/TypedTree/TypedTreePickle.fs:1037-1085`.
/// The `phase1_bytes` blob is owned so the caller can drop the original
/// payload buffer; phase 6b will re-wrap it in a new reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledHeader {
    /// CCUs (compilation units) referenced by this signature.
    pub ccu_refs: Vec<CcuRef>,
    /// Number of tycon (Entity) stamp slots reserved in phase 1.
    pub ntycons: u32,
    /// Number of typar stamp slots reserved in phase 1.
    pub ntypars: u32,
    /// Number of val stamp slots reserved in phase 1.
    pub nvals: u32,
    /// Number of anon-record-info stamp slots reserved in phase 1; `0` for
    /// signatures produced by pre-F# 3.0 compilers (see
    /// `TypedTreePickle.fs:1072-1075`).
    pub nanoninfos: u32,
    /// Interned strings. All other tables reference these by index.
    pub strings: Vec<String>,
    /// Public paths: each entry is a sequence of indices into `strings`.
    pub pubpaths: Vec<Vec<u32>>,
    /// Non-local entity refs: a CCU index plus a path of string indices.
    pub nlerefs: Vec<PickledNleRef>,
    /// "Simple types" (frequent type apps like `int`, `string`): an index
    /// into the `nlerefs` table per entry. See
    /// `decode_simpletyp` at `TypedTreePickle.fs:911`.
    pub simpletys: Vec<u32>,
    /// The phase-1 bytes. Phase 6b decodes these against the header
    /// tables; phase 6a stores them owned to keep the lifetime story
    /// simple.
    pub phase1_bytes: Vec<u8>,
}

/// A CCU reference as it sits in the phase-2 table.
///
/// `u_encoded_ccuref` at `TypedTreePickle.fs:842-845` reads a single
/// length-prefixed UTF-8 string (after a leading tag byte that the
/// encoder always emits as `0`). There is no public-key-token in the
/// pickle stream; assembly identity is resolved via the host loader.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CcuRef {
    pub name: String,
}

/// A non-local entity reference: a CCU plus a path of name indices.
///
/// Mirrors `u_encoded_nleref` at `TypedTreePickle.fs:877`. Indices are
/// stored raw; resolution against `ccu_refs` and `strings` happens in
/// phase 6b (or via helper methods added when 6c needs them).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PickledNleRef {
    pub ccu: u32,
    pub path: Vec<u32>,
}

/// One node in the F# typed-tree's `TType` lattice as it appears in a
/// pickled signature. Mirrors `u_ty` (`TypedTreePickle.fs:2497-2579`)
/// with cross-references kept as raw header-table indices.
///
/// Phase 6b4 decodes every variant except `Anon` (tag 9, deferred —
/// no MiniLibFs fixture exercises it yet):
///
/// - `Tuple` / `Forall` map to tag 0, 5, 8 (tag 8 is struct-tuple);
///   `Forall` carries the osgn-decl indices populated by
///   `u_tyar_specs` at the same wire position.
/// - `AppSimple`, `App`, `Fun`, `Var` each carry a `Nullness` tag read
///   from the B-stream; `Nullness::Ambivalent` is also the implicit
///   value when the B stream is absent.
/// - `Measure` wraps a `Measure` expression.
/// - `UCase` references a union case constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledType {
    /// `TType_tuple` — tag 0 (reference) or tag 8 (struct).
    Tuple {
        kind: TupleKind,
        elems: Vec<PickledType>,
    },
    /// `TType_app` materialised from the `simpletys` table — tag 1.
    /// `simpletyp_index` is an index into `PickledHeader.simpletys`.
    AppSimple {
        simpletyp_index: u32,
        nullness: Nullness,
    },
    /// `TType_app` with explicit tcref and type arguments — tag 2.
    App {
        tcref: PickledTcRef,
        args: Vec<PickledType>,
        nullness: Nullness,
    },
    /// `TType_fun` — tag 3.
    Fun {
        domain: Box<PickledType>,
        range: Box<PickledType>,
        nullness: Nullness,
    },
    /// `TType_var` — tag 4. `typar_index` is an osgn index into the
    /// (future) typar stamp table; 6b1 records the raw value and 6b2
    /// links it.
    Var {
        typar_index: u32,
        nullness: Nullness,
    },
    /// `TType_measure` — tag 6.
    Measure(Measure),
    /// `TType_ucase` — tag 7.
    UCase {
        ucref: PickledUCaseRef,
        args: Vec<PickledType>,
    },
    /// `TType_forall(typars, body)` — tag 5. `typars` are osgn-decl
    /// indices into the typar OSGN table populated by `u_tyar_specs`
    /// at this point in the stream; `body` is the inner type that may
    /// reference them via `TType_var`.
    Forall {
        typars: Vec<u32>,
        body: Box<PickledType>,
    },
}

/// Whether a `TType_tuple` is a reference tuple (`a * b`) or a struct
/// tuple (`struct (a * b)`). Pickle tags 0 vs 8 at
/// `TypedTreePickle.fs:2501,2570`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TupleKind {
    Reference,
    Struct,
}

/// Nullness annotation carried alongside `App`/`AppSimple`/`Fun`/`Var`
/// types. FCS encodes three values across two streams: the canonical
/// "ambivalent" tag in the B-stream (11, 14, 17, or 20 depending on
/// variant) and the B-absent value (`u_byteB` returns 0 in that case)
/// both project to `Ambivalent`, so consumers see the same logical
/// nullness whether or not the producing F# compiler emitted a B stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Nullness {
    /// `KnownAmbivalentToNull` in FCS — either the F# compiler did not
    /// emit a B-stream annotation, or it explicitly chose the
    /// ambivalent canonical tag.
    Ambivalent,
    /// `KnownWithNull` — the type permits null.
    WithNull,
    /// `KnownWithoutNull` — the type forbids null.
    WithoutNull,
}

/// A type-constructor reference. Mirrors `u_tcref` at
/// `TypedTreePickle.fs:1942-1948`.
///
/// Phase 6b3 decodes both branches: the *decode* of `Local` is just a
/// compressed-int stamp index. Resolution against the entity OSGN
/// table (built by the entity walker in 6b4) is deferred — consumers
/// of a `Local(stamp)` only get a typed wrapper around the raw stamp
/// number until the walker brings the entity table online.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PickledTcRef {
    /// Local tcref — `stamp` is an osgn index into the (future)
    /// entity stamp table. Decoded eagerly in 6b3; resolution waits
    /// for 6b4's walker.
    Local(u32),
    /// Non-local tcref: `nleref_index` indexes into `PickledHeader.nlerefs`.
    NonLocal(u32),
}

/// A union-case reference: tcref plus the union case's logical name as
/// an index into the strings table. Mirrors `u_ucref` at `:1950-1952`,
/// noting that `u_string` itself indexes through the strings table
/// (`:831`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PickledUCaseRef {
    pub tcref: PickledTcRef,
    pub case_name_index: u32,
}

/// An F# measure expression. Mirrors `u_measure_expr` at
/// `TypedTreePickle.fs:2257-2278`. `range0` arguments at each tag are
/// dropped — F# source locations are not part of the cross-CCU view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Measure {
    /// `Measure.Const tcref` — tag 0.
    Const { tcref: PickledTcRef },
    /// `Measure.Inv` — tag 1.
    Inv(Box<Measure>),
    /// `Measure.Prod (a, b)` — tag 2.
    Prod(Box<Measure>, Box<Measure>),
    /// `Measure.Var typar` — tag 3. `typar_index` is an osgn index,
    /// resolved in 6b2.
    Var { typar_index: u32 },
    /// `Measure.One` — tag 4.
    One,
    /// `Measure.RationalPower (m, n/d)` — tag 5. `n` and `d` are the
    /// numerator and denominator of the rational exponent
    /// (`u_rational` at `:2253-2255`).
    RationalPower {
        base: Box<Measure>,
        num: i32,
        den: i32,
    },
}

/// An F# typar constraint. Mirrors `u_tyar_constraint` at
/// `TypedTreePickle.fs:2331-2350` (primary stream) and
/// `u_tyar_constraintB` at `:2353-2359` (B-stream tail, F# 9+).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FSharpTyparConstraint {
    /// `:> T` — tag 0.
    CoercesTo(PickledType),
    /// `(member …)` — tag 1. Statically resolved type-parameter
    /// constraint; carries the member signature the typar must
    /// support. The trait solution (`u_trait_sln`) is intentionally
    /// not decoded — `read_trait` hard-errors if the wire `Some`s
    /// one, since signature data (as opposed to optimisation data)
    /// nearly always pickles `None` per FCS's own comment at
    /// `TypedTreePickle.fs:2143`.
    MayResolveMember(PickledTrait),
    /// `default { priority } : T` — tag 2. The priority is the reverse
    /// index produced by `u_list_revi` (last constraint in the source
    /// list gets priority `0`; the first gets the highest priority).
    DefaultsTo { priority: u32, ty: PickledType },
    /// `null` — tag 3.
    SupportsNull,
    /// `struct` — tag 4.
    IsNonNullableStruct,
    /// `not struct` — tag 5.
    IsReferenceType,
    /// `new()` — tag 6.
    RequiresDefaultConstructor,
    /// `or` constraint over a finite set of types — tag 7.
    SimpleChoice(Vec<PickledType>),
    /// `enum<T>` — tag 8.
    IsEnum(PickledType),
    /// `delegate<T, U>` — tag 9.
    IsDelegate(PickledType, PickledType),
    /// `comparison` — tag 10.
    SupportsComparison,
    /// `equality` — tag 11.
    SupportsEquality,
    /// `unmanaged` — tag 12.
    IsUnmanaged,
    /// `not null` — B-stream tag 1 (F# 9+).
    NotSupportsNull,
    /// `allows ref struct` — B-stream tag 2 (F# 9+).
    AllowsRefStruct,
}

/// An SRTP (statically resolved type-parameter) constraint body.
/// Mirrors `u_trait` at `TypedTreePickle.fs:2170-2174`: the six
/// pickled fields are support tys, member name, member flags, arg
/// tys, optional return ty, and an optional trait solution. The
/// solution is not modelled — `read_trait` hard-errors on a
/// wire `Some`-solution per the deferral note on
/// [`FSharpTyparConstraint::MayResolveMember`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledTrait {
    pub support_tys: Vec<PickledType>,
    pub member_name: String,
    pub member_flags: PickledMemberFlags,
    pub arg_tys: Vec<PickledType>,
    pub return_ty: Option<PickledType>,
}

/// Typar binder kind — `TyparKind.Type` for ordinary `'T`, `Measure` for
/// unit-of-measure variables. Mirrors `u_kind` at
/// `TypedTreePickle.fs:2060-2064`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TyparKind {
    Type,
    Measure,
}

/// `IsType` — the kind associated with each `(name, kind)` pair inside a
/// `CompPath`. Mirrors `u_istype` at `TypedTreePickle.fs:2643-2650`.
///
/// `FSharpModuleWithSuffix` distinguishes modules whose CLR class has the
/// `Module` suffix (F#'s default for modules that share their name with a
/// type) from `ModuleOrType` (everything else). `Namespace true` is the
/// only `Namespace` variant FCS emits — the boolean records whether the
/// namespace was implicitly declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsType {
    FSharpModuleWithSuffix,
    ModuleOrType,
    Namespace,
}

/// 1-based source position. Mirrors `u_pos` at
/// `TypedTreePickle.fs:1899-1902`; the two ints are line + column,
/// both `u_int` (compressed). Source positions are kept for diagnostic
/// fidelity even though most consumers will not use them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PickledPos {
    pub line: u32,
    pub column: u32,
}

/// A range — file (as a string-table index) plus start/end positions.
/// Mirrors `u_range` at `:1904-1908`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PickledRange {
    pub file: u32,
    pub start: PickledPos,
    pub end: PickledPos,
}

/// XML documentation lines. Stored as string-table indices rather than
/// resolved strings so the per-walk allocation cost stays minimal;
/// callers that want the strings resolve via the pickled header's
/// strings table. Mirrors `u_xmldoc` at `:1918`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledXmlDoc {
    pub lines: Vec<u32>,
}

/// An IL scope reference. Mirrors `u_ILScopeRef` at
/// `TypedTreePickle.fs:1223-1231`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PickledILScopeRef {
    /// `ILScopeRef.Local` — tag 0. The scope is the assembly currently
    /// being read.
    Local,
    /// `ILScopeRef.Module` — tag 1.
    Module(PickledILModuleRef),
    /// `ILScopeRef.Assembly` — tag 2.
    Assembly(PickledILAssemblyRef),
}

/// IL module reference. Mirrors `u_ILModuleRef` at
/// `TypedTreePickle.fs:1205-1207`: `u_tup3 u_string u_bool (u_option u_bytes)`
/// → `(name, hasMetadata, hash)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PickledILModuleRef {
    pub name: String,
    pub has_metadata: bool,
    pub hash: Option<Vec<u8>>,
}

/// IL assembly reference. Mirrors `u_ILAssemblyRef` at
/// `TypedTreePickle.fs:1209-1218`: tag byte (must be `0`), then `u_tup6`
/// of name, optional hash, optional public key, retargetable bool,
/// optional version, optional locale.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PickledILAssemblyRef {
    pub name: String,
    pub hash: Option<Vec<u8>>,
    pub public_key: Option<PickledILPublicKey>,
    pub retargetable: bool,
    pub version: Option<PickledILVersion>,
    pub locale: Option<String>,
}

/// IL public key. Mirrors `u_ILPublicKey` at
/// `TypedTreePickle.fs:1193-1199`: tag byte (0 = full key, 1 = token)
/// + length-prefixed bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PickledILPublicKey {
    PublicKey(Vec<u8>),
    PublicKeyToken(Vec<u8>),
}

/// IL assembly version — `u_tup4 u_uint16 u_uint16 u_uint16 u_uint16`.
/// `TypedTreePickle.fs:1201-1203`. (Note `u_uint16` is actually
/// `u_int32` truncated, per `:438`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PickledILVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

/// A composition path — an `ILScopeRef` (where this module / type came
/// from) plus a list of `(name, kind)` pairs that walk into nested
/// modules and types. Mirrors `u_cpath` at `TypedTreePickle.fs:2652-2654`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledCPath {
    pub scope: PickledILScopeRef,
    /// Each entry is a `(name, kind)` pair: the name is held as a
    /// resolved `String` (matching FCS's `u_string` which already
    /// looks it up) and the kind tags how to descend into it.
    pub path: Vec<(String, IsType)>,
}

/// Access scope of an F# declaration. Mirrors `u_access` at
/// `TypedTreePickle.fs:3088-3091`. An empty list means `TAccess []` =
/// `taccessPublic`; a non-empty list narrows access to the union of
/// the listed paths.
pub type PickledAccess = Vec<PickledCPath>;

/// A constant value used at attribute-argument positions and inside
/// pickled `Expr.Const` nodes, and as the `[<Literal>]` value of a val or
/// record field. Models the complete `u_const` dispatcher at
/// `TypedTreePickle.fs:3394-3416` (tags 0–17); a tag outside that closed
/// range raises `UnsupportedPickleTag`, matching FCS's `ufailwith`.
///
/// The wire tag → variant mapping is FCS canonical (`TypedTreePickle.fs`
/// reader primitives at `:435-454`):
///
/// | Tag | Variant   | Wire payload                                    |
/// |-----|-----------|-------------------------------------------------|
/// | 0   | `Bool`    | `u_bool` (1 byte)                               |
/// | 1   | `SByte`   | `sbyte (u_int32)` (compressed)                  |
/// | 2   | `Byte`    | `byte (u_byte)` — a *raw* byte, not compressed  |
/// | 3   | `Int16`   | `int16 (u_int32)` (compressed)                  |
/// | 4   | `UInt16`  | `uint16 (u_int32)` (compressed)                 |
/// | 5   | `Int32`   | `u_int32` (compressed)                          |
/// | 6   | `UInt32`  | `uint32 (u_int32)` (compressed)                 |
/// | 7   | `Int64`   | `u_int64` (two compressed words, low then high) |
/// | 8   | `UInt64`  | `uint64 (u_int64)`                              |
/// | 9   | `IntPtr`  | `u_int64`                                       |
/// | 10  | `UIntPtr` | `uint64 (u_int64)`                              |
/// | 11  | `Single`  | `float32_of_bits (u_int32)` — the 32-bit pattern |
/// | 12  | `Double`  | `float_of_bits (u_int64)` — the 64-bit pattern   |
/// | 13  | `Char`    | `char (uint16 (u_int32))` — a UTF-16 code unit  |
/// | 14  | `String`  | `u_string` (string-table index)                 |
/// | 15  | `Unit`    | —                                               |
/// | 16  | `Zero`    | —                                               |
/// | 17  | `Decimal` | `u_array u_int32` — the four `Decimal.GetBits` words |
///
/// (`Zero` is FCS's "default value of any type" sentinel — used for the
/// `Const.Zero` case in `Expr.Const`. Distinct from `Unit`.)
///
/// Representation notes, chosen so the whole enum stays `Eq`/`Hash`-able
/// (it sits at the bottom of the `PickledExpr`/`PickledVal`/entity tree,
/// every level of which derives `Eq`):
///
/// - `Single`/`Double` carry the raw IEEE-754 bit pattern (`u32`/`u64`),
///   not `f32`/`f64`. This is exactly what the wire stores — FCS pickles
///   `p_int32 (bits_of_float x)` — so it is a faithful decode, it sidesteps
///   `f32: !Eq`, and it gives bit-exact constant identity (`-0.0` ≠ `+0.0`,
///   distinct `NaN` payloads stay distinct). Use [`PickledConst::as_f32`] /
///   [`PickledConst::as_f64`] for the floating-point value.
/// - `Char` is a `u16` (a raw UTF-16 code unit), not a Rust `char`,
///   because a lone surrogate is a legal `Const.Char` that `char` cannot
///   represent.
/// - `Decimal` keeps the raw `[lo, mid, hi, flags]` quadruple that
///   `System.Decimal.GetBits` round-trips, rather than depending on a
///   decimal crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledConst {
    Bool(bool),
    SByte(i8),
    Byte(u8),
    Int16(i16),
    UInt16(u16),
    Int32(i32),
    UInt32(u32),
    Int64(i64),
    UInt64(u64),
    IntPtr(i64),
    UIntPtr(u64),
    /// IEEE-754 single-precision *bit pattern* (see [`PickledConst::as_f32`]).
    Single(u32),
    /// IEEE-754 double-precision *bit pattern* (see [`PickledConst::as_f64`]).
    Double(u64),
    /// A UTF-16 code unit (F# `char`), kept raw so lone surrogates survive.
    Char(u16),
    /// String value already resolved through the strings table.
    String(String),
    Unit,
    Zero,
    /// The four `System.Decimal.GetBits` words: `[lo, mid, hi, flags]`.
    Decimal([i32; 4]),
}

impl PickledConst {
    /// The floating-point value of a [`PickledConst::Single`], decoding the
    /// stored IEEE-754 bit pattern. Returns `None` for any other variant.
    #[must_use]
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            PickledConst::Single(bits) => Some(f32::from_bits(*bits)),
            _ => None,
        }
    }

    /// The floating-point value of a [`PickledConst::Double`], decoding the
    /// stored IEEE-754 bit pattern. Returns `None` for any other variant.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PickledConst::Double(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }
}

/// A pickled `Expr` value as reached from attribute-argument position
/// (`u_attrib_expr`).
///
/// FCS pickles the *full* original attribute-argument expression
/// (`TypedTreePickle.fs:2878`), so the decoder walks the whole `u_expr`
/// tree (see [`read_expr`](crate::fsharp_pickle) — module `expr`). The
/// shapes a *literal* argument takes keep a structured value
/// (`Const`/`Val`/`App`, plus the two `Expr.Op`s `Array`/`Coerce`); every
/// other arm is decoded for alignment (and for its sub-decoders' osgn side
/// effects) and collapses to [`PickledExpr::Other`]. The genuinely-unported
/// shapes (`Match`/`Obj`, payload-bearing ops, …) stay loud-on-unknown.
///
/// Source positions (`u_dummy_range`) and the `vrefFlags` /
/// function-value type are decoded for stream alignment but dropped —
/// none of them are part of the cross-CCU view, and nothing downstream
/// (the measure overlay walks `tycon` kinds, never attribute-argument
/// *values*) consumes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledExpr {
    /// `Expr.Const(c, _, ty)` — tag 0. Wire form `u_const + u_dummy_range
    /// + u_ty` (`:3799-3803`).
    Const {
        value: PickledConst,
        ty: PickledType,
    },
    /// `Expr.Val(vref, _flags, _)` — tag 1. The head of an intrinsic
    /// application such as the `typeof` operator in `typeof<T>`.
    Val(PickledVRef),
    /// `Expr.App(func, _funcTy, tyArgs, args, _)` — tag 6. A
    /// `typeof<int>` attribute argument pickles as
    /// `App(Val(typeof), _, [int], [], _)`.
    App {
        func: Box<PickledExpr>,
        ty_args: Vec<PickledType>,
        args: Vec<PickledExpr>,
    },
    /// `Expr.Op(TOp.Array, [elemTy], elems, _)` — tag 2 with `u_op`
    /// tag 19. The single array-shaped `Expr.Op` that reaches
    /// attribute-argument position: an array-literal argument
    /// `[<Attr([| … |])>]`. The element *type* list (`u_tys`) is
    /// decoded for stream alignment and dropped — nothing in the
    /// cross-CCU view consumes it — while the elements are kept so the
    /// decode stays inspectable/testable. Elements recurse through the
    /// same attribute-argument subset (`Const`, `Val`, `App`), so a
    /// `typeof<T>[]` argument nests `App` here.
    Array { elements: Vec<PickledExpr> },
    /// `Expr.Op(TOp.Coerce, _, [arg], _)` — tag 2 with `u_op` tag 15.
    /// A transparent up-cast wrapping a single sub-expression, which
    /// arises when an attribute constructor parameter is typed as a
    /// supertype: a literal argument to an `obj`-typed parameter
    /// (`[<Attr("x")>]` where `Attr(o: obj)`) pickles its `orig` as
    /// `Coerce(Const "x")`. FCS treats the coercion as pass-through in
    /// attribute position (`CheckAttribArgExpr` recurses into the
    /// operand), and the coercion target types (`u_tys`) are decoded for
    /// alignment and dropped; we keep the wrapper for fidelity. The
    /// operand recurses through the same attribute-argument subset.
    Coerce { arg: Box<PickledExpr> },
    /// Any other `u_expr` arm that reaches attribute-argument position but
    /// whose *value* the cross-CCU view never inspects — `Lambda`,
    /// `Sequential`, `Op` other than `Array`/`Coerce`, etc. The decoder
    /// still consumes the arm's full wire structure (and runs every
    /// osgn-publishing sub-decoder, e.g. `u_Val`/`u_tyar_specs`, for its
    /// side effects) so the stream stays aligned; only the reconstructed
    /// value is dropped. `tag` is the `u_expr` discriminator (`:3795`),
    /// retained as a breadcrumb for diagnostics. These appear because
    /// FCS's `p_attrib_expr` pickles the *full* original expression
    /// (`TypedTreePickle.fs:2878`), so e.g. `[<AttributeUsage(A ||| B)>]`
    /// carries the inline `(|||)` operator as `App(Lambda(…, ILAsm …))`.
    Other { tag: u8 },
}

/// One element of `u_list u_attrib_expr` — a pair of `u_expr` values
/// (the original and the constant-evaluated form). Mirrors `u_attrib_expr`
/// at `TypedTreePickle.fs:3238-3240`.
///
/// FCS pickles both forms because pre-evaluation can lose information
/// (e.g. an `Expr.Val` reference to a literal lets the consumer find
/// the source `Val`, while `evaluated` flattens it to a `Const`). The
/// signature pickle normalises `orig` to `Const` for literal-`Val`
/// references at `p_attrib_expr:2878-2889`, so both halves usually
/// decode to `PickledExpr::Const` — but the wire format is two
/// independent `u_expr`s, not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledAttribExpr {
    /// The original expression as it appeared in source.
    pub orig: PickledExpr,
    /// The constant-folded form (FCS's `evaluatedExpr`).
    pub evaluated: PickledExpr,
}

/// An F# attribute applied to a declaration. Mirrors `u_attrib` at
/// `TypedTreePickle.fs:3232-3236`.
///
/// FCS reads six wire fields and stuffs `None` into a seventh slot
/// (`AttributeTargets`, which is *not* preserved by the pickler at
/// `:3236`). We drop that slot entirely since it's always `None` for
/// signature-pickled attributes — there's nothing to model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledAttribute {
    /// The attribute *class*'s tcref (i.e. which type's constructor
    /// is being invoked).
    pub tcref: PickledTcRef,
    /// Whether the attribute is a CLR-defined one (decoded via
    /// `u_ILMethodRef`) or an F#-defined one (decoded via `u_vref`).
    pub kind: PickledAttribKind,
    /// Positional (unnamed) constructor arguments.
    pub args_unnamed: Vec<PickledAttribExpr>,
    /// Named arguments / property setters.
    pub args_named: Vec<PickledAttribNamedArg>,
    /// FCS's `appliedToGetterOrSetter` — true if the attribute was
    /// applied to a synthetic getter/setter rather than the source
    /// property declaration. Mostly a diagnostic hint; preserved for
    /// fidelity.
    pub applied_to_getter_or_setter: bool,
}

/// The `AttribKind` discriminator at `u_attribkind:3224-3230`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledAttribKind {
    /// `ILAttrib (ILMethodRef)` — tag 0. The attribute's `.ctor` is
    /// identified by an IL method ref (CLR-defined attribute classes
    /// like `[<Obsolete>]`, `[<CompilationMapping>]`). Boxed because
    /// `PickledILMethodRef` is ~400 bytes — four times the other
    /// variant — and almost every `PickledAttribute` ends up holding
    /// one, so paying the indirection is cheaper than carrying the
    /// inline footprint everywhere.
    ILAttrib(Box<PickledILMethodRef>),
    /// `FSAttrib (ValRef)` — tag 1. The attribute's `.ctor` is
    /// identified by an F# `vref` (F#-defined attribute classes).
    FSAttrib(PickledVRef),
}

/// A named (property-style) attribute argument. Mirrors
/// `u_attrib_arg:3242-3244`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledAttribNamedArg {
    /// The argument's name (property or field name).
    pub name: String,
    /// The argument's declared type (used by FCS for overload
    /// resolution; we preserve it for fidelity).
    pub ty: PickledType,
    /// `true` for a field setter, `false` for a property setter.
    pub is_field: bool,
    /// The argument value as an `AttribExpr` pair.
    pub value: PickledAttribExpr,
}

/// An IL type reference. Mirrors `u_ILTypeRef` at
/// `TypedTreePickle.fs:1321-1323`: `u_tup3 u_ILScopeRef u_strings u_string`
/// → `(scope, enclosing_namespace_segments, name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledILTypeRef {
    pub scope: PickledILScopeRef,
    /// Enclosing namespace + nested-type path segments, outermost
    /// first.
    pub enclosing: Vec<String>,
    /// The (unmangled) type name.
    pub name: String,
}

/// IL calling convention: `(hasThis, basic)`. Mirrors `u_ILCallConv`
/// at `:1317-1319`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PickledILCallConv {
    pub has_this: PickledILHasThis,
    pub basic: PickledILBasicCallConv,
}

/// `ILThisConvention`. Mirrors `u_ILHasThis` at `:1310-1315`: a single
/// byte tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickledILHasThis {
    /// Instance method (implicit `this`).
    Instance,
    /// Instance method with an *explicit* `this` argument.
    InstanceExplicit,
    /// Static method.
    Static,
}

/// `ILArgConvention`. Mirrors `u_ILBasicCallConv` at `:1300-1308`: a
/// single byte tag, six valid values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickledILBasicCallConv {
    Default,
    CDecl,
    StdCall,
    ThisCall,
    FastCall,
    VarArg,
}

/// IL array shape — list of `(lowerBound, size)` pairs per dimension.
/// Mirrors `u_ILArrayShape` at `:1325-1326`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledILArrayShape {
    pub bounds: Vec<(Option<i32>, Option<i32>)>,
}

/// IL type spec: a type ref plus its generic arguments. Mirrors
/// `u_ILTypeSpec` at `:1355-1357`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledILTypeSpec {
    pub type_ref: PickledILTypeRef,
    pub generic_args: Vec<PickledILType>,
}

/// IL call signature (for function-pointer types). Mirrors
/// `u_ILCallSig` at `:1345-1353`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledILCallSig {
    pub call_conv: PickledILCallConv,
    pub args: Vec<PickledILType>,
    pub return_type: Box<PickledILType>,
}

/// An IL type. Mirrors `u_ILType` at `TypedTreePickle.fs:1328-1341`,
/// 9 tags, all recursive.
///
/// Every tag is implemented eagerly — per D6.5 we do *not* stub
/// unreachable variants. A corrupt resource that emits an
/// unfamiliar tag should hard-error at the tag dispatch, not be
/// silently consumed by a placeholder branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledILType {
    /// `ILType.Void` — tag 0.
    Void,
    /// `ILType.Array(shape, elt)` — tag 1.
    Array(PickledILArrayShape, Box<PickledILType>),
    /// `ILType.Value(typespec)` — tag 2 (a CLR struct or enum).
    Value(PickledILTypeSpec),
    /// `ILType.Boxed(typespec)` — tag 3 (a CLR reference type), via
    /// `mkILBoxedType` at `:1335`.
    Boxed(PickledILTypeSpec),
    /// `ILType.Ptr(ty)` — tag 4.
    Ptr(Box<PickledILType>),
    /// `ILType.Byref(ty)` — tag 5.
    Byref(Box<PickledILType>),
    /// `ILType.FunctionPointer(callsig)` — tag 6.
    FunctionPointer(PickledILCallSig),
    /// `ILType.TypeVar(i)` — tag 7 (an IL generic-parameter
    /// reference), via `mkILTyvarTy` at `:1339`.
    Tyvar(u16),
    /// `ILType.Modified { required, modifier, ty }` — tag 8 (a custom
    /// modreq/modopt).
    Modified {
        required: bool,
        modifier: PickledILTypeRef,
        ty: Box<PickledILType>,
    },
}

/// An IL method reference. Mirrors `u_ILMethodRef` at `:1412-1416`:
/// `u_tup6 u_ILTypeRef u_ILCallConv u_int u_string u_ILTypes u_ILType`.
///
/// The wire field order is `(parent, callConv, genericArity, name,
/// argTypes, returnType)`. Note that FCS's `ILMethodRef.Create` call
/// swaps `genericArity` and `name` in *construction* order
/// (`:1416`) — the field order on the *wire* matches what we record
/// here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledILMethodRef {
    pub parent: PickledILTypeRef,
    pub call_conv: PickledILCallConv,
    pub generic_arity: u32,
    pub name: String,
    pub arg_types: Vec<PickledILType>,
    pub return_type: PickledILType,
}

/// A pickled `ValRef`. Mirrors `u_vref` at `:2032-2038`.
///
/// Both branches are decoded:
/// - `Local(stamp)` — tag 0 — is an `u_osgn_ref` index into the val
///   stamp table. We store the raw stamp so callers project against
///   `PickledOsgnTables::vals` post-walk; this matches FCS's lazy
///   `ValRef` cell semantics.
/// - `NonLocal` — tag 1 — embeds the full linkage key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledVRef {
    Local(u32),
    NonLocal(PickledNonLocalValRef),
}

/// A non-local value reference: the enclosing entity tcref plus the
/// `ValLinkageFullKey` body that uniquely identifies the val within
/// that entity. Mirrors `u_nonlocal_val_ref` at `:2010-2030`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledNonLocalValRef {
    /// The entity (module / namespace / class) that owns the val.
    pub enclosing_entity: PickledTcRef,
    /// For member vals, the mangled name of the enclosing class.
    /// `None` for module-level functions/values.
    pub member_parent_mangled_name: Option<String>,
    /// `true` for `override` members.
    pub member_is_override: bool,
    /// The val's logical (un-mangled) name.
    pub logical_name: String,
    /// Total argument count — used by FCS's linkage key for overload
    /// disambiguation. `0` for non-method vals.
    pub total_arg_count: u32,
    /// Optional disambiguating type for overload resolution. `Some`
    /// only when the linkage key needs more than `(logical_name,
    /// arg_count)` to identify the val uniquely (e.g. an overloaded
    /// member).
    pub partial_type: Option<PickledType>,
}

/// A pickled identifier: a name and the source range where it
/// appeared. Mirrors `u_ident` at `:1913-1916`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledIdent {
    pub name: String,
    pub range: PickledRange,
}

/// The body of one typar declaration. Mirrors `u_tyar_spec_data` at
/// `:2389-2411`.
///
/// The osgn-decl wrapper that *publishes* this body to the typar
/// stamp table (`u_tyar_spec` at `:2413-2414`) is **not** included
/// here — it requires the typar OSGN table, which 6b4 brings online.
/// 6b3 ships only the body decoder so that unit tests can pin the
/// wire shape end-to-end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledTyparSpecData {
    /// Source identifier (name + range).
    pub ident: PickledIdent,
    /// Attributes (e.g. `[<Measure>]`).
    pub attribs: Vec<PickledAttribute>,
    /// `TyparFlags` packed into an i64 — see FCS `:2395` for the
    /// `TyparFlags(int32 d)` cast. Stored raw so that future
    /// projections can decompose the bitfield without re-walking the
    /// wire.
    pub flags: i64,
    /// F# constraints (e.g. `:> seq<_>`).
    pub constraints: Vec<FSharpTyparConstraint>,
    pub xmldoc: PickledXmlDoc,
}

/// `ParentRef`. Mirrors `u_parentref` at `:3216-3222`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledParentRef {
    None,
    Parent(PickledTcRef),
}

/// `SynMemberKind` tag. Mirrors `u_member_kind` at `:2066-2073`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickledMemberKind {
    Member,
    PropertyGet,
    PropertySet,
    Constructor,
    ClassConstructor,
}

/// `SynMemberFlags`. Mirrors `u_MemberFlags` at `:2086-2097` — the
/// second wire bool is `_x3UnusedBoolInFormat` and is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PickledMemberFlags {
    pub is_instance: bool,
    pub is_dispatch_slot: bool,
    pub is_override_or_explicit_impl: bool,
    pub is_final: bool,
    pub kind: PickledMemberKind,
}

/// One parameter of a slot signature. Mirrors `u_slotparam` at
/// `:3926-3930`: `u_tup6 (u_option u_string) u_ty u_bool u_bool u_bool u_attribs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledSlotParam {
    pub name: Option<String>,
    pub ty: PickledType,
    pub is_in_arg: bool,
    pub is_out_arg: bool,
    pub is_optional: bool,
    pub attribs: Vec<PickledAttribute>,
}

/// A slot signature — an abstract or virtual member signature.
/// Mirrors `u_slotsig` at `:3932-3936`: `u_tup6 u_string u_ty
/// u_tyar_specs u_tyar_specs (u_list (u_list u_slotparam)) (u_option
/// u_ty)`. The `u_tyar_specs` osgn-decl wrappers populate the typar
/// stamp table; we store the resulting osgn-decl indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledSlotSig {
    pub name: String,
    pub implemented_ty: PickledType,
    pub class_typars: Vec<u32>,
    pub method_typars: Vec<u32>,
    pub params: Vec<Vec<PickledSlotParam>>,
    pub return_ty: Option<PickledType>,
}

/// One typar's repr info. Mirrors `u_TyparReprInfo` at `:2619-2622`:
/// `u_ident, u_kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledTyparReprInfo {
    pub ident: PickledIdent,
    pub kind: TyparKind,
}

/// One argument's repr info. Mirrors `u_ArgReprInfo` at `:2606-2617`:
/// `u_attribs, u_option u_ident`. (FCS exposes more fields in the
/// in-memory type, but cross-assembly pickles emit only these two.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledArgReprInfo {
    pub attribs: Vec<PickledAttribute>,
    pub name: Option<PickledIdent>,
}

/// A val's representation info — what FCS uses to drive code-gen and
/// printing. Mirrors `u_ValReprInfo` at `:2624-2628`: `u_list
/// u_TyparReprInfo, u_list (u_list u_ArgReprInfo), u_ArgReprInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledValReprInfo {
    pub typar_repr: Vec<PickledTyparReprInfo>,
    pub arg_repr: Vec<Vec<PickledArgReprInfo>>,
    pub return_repr: PickledArgReprInfo,
}

/// Member-level metadata for a `Val`. Mirrors `u_member_info` at
/// `:3246-3254`: `u_tup4 u_tcref u_MemberFlags (u_list u_slotsig)
/// u_bool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledMemberInfo {
    pub apparent_parent: PickledTcRef,
    pub flags: PickledMemberFlags,
    pub implemented_slots: Vec<PickledSlotSig>,
    pub is_implemented: bool,
}

/// The body of one val declaration. Mirrors `u_ValData` at
/// `:3278-3329`: a 13-tuple in wire order. `u_ranges` is `u_option
/// (u_tup2 u_range u_range)` — we keep the optional `other_range`
/// alongside `range`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledVal {
    pub logical_name: String,
    pub compiled_name: Option<String>,
    pub range: Option<PickledRange>,
    pub other_range: Option<PickledRange>,
    pub ty: PickledType,
    pub flags: i64,
    pub member_info: Option<PickledMemberInfo>,
    pub attribs: Vec<PickledAttribute>,
    pub repr_info: Option<PickledValReprInfo>,
    pub xmldoc_sig: String,
    pub access: PickledAccess,
    pub parent: PickledParentRef,
    pub literal_value: Option<PickledConst>,
    pub xmldoc: Option<PickledXmlDoc>,
}

/// One record field. Mirrors `u_recdfield_spec` at `:3093-3123` —
/// eleven wire reads. Wire order: `is_mutable, is_volatile, ty,
/// is_static, is_secret, literal_value, ident, (xmldoc + property
/// attribs via u_attribs_ext), field_attribs, xmldoc_sig, access`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledRecdField {
    pub is_mutable: bool,
    pub is_volatile: bool,
    pub ty: PickledType,
    pub is_static: bool,
    pub is_secret: bool,
    pub literal_value: Option<PickledConst>,
    pub ident: PickledIdent,
    pub property_attribs: Vec<PickledAttribute>,
    pub field_attribs: Vec<PickledAttribute>,
    pub xmldoc: Option<PickledXmlDoc>,
    pub xmldoc_sig: String,
    pub access: PickledAccess,
}

/// One union case. Mirrors `u_unioncase_spec` at `:3054-3076` —
/// seven wire reads. The middle `u_string` (deprecated compiled name)
/// is discarded by FCS; we drop it too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledUnionCase {
    pub fields: Vec<PickledRecdField>,
    pub return_ty: PickledType,
    pub ident: PickledIdent,
    pub attribs: Vec<PickledAttribute>,
    pub xmldoc: Option<PickledXmlDoc>,
    pub xmldoc_sig: String,
    pub access: PickledAccess,
}

/// The "object model" face of a tycon — what kind of CLR type the
/// tycon corresponds to. Mirrors `u_tycon_objmodel_kind` at
/// `:3256-3267`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledTyconObjModelKind {
    /// `TFSharpClass` — tag 0.
    Class,
    /// `TFSharpInterface` — tag 1.
    Interface,
    /// `TFSharpStruct` — tag 2.
    Struct,
    /// `TFSharpDelegate(slotsig)` — tag 3.
    Delegate(Box<PickledSlotSig>),
    /// `TFSharpEnum` — tag 4.
    Enum,
    /// `TFSharpUnion` — tag 5.
    Union,
    /// `TFSharpRecord` — tag 6.
    Record,
}

/// Object-model body. Mirrors `u_tycon_objmodel_data` at
/// `:3042-3050`: `u_tup3 u_tycon_objmodel_kind u_vrefs u_rfield_table`.
/// `vslots` is a list of vrefs (one per virtual slot). `rfields` is
/// the underlying record-field table (also used for union case
/// fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledTyconObjModelData {
    pub kind: PickledTyconObjModelKind,
    pub vslots: Vec<PickledVRef>,
    pub rfields: Vec<PickledRecdField>,
}

/// Exception-representation. Mirrors `u_exnc_repr` at `:3078-3086`,
/// 4 tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledExnRepr {
    Abbrev(PickledTcRef),
    Asm(PickledILTypeRef),
    Fresh(Vec<PickledRecdField>),
    None,
}

/// One tycon's compiled representation. Mirrors `u_tycon_repr` at
/// `:2961-3040` — a closure-returning dispatcher flattened to an
/// enum at decode time. The `flag_bit` (entity-flags bit
/// `ReservedBitForPickleFormatTyconReprFlag` = `0x80` per
/// `TypedTree.fs`) discriminates the inner-tag-2/outer-tag-1 branch
/// between `ILType` (false) and provider-type (true); the provider
/// branch is not supported here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickledTyconRepr {
    /// `TNoRepr` — outer tag 0.
    NoRepr,
    /// Record. Outer tag 1, inner tag 0. Body = `u_rfield_table`.
    Record(Vec<PickledRecdField>),
    /// Plain union (no static fields). Outer tag 1, inner tag 1.
    /// Body = `u_list u_unioncase_spec`.
    Union(Vec<PickledUnionCase>),
    /// `TAsmRepr(il_type)` — outer tag 1, inner tag 2, `flag_bit =
    /// false`. The wire body is one `u_ILType`.
    AsmRepr(PickledILType),
    /// F# object-model body (class / interface / struct / delegate /
    /// enum / union / record). Outer tag 1, inner tag 3.
    FSharpObjectModel(PickledTyconObjModelData),
    /// `TMeasureableRepr(ty)` — outer tag 1, inner tag 4.
    Measureable(PickledType),
    /// Union with static fields. Outer tag 2. Wire = `u_array
    /// u_unioncase_spec` then `u_tycon_objmodel_data`.
    UnionWithStaticFields {
        cases: Vec<PickledUnionCase>,
        objmodel: PickledTyconObjModelData,
    },
}

/// One module-or-namespace `ModuleOrNamespaceType` body. Mirrors
/// `u_modul_typ` at `:3333-3335`: `u_tup3 u_istype (u_qlist u_Val)
/// (u_qlist u_entity_spec)`. The `vals` and `entities` lists are
/// stored as osgn-decl indices into the relevant tables; resolving
/// them against `PickledOsgnTables::vals` / `::tycons` is a
/// post-walk operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledModulType {
    pub is_type: IsType,
    pub vals: Vec<u32>,
    pub entities: Vec<u32>,
}

/// `TyconAugmentation`. Mirrors `u_tcaug` at `:3183-3211` — a 9-tuple
/// terminated by `u_space 1`. Carries the user-defined compare /
/// equality / hash bindings, the adhoc remap (extension members),
/// implemented interfaces, an optional explicit super-type, and an
/// `abstract` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledTcAug {
    pub compare: Option<(PickledVRef, PickledVRef)>,
    pub compare_withc: Option<PickledVRef>,
    pub hash_and_equals_withc: Option<(PickledVRef, PickledVRef, PickledVRef)>,
    pub equals: Option<(PickledVRef, PickledVRef)>,
    pub adhoc: Vec<(String, PickledVRef)>,
    pub interfaces: Vec<(PickledType, bool)>,
    pub super_type: Option<PickledType>,
    pub is_abstract: bool,
}

/// The body of one entity (`u_entity_spec_data` at `:3128-3181`),
/// flattened. Fields preserve FCS's 17-tuple order:
///
/// 1. `typars` — osgn-decl indices into the typar OSGN table.
/// 2. `logical_name`.
/// 3. `compiled_name`.
/// 4. `range`.
/// 5. `pub_path`.
/// 6. `(access, repr_access)`.
/// 7. `attribs`.
/// 8. `repr` — the resolved tycon repr.
/// 9. `type_abbrev`.
/// 10. `tcaug`.
/// 11. (deprecated string placeholder, dropped).
/// 12. `typar_kind`.
/// 13. `flags` — already masked so the format bit
///     (`ReservedBitForPickleFormatTyconReprFlag`, `0x80`) is
///     cleared. The bit was consumed when resolving the repr
///     closure.
/// 14. `cpath`.
/// 15. `module_type` — the lazy body decoded inline.
/// 16. `exn_repr`.
/// 17. `xmldoc` — optional, from the extended-format marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickledEntity {
    pub typars: Vec<u32>,
    pub logical_name: String,
    pub compiled_name: Option<String>,
    pub range: PickledRange,
    pub pub_path: Option<Vec<u32>>,
    pub access: PickledAccess,
    pub repr_access: PickledAccess,
    pub attribs: Vec<PickledAttribute>,
    pub repr: PickledTyconRepr,
    pub type_abbrev: Option<PickledType>,
    pub tcaug: PickledTcAug,
    pub typar_kind: TyparKind,
    pub flags: i64,
    pub cpath: Option<PickledCPath>,
    pub module_type: PickledModulType,
    pub exn_repr: PickledExnRepr,
    pub xmldoc: Option<PickledXmlDoc>,
}
