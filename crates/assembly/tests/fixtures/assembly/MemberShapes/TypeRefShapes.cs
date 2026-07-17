// TypeRef-shape projection: how `extends` / member-type tokens resolve to a
// `TypeRef::Named` — same-assembly collapse (`assembly: None`), backtick-arity
// stripping, and slash-qualified names for nested types (both same-assembly and
// cross-assembly chains). Each type maps 1:1 to a happy-path test in
// `tests/all/projector_typeref_shapes.rs`. The assembly-identity test reads the
// `AssemblyVersion` pinned in `MemberShapes.csproj`.

using System;
using System.Collections.Generic;

namespace MemberShapes.Shapes;

// same_assembly_base_collapses_to_assembly_none: a base class defined in this
// assembly resolves to `assembly: None`, not a cross-asm ref to our own identity.
public class BaseHelper { }

public class DerivedUser : BaseHelper { }

// backtick_arity_stripped_from_typedef_name: `GenericHolder`1` on disk projects
// to the bare entity name `GenericHolder`.
public class GenericHolder<T> { }

// backtick_arity_stripped_from_typeref_name: deriving from the cross-asm generic
// `List`1` projects a base `TypeRef` named `List`, not `List`1`.
public class MyIntList : List<int> { }

// nested_typedef_base_uses_slash_qualified_name: a same-assembly nested base
// projects `name = "Outer/Inner"` with the outer type's namespace.
public class Outer
{
    public class Inner { }
}

public class NestedDerived : Outer.Inner { }

// nested_cross_asm_typeref_uses_slash_qualified_name: a field whose type is the
// cross-asm nested `System.Environment+SpecialFolder` projects the walked chain
// `name = "Environment/SpecialFolder"`, namespace `System`, external assembly.
public class CrossAsmNestedRef
{
    public Environment.SpecialFolder Folder;
}

// nested_generic_encloser_records_per_segment_arity (cross-asm): a field typed as
// the nested `Dictionary<int,string>.Enumerator` projects `name =
// "Dictionary/Enumerator"`, `type_args = [int, string]`, and per-segment arity
// `[2, 0]` (the generic enclosing `Dictionary`2`, the non-generic `Enumerator`).
public class CrossAsmNestedGenericRef
{
    public Dictionary<int, string>.Enumerator Enum;
}

// nested_generic_encloser_records_per_segment_arity (same-asm): a base typed as
// the same-assembly nested generic `Outer2<int>.Inner2<string>` projects `name =
// "Outer2/Inner2"`, `type_args = [int, string]`, and per-segment arity `[1, 1]`
// (each segment introduces one generic parameter).
public class Outer2<T>
{
    public class Inner2<U> { }
}

public class SameAsmNestedGenericDerived : Outer2<int>.Inner2<string> { }
