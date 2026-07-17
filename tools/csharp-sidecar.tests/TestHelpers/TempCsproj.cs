namespace CSharpSidecar.Tests.TestHelpers;

/// <summary>
/// <see cref="IDisposable"/> wrapper around a temp csproj file written to
/// disk. <c>Cache.ComputeKey</c> reads csproj bytes off disk; tests that
/// hash a csproj need a real file path.
/// </summary>
internal sealed class TempCsproj : IDisposable
{
    public string Path { get; }

    public TempCsproj(byte[] contents)
    {
        Path = System.IO.Path.Combine(
            System.IO.Path.GetTempPath(),
            $"csharp-sidecar-tests-{Guid.NewGuid():N}.csproj");
        File.WriteAllBytes(Path, contents);
    }

    public TempCsproj(string contents)
        : this(System.Text.Encoding.UTF8.GetBytes(contents)) { }

    public void Dispose()
    {
        try
        {
            File.Delete(Path);
        }
        catch (IOException) { }
        catch (UnauthorizedAccessException) { }
    }
}
