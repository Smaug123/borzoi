namespace MultiTfmCondTop;

using MultiTfmCondLeaf;

public sealed class TopBeacon
{
    public LeafBeacon? Leaf { get; }

    public TopBeacon(LeafBeacon? leaf)
    {
        Leaf = leaf;
    }

    public string Describe() => Leaf?.Tag ?? "no-leaf";
}
