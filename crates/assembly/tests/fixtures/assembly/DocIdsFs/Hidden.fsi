/// Shapes where the val a doc key belongs to shares its IL slot — name,
/// arity, staticness — with a member the signature hides or renames, so only
/// the key's parameter types tell the members apart.
module DocIdsFs.Hidden

/// The exposed function, compiled as `Shared` beside a hidden helper of the
/// same compiled name and arity.
[<CompiledName("Shared")>]
val internal exposed: x: int -> int

/// A class whose renamed indexed properties trade IL names.
type Renamed =
    new: unit -> Renamed

    /// Compiled as the IL property `B`.
    [<CompiledName("B")>]
    member A: i: int -> int with get

    /// Compiled as the IL property `D`.
    [<CompiledName("D")>]
    member B: s: string -> string with get
