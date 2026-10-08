module DocIdsFs.Hidden

open System.Runtime.CompilerServices

// Each hidden helper is declared before the exposed val it shadows and kept out
// of line, so the IL holds both methods of a slot with the helper first.
[<CompiledName("Shared"); MethodImpl(MethodImplOptions.NoInlining)>]
let internal helper (s: string) = s.Length

[<CompiledName("Shared")>]
let internal exposed (x: int) = x + helper "x"

[<CompiledName("GenericShared"); MethodImpl(MethodImplOptions.NoInlining)>]
let internal plainHelper (x: int) = x

[<CompiledName("GenericShared")>]
let internal generic<'a> (x: int) = plainHelper x

[<CompiledName("RankShared"); MethodImpl(MethodImplOptions.NoInlining)>]
let internal rank3Helper (a: int[,,]) = a.[0, 0, 0]

[<CompiledName("RankShared")>]
let internal rank2 (a: int[,]) = a.[0, 0] + rank3Helper (Array3D.zeroCreate 1 1 1)

type Renamed() =
    [<CompiledName("B")>]
    member _.A
        with get (i: int) = i

    [<CompiledName("D")>]
    member _.B
        with get (s: string) = s

type RenamedAlike() =
    [<CompiledName("B")>]
    member _.A
        with get (i: int) = i

    [<CompiledName("D")>]
    member _.B
        with get (i: int) = i + 1

type Elided =
    { V: int }

    // `[<CompilerGenerated>]` makes the projection elide this documented
    // overload without a record; the signature hides the other.
    [<CompilerGenerated>]
    member r.Corner(a: int[,,]) = a.[0, 0, 0] + r.V

    member internal r.Corner(a: int[,]) = a.[0, 0] + r.V
