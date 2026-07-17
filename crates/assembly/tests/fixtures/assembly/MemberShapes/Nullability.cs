// Value-type nullability gating: value types stay `Oblivious` even inside a
// `#nullable enable` scope whose `NullableContextAttribute(1)` defaults every
// reference position to `NotAnnotated`. Each member maps 1:1 to a happy-path
// test in `tests/all/projector_nullable.rs`. MiniLib's nullable-indexer fixture
// already pins reference-type outer/inner annotation, so this file is the
// value-type complement it never reaches.

#nullable enable

using System;
using System.Collections.Generic;

namespace MemberShapes.Nullability;

public class ValueTypeNullability
{
    // Reference anchors: several non-null `string`s (plus the `List<int>`
    // outer below) give Roslyn enough annotable positions to condense to a
    // type-level `NullableContextAttribute(1)`. The value-type members then
    // genuinely inherit a not-null scope. `Anchor` is asserted `NotAnnotated`
    // to prove that context is live before each value-type position is checked.
    public string Anchor = "";
    public string Anchor2 = "";
    public string Anchor3 = "";

    // named_value_type_field_stays_oblivious: a `DateTime` field inherits the
    // type-level not-null context but must stay Oblivious.
    public DateTime When;

    // list_of_value_type_outer_byte_only: outer `List` reads NotAnnotated, the
    // non-annotable inner `int` consumes no byte and ends up Oblivious.
    public List<int> Ints = new();

    // value_type_parameter_stays_oblivious: a primitive `int` parameter.
    public void TakeInt(int n) { }

    // named_value_type_parameter_stays_oblivious: a named `DateTime` parameter.
    public void TakeWhen(DateTime when) { }

    // system_nullable_does_not_consume_byte: `int?` = `System.Nullable<int>`,
    // a non-annotable value type — outer and inner `int` both Oblivious.
    public void TakeMaybeInt(int? n) { }

    // generic_value_type_outer_byte_is_discarded:
    // `KeyValuePair<string?, int>` — the outer generic value type is Oblivious,
    // the inner `string?` is Annotated, and the inner `int` is Oblivious.
    public void TakeKvp(KeyValuePair<string?, int> kv) { }
}
