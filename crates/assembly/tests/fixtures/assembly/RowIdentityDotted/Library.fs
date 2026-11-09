// F# fixture for a row that is its entity's own but not at its F# path
// (`crates/assembly/src/pickle_join.rs`, `JoinVerdict::Displaced`).
//
// The IL writer splits a type name at its last `.`, so `[<CompiledName("Inner.
// Dotted")>]` in namespace `RowIdentityDotted` writes a row named `Dotted` in
// namespace `RowIdentityDotted.Inner` — while F# names the type
// `RowIdentityDotted.Dotted`, and `RowIdentityDotted.Inner` is FS0039. No
// reading of that row may make it look like an F# entity, and an image holding
// one is not one whose pickle describes every row it reaches.

namespace RowIdentityDotted

/// Row `Dotted` in namespace `RowIdentityDotted.Inner`.
[<CompiledName("Inner.Dotted")>]
type Dotted =
    | DA
    | DB

/// A plain union beside it, which on an authoritative image would get its cases.
type Plain =
    | PA
    | PB
