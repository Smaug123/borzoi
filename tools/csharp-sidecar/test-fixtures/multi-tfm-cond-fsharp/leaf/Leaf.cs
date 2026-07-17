namespace MultiTfmCondFsharpLeaf;

// The leaf compiles under both netstandard2.0 and net6.0. NuGet's
// nearest-compatible algorithm resolves net6.0 for the net10.0 F# top, so
// the inner conditional ProjectReference to Polyfill fires under the
// chosen TFM and the closure walker must surface it in `projectTfms`.
#if NET6_0_OR_GREATER
using MultiTfmCondFsharpPolyfill;
#endif

public sealed class LeafBeacon
{
#if NET6_0_OR_GREATER
    public PolyfillBeacon? Polyfill { get; }

    public LeafBeacon(PolyfillBeacon? polyfill)
    {
        Polyfill = polyfill;
    }
#endif

    public string Tag =>
#if NET6_0_OR_GREATER
        "multi-tfm-cond-fsharp-leaf-net6";
#else
        "multi-tfm-cond-fsharp-leaf-netstandard";
#endif
}
