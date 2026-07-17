namespace MultiTfmCondLeaf;

// Source has to compile under both netstandard2.0 and net6.0 (the leaf
// declares both TFMs). The conditional polyfill reference doesn't need
// to be *used* from here — the closure-walker test cares about the
// edge structure, not the surface — but a reference that is consumed
// under net6.0 gives the differential test something to assert on if
// the test ever grows to include `dotnet build`.
#if NET6_0_OR_GREATER
using MultiTfmCondPolyfill;
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
        "multi-tfm-cond-leaf-net6";
#else
        "multi-tfm-cond-leaf-netstandard";
#endif
}
