namespace TopFixture;

using LeafFixture;

public sealed class TopType
{
    // Constructor-init rather than a `{ get; init; }` setter so the public
    // surface has no `void modreq(IsExternalInit)` return type — the
    // phase-3a assembly reader's signature projector refuses those, and
    // the differential test loads the emitted DLL through it.
    public LeafType? Leaf { get; }

    public TopType(LeafType? leaf)
    {
        Leaf = leaf;
    }

    public string Describe() => Leaf?.Tag ?? "no-leaf";
}
