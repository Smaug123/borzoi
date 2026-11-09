// F# fixture for rows that collide on the **projected key**
// `(strip_arity(clr_name), generic_parameters.len())` — the key the pickle
// overlays once matched rows by, and which is not injective. F# forbids two
// types of one name and arity in a namespace, so an ordinary program never
// collides — but a `[<CompiledName>]` that fabricates a backtick-arity name
// does, and it compiles with no warning. Both collisions below are legal F#.
// The overlays reach a row by its exact metadata name now
// (`crates/assembly/src/pickle_join.rs`), and these are the shapes that show it.
//
// `U` — a **class** sitting at a pickled union's key. Selecting it would hand a
// class another type's cases and, since the property retention is destructive,
// strip the public members it genuinely declares. `matches_union`'s
// `EntityKind::Union` requirement is what stops that, and `P` is the member
// whose survival witnesses it.
//
// `V` — **two unions** at one key. The pickle's case list and published-member
// list belong to one of them and fit the other as readily, so neither can be
// applied to a row chosen by the key alone. The `_unique_<case>` backing fields
// fsc emits are what let a test say *which* source type a projected row is
// without reading the metadata name the overlays themselves match by.

namespace KeyCollision

/// Sits at key `("U", 0)` — the same key as `Other` below.
type U() =
    /// Survives only because the union overlay refuses to select a class.
    member _.P = 1

/// Compiles to CLR name ``U`0``, which strips to `("U", 0)`.
[<CompiledName("U`0")>]
type Other =
    | A
    | B

/// Sits at key `("V", 0)`, and is the row a key-only match selects first.
/// Its cases are `X` and `Y`; its backing fields are `_unique_X`/`_unique_Y`.
type V =
    | X
    | Y

/// Compiles to CLR name ``V`0``, which strips to `("V", 0)`. Its cases are `R`
/// and `S` — the names that must never appear on `V`.
[<CompiledName("V`0")>]
type W =
    | R
    | S
