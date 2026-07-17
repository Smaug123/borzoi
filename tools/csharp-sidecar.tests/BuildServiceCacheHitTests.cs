namespace CSharpSidecar.Tests;

using Microsoft.CodeAnalysis.Emit;

using CSharpSidecar.Tests.TestHelpers;

using Xunit;

/// <summary>
/// The cache-key tests pin <see cref="Cache.ComputeKey"/>'s determinism;
/// this test pins the load-bearing consequence — that
/// <see cref="BuildService.EmitOrCache"/> actually short-circuits when a
/// keyed DLL is already on disk. The end-to-end Rust integration tests at
/// <c>crates/lsp/tests/csharp_sidecar.rs</c> observe the wire-level
/// <c>fromCache</c> flag from a fresh sidecar; this test pins the
/// in-process behaviour without standing up MSBuildWorkspace.
/// </summary>
public sealed class BuildServiceCacheHitTests
{
    private const string MinimalCsproj =
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n"
        + "  <PropertyGroup>\n"
        + "    <TargetFramework>net10.0</TargetFramework>\n"
        + "  </PropertyGroup>\n"
        + "</Project>\n";

    /// <summary>
    /// The production code in <c>EmitOrCache</c> constructs these options
    /// inline; we have to mirror them exactly so <c>Cache.ComputeKey</c>
    /// produces the same hex as the cache-lookup will derive.
    /// </summary>
    private static readonly EmitOptions EmitOpts = new(
        metadataOnly: true,
        includePrivateMembers: true,
        tolerateErrors: true);

    [Fact]
    public void CacheHitReturnsExistingDllWithoutReEmit()
    {
        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build("hit", csproj.Path);

        var cacheRoot = Path.Combine(
            Path.GetTempPath(), $"csharp-sidecar-cache-{Guid.NewGuid():N}");
        Directory.CreateDirectory(cacheRoot);
        try
        {
            // Derive the path EmitOrCache will look up. Cache key derivation
            // is already pinned by the other tests in this project; here we
            // reuse it as a fixture-builder, with no transitive refs (leaf
            // project, matching the simplest production caller shape).
            var key = Cache.ComputeKey(
                built.Project,
                built.Compilation,
                EmitOpts,
                csproj.Path,
                Array.Empty<(string, byte[])>());
            var hex = Cache.ToHexLower(key);
            var prefixDir = Path.Combine(cacheRoot, hex[..2]);
            var finalPath = Path.Combine(prefixDir, $"{hex}.dll");

            // Sentinel bytes that cannot possibly be a valid PE file. If
            // EmitOrCache fell through to the emit path, Roslyn would
            // overwrite this with real metadata; the byte-equality assertion
            // below is what proves the cache short-circuit fired.
            Directory.CreateDirectory(prefixDir);
            var sentinel = new byte[] { 0xDE, 0xAD, 0xBE, 0xEF };
            File.WriteAllBytes(finalPath, sentinel);
            var sentinelMtime = File.GetLastWriteTimeUtc(finalPath);

            var outcome = BuildService.EmitOrCache(
                built.Project,
                csproj.Path,
                cacheRoot,
                Array.Empty<(string ProjectPath, byte[] Key)>(),
                Array.Empty<WorkspaceDiagnosticDto>());

            var hit = Assert.IsType<BuildMetadataOutcome.Built>(outcome);
            Assert.True(hit.FromCache, "EmitOrCache must report FromCache=true when the keyed DLL exists.");
            Assert.Equal(finalPath, hit.MetadataDllPath);
            Assert.Equal(key, hit.ContentHash);
            // Strongest assertion: the file we pre-planted is byte-identical
            // afterwards. Re-emit would have replaced these 4 bytes with a
            // multi-KB PE blob.
            Assert.Equal(sentinel, File.ReadAllBytes(finalPath));
            Assert.Equal(sentinelMtime, File.GetLastWriteTimeUtc(finalPath));
        }
        finally
        {
            try { Directory.Delete(cacheRoot, recursive: true); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }
    }
}
