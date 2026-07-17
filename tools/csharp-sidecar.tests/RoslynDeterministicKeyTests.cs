namespace CSharpSidecar.Tests;

using Microsoft.CodeAnalysis.Emit;

using CSharpSidecar.Tests.TestHelpers;

using Xunit;

/// <summary>
/// Coverage for <see cref="RoslynDeterministicKey"/>. The wrapper is a
/// reflection binding into Roslyn-internal API; a Roslyn upgrade that
/// renamed or restructured <c>Compilation.GetDeterministicKey</c> would
/// make the sidecar silently produce wrong cache keys. <see cref="Probe"/>
/// is the first line of defence; this test fires it on the pinned Roslyn
/// to catch a binding break at build time rather than at first
/// <c>buildMetadata</c>.
/// </summary>
public sealed class RoslynDeterministicKeyTests
{
    private static readonly EmitOptions DefaultEmitOpts = new(
        metadataOnly: true,
        includePrivateMembers: true,
        tolerateErrors: true);

    [Fact]
    public void ProbeSucceedsOnPinnedRoslyn()
    {
        RoslynDeterministicKey.Probe();
    }

    [Fact]
    public void ComputeIsStableForStableInputs()
    {
        using var csproj = new TempCsproj("<Project Sdk=\"Microsoft.NET.Sdk\"/>");
        var built = AdhocCompilation.Build(
            projectName: "stable",
            csprojPath: csproj.Path,
            sources: new[] { ("C.cs", "namespace S; public class C {}") });

        var first = RoslynDeterministicKey.Compute(built.Compilation, DefaultEmitOpts);
        var second = RoslynDeterministicKey.Compute(built.Compilation, DefaultEmitOpts);

        Assert.Equal(first, second);
        Assert.NotEmpty(first);
    }

    /// <summary>
    /// IVT correctness depends on metadata-only emit preserving internals
    /// (D5). If <see cref="EmitOptions.IncludePrivateMembers"/> stopped
    /// participating in Roslyn's deterministic key, two emits that differ
    /// only in that flag would share a cache entry — and an IVT-friend
    /// consumer would see the wrong surface. This test pins the dependency.
    /// </summary>
    [Fact]
    public void ComputeDiffersWhenIncludePrivateMembersFlips()
    {
        using var csproj = new TempCsproj("<Project Sdk=\"Microsoft.NET.Sdk\"/>");
        var built = AdhocCompilation.Build(
            projectName: "ivt-shape",
            csprojPath: csproj.Path,
            sources: new[] { ("C.cs", "namespace S; internal class Hidden {}") });

        var withPrivates = new EmitOptions(
            metadataOnly: true, includePrivateMembers: true, tolerateErrors: true);
        var withoutPrivates = new EmitOptions(
            metadataOnly: true, includePrivateMembers: false, tolerateErrors: true);

        var keyWith = RoslynDeterministicKey.Compute(built.Compilation, withPrivates);
        var keyWithout = RoslynDeterministicKey.Compute(built.Compilation, withoutPrivates);

        Assert.NotEqual(keyWith, keyWithout);
    }
}
