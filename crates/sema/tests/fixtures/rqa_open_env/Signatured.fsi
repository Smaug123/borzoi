namespace RqaSig

// The signature, not the implementation, is the module FCS sees.

/// The implementation carries the attribute; this signature does not.
module ImplOnly =
    val implOnlyValue: int

/// This signature carries the attribute; the implementation does not.
[<RequireQualifiedAccess>]
module SigOnly =
    val sigOnlyValue: int
