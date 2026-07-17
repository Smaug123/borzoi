// F# fixture for the assembly-reader phase-4c differential test.
//
// Pins how F#-native optional type extensions (`type T with member …`)
// compile, and the divergence FCS shows between the F# surface and the
// raw IL signature that the Rust-side projector reads.
//
// Both kinds of augmentation below target `MiniLib.Counter` (a C# class
// in the sibling fixture). The F# compiler emits them as static methods
// on a synthetic `Extensions` class — there is no way to inject IL into
// the consumed `Counter` type itself, so optional augmentations are
// always lifted to a wrapping module class.
//
//   - `type Counter with member this.Tripled() = …`
//
//     Instance-style augmentation. IL: static method
//     `MiniLibFsExt.Extensions::Counter.Tripled(MiniLib.Counter) : Int32`.
//     On the net10.0 toolchain the F# compiler emits NO
//     `[ExtensionAttribute]` on these optional augmentations (verified:
//     the produced DLL carries no `ExtensionAttribute` reference at all).
//     FCS still reports `IsExtensionMember = true`; `CurriedParameterGroups`
//     strips the compiled `this` receiver, so the projector must re-prepend
//     the receiver type to match the MethodDef signature the Rust side
//     reads. The `CompiledName` is `Counter.Tripled` (qualified by the
//     target type — the F# compiler's mangling convention).
//
//   - `type Counter with static member Make() = …`
//
//     Static-only augmentation. IL: static method
//     `MiniLibFsExt.Extensions::Counter.Make.Static() : Counter`. FCS
//     reports `IsExtensionMember = true` and
//     `CompiledName = Counter.Make.Static` — both projectors must use the
//     compiled name and not the F# `LogicalName` for these, and neither
//     flags the static shape as an `extension`.
//
//   - `[<CompiledName("A.B")>] let f (x: int) = x`
//
//     A plain module `let` whose user-chosen compiled name contains a
//     dot. IL: static method `MiniLibFsExt.Extensions::A.B(Int32) : Int32`.
//     FCS reports `IsExtensionMember = false` — it is NOT an extension
//     member. This pins the false-positive the old IL-name heuristic
//     produced: a one-dot module-method name is indistinguishable in pure
//     IL from a genuine `Counter.Tripled` augmentation, so the projector
//     must read the authoritative `IsExtensionMember` from the F#
//     signature pickle rather than count dots.
//
//   - `[<CompiledName("Counter.Tripled")>] let tripledClash (a: int) (b: int) = …`
//
//     A plain module `let` deliberately given the *same* compiled IL name
//     as the `Counter.Tripled` augmentation above, but a different
//     signature: IL static method
//     `MiniLibFsExt.Extensions::Counter.Tripled(Int32, Int32) : Int32`.
//     The `Extensions` class therefore carries two `Counter.Tripled`
//     MethodDefs — the genuine instance augmentation (one `MiniLib.Counter`
//     parameter, `IsExtensionMember = true`) and this plain `let` (two
//     `Int32` parameters, `IsExtensionMember = false`). Reading the pickle
//     `IsExtensionMember` bit per *name* over-flags both; the overlay must
//     resolve the bit per *method*, breaking the name collision by the
//     compiled arity (the augmentation's re-prepended receiver gives it
//     arity 1; the `let` has arity 2). This pins that only the augmentation
//     is flagged `extension`.

namespace MiniLibFsExt

module Extensions =

    // Optional augmentation of the C# `MiniLib.Counter` type. The
    // augmentation is defined inside a module rather than at the
    // namespace level so it is treated as an OPTIONAL extension (which
    // crosses assembly boundaries via the `Counter.Method` /
    // `Counter.Method.Static` name mangling), not an INTRINSIC type
    // extension (which only applies in the same file and would not
    // round-trip through metadata at all).
    type MiniLib.Counter with

        member this.Tripled() = this.Value * 3

        static member Make() = MiniLib.Counter()

    // A plain module `let` whose compiled name carries a dot. It is NOT an
    // extension member; it exists to pin that the projector does not
    // mistake a dotted compiled name for the augmentation mangling.
    [<CompiledName("A.B")>]
    let f (x: int) = x

    // A plain module `let` whose compiled name *collides* with the
    // `Counter.Tripled` augmentation above, but with a different arity
    // (two parameters vs the augmentation's single re-prepended receiver).
    // It is NOT an extension member; it exists to pin that the overlay
    // resolves the `IsExtensionMember` bit per method (breaking the name
    // collision by arity) rather than flagging every method that shares the
    // compiled name.
    [<CompiledName("Counter.Tripled")>]
    let tripledClash (a: int) (b: int) = a + b
