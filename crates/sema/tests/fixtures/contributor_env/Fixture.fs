namespace GridFix

/// A static of every probed name, declared directly.
type Holder =
    static member id(x: int) = x + 1
    static member gridV(x: int) = x + 1
    static member Some(x: int) = x + 1
    static member GridC(x: int) = x + 1

/// The same statics, one level up a class hierarchy.
type Base() =
    static member id(x: int) = x + 1
    static member gridV(x: int) = x + 1
    static member Some(x: int) = x + 1
    static member GridC(x: int) = x + 1

/// Inherits every static it carries.
type Derived() =
    inherit Base()

/// Statics the assembly projection drops: an indexed property whose index is
/// a byref has no model in `borzoi-assembly`.
type Dropped =
    static member id
        with get (x: inref<int>) = x + 1

    static member gridV
        with get (x: inref<int>) = x + 1

    static member Some
        with get (x: inref<int>) = x + 1

    static member GridC
        with get (x: inref<int>) = x + 1

/// A computation-expression builder with a custom operation of every probed
/// name, each on a differently named method.
type FixB() =
    member _.Yield(()) = 0
    member _.Return(_: 'a) = 0
    [<CustomOperation("id")>]
    member _.OpId(state: int, v: int) = state + v
    [<CustomOperation("gridV")>]
    member _.OpGridV(state: int, v: int) = state + v
    [<CustomOperation("Some")>]
    member _.OpSome(state: int, v: int) = state + v
    [<CustomOperation("GridC")>]
    member _.OpGridC(state: int, v: int) = state + v

/// A builder whose one custom operation is named like no probe.
type FixBNoOp() =
    member _.Yield(()) = 0
    member _.Return(_: 'a) = 0
    [<CustomOperation("fixOther")>]
    member _.OpOther(state: int, v: int) = state + v

/// The custom operations of `FixB`, one level up a class hierarchy.
type FixBBase() =
    [<CustomOperation("id")>]
    member _.OpId(state: int, v: int) = state + v
    [<CustomOperation("gridV")>]
    member _.OpGridV(state: int, v: int) = state + v
    [<CustomOperation("Some")>]
    member _.OpSome(state: int, v: int) = state + v
    [<CustomOperation("GridC")>]
    member _.OpGridC(state: int, v: int) = state + v

/// Inherits every custom operation it offers.
type FixBDerived() =
    inherit FixBBase()
    member _.Yield(()) = 0
    member _.Return(_: 'a) = 0

/// A builder whose custom operations take their methods' names: the
/// attribute has no argument.
type FixBMethodName() =
    member _.Yield(()) = 0
    member _.Return(_: 'a) = 0
    [<CustomOperation>]
    member _.id(state: int, v: int) = state + v
    [<CustomOperation>]
    member _.gridV(state: int, v: int) = state + v
    [<CustomOperation>]
    member _.Some(state: int, v: int) = state + v
    [<CustomOperation>]
    member _.GridC(state: int, v: int) = state + v

module FixBuilders =
    let fixB = FixB()
    let fixBMethodName = FixBMethodName()
    let fixBNoOp = FixBNoOp()
    let fixBDerived = FixBDerived()

/// No probed statics of its own: the grid's source extensions add them.
type Plain =
    static member Marker = 0

/// No probed statics of its own either: `PlainExt` adds them as extension
/// members, in scope wherever `GridFix` is opened.
type Plain2 =
    static member Marker = 0

[<AutoOpen>]
module PlainExt =
    type Plain2 with
        static member id(x: int) = x + 1
        static member gridV(x: int) = x + 1
        static member Some(x: int) = x + 1
        static member GridC(x: int) = x + 1
