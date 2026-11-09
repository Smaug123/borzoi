/// No attribute: the cases are in bare scope.
namespace RqaFix.Plain

type GridU =
    | GridC
    | GridD

/// FSharp.Core's attribute, short spelling.
namespace RqaFix.Short

[<RequireQualifiedAccess>]
type GridU =
    | GridC
    | GridD

/// FSharp.Core's attribute, long spelling.
namespace RqaFix.Long

[<RequireQualifiedAccessAttribute>]
type GridU =
    | GridC
    | GridD

/// FSharp.Core's attribute through an abbreviation.
namespace RqaFix.Abbrev

type GridRqa = RequireQualifiedAccessAttribute

[<GridRqa>]
type GridU =
    | GridC
    | GridD

/// A type of this assembly's own that the short spelling resolves to: the
/// cases are in bare scope.
namespace RqaFix.Imp

type RequireQualifiedAccessAttribute() =
    inherit System.Attribute()

[<RequireQualifiedAccess>]
type GridU =
    | GridC
    | GridD

/// A type named like the attribute's long spelling, and nothing else.
namespace RqaFix.ImpType

type RequireQualifiedAccessAttribute() =
    inherit System.Attribute()
