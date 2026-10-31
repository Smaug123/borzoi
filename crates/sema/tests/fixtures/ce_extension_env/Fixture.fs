namespace GridCeExt

/// Custom operations of every probed name, added to FSharp.Core's
/// `AsyncBuilder` by an extension that is in scope wherever `GridCeExt` is
/// opened.
[<AutoOpen>]
module AsyncOps =
    type Microsoft.FSharp.Control.AsyncBuilder with
        [<CustomOperation("id")>]
        member _.OpId(state: int, v: int) = state + v
        [<CustomOperation("gridV")>]
        member _.OpGridV(state: int, v: int) = state + v
        [<CustomOperation("Some")>]
        member _.OpSome(state: int, v: int) = state + v
        [<CustomOperation("GridC")>]
        member _.OpGridC(state: int, v: int) = state + v
