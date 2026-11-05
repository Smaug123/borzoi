namespace AliasFix

/// `[<AutoOpen>]` under another name.
type AO = Microsoft.FSharp.Core.AutoOpenAttribute

/// `[<CustomOperation>]` under another name, declared with the `Attribute`
/// suffix and written without it.
type GridCOAttribute = Microsoft.FSharp.Core.CustomOperationAttribute
