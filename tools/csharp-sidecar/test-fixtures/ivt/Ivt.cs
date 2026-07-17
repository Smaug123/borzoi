using System.Runtime.CompilerServices;

// The downstream F# project marked as a friend by the LSP's tests; the
// concrete name doesn't matter for emit, but it has to be syntactically
// valid because Roslyn parses it during attribute resolution.
[assembly: InternalsVisibleTo("IvtFixture.Consumer")]

namespace IvtFixture;

/// <summary>
/// Internal type intended to be visible to <c>IvtFixture.Consumer</c>
/// through <c>[InternalsVisibleTo]</c>. The sidecar emits with
/// <c>EmitOptions.IncludePrivateMembers = true</c> so this type must
/// appear in the produced metadata DLL — that's what the corresponding
/// Rust-side test pins. A strict reference-assembly emit
/// (<c>ProduceReferenceAssembly=true</c>) would strip it, which is why
/// the test directly inspects the sidecar DLL rather than comparing the
/// two pipelines.
/// </summary>
internal sealed class InternalGreeter
{
    public string Greet(string name) => $"hello, {name}";
}

/// <summary>
/// Public type alongside the internal one, so the test can also confirm
/// public-vs-internal classification is preserved (rather than the
/// sidecar accidentally surfacing every type as Public, which would
/// trivially pass an internal-presence assertion).
/// </summary>
public sealed class PublicGreeter
{
    public string Greet(string name) => $"hi, {name}";
}
