using System.Runtime.InteropServices;

namespace MemberShapes;

// Each member maps 1:1 to a happy-path projection test in
// `tests/all/projector_member_shapes.rs`. Compiling these as a real DLL that the
// test reads through `Ecma335Assembly::parse` lets the projection be asserted
// against real PE bytes.
public class Counter
{
    // projects_parameterless_default_constructor
    public Counter() { }

    // projects_constructor_with_parameter_name_from_metadata
    public Counter(int start) { _ = start; }

    // projects_static_method
    public static void Zero() { }

    // projects_virtual_instance_method
    public virtual int Combine(int other) => other;

    // projects_byref_out_parameter
    public bool TryGet(out int value) { value = 0; return true; }

    // optional_parameter_without_constant_is_optional:
    // `[Optional]` alone sets ParameterAttributes.Optional with no Constant
    // row.
    public void Take([Optional] int x) { _ = x; }

    // dotnet_default_value_parameter_is_optional: a C# default value sets the
    // `Optional` flag plus a `Constant` row. It is a .NET optional, not an F#
    // `?optional`, so it projects as `ParamDefault::Optional`.
    public void TakeDefault(int n = 5) { _ = n; }

    // decimal_default_decodes_from_decimal_constant_attribute: `decimal` is not a
    // primitive ELEMENT_TYPE, so `d = 1.5m` cannot sit in a `Constant` row.
    // Roslyn emits `[DecimalConstantAttribute(1, 0, 0, 0, 15)]` (the
    // `byte, byte, uint, uint, uint` ctor) plus the `Optional` flag instead.
    public void TakeDecimalDefault(decimal d = 1.5m) { _ = d; }

    // negative_decimal_default_decodes: the sign byte is non-zero; `2.5m` is
    // mantissa 25 at scale 1.
    public void TakeNegativeDecimalDefault(decimal d = -2.5m) { _ = d; }

    // decimal_constant_without_optional_stays_required: `[DecimalConstant]`
    // applied directly, without `[Optional]`/a default, does *not* set the
    // `Optional` flag — the parameter is still required and must not be rendered
    // as omittable just because the value-carrying attribute is present.
    public void TakeDecimalAttrNotOptional(
        [System.Runtime.CompilerServices.DecimalConstant(0, 0, 0u, 0u, 7u)] decimal d)
    {
        _ = d;
    }

    // datetime_default_decodes_from_datetime_constant_attribute: C# cannot write
    // `DateTime t = <value>` (not a compile-time constant), so the value is
    // applied with the explicit `[Optional, DateTimeConstant(ticks)]` attributes
    // — the only way DateTime defaults reach metadata. 630822816000000000 ticks
    // is 2000-01-01T00:00:00Z.
    public void TakeDateTimeDefault(
        [Optional, System.Runtime.CompilerServices.DateTimeConstant(630822816000000000L)]
            System.DateTime t)
    {
        _ = t;
    }

    // literal_field_carries_is_literal: a C# `const` is a compile-time constant
    // (the CLR `Literal` flag, not `initonly`), so it projects `is_literal` true
    // and `is_init_only` false…
    public const int Limit = 10;

    // …whereas a `static readonly` field is `initonly`, not a literal.
    public static readonly int Shared = 5;
}

// projects_abstract_method
public abstract class Stepper
{
    public abstract void Step();
}

// in_out_parameter_renders_as_byref_not_out:
// COM-style `[In, Out] ref` carries both metadata bits; the projector must
// keep it byref and NOT promote it to `out`.
public class Interop
{
    public void Exchange([In, Out] ref int value) { _ = value; }
}

// discriminates_struct_from_class: a value type extends System.ValueType, so
// the projector must classify it as Struct rather than Class.
public struct PointStruct
{
    public int X;
}

// discriminates_interface_from_class: an interface carries the Interface flag
// and has no base type.
public interface IThing
{
}

// projects_interfaces_implemented_by_type: Thing's InterfaceImpl row points at
// the same-assembly IThing (so the projected TypeRef carries no assembly).
public class Thing : IThing
{
}

// nests_children_under_parents_and_omits_them_from_top_level: Inner must
// project under Outer.nested_types and not appear at the top level.
public class Outer
{
    public class Inner
    {
    }
}
