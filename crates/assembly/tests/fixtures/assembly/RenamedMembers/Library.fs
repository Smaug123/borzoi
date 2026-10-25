// Fixture for the F#-kind property candidates `project_fsharp_members` keeps
// on records, exceptions and unions: `[<CompiledName>]` renames an IL
// *property* but not its accessors, so two renamed properties can trade IL
// names. Not diffed against fcs-dump: the projection refuses the renamed
// properties (it has no source name to give them), where FCS surfaces them.
namespace RenamedMembersNs

/// A record whose member properties trade IL names, beside a plain one.
type Swapped =
    { V: int }

    /// Compiled as the IL property `Second`, with accessor `get_First`.
    [<CompiledName("Second")>]
    member r.First = r.V

    /// Compiled as the IL property `First`, with accessor `get_Second`.
    [<CompiledName("First")>]
    member r.Second = string r.V

    /// Not renamed.
    member r.Plain = r.V + 1

/// A union with a renamed member property.
type Coin =
    | Heads
    | Tails

    /// Compiled as the IL property `Other`.
    [<CompiledName("Other")>]
    member c.Renamed = 1
