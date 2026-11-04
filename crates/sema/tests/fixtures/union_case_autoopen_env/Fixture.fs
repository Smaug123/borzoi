// Auto-opened into every file that references this assembly, beside
// FSharp.Core's own implicit opens (the attribute is at the end). Which of two
// same-named implicit names a file binds is decided by assembly reference
// order.
namespace Shadow.Implicit

/// `Ok` collides with FSharp.Core's `Result.Ok` across implicit surfaces;
/// `Fresh` collides with nothing.
type Alt =
    | Ok of int
    | Fresh of int

/// Two unions of one namespace declaring one case name: the later wins, by an
/// order the projection reads from metadata rather than from the signature.
type First =
    | Twice of int
    | OnlyFirst

type Second =
    | Twice of string
    | OnlySecond

/// A constructible class takes `Error`'s unqualified constructor slot in
/// every file, ahead of anything the file opens.
type Error(message: string) =
    member _.Message = message

module internal AssemblyAttributes =
    [<assembly: AutoOpen("Shadow.Implicit")>]
    do ()
