// Generic-parameter nullability decoding: how the constraint on a method type
// parameter, and the enclosing `#nullable` scope, set the typar's `Nullability`.
// Each method maps 1:1 to a happy-path test in
// `tests/all/projector_generic_nullability.rs`.
//
// Each generic method's typar carries the MINORITY nullable byte among its
// method's reference positions: the five opposite-polarity `string` siblings
// make the *other* byte the method's most-common value, so Roslyn condenses the
// method-level `NullableContextAttribute` to that value and stamps the typar
// with a DIRECT `[NullableAttribute(byte)]` on the GenericParam row rather than
// letting it inherit the context. A typar that projects to the byte OPPOSITE
// its siblings can only have decoded that direct attribute — a context fallback
// would yield the siblings' byte. This is what lets the real-DLL pins exercise
// the direct-attribute decode path, not merely the context fallback.

#nullable enable

namespace MemberShapes.GenericNullability;

public class NotNullTypar
{
    // notnull_constraint_typar_is_not_annotated: `where T : notnull` is byte 1.
    // The five nullable `string?` siblings condense the method context to byte
    // 2, forcing a direct `[Nullable(1)]` on the typar → NotAnnotated. `notnull`
    // sets neither the reference- nor value-type special constraint.
    public T Pick<T>(T x, string? a, string? b, string? c, string? d, string? e)
        where T : notnull => x;
}

public class ClassQuestionTypar
{
    // class_question_constraint_typar_is_annotated: `where T : class?` is byte 2.
    // The five not-null `string` siblings condense the context to byte 1,
    // forcing a direct `[Nullable(2)]` → Annotated, reference-type constraint set.
    public T Pick<T>(T x, string a, string b, string c, string d, string e)
        where T : class? => x;
}

public class ClassTypar
{
    // class_constraint_typar_is_not_annotated: `where T : class` is byte 1. The
    // five nullable siblings condense the context to byte 2, forcing a direct
    // `[Nullable(1)]` → NotAnnotated, reference-type constraint set.
    public T Pick<T>(T x, string? a, string? b, string? c, string? d, string? e)
        where T : class => x;
}

public class UnconstrainedTypar
{
    // unconstrained_typar_under_nullable_enable_is_annotated: an unconstrained,
    // reference-capable typar defaults to byte 2. The five not-null siblings
    // condense the context to byte 1, forcing a direct `[Nullable(2)]` →
    // Annotated (the typar may be substituted with a nullable reference type).
    public T Pick<T>(T x, string a, string b, string c, string d, string e) => x;
}

// The nullability that rides on a *constraint* rather than on the typar: the
// `[Nullable]` Roslyn hangs off the GenericParamConstraint row. The constraint
// type's own outer nullability has no slot in `TypeParameter` (a constraint is
// not a value position), but the annotations *inside* it — a generic argument, an
// array element — are exactly what `TypeRef`'s `NullableType` args model, so they
// must survive the projection rather than collapse to `Oblivious`.
public class ConstraintNullability
{
    // constraint_generic_argument_nullability_survives: the constraint row's
    // `[Nullable]` payload covers `IEquatable<string?>` — one byte for the
    // interface, one for `string?`. The inner `Annotated` is representable.
    public T PickAnnotated<T>(T x) where T : System.IEquatable<string?> => x;

    // constraint_generic_argument_inherits_the_scope_context: the same shape with
    // the opposite inner byte. Roslyn omits the constraint's `[Nullable]` row here
    // because the annotation equals the enclosing `[NullableContext]`, so the
    // decode must resolve through the context rung — distinguishing a real decode
    // from "everything reads Annotated".
    public T PickNotNull<T>(T x) where T : System.IEquatable<string> => x;
}

#nullable disable

public class ObliviousTypar
{
    // unconstrained_typar_under_nullable_disable_is_oblivious: outside any
    // nullable scope the typar carries no `NullableAttribute` and there is no
    // context to inherit → Oblivious.
    public T Pick<T>(T x) => x;
}
