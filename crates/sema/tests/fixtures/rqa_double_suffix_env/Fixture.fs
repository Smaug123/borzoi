/// A type the long spelling `[<RequireQualifiedAccessAttribute>]` resolves
/// to ahead of FSharp.Core's, since FCS looks the long spelling up with the
/// suffix appended first. Alone in its assembly, so nothing else here is named
/// like the attribute.
namespace RqaDouble

type RequireQualifiedAccessAttributeAttribute() =
    inherit System.Attribute()
