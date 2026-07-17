// Raw MethodDef/TypeDef flag-word projection (OV-2): `Entity::is_sealed` and
// `MethodLike::{is_final, is_newslot, is_hide_by_sig}`. Each type/member maps
// 1:1 to an assertion in `tests/all/projector_overload_flags.rs`. The C# modifiers
// below pin exact IL bits (the reliable ground truth for raw flag words — far
// more precise than FCS's semantic/pickle view, which carries no IL MethodDef
// for F#-authored members):
//
//   - a C# `virtual` method emits `newslot virtual hidebysig`;
//   - a plain `override` emits `virtual hidebysig` (reuses the base slot — NOT
//     newslot, NOT final);
//   - a `sealed override` emits `final virtual hidebysig`;
//   - an `abstract` method emits `newslot abstract virtual hidebysig`;
//   - a non-virtual instance/static method emits just `hidebysig`;
//   - every C#-compiled method is `hidebysig`.

namespace MemberShapes.OverloadFlags;

// A sealed reference type: TypeDef `sealed` bit set.
public sealed class SealedType
{
    // Non-virtual instance method: hidebysig only.
    public void Plain() { }
}

// A non-sealed reference type: TypeDef `sealed` bit clear.
public class OpenBase
{
    // A NEW virtual: newslot + virtual + hidebysig.
    public virtual void V() { }

    // A non-virtual instance method: hidebysig only.
    public void P() { }
}

public class Derived : OpenBase
{
    // A `sealed override`: final + virtual + hidebysig; NOT newslot (it reuses
    // OpenBase.V's vtable slot).
    public sealed override void V() { }
}

// An abstract class with an abstract member: newslot + abstract + virtual +
// hidebysig.
public abstract class AbstractHost
{
    public abstract void A();
}

// A value type is always sealed in the CLR: TypeDef `sealed` bit set.
public struct SealedStruct
{
    public void Plain() { }
}
