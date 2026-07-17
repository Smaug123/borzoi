namespace MultiTfmTop;

using MultiTfmLeaf;

public sealed class TopBeacon
{
    // Same constructor-init shape as the proj-ref fixture so the assembly
    // reader's signature projector sees identical surface modulo the type
    // names. The point of *this* fixture is the multi-TFM picker on the
    // Leaf side, not the Top's surface — Top is just here to give the
    // closure walk something to hang the producer-TFM resolution off.
    public LeafBeacon? Leaf { get; }

    public TopBeacon(LeafBeacon? leaf)
    {
        Leaf = leaf;
    }

    public string Describe() => Leaf?.Tag ?? "no-leaf";
}
