// Regression fixture for the full `u_const` decoder (tags 0–17).
//
// `read_const` backs every `[<Literal>]` value and every constant
// attribute argument in a CCU's signature pickle. Before the full tag
// set landed, the decoder handled only Bool/Int32/String/Unit/Zero and
// hard-errored on the rest. Because attributes and literals are decoded
// eagerly while walking each entity's signature, a *single* literal of
// any other type — an `int64`, `char`, `float`, `decimal`, … — failed the
// whole CCU decode; enumeration then recorded skipped F# overlays and left
// the `[<Measure>] type m` below projected as `Class` (its IL truth) instead
// of `Measure`.
//
// This fixture pins one literal of each previously-unsupported,
// `[<Literal>]`-expressible type, plus a constant `int64` *attribute
// argument*, beside that measure type. The regression test asserts `m`
// recovers `Measure`, which only holds if every literal and attribute
// argument in the signature pickle decodes cleanly. (IntPtr/UIntPtr —
// tags 9/10 — are not `[<Literal>]`-expressible in F#; their decode path
// is identical to Int64/UInt64 and is pinned by the `consts.rs` unit
// tests.)
//
// Like MeasureAttrArgs, this fixture is not diffed whole against
// fcs-dump; it asserts only the measure kind, so it stays focused on the
// decoder.

namespace LiteralConsts

// One `[<Literal>]` of each previously-unsupported, literal-expressible
// `u_const` tag. Each pickles its `literal_value` into the signature
// stream, reached eagerly while decoding this module's vals.
module Values =
    [<Literal>]
    let SByteVal = -5y // tag 1

    [<Literal>]
    let ByteVal = 250uy // tag 2 (raw byte)

    [<Literal>]
    let Int16Val = -3000s // tag 3

    [<Literal>]
    let UInt16Val = 60000us // tag 4

    [<Literal>]
    let UInt32Val = 4000000000u // tag 6

    [<Literal>]
    let Int64Val = 9999999999L // tag 7

    [<Literal>]
    let UInt64Val = 18000000000000000000UL // tag 8

    [<Literal>]
    let SingleVal = 3.5f // tag 11

    [<Literal>]
    let DoubleVal = 3.14159 // tag 12

    [<Literal>]
    let CharVal = 'Z' // tag 13

    [<Literal>]
    let DecimalVal = 12.34m // tag 17

    // A `[<CompiledName>]`-RENAMED literal (review round 12). Unlike a renamed
    // *method* — where fsc strips `CompiledNameAttribute` and emits
    // `CompilationSourceNameAttribute` carrying the F# name — the literal-field path
    // in `IlxGen.fs` preserves every attribute on the field itself and adds no source
    // name. So IL holds a field named `RenamedLit` bearing
    // `[<CompiledName("RenamedLit")>]`, and the F# name `OriginalLit` is recoverable
    // only from the signature pickle. The attribute is therefore an *uncertainty
    // marker*: on the IL-heuristic path (no authoritative pickle) the projector must
    // not surface this field, or a consumer would resolve `Values.RenamedLit` — a name
    // F# does not expose.
    [<Literal; CompiledName("RenamedLit")>]
    let OriginalLit = 99

// A constant `int64` *attribute argument* pickles as `Expr.Const(Int64
// _)`, exercising the attribute-argument route into `read_const`
// alongside the literal-value route above.
type StampAttribute(value: int64) =
    inherit System.Attribute()

[<Stamp(123456789012L)>]
type Stamped = { SField: int }

// The unit-of-measure type whose recovered kind is the end-to-end signal.
[<Measure>]
type m
