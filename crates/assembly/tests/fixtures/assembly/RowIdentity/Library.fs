// F# fixture for the join between the host signature pickle's entities and
// the TypeDef rows they compiled to (`crates/assembly/src/pickle_join.rs`).
//
// Each shape below puts two rows at one *stripped* name — the name a row has
// once its backtick arity suffix is dropped, and the key the pickle overlays
// once matched by. All of it is legal F#: a `[<CompiledName>]` can spell any
// row name fsc does not already write. (One it does write is refused: fsc fails
// with FS2014, "duplicate entry … in type index table", so two rows of one
// full name never come out of a single compilation. A module name cannot carry
// a backtick either, FS0883.) Every row here is one fsc writes for exactly one
// pickled entity, so the join must attribute every one of them, and each
// overlay must land on its own row.

namespace RowIdentity

/// An attribute the reader cannot project on a generic parameter, so a type
/// whose parameter carries one is dropped from the projection.
type ParamMarker() =
    inherit System.Attribute()

/// Row `V`.
type V =
    | X
    | Y

/// Row ``V`0``, which strips to `V` too.
[<CompiledName("V`0")>]
type W =
    | R
    | S

/// Row ``Dr`2`` on a one-parameter type, so its stripped name and arity are
/// those of `Dr<'T>` below. Declared first: a key that lets the last writer win
/// would hand it the dropped union's cases.
[<CompiledName("Dr`2")>]
type Squatter<'T> =
    | SqA
    | SqB of 'T

/// Row ``Dr`1``, which the projection drops: its parameter carries
/// `ParamMarker`.
type Dr<[<ParamMarker>] 'T> =
    | DrA
    | DrB of 'T

/// Row `Dotted` in namespace `RowIdentity.Inner`: the IL writer splits a type
/// name at its last `.`.
[<CompiledName("Inner.Dotted")>]
type Dotted =
    | DA
    | DB

/// Row ``M`1``, which strips to the name of module `M` below. Its case `V`
/// carries a field, so fsc nests a case class named `V` in it — a namesake of
/// the union `M.V`. Declared before the module, so its row comes first.
[<CompiledName("M`1")>]
type Q<'T> =
    | V of 'T
    | Other

/// Row `M`, holding the union row `V`.
module M =
    type V =
        | MA
        | MB
