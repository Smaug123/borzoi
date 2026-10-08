// F# fixture for the documentation-comment ID differential
// (`tests/all/doc_id_fsharp_diff.rs`).
//
// Every declaration carries a `///` comment, so fsc writes a key for each
// into `DocIdsFs.xml` — the oracle. The shapes are the ones where the F#
// compiler's keys differ from Roslyn's, or from a naive reading of the IL:
// record fields keyed `P:`, union cases keyed `T:`, SRTP witness parameters,
// `[0:]` multidimensional arrays, settable properties keyed with the setter's
// argument, measure-erased and measure-generic signatures, `nativeptr`,
// conversion operators without `~`, type abbreviations, measure types, and
// extension members keyed under their extension container.
namespace DocIdsFs

open Microsoft.FSharp.NativeInterop

/// A unit of measure.
[<Measure>]
type m

/// A measure abbreviation.
[<Measure>]
type metre = m

/// A type abbreviation.
type IntId = int

/// A generic type abbreviation.
type Pair<'a> = 'a * 'a

/// A record.
type Point =
    {
        /// The X coordinate.
        X: int
        /// The Y coordinate.
        mutable Y: int
    }

    /// A record member property.
    member p.Sum = p.X + p.Y

    /// A record member method.
    member p.Scale(k: int) = { X = p.X * k; Y = p.Y * k }

/// A struct record.
[<Struct>]
type SPoint =
    {
        /// The S coordinate.
        S: float
    }

/// A record over a measure-generic field.
type Quantity<[<Measure>] 'u> =
    {
        /// The amount.
        Amount: float<'u>
    }

/// A union.
type Shape =
    /// A circle.
    | Circle of radius: float
    /// A rectangle.
    | Rect of
        /// The width.
        width: float *
        /// The height.
        height: float
    /// The empty shape.
    | Empty

    /// A union member property.
    member s.Area =
        match s with
        | Circle r -> r * r
        | Rect(w, h) -> w * h
        | Empty -> 0.0

    /// A union member method.
    member s.Scaled(k: float) =
        match s with
        | Circle r -> Circle(r * k)
        | other -> other

/// A generic union.
type Tree<'T> =
    /// A leaf.
    | Leaf
    /// A node.
    | Node of Tree<'T> * 'T * Tree<'T>

/// An exception.
exception Boom of
    /// The code.
    code: int

/// An interface.
type IShape =
    /// An abstract property.
    abstract Area: float
    /// An abstract method.
    abstract Describe: unit -> string

/// A delegate.
type Handler = delegate of int -> unit

/// An enum.
type Colour =
    /// Red.
    | Red = 0
    /// Green.
    | Green = 1

/// A class.
type Widget(name: string) =
    let mutable count = 0
    let changed = Event<int>()

    /// A secondary constructor.
    new() = Widget("anon")

    /// A read-only property.
    member _.Name = name

    /// A read-write property.
    member _.Count
        with get () = count
        and set v = count <- v

    /// An auto-property.
    member val Label = "" with get, set

    /// A write-only property.
    member _.Sink
        with set (v: string) = count <- v.Length

    /// An indexed property.
    member _.Item
        with get (i: int) = name.[i]

    /// The first of two overloads.
    member _.Add(x: int) = count + x

    /// The second of two overloads.
    member _.Add(x: string) = name + x

    /// A curried method.
    member _.Curried (a: int) (b: int) = a + b

    /// A static method.
    static member Make(n: string) = Widget(n)

    /// A static property.
    static member Default = Widget()

    /// A generic method.
    member _.Echo<'a>(x: 'a) = x

    /// An event.
    [<CLIEvent>]
    member _.Changed = changed.Publish

    /// An operator.
    static member (+)(a: Widget, b: Widget) = Widget(a.Name + b.Name)

    /// A conversion operator.
    static member op_Implicit(w: Widget) : string = w.Name

    /// A method over a two-dimensional array.
    member _.Corner(a: int[,]) = a.[0, 0]

    /// A method over a measure-typed float.
    member _.Dist(d: float<m>) = d

    /// An inline member with a statically resolved type parameter.
    static member inline Twice(x: ^a) = x + x

    interface IShape with
        member _.Area = 0.0
        member _.Describe() = name

/// A generic class.
type Box<'T>(v: 'T) =
    /// The boxed value.
    member _.Value = v

    /// A generic method on a generic class.
    member _.Map<'U>(f: 'T -> 'U) = Box<'U>(f v)

/// A class with an explicit field.
type Fields =
    /// An explicit mutable field.
    val mutable Raw: int

    /// The constructor.
    new() = { Raw = 0 }

/// A struct with explicit fields.
[<Struct>]
type Pos =
    /// The line.
    val Line: int
    /// The column.
    val Col: int

    /// The constructor.
    new(l, c) = { Line = l; Col = c }

/// A record whose name a module shares, so fsc suffixes the module.
type Thing =
    {
        /// The value.
        V: int
    }

/// The module paired with `Thing`.
module Thing =
    /// Make a thing.
    let make v = { V = v }

/// A module.
module Funcs =
    /// A value.
    let answer = 42

    /// A mutable value.
    let mutable counter = 0

    /// A literal.
    [<Literal>]
    let Limit = 10

    /// A function.
    let inc x = x + 1

    /// A tupled function.
    let add (a: int, b: int) = a + b

    /// A curried function.
    let addc (a: int) (b: int) = a + b

    /// A renamed function.
    [<CompiledName("RenamedAtIl")>]
    let renamed x = x * 2

    /// An inline function with a statically resolved type parameter.
    let inline twice (x: ^T) = x + x

    /// An inline function with an explicit member constraint.
    let inline unitOf< ^T when ^T: (static member Unit: ^T)> () = (^T: (static member Unit: ^T) ())

    /// A function over a two-dimensional array.
    let sum2 (a: int[,]) = a.[0, 0]

    /// A function over a three-dimensional array.
    let sum3 (a: int[,,]) = a.[0, 0, 0]

    /// A function over a jagged array.
    let first (a: int[][]) = a.[0].[0]

    /// A function over a measure-typed float.
    let scale (x: float<m>) = x * 2.0

    /// A measure-generic function.
    let scaleG (x: float<'u>) = x

    /// A function over a native pointer.
    let deref (p: nativeptr<int>) = NativePtr.read p

    /// A generic function.
    let id2<'a> (x: 'a) = x

    /// A generic value.
    let empty<'a> : 'a list = []

    /// A total active pattern.
    let (|Even|Odd|) n = if n % 2 = 0 then Even else Odd

    /// A partial active pattern.
    let (|Positive|_|) n = if n > 0 then Some n else None

    /// A higher-order function.
    let apply (f: int -> int) x = f x

    /// A function over a byref.
    let bump (x: byref<int>) = x <- x + 1

    /// A function over an option.
    let orZero (x: int option) = defaultArg x 0

    /// A nested module.
    module Inner =
        /// A nested value.
        let deep = "deep"

/// Extension members.
module Ext =
    type System.String with
        /// An instance extension method.
        member s.Shout() = s.ToUpperInvariant()

        /// A static extension property.
        static member Blank = ""

        /// An instance extension property.
        member s.Len = s.Length

    type Widget with
        /// An optional extension on a type from this assembly.
        member w.Twice = w.Count * 2
