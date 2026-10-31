// Every namespace here is opened by only the few rows of the union-case table
// that ask for it, so each shape reaches only the rows written against it.

/// A non-`[<RequireQualifiedAccess>]` union reusing `Result`'s case names:
/// `open Shadow.Cases` puts both in bare scope, over FSharp.Core's.
namespace Shadow.Cases

type Alt =
    | Ok of int
    | Error of string

/// A generic union reusing `Option`'s case names.
type Maybe<'T> =
    | Some of 'T
    | None

namespace Shadow.Rqa

/// `[<RequireQualifiedAccess>]`: opening the namespace must NOT bring the
/// cases into bare scope.
[<RequireQualifiedAccess>]
type Alt =
    | Ok of int
    | Error of string

namespace Shadow.Classes

/// A constructible class takes FCS's unqualified constructor slot, so an
/// `open` of this namespace evicts an earlier same-named union case in
/// expression position (never in pattern position).
type Error(message: string) =
    member _.Message = message

/// A struct takes the slot too.
[<Struct>]
type Ok =
    val Value: int
    new(v) = { Value = v }

/// A record does not take the slot.
type None = { Unused: int }

namespace Shadow.Modules

/// A union nested in a module: `open Shadow.Modules.Inner` brings its cases
/// into bare scope.
module Inner =
    type Opt =
        | Some of int
        | None

    let someValue = 1

/// A module whose union shares a case name with another module's, for rows
/// that open both.
module Other =
    type Opt2 =
        | Some of string
        | ValueNone

/// A module holding a class: opening it puts the class's name in the bare
/// constructor slot, and nothing in the constructor (pattern) namespace.
module WithClass =
    type Error(message: string) =
        member _.Message = message

/// Two unions of one module declaring one case name: the later wins, by an
/// order the projection reads from metadata rather than from the signature.
module Dup =
    type First =
        | Twice of int
        | OnlyFirst

    type Second =
        | Twice of string
        | OnlySecond

namespace Shadow.AutoNs

/// An `[<AutoOpen>]` module: opening the namespace opens it too.
[<AutoOpen>]
module Auto =
    type Choice =
        | Choice1Of2 of int
        | Choice2Of2 of string

namespace Shadow.Statics

/// A type whose static members `open type` puts in bare scope.
type Holder =
    static member Ok = 42
    static member None(x: int) = x

namespace Shadow.Plain

/// A union whose cases collide with nothing: the explicit-open fold's own
/// definite target, with no FSharp.Core counterpart to contest it.
type Shape =
    | Circle of float
    | Empty
