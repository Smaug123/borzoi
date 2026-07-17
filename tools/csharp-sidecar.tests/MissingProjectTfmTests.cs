namespace CSharpSidecar.Tests;

using CSharpSidecar.Tests.TestHelpers;

using Xunit;

/// <summary>
/// Phase 4 of <c>docs/completed/multi-tfm-resolution-plan.md</c> hard-errors with
/// <see cref="BuildMetadataOutcome.MissingProjectTfmOutcome"/> when the
/// <c>projectTfms</c> map supplied with the request does not contain an
/// entry for the top csproj (D5). These tests pin both halves of that
/// policy:
/// <list type="bullet">
///   <item>A missing entry on the top is surfaced verbatim, with the
///   canonicalised path Roslyn will subsequently report.</item>
///   <item>A non-canonical key (e.g. <c>top_dir/../top/Top.csproj</c>,
///   which is the form Rust's <c>resolve_transitive_project_tfms</c>
///   produces by joining the closure walker's csproj-relative
///   <c>&lt;ProjectReference&gt;</c> Include onto the top's directory)
///   matches the canonical csproj path. Without this normalisation, the
///   sidecar would have hard-errored on every multi-csproj closure call
///   from the Rust LSP.</item>
/// </list>
/// </summary>
public sealed class MissingProjectTfmTests
{
    private const string MinimalCsproj =
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n"
        + "  <PropertyGroup>\n"
        + "    <TargetFramework>net10.0</TargetFramework>\n"
        + "  </PropertyGroup>\n"
        + "</Project>\n";

    /// <summary>
    /// A caller that forgets to include the top csproj in <c>projectTfms</c>
    /// must observe <see cref="BuildMetadataOutcome.MissingProjectTfmOutcome"/>
    /// — not a silent fallback to the consumer TFM. The early-return fires
    /// before MSBuildWorkspace is touched, so this test runs without
    /// MSBuildLocator. The path on the response is the canonicalised form
    /// the caller can match against Roslyn's downstream <c>Project.FilePath</c>.
    /// </summary>
    [Fact]
    public void BuildMetadataReturnsMissingProjectTfmWhenTopNotInMap()
    {
        using var csproj = new TempCsproj(MinimalCsproj);
        var cacheRoot = Path.Combine(
            Path.GetTempPath(), $"csharp-sidecar-missing-tfm-{Guid.NewGuid():N}");

        try
        {
            var outcome = BuildService.Create().BuildMetadata(
                csprojPath: csproj.Path,
                configuration: "Debug",
                targetFramework: "net10.0",
                projectTfms: new Dictionary<string, string>(StringComparer.Ordinal),
                cacheRoot: cacheRoot);

            var missing = Assert.IsType<BuildMetadataOutcome.MissingProjectTfmOutcome>(outcome);
            // Canonical path: Path.GetFullPath is idempotent for a path that
            // is already canonical, so we expect equality with the input.
            Assert.Equal(Path.GetFullPath(csproj.Path), missing.CsprojPath);
        }
        finally
        {
            try { Directory.Delete(cacheRoot, recursive: true); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }
    }

    /// <summary>
    /// Direct pin on <see cref="BuildService.CanonicaliseProjectTfms"/>: a
    /// non-canonical key (here, the <c>./</c> form Rust's closure walker
    /// emits when joining csproj-relative paths) must resolve to the same
    /// canonical key as the file's path-on-disk. Going through
    /// <see cref="BuildService.BuildMetadata"/> end-to-end would also exercise
    /// this, but pulls in MSBuildLocator; the helper is exposed at internal
    /// visibility so the policy is testable without that cost.
    /// </summary>
    [Fact]
    public void CanonicaliseProjectTfmsResolvesDotSegmentsToSamePath()
    {
        var dir = Path.GetTempPath();
        var canonicalPath = Path.GetFullPath(Path.Combine(dir, "Top.csproj"));
        var nonCanonicalPath = Path.Combine(dir, ".", "Top.csproj");

        Assert.NotEqual(canonicalPath, nonCanonicalPath);

        var canonical = BuildService.CanonicaliseProjectTfms(
            new Dictionary<string, string>(StringComparer.Ordinal)
            {
                [nonCanonicalPath] = "net6.0",
            });

        Assert.True(
            canonical.TryGetValue(canonicalPath, out var tfm),
            $"Expected canonicalised key {canonicalPath} after normalising {nonCanonicalPath}");
        Assert.Equal("net6.0", tfm);
    }

    /// <summary>
    /// The closure walker's path form: <c>top_dir/../leaf/Leaf.csproj</c>.
    /// <see cref="Path.GetFullPath(string)"/> resolves the <c>..</c> segment
    /// without touching the filesystem, so the canonical key matches what
    /// Roslyn reports for the leaf on disk.
    /// </summary>
    [Fact]
    public void CanonicaliseProjectTfmsResolvesDoubleDotSegmentsToSamePath()
    {
        var topDir = Path.Combine(Path.GetTempPath(), "top");
        var leafCanonical = Path.GetFullPath(Path.Combine(Path.GetTempPath(), "leaf", "Leaf.csproj"));
        // Construct exactly the form Rust's `top_dir.join("../leaf/Leaf.csproj")`
        // would produce — without separately calling Path.GetFullPath. This
        // is the input that broke the proj-ref integration tests before the
        // canonicalisation was added.
        var leafFromTop = Path.Combine(topDir, "..", "leaf", "Leaf.csproj");

        Assert.NotEqual(leafCanonical, leafFromTop);

        var canonical = BuildService.CanonicaliseProjectTfms(
            new Dictionary<string, string>(StringComparer.Ordinal)
            {
                [leafFromTop] = "net6.0",
            });

        Assert.True(
            canonical.TryGetValue(leafCanonical, out var tfm),
            $"Expected canonical key {leafCanonical} after normalising {leafFromTop}");
        Assert.Equal("net6.0", tfm);
    }
}
