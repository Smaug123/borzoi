/// Shapes where the val a doc key belongs to shares its IL slot — name,
/// arity, staticness — with a member the signature hides or renames, so only
/// the key's parameter types, if anything, tell the members apart.
module DocIdsFs.Hidden

/// The exposed function, compiled as `Shared` beside a hidden helper of the
/// same compiled name and arity.
[<CompiledName("Shared")>]
val internal exposed: x: int -> int

/// A generic function compiled as `GenericShared` beside a hidden non-generic
/// helper of the same compiled name and parameter types.
[<CompiledName("GenericShared")>]
val internal generic<'a> : x: int -> int

/// A function over a two-dimensional array, compiled as `RankShared` beside a
/// hidden helper over a three-dimensional one.
[<CompiledName("RankShared")>]
val internal rank2: a: int[,] -> int

/// A class whose renamed indexed properties trade IL names.
type Renamed =
    new: unit -> Renamed

    /// Compiled as the IL property `B`.
    [<CompiledName("B")>]
    member A: i: int -> int with get

    /// Compiled as the IL property `D`.
    [<CompiledName("D")>]
    member B: s: string -> string with get

/// As `Renamed`, with both properties indexed by the same type.
type RenamedAlike =
    new: unit -> RenamedAlike

    /// Compiled as the IL property `B`.
    [<CompiledName("B")>]
    member A: i: int -> int with get

    /// Compiled as the IL property `D`.
    [<CompiledName("D")>]
    member B: i: int -> int with get

/// A record whose documented member the projection elides, beside a hidden
/// overload of the same name and arity.
type Elided =
    {
        /// The value.
        V: int
    }

    /// The elided overload.
    [<System.Runtime.CompilerServices.CompilerGenerated>]
    member Corner: a: int[,,] -> int

/// Two getters renamed to one IL property name, the first hidden: fsc backs
/// the `Shared` property with the hidden `get_First`, and the documented
/// `get_Second` backs nothing.
type SharedHidden =
    new: unit -> SharedHidden

    /// The documented getter.
    [<CompiledName("Shared")>]
    member Second: int
