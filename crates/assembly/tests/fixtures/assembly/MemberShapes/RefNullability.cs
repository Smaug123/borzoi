// Reference-type nullability decoding: how a `#nullable enable` scope's
// `NullableContextAttribute(1)` default, a tighter per-position
// `NullableAttribute`, and the pre-order DFS byte-walk over composite types
// combine to assign `Nullability` to every reference position. Each member
// maps 1:1 to a happy-path test in `tests/all/projector_ref_nullability.rs`.
//
// MiniLib's nullable-indexer fixture pins exactly one reference position
// (outer/inner of an indexer parameter); this file is the byte-walk complement
// it never reaches — left-to-right K/V order, nested generics, and arrays in
// both `string?[]` and `string[]?` forms.

#nullable enable

using System.Collections.Generic;

namespace MemberShapes.RefNullability;

public class ReferenceNullability
{
    // Not-null `string` anchors. Enough annotable positions agreeing with the
    // not-null default biases Roslyn to condense to a type-level
    // `NullableContextAttribute(1)`, so the members below that lack their own
    // `NullableAttribute` genuinely *inherit* a not-null scope. `Anchor` doubles
    // as the field_inherits subject: asserting it NotAnnotated proves the
    // context is live.
    public string Anchor = "";
    public string Anchor2 = "";
    public string Anchor3 = "";

    // property_inherits_type_nullable_context: a not-null `string` property with
    // no direct `NullableAttribute` reads NotAnnotated via the type-level context.
    public string Name { get; set; } = "";

    // parameter_direct_nullable_attribute_wins: the not-null siblings make
    // NotAnnotated the method's majority, so Roslyn stamps a direct
    // `NullableAttribute(2)` on the lone nullable `s` rather than flipping the
    // whole method context. The direct row then beats the not-null context →
    // `s` is Annotated while `a`/`b` stay NotAnnotated, which is what proves the
    // precedence (a single nullable param would condense to a method-level
    // context and never exercise it).
    public void TakeNullableString(string a, string b, string? s) { }

    // parameter_inherits_nullable_context: a not-null `string` parameter with no
    // direct attribute inherits the not-null context → NotAnnotated. (Whether
    // that context lands on the method or the type is Roslyn's encoding choice;
    // the observable outcome is the same and is what we pin.)
    public void TakeString(string s) { }

    // list_of_annotated_string: `List<string?>` → pre-order bytes [1, 2]: outer
    // List NotAnnotated, inner string Annotated.
    public void TakeListOfNullable(List<string?> xs) { }

    // dictionary_mixed_inner_nullability_left_to_right_walk_order:
    // `Dictionary<string, string?>` → bytes [1, 1, 2] in declaration order:
    // Dictionary NotAnnotated, K string NotAnnotated, V string Annotated.
    public Dictionary<string, string?> Map = new();

    // nested_generic_list_of_list_of_annotated_string: `List<List<string?>>` →
    // bytes [1, 1, 2]: outer List, inner List, innermost string Annotated.
    public void TakeNestedList(List<List<string?>> xss) { }

    // annotated_array_element_decodes: `string?[]` → bytes [1, 2]: array
    // NotAnnotated, element string Annotated.
    public void TakeNullableElemArray(string?[] xs) { }

    // array_outer_annotated_decodes: `string[]?` → bytes [2, 1]: array
    // Annotated, element string NotAnnotated.
    public void TakeNullableArray(string[]? xs) { }
}
