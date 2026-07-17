namespace PkgRefFixture;

using Newtonsoft.Json.Linq;

/// <summary>A trivial public surface that names a Newtonsoft.Json type, so
/// the emitted metadata DLL carries a TypeRef into the package.</summary>
public sealed class Wrapper
{
    public JObject Payload { get; }

    public Wrapper(JObject payload)
    {
        Payload = payload;
    }
}
