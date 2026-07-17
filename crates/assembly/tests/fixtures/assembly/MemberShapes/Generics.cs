// Generic-parameter, constraint, and variance projection. Each type or member
// maps 1:1 to a happy-path test in `tests/all/projector_generics.rs`.

using System;

namespace MemberShapes.Generics;

public class GenericMethods
{
    // projects_generic_method_with_method_typar: `T Echo<T>(T x)` — the return
    // and parameter both resolve to the method typar Var { index 0, is_method }.
    public T Echo<T>(T x) => x;

    // projects_method_special_constraints: `where T : class, new()` sets the
    // reference-type and default-ctor flags without a value-type flag.
    public T MakeRef<T>() where T : class, new() => new T();

    // projects_method_type_constraints: `where T : IComparable` records one
    // named entry in type_constraints.
    public T PickComparable<T>(T x) where T : IComparable => x;

    // projects_struct_value_type_constraint: `where T : struct` sets the
    // value-type flag (C# also emits an explicit System.ValueType constraint).
    public T MakeValue<T>() where T : struct => default;

    // projects_byref_return_type: `ref int Slot()` wraps the return in ByRef.
    public ref int Slot() => ref _slot;

    private int _slot;
}

// projects_type_generic_parameter: `class Box<T>` — one unconstrained typar.
public class Box<T>
{
}

// projects_type_generic_variance_and_constraint:
// `interface IPair<out T, in U> where T : class where U : IComparable, new()`.
public interface IPair<out T, in U>
    where T : class
    where U : IComparable, new()
{
}
