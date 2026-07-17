namespace NullableFixture;

/// <summary>
/// Public surface that mixes nullable-annotated and non-nullable members so
/// the differential test (sidecar metadata-only vs `dotnet build
/// -p:ProduceReferenceAssembly=true`) covers both. Roslyn emits
/// <c>System.Runtime.CompilerServices.NullableAttribute</c> /
/// <c>NullableContextAttribute</c> on the user-visible members; we don't
/// project those into <c>NormalisedEntity</c>, so for diff purposes both
/// pipelines see the same erased signatures — what we're actually testing is
/// that the SDK's nullable processing doesn't *change* which members survive
/// (e.g. by mistakenly stripping a property whose backing field is
/// nullable-only).
/// </summary>
public sealed class Holder
{
    public string Required { get; }
    public string? Optional { get; }

    public Holder(string required, string? optional)
    {
        Required = required;
        Optional = optional;
    }

    // Scalar nullable on both parameter and return position. We deliberately
    // avoid composite shapes (`string[]?`, `List<string?>`, etc.) here:
    // Roslyn emits those with `NullableAttribute(byte[])`, which the
    // assembly reader currently refuses loud — that's the signal flagging
    // where phase 4m.3's composite-form decoder will land. Until then,
    // exercising the scalar form keeps this differential test productive
    // without tripping the (intentional) 4m.3 guard.
    public string? FirstOrNull(string? candidate) => candidate;
}
