// Two members the F# compiler keys differently from their IL shape.
//
// fsc writes a member's documentation key from its *F#* signature, where a
// measure type parameter counts towards the generic arity; IL erases it. So
// `Measured` is IL `F(float64)` but keyed ``M:MeasureKey.D.F``1(System.Double)``,
// and `Plain` is IL ``F<T>(float64)``, whose IL-derived key is that same string.
// Only `Measured` is documented, so the one entry in the `.xml` is not
// `Plain`'s, whatever an IL-derived key says. (Without the `CompiledName`s fsc
// refuses the pair as duplicates once measures are erased, FS0438.)
namespace MeasureKey

type D =
    /// Measured's documentation.
    [<CompiledName("F")>]
    static member Measured<[<Measure>] 'u>(x: float<'u>) : float<'u> = x

    [<CompiledName("F")>]
    static member Plain<'T>(x: float) : float = x
