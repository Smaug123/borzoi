// Accessor vtable flags (`Property::accessor_slots`, `Event::accessor_slots`)
// and `MethodLike::has_other_method_impl`. Each member maps to an assertion in
// `tests/all/projector_accessor_slots.rs`:
//
//   - a C# `virtual` property/event emits `newslot virtual` accessors;
//   - an `override` that overrides only the getter emits one non-newslot
//     virtual accessor (the property has no setter of its own);
//   - a covariant-return `override` emits a *newslot* virtual plus a
//     `MethodImpl` row redirecting it to the base class method — the one
//     override whose flags alone do not say "override".

namespace MemberShapes.AccessorSlots;

public class SlotBase
{
    public virtual int Both { get; set; }

    public virtual event System.EventHandler? Changed;

    public virtual SlotBase Clone() => this;

    public int Plain { get; set; }

    protected void Raise() => Changed?.Invoke(this, System.EventArgs.Empty);
}

public class SlotDerived : SlotBase
{
    public override int Both => 1;

    public override event System.EventHandler? Changed;

    public override SlotDerived Clone() => this;

    public new int Plain { get; set; }

    protected void RaiseDerived() => Changed?.Invoke(this, System.EventArgs.Empty);
}
