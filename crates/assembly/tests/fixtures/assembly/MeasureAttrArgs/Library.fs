// Regression fixture for the `Expr.Op` attribute-argument decoder slice.
// It pairs the two `Expr.Op` shapes FCS's `CheckAttribArgExpr` admits in
// attribute position — `TOp.Array` (array literal) and `TOp.Coerce` (a
// literal up-cast to an `obj`-typed parameter) — with a `[<Measure>]`
// type, so a single `enumerate_type_defs` walk pins the end-to-end
// property the decoder exists to restore.
//
// Why this lives in its own fixture rather than in MiniLibFs: the array
// attribute class needs an array-typed constructor parameter (`int[]`),
// and fcs-dump renders an array parameter's *element* with an F# 9
// non-null annotation (`System.Int32![]`) that the Rust projector doesn't
// reproduce for nested array elements — an unrelated member-signature
// rendering gap. That would break MiniLibFs's full entity-vs-fcs diff.
// Here we never diff the whole entity set against fcs-dump; we only assert
// the measure *kind*, so the rendering gap is irrelevant and the fixture
// stays focused on the decoder.

namespace MeasureAttrArgs

// An attribute taking an `int[]`, applied with an array literal. Its
// argument pickles as `Expr.Op(TOp.Array, [int], [Const 1; Const 2;
// Const 3], _)` into the signature stream, which the unpickler reaches
// eagerly while decoding every entity's attribute list.
type TagsAttribute(tags: int[]) =
    inherit System.Attribute()

[<Tags([| 1; 2; 3 |])>]
type Tagged = { Field: int }

// An attribute whose constructor parameter is typed `obj`. A literal
// argument to it is implicitly up-cast, so the `orig` side of the
// attribute argument pickles as `Expr.Op(TOp.Coerce, [obj], [Const "hi"],
// _)` — the second `Expr.Op` shape the decoder must handle. (F# has no
// covariant array cast and rejects `box` in attribute position, so the
// `obj`-parameter route is how a real `TOp.Coerce` reaches the pickle.)
type BoxedAttribute(value: obj) =
    inherit System.Attribute()

[<Boxed("hi")>]
type Boxed = { BField: int }

// A unit-of-measure type. Before this decoder slice, either `Expr.Op`
// attribute argument above hard-errored on `u_expr` tag 2, failing the
// *whole* CCU decode; enumeration then recorded skipped F# overlays and
// left this projected as `Class` (its IL truth) instead of `Measure`.
// The regression test asserts `m` recovers `Measure`, which only holds if
// the host signature pickle — including both `Expr.Op` attribute
// arguments — decodes cleanly.
[<Measure>]
type m
