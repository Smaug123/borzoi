namespace CSharpSidecar.Tests;

using Microsoft.CodeAnalysis.Emit;

using CSharpSidecar.Tests.TestHelpers;

using Xunit;

/// <summary>
/// Example-based coverage of <see cref="Cache.ComputeKey"/>, per
/// <c>docs/completed/csharp-sidecar-plan.md</c> D6 ("same inputs → same hash; one byte
/// flip → different hash"). Iteration-order invariance lives next to the
/// sensitivity tests here because both halves of the contract have to hold
/// for the cache to be sound; <c>CacheKeyPropertyTests</c> generalises the
/// same shape via FsCheck.
/// </summary>
public sealed class CacheKeyTests
{
    private static readonly EmitOptions EmitOpts = new(
        metadataOnly: true,
        includePrivateMembers: true,
        tolerateErrors: true);

    private const string MinimalCsproj =
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n"
        + "  <PropertyGroup>\n"
        + "    <TargetFramework>net10.0</TargetFramework>\n"
        + "  </PropertyGroup>\n"
        + "</Project>\n";

    [Fact]
    public void SameInputsProduceSameKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build(
            projectName: "stable",
            csprojPath: csproj.Path,
            sources: new[] { ("Class1.cs", "namespace S; public class C {}") });

        var first = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());
        var second = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());

        Assert.Equal(first, second);
        Assert.Equal(32, first.Length);
    }

    [Fact]
    public void CsprojByteFlipChangesKey()
    {
        var bytes = System.Text.Encoding.UTF8.GetBytes(MinimalCsproj);
        using var original = new TempCsproj(bytes);
        var flipped = (byte[])bytes.Clone();
        // Flip a byte inside the TargetFramework value so the change is in a
        // semantically relevant span even though the cache key doesn't parse
        // the XML — we hash bytes, so any flip would do.
        var idx = Array.IndexOf(flipped, (byte)'1');
        Assert.True(idx >= 0, "test relies on a literal '1' in the csproj template");
        flipped[idx] = (byte)'2';
        using var mutated = new TempCsproj(flipped);

        var built = AdhocCompilation.Build("flip", original.Path);

        var originalKey = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, original.Path,
            Array.Empty<(string, byte[])>());
        var mutatedKey = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, mutated.Path,
            Array.Empty<(string, byte[])>());

        Assert.NotEqual(originalKey, mutatedKey);
    }

    [Fact]
    public void AddingSourceChangesKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);

        var oneSource = AdhocCompilation.Build(
            projectName: "src",
            csprojPath: csproj.Path,
            sources: new[] { ("A.cs", "public class A {}") });
        var twoSources = AdhocCompilation.Build(
            projectName: "src",
            csprojPath: csproj.Path,
            sources: new[]
            {
                ("A.cs", "public class A {}"),
                ("B.cs", "public class B {}"),
            });

        var oneKey = Cache.ComputeKey(
            oneSource.Project, oneSource.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());
        var twoKey = Cache.ComputeKey(
            twoSources.Project, twoSources.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());

        Assert.NotEqual(oneKey, twoKey);
    }

    [Fact]
    public void AdditionalDocumentOrderDoesNotChangeKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);

        var forward = AdhocCompilation.Build(
            projectName: "addn",
            csprojPath: csproj.Path,
            additionalDocs: new[]
            {
                ("a.txt", "alpha"),
                ("b.txt", "bravo"),
                ("c.txt", "charlie"),
            });
        var backward = AdhocCompilation.Build(
            projectName: "addn",
            csprojPath: csproj.Path,
            additionalDocs: new[]
            {
                ("c.txt", "charlie"),
                ("b.txt", "bravo"),
                ("a.txt", "alpha"),
            });

        var fk = Cache.ComputeKey(
            forward.Project, forward.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());
        var bk = Cache.ComputeKey(
            backward.Project, backward.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());

        Assert.Equal(fk, bk);
    }

    [Fact]
    public void AnalyzerConfigDocumentOrderDoesNotChangeKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);

        var forward = AdhocCompilation.Build(
            projectName: "cfg",
            csprojPath: csproj.Path,
            analyzerConfigDocs: new[]
            {
                (".editorconfig", "root = true"),
                ("subdir/.editorconfig", "[*.cs]\nindent_size = 4"),
            });
        var backward = AdhocCompilation.Build(
            projectName: "cfg",
            csprojPath: csproj.Path,
            analyzerConfigDocs: new[]
            {
                ("subdir/.editorconfig", "[*.cs]\nindent_size = 4"),
                (".editorconfig", "root = true"),
            });

        var fk = Cache.ComputeKey(
            forward.Project, forward.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());
        var bk = Cache.ComputeKey(
            backward.Project, backward.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());

        Assert.Equal(fk, bk);
    }

    [Fact]
    public void AdditionalDocumentContentChangeChangesKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);

        var original = AdhocCompilation.Build(
            projectName: "addn",
            csprojPath: csproj.Path,
            additionalDocs: new[] { ("notes.txt", "alpha") });
        var modified = AdhocCompilation.Build(
            projectName: "addn",
            csprojPath: csproj.Path,
            additionalDocs: new[] { ("notes.txt", "ALPHA") });

        var ok = Cache.ComputeKey(
            original.Project, original.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());
        var mk = Cache.ComputeKey(
            modified.Project, modified.Compilation, EmitOpts, csproj.Path,
            Array.Empty<(string, byte[])>());

        Assert.NotEqual(ok, mk);
    }

    [Fact]
    public void TransitiveProjectKeyOrderDoesNotChangeKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build("trans", csproj.Path);

        var depA = ("/abs/A.csproj", new byte[] { 1, 2, 3, 4 });
        var depB = ("/abs/B.csproj", new byte[] { 5, 6, 7, 8 });

        var forward = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { depA, depB });
        var backward = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { depB, depA });

        Assert.Equal(forward, backward);
    }

    [Fact]
    public void TransitiveProjectKeyMutationChangesKey()
    {
        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build("trans", csproj.Path);

        var depOriginal = ("/abs/Leaf.csproj", new byte[] { 1, 2, 3, 4 });
        var depMutated = ("/abs/Leaf.csproj", new byte[] { 1, 2, 3, 5 });

        var ok = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { depOriginal });
        var mk = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { depMutated });

        Assert.NotEqual(ok, mk);
    }

    [Fact]
    public void ToHexLowerRoundTrips()
    {
        var key = new byte[]
        {
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        };

        var hex = Cache.ToHexLower(key);
        Assert.Equal(64, hex.Length);
        Assert.Equal(hex, hex.ToLowerInvariant());
        Assert.Equal(
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
            hex);
    }
}
