//! Failure modes for assembly import.
//!
//! By design, the importer fails loud. Each
//! variant pins a *specific* "I don't understand this" case so the caller
//! can surface it as a diagnostic rather than substitute a silent fallback.
//! Variants are deliberately fine-grained: a phase-2/3 ECMA-335 reader
//! encountering an unknown signature element should not collapse into a
//! catch-all `Other(String)` — that would be the very papering-over the
//! design forbids.

use std::fmt;

/// Reason an entity (or one of its members) could not be imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The F# signature data resource carried a version tag the unpickler
    /// has not been ported to. Carries the raw version bytes from the
    /// pickle header.
    UnsupportedPickleVersion { bytes: Vec<u8> },

    /// The CLI metadata header advertised a layout the reader has not been
    /// updated for (new stream name, unfamiliar table-id, etc).
    UnsupportedEcmaLayout { detail: String },

    /// The `nested`/`enclosing` type linkage formed a cycle, or nested
    /// pathologically deep (past the recursion bound). Unlike an unsupported
    /// *feature* — which is dropped and recorded per-type so the rest of the
    /// assembly stays usable — this is structural corruption no real compiler
    /// emits, so it stays **fatal**: it propagates out of type enumeration
    /// rather than being isolated to the offending type. Bounding the recursion
    /// and failing loud here is what guards against the stack exhaustion / ~81
    /// GiB OOM the cycle would otherwise cause. `detail` describes the trip.
    CyclicTypeNesting { detail: String },

    /// A `TypeRef` named an assembly we could not locate on the resolver's
    /// search path. The unresolved name is preserved so the LSP can surface
    /// it; we do not substitute `System.Object` or a placeholder typar.
    UnresolvedTypeRef {
        assembly: String,
        namespace: Vec<String>,
        name: String,
    },

    /// A custom-attribute blob referenced a constructor we could not decode
    /// — typically because the attribute's argument list uses an
    /// `ELEMENT_TYPE_*` we haven't ported, or names an enum type we cannot
    /// resolve. Attached to the entity but not to the attribute (which is
    /// dropped), so the rest of the entity remains importable.
    UnknownCustomAttribute {
        attribute_type: String,
        detail: String,
    },

    /// A signature blob (method, field, property, function-pointer)
    /// contained an element type the parser hasn't been taught yet. The
    /// containing entity is rejected as a whole — a half-parsed signature
    /// would be worse than no signature.
    UnsupportedSignature { detail: String },

    /// A managed resource whose name began with `FSharp` but did not match
    /// any of the prefixes in `PrettyNaming.fs`. We deliberately do not
    /// silently ignore — when the F# compiler adds a new resource format,
    /// this is the trip-wire.
    UnknownFSharpResource { name: String },

    /// The pickle reader tried to consume past the end of its byte slice.
    /// `context` is a static string naming the read site (e.g.
    /// `"phase 2: ntycons"`) so the failure pinpoints which decoder ran out
    /// of input.
    UnexpectedEndOfStream { context: &'static str },

    /// A tag byte in the pickle stream did not match any known variant for
    /// its decoder. `context` names the decoder (e.g.
    /// `"p_tycon_objmodel_kind"`); `tag` is the offending value.
    UnsupportedPickleTag { context: &'static str, tag: u32 },

    /// An osgn reference (compressed-int index into a stamp table) pointed
    /// outside the table's pre-allocated range. `kind` is the table name
    /// (`"tycon"`, `"typar"`, `"val"`); `index` is the offending value and
    /// `max` is the table's current length.
    OsgnIndexOutOfRange {
        kind: &'static str,
        index: u32,
        max: usize,
    },

    /// A `u_expr` opcode appeared in a signature-data stream:
    /// inline expression bodies live in the optimisation stream, not the
    /// signature stream; encountering one here means either the resource
    /// kind was mis-routed or the signature pickle is corrupt. `context`
    /// names the decoder that hit it; `tag` is the offending byte.
    UnsupportedPickleExpr { context: &'static str, tag: u32 },

    /// A cross-table index in the pickle stream did not point into the
    /// header table it referred to (e.g. an `nleref`-index ≥
    /// `header.nlerefs.len()`, or a `string`-index ≥ `header.strings.len()`).
    /// `kind` names the offending table, `index` the offending value.
    DanglingPickleRef { kind: &'static str, index: u32 },

    /// The pickle phase-2 header was internally inconsistent in a way that
    /// is not specific to a single field (e.g. a count and an array length
    /// disagree). `detail` is a human-readable description.
    MalformedPickleHeader { detail: String },

    /// An OSGN slot was never linked (`u_osgn_decl` never fired for the
    /// index). After phase-1 walk completion, every pre-allocated slot
    /// in `itycons` / `itypars` / `ivals` must have been populated; the
    /// first unfilled slot raises this. Matches FCS's
    /// `check` (`TypedTreePickle.fs:1026-1031`).
    OsgnSlotNotLinked { kind: &'static str, index: u32 },

    /// Two `u_osgn_decl` calls targeted the same slot with *conflicting*
    /// bodies. FCS's `LinkNode` (`TypedTree.fs:1080`/`2456`/`3382`)
    /// overwrites a stub unconditionally and `check`
    /// (`TypedTreePickle.fs:1026-1031`) only requires every stub to end up
    /// linked — never that it is linked exactly once — so a stamp is
    /// legitimately re-declared when the same generalised node is pickled
    /// inline in several places (e.g. a method typar re-emitted as a fresh
    /// `TType_forall` binder in both an abstract slot's `vref` partial-type
    /// and that member's `tcaug.adhoc` `vref`). Those re-links carry
    /// identical data; a re-link whose body *differs* cannot come from a
    /// valid pickle and indicates a corrupt resource or a walker stream
    /// misalignment.
    OsgnConflictingRelink { kind: &'static str, index: u32 },

    /// A `u_lazy` framed body's recorded length disagreed with the
    /// number of bytes the inline decoder actually consumed. `expected`
    /// is the framing word; `actual` is the post-decode delta.
    MalformedPickleLazyFrame { expected: u32, actual: u32 },

    /// The phase-1 walk recursed past the fixed depth bound. The
    /// recursive decoders (`u_ty`, `u_measure_expr`, `u_expr`,
    /// `u_ILType`, `u_entity_spec`) each cost native stack per level,
    /// and a malformed stream can encode one level per *byte* (e.g. a
    /// run of `u_ty` tag-3 `TType_fun` bytes), so an unbounded walk is
    /// a stack-overflow abort on adversarial input. Real compiler
    /// output nests nowhere near the bound (FSharp.Core's deepest walk
    /// measures 19); a trip is corruption, not a capability gap.
    /// `context` names the decoder that tripped; `limit` is the bound.
    PickleRecursionLimitExceeded { context: &'static str, limit: u32 },

    /// The OS refused to spawn the dedicated pickle-walk thread (the
    /// thread that owns the walk's stack reservation — see
    /// `PICKLE_WALK_STACK_BYTES` in `fsharp_pickle/mod.rs`). This is an
    /// environmental failure (thread or address-space exhaustion), not
    /// a statement about the pickle bytes, but it is surfaced as a loud
    /// per-assembly error rather than a panic so a resource-squeezed
    /// host degrades one assembly's F# overlays instead of crashing the
    /// process. `detail` carries the OS error text.
    PickleWalkThreadSpawnFailed { detail: String },

    /// The F# signature pickle described an entity (by its
    /// `cpath`-derived FQN) that the projector merge could not match
    /// against the ECMA-projected type tree, or matched it but found
    /// the ECMA-side kind incompatible with the pickled repr. Both
    /// cases mean the pickle and the ECMA metadata disagree; we
    /// hard-error rather than silently continue. `detail` names the
    /// offending FQN and the specific disagreement.
    FsharpPickleMergeMismatch { detail: String },

    /// A merge overlay walker revisited an entity stamp already on its
    /// current descent path — the linked entity graph contains a cycle.
    /// Valid FCS output is a tree (each entity is declared inline at
    /// exactly one position), so a cycle can only come from a corrupt or
    /// crafted pickle: an *idempotent* OSGN re-link of an ancestor's stamp,
    /// which `OsgnTable::link` accepts as a no-op and the conflicting-relink
    /// guard never sees. Unlike the phase-1 decode, the overlay walkers run
    /// on the caller's normal stack (not the 64 MB pickle-walk thread), so an
    /// unguarded cyclic walk is a stack-overflow *abort*; failing loud here
    /// keeps it a recoverable per-assembly error. `stamp` is the tycon OSGN
    /// index that closed the cycle.
    PickleEntityCycle { stamp: u32 },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::UnsupportedPickleVersion { bytes } => {
                write!(f, "unsupported F# pickle version: bytes={bytes:02x?}")
            }
            ImportError::UnsupportedEcmaLayout { detail } => {
                write!(f, "unsupported ECMA-335 layout: {detail}")
            }
            ImportError::CyclicTypeNesting { detail } => {
                write!(f, "cyclic or pathologically deep type nesting: {detail}")
            }
            ImportError::UnresolvedTypeRef {
                assembly,
                namespace,
                name,
            } => {
                write!(
                    f,
                    "unresolved type reference: {}.{name} in assembly {assembly}",
                    namespace.join(".")
                )
            }
            ImportError::UnknownCustomAttribute {
                attribute_type,
                detail,
            } => {
                write!(
                    f,
                    "could not decode custom attribute {attribute_type}: {detail}"
                )
            }
            ImportError::UnsupportedSignature { detail } => {
                write!(f, "unsupported signature element: {detail}")
            }
            ImportError::UnknownFSharpResource { name } => {
                write!(f, "unknown FSharp* resource name: {name}")
            }
            ImportError::UnexpectedEndOfStream { context } => {
                write!(f, "unexpected end of pickle stream: {context}")
            }
            ImportError::UnsupportedPickleTag { context, tag } => {
                write!(f, "unsupported pickle tag {tag} in {context}")
            }
            ImportError::OsgnIndexOutOfRange { kind, index, max } => {
                write!(
                    f,
                    "osgn {kind} index {index} out of range (table length {max})"
                )
            }
            ImportError::UnsupportedPickleExpr { context, tag } => {
                write!(
                    f,
                    "unsupported pickle expression tag {tag} in {context} \
                     (u_expr opcodes are not legal in the signature stream)"
                )
            }
            ImportError::DanglingPickleRef { kind, index } => {
                write!(
                    f,
                    "dangling pickle reference: {kind} index {index} is out of range"
                )
            }
            ImportError::MalformedPickleHeader { detail } => {
                write!(f, "malformed F# pickle header: {detail}")
            }
            ImportError::OsgnSlotNotLinked { kind, index } => {
                write!(
                    f,
                    "osgn {kind} slot {index} was never linked by u_osgn_decl"
                )
            }
            ImportError::OsgnConflictingRelink { kind, index } => {
                write!(
                    f,
                    "osgn {kind} slot {index} was re-linked by u_osgn_decl with a conflicting body"
                )
            }
            ImportError::MalformedPickleLazyFrame { expected, actual } => {
                write!(
                    f,
                    "u_lazy frame mismatch: header recorded {expected} bytes but inline decode consumed {actual}"
                )
            }
            ImportError::FsharpPickleMergeMismatch { detail } => {
                write!(f, "F# pickle / ECMA merge mismatch: {detail}")
            }
            ImportError::PickleRecursionLimitExceeded { context, limit } => {
                write!(
                    f,
                    "F# pickle recursion depth exceeded the bound of {limit} in {context} \
                     (no real compiler output nests this deep; the stream is corrupt)"
                )
            }
            ImportError::PickleWalkThreadSpawnFailed { detail } => {
                write!(f, "could not spawn the F# pickle walk thread: {detail}")
            }
            ImportError::PickleEntityCycle { stamp } => {
                write!(
                    f,
                    "F# pickle entity graph contains a cycle: tycon stamp {stamp} \
                     is its own ancestor (valid compiler output is a tree; the resource is corrupt)"
                )
            }
        }
    }
}

impl std::error::Error for ImportError {}
