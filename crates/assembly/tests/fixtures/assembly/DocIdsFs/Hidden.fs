module DocIdsFs.Hidden

open System.Runtime.CompilerServices

// Declared before `exposed` and kept out of line, so the IL holds both
// `Shared` methods with the helper first.
[<CompiledName("Shared"); MethodImpl(MethodImplOptions.NoInlining)>]
let internal helper (s: string) = s.Length

[<CompiledName("Shared")>]
let internal exposed (x: int) = x + helper "x"

type Renamed() =
    [<CompiledName("B")>]
    member _.A
        with get (i: int) = i

    [<CompiledName("D")>]
    member _.B
        with get (s: string) = s
