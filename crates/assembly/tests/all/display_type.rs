//! Tests for the F# type pretty-printer ([`borzoi_assembly::format_type`]).
//!
//! Two layers: a curated pin table asserting exact output for known shapes
//! (the spec, by example), and structural property tests over arbitrary
//! `TypeRef`s (no panic, balanced delimiters, no placeholder/arity leakage).

use borzoi_assembly::{
    Nullability, NullableType, Primitive, TyparScope, TypeParameter, TypeRef, Variance,
    format_nullable_type, format_type,
};
use proptest::prelude::*;

// ---- construction helpers -------------------------------------------------

fn p(prim: Primitive) -> TypeRef {
    TypeRef::Primitive(prim)
}

fn named(ns: &[&str], name: &str, args: Vec<TypeRef>) -> TypeRef {
    debug_assert!(
        !name.contains('/'),
        "use named_nested for nested name {name}"
    );
    let segment_arities = vec![args.len()];
    TypeRef::Named {
        assembly: None,
        namespace: ns.iter().map(|s| s.to_string()).collect(),
        name: name.to_string(),
        type_args: args.into_iter().map(NullableType::oblivious).collect(),
        segment_arities,
    }
}

/// Build a nested `TypeRef::Named` from `(segment_name, delta_arity)` pairs,
/// outermost first. The names join with `/` and the arities record directly.
fn named_nested(ns: &[&str], segments: &[(&str, usize)], args: Vec<TypeRef>) -> TypeRef {
    let name = segments
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join("/");
    let segment_arities = segments.iter().map(|(_, a)| *a).collect();
    TypeRef::Named {
        assembly: None,
        namespace: ns.iter().map(|s| s.to_string()).collect(),
        name,
        type_args: args.into_iter().map(NullableType::oblivious).collect(),
        segment_arities,
    }
}

fn array(elem: TypeRef, rank: u8) -> TypeRef {
    TypeRef::Array {
        element: Box::new(NullableType::oblivious(elem)),
        rank,
        sizes: vec![],
        lower_bounds: vec![],
    }
}

fn fsharp_func(from: TypeRef, to: TypeRef) -> TypeRef {
    named(
        &["Microsoft", "FSharp", "Core"],
        "FSharpFunc",
        vec![from, to],
    )
}

fn typar(name: &str) -> TypeParameter {
    TypeParameter {
        name: name.to_string(),
        variance: Variance::Invariant,
        reference_type_constraint: false,
        value_type_constraint: false,
        default_constructor_constraint: false,
        is_unmanaged: false,
        allows_ref_struct: false,
        nullability: Nullability::Oblivious,
        type_constraints: vec![],
    }
}

/// An `Annotated` (nullable reference) wrapper around `ty`.
fn annotated(ty: TypeRef) -> NullableType {
    NullableType {
        ty,
        nullability: Nullability::Annotated,
    }
}

/// A `Named` whose type arguments carry explicit nullability.
fn named_nullable(ns: &[&str], name: &str, args: Vec<NullableType>) -> TypeRef {
    let segment_arities = vec![args.len()];
    TypeRef::Named {
        assembly: None,
        namespace: ns.iter().map(|s| s.to_string()).collect(),
        name: name.to_string(),
        type_args: args,
        segment_arities,
    }
}

/// An array whose element carries explicit nullability.
fn array_of(elem: NullableType, rank: u8) -> TypeRef {
    TypeRef::Array {
        element: Box::new(elem),
        rank,
        sizes: vec![],
        lower_bounds: vec![],
    }
}

/// Render under an empty typar scope (no `Var`s in the input).
fn render(ty: &TypeRef) -> String {
    format_type(ty, &TyparScope::empty())
}

// ---- nullability (`T?`) ----------------------------------------------------

fn render_nullable(nt: &NullableType) -> String {
    format_nullable_type(nt, &TyparScope::empty())
}

#[test]
fn annotated_atom_gets_question_mark() {
    // An annotated reference renders the C#-style `?`; oblivious/not-annotated
    // render plain.
    assert_eq!(render_nullable(&annotated(p(Primitive::String))), "string?");
    assert_eq!(
        render_nullable(&NullableType::oblivious(p(Primitive::String))),
        "string"
    );
    assert_eq!(
        render_nullable(&NullableType {
            ty: p(Primitive::String),
            nullability: Nullability::NotAnnotated,
        }),
        "string"
    );
}

#[test]
fn nullable_generic_arg_renders_inside() {
    // Two-arg generic uses angle brackets, which self-delimit: the inner
    // annotation renders without extra parens (`Dictionary<string?, int>`).
    let t = named_nullable(
        &["System", "Collections", "Generic"],
        "Dictionary",
        vec![
            annotated(p(Primitive::String)),
            NullableType::oblivious(p(Primitive::I4)),
        ],
    );
    assert_eq!(render(&t), "Dictionary<string?, int>");
}

#[test]
fn nullable_single_arg_generic_is_postfix() {
    // A one-arg generic renders postfix (`string? List`); the `?` is on the arg.
    let t = named_nullable(
        &["System", "Collections", "Generic"],
        "List",
        vec![annotated(p(Primitive::String))],
    );
    assert_eq!(render(&t), "string? List");
}

#[test]
fn nullable_array_element_parenthesises_atom_only() {
    // `string?[]`: `?` binds tighter than `[]`, so the atom needs no parens.
    assert_eq!(
        render(&array_of(annotated(p(Primitive::String)), 1)),
        "string?[]"
    );
}

#[test]
fn nullable_postfix_operand_is_parenthesised() {
    // `(int list)?`: a non-atom operand parenthesises before `?`.
    let int_list = named(
        &["Microsoft", "FSharp", "Collections"],
        "list",
        vec![p(Primitive::I4)],
    );
    assert_eq!(render_nullable(&annotated(int_list)), "(int list)?");
}

#[test]
fn nullable_byref_puts_question_mark_on_referent() {
    // The public `format_nullable_type` must place a byref's annotation on the
    // referent — `byref<string?>`, never `byref<string>?` — for direct callers
    // (a byref return), not just the member-formatter path.
    let t = annotated(TypeRef::ByRef {
        inner: Box::new(p(Primitive::String)),
        readonly: false,
    });
    assert_eq!(render_nullable(&t), "byref<string?>");
}

#[test]
fn nullable_typar_gets_question_mark() {
    // `'T?` — a generic parameter annotated nullable.
    let typars = [typar("T")];
    let scope = TyparScope::new(&typars, &[]);
    let t = NullableType {
        ty: TypeRef::Var {
            index: 0,
            is_method: false,
        },
        nullability: Nullability::Annotated,
    };
    assert_eq!(format_nullable_type(&t, &scope), "'T?");
}

// ---- pin table: the spec by example ---------------------------------------

#[test]
fn primitive_aliases() {
    assert_eq!(render(&p(Primitive::I4)), "int");
    assert_eq!(render(&p(Primitive::U4)), "uint");
    assert_eq!(render(&p(Primitive::I8)), "int64");
    assert_eq!(render(&p(Primitive::U8)), "uint64");
    assert_eq!(render(&p(Primitive::I2)), "int16");
    assert_eq!(render(&p(Primitive::U2)), "uint16");
    assert_eq!(render(&p(Primitive::I1)), "sbyte");
    assert_eq!(render(&p(Primitive::U1)), "byte");
    assert_eq!(render(&p(Primitive::R8)), "float");
    assert_eq!(render(&p(Primitive::R4)), "float32");
    assert_eq!(render(&p(Primitive::Bool)), "bool");
    assert_eq!(render(&p(Primitive::Char)), "char");
    assert_eq!(render(&p(Primitive::String)), "string");
    assert_eq!(render(&p(Primitive::Object)), "obj");
    assert_eq!(render(&p(Primitive::IntPtr)), "nativeint");
    assert_eq!(render(&p(Primitive::UIntPtr)), "unativeint");
    assert_eq!(render(&p(Primitive::Void)), "unit");
}

#[test]
fn well_known_abbreviations() {
    assert_eq!(
        render(&named(
            &["Microsoft", "FSharp", "Collections"],
            "FSharpList",
            vec![p(Primitive::I4)]
        )),
        "int list"
    );
    assert_eq!(
        render(&named(
            &["Microsoft", "FSharp", "Core"],
            "FSharpOption",
            vec![p(Primitive::String)]
        )),
        "string option"
    );
    assert_eq!(
        render(&named(
            &["Microsoft", "FSharp", "Core"],
            "FSharpRef",
            vec![p(Primitive::I4)]
        )),
        "int ref"
    );
    assert_eq!(
        render(&named(
            &["System", "Collections", "Generic"],
            "IEnumerable",
            vec![p(Primitive::I4)]
        )),
        "int seq"
    );
    assert_eq!(
        render(&named(
            &["Microsoft", "FSharp", "Collections"],
            "FSharpMap",
            vec![p(Primitive::I4), p(Primitive::String)]
        )),
        "Map<int, string>"
    );
    assert_eq!(
        render(&named(&["System", "Numerics"], "BigInteger", vec![])),
        "bigint"
    );
    // `decimal` is a named value type, not an ECMA primitive, so it must be
    // aliased in the named-type table rather than `primitive_name`.
    assert_eq!(render(&named(&["System"], "Decimal", vec![])), "decimal");
}

#[test]
fn functions_render_as_arrows() {
    assert_eq!(
        render(&fsharp_func(p(Primitive::I4), p(Primitive::String))),
        "int -> string"
    );
    // Curried: right-associative, no parens on the right.
    assert_eq!(
        render(&fsharp_func(
            p(Primitive::I4),
            fsharp_func(p(Primitive::String), p(Primitive::Bool))
        )),
        "int -> string -> bool"
    );
    // Higher-order: a function argument is parenthesized.
    assert_eq!(
        render(&fsharp_func(
            fsharp_func(p(Primitive::I4), p(Primitive::String)),
            p(Primitive::Bool)
        )),
        "(int -> string) -> bool"
    );
}

#[test]
fn tuples() {
    assert_eq!(
        render(&named(
            &["System"],
            "Tuple",
            vec![p(Primitive::I4), p(Primitive::String)]
        )),
        "(int * string)"
    );
    assert_eq!(
        render(&named(
            &["System"],
            "ValueTuple",
            vec![p(Primitive::I4), p(Primitive::Bool)]
        )),
        "struct (int * bool)"
    );
}

#[test]
fn generics_arrangement() {
    // Zero args: bare short name.
    assert_eq!(render(&named(&["System"], "Console", vec![])), "Console");
    // One arg, no abbreviation: postfix.
    assert_eq!(
        render(&named(
            &["System", "Threading", "Tasks"],
            "Task",
            vec![p(Primitive::I4)]
        )),
        "int Task"
    );
    // Two args, no abbreviation: angle brackets.
    assert_eq!(
        render(&named(
            &["System", "Collections", "Generic"],
            "Dictionary",
            vec![p(Primitive::String), p(Primitive::I4)]
        )),
        "Dictionary<string, int>"
    );
}

#[test]
fn arrays_byref_ptr() {
    assert_eq!(render(&array(p(Primitive::I4), 1)), "int[]");
    assert_eq!(render(&array(p(Primitive::I4), 2)), "int[,]");
    assert_eq!(render(&array(p(Primitive::I4), 3)), "int[,,]");
    assert_eq!(
        render(&TypeRef::ByRef {
            inner: Box::new(p(Primitive::I4)),
            readonly: false
        }),
        "byref<int>"
    );
    // A read-only byref (C# `in` / `ref readonly`) is F#'s `inref<'T>`: the
    // referent may be read but not written through, and the two must not render
    // alike.
    assert_eq!(
        render(&TypeRef::ByRef {
            inner: Box::new(p(Primitive::I4)),
            readonly: true
        }),
        "inref<int>"
    );
    assert_eq!(
        render(&TypeRef::Ptr(Some(Box::new(p(Primitive::I4))))),
        "nativeptr<int>"
    );
    assert_eq!(render(&TypeRef::Ptr(None)), "voidptr");
}

#[test]
fn nesting_precedence() {
    // A function inside a postfix generic is parenthesized.
    assert_eq!(
        render(&named(
            &["Microsoft", "FSharp", "Collections"],
            "FSharpList",
            vec![fsharp_func(p(Primitive::I4), p(Primitive::String))]
        )),
        "(int -> string) list"
    );
    // A postfix generic as an array element is parenthesized.
    assert_eq!(
        render(&array(
            named(
                &["Microsoft", "FSharp", "Collections"],
                "FSharpList",
                vec![p(Primitive::I4)]
            ),
            1
        )),
        "(int list)[]"
    );
    // Nested postfix generics chain without parens.
    assert_eq!(
        render(&named(
            &["Microsoft", "FSharp", "Core"],
            "FSharpOption",
            vec![named(
                &["Microsoft", "FSharp", "Collections"],
                "FSharpList",
                vec![p(Primitive::I4)]
            )]
        )),
        "int list option"
    );
}

#[test]
fn type_vars() {
    let tps = vec![typar("T"), typar("U")];
    let mtps = vec![typar("a")];
    let scope = TyparScope::new(&tps, &mtps);
    assert_eq!(
        format_type(
            &TypeRef::Var {
                index: 0,
                is_method: false
            },
            &scope
        ),
        "'T"
    );
    assert_eq!(
        format_type(
            &TypeRef::Var {
                index: 1,
                is_method: false
            },
            &scope
        ),
        "'U"
    );
    assert_eq!(
        format_type(
            &TypeRef::Var {
                index: 0,
                is_method: true
            },
            &scope
        ),
        "'a"
    );
}

#[test]
fn out_of_range_typar_is_visible_placeholder() {
    let s = format_type(
        &TypeRef::Var {
            index: 7,
            is_method: false,
        },
        &TyparScope::empty(),
    );
    // Never panics; renders a visible placeholder rather than a wrong name.
    assert!(s.starts_with("'?"), "expected placeholder, got {s}");
}

#[test]
fn large_tuple_flattens_clr_rest() {
    // The CLR encodes an 8+ element tuple as a 7-element head plus a nested
    // `TRest` tuple in the 8th generic slot (`ValueTuple<T1..T7, TRest>`). The
    // printer must flatten that chain back into one F# tuple, not render the
    // `Rest` as a nested element.
    let rest = named(&["System"], "ValueTuple", vec![p(Primitive::Bool)]);
    let eight = named(
        &["System"],
        "ValueTuple",
        vec![
            p(Primitive::I4),
            p(Primitive::I4),
            p(Primitive::I4),
            p(Primitive::I4),
            p(Primitive::I4),
            p(Primitive::I4),
            p(Primitive::I4),
            rest,
        ],
    );
    assert_eq!(
        render(&eight),
        "struct (int * int * int * int * int * int * int * bool)"
    );

    // Reference tuples flatten the same way (`Tuple<T1..T7, TRest>`).
    let rest_ref = named(&["System"], "Tuple", vec![p(Primitive::Char)]);
    let eight_ref = named(
        &["System"],
        "Tuple",
        vec![
            p(Primitive::String),
            p(Primitive::String),
            p(Primitive::String),
            p(Primitive::String),
            p(Primitive::String),
            p(Primitive::String),
            p(Primitive::String),
            rest_ref,
        ],
    );
    assert_eq!(
        render(&eight_ref),
        "(string * string * string * string * string * string * string * char)"
    );
}

#[test]
fn nested_type_names_use_dot() {
    // The projector stores a nested type's short name CLR-style as
    // `Outer/Inner` (the enclosing chain joined with `/`); F# accesses a
    // nested type with `.`, so the separator must be converted.
    assert_eq!(
        render(&named_nested(
            &["System"],
            &[("Environment", 0), ("SpecialFolder", 0)],
            vec![]
        )),
        "Environment.SpecialFolder"
    );
}

#[test]
fn nested_generic_placement() {
    // Each generic argument lands on its declaring segment, driven by the
    // model's per-segment arity.
    // `Dictionary`2/Enumerator`: both args on the encloser, none on the nested.
    assert_eq!(
        render(&named_nested(
            &["System", "Collections", "Generic"],
            &[("Dictionary", 2), ("Enumerator", 0)],
            vec![p(Primitive::I4), p(Primitive::String)]
        )),
        "Dictionary<int, string>.Enumerator"
    );
    // `Outer`1/Inner`1`: one argument on each segment.
    assert_eq!(
        render(&named_nested(
            &["N"],
            &[("Outer", 1), ("Inner", 1)],
            vec![p(Primitive::I4), p(Primitive::String)]
        )),
        "Outer<int>.Inner<string>"
    );
    // `Outer/Inner`1`: non-generic encloser, generic nested type.
    assert_eq!(
        render(&named_nested(
            &["N"],
            &[("Outer", 0), ("Inner", 1)],
            vec![p(Primitive::I4)]
        )),
        "Outer.Inner<int>"
    );
}

#[test]
fn nested_arity_sum_overflow_falls_back() {
    // Adversarial metadata: per-segment arities whose sum overflows `usize`.
    // The mismatch check must not panic (in debug, an unchecked `sum()` would);
    // the renderer falls back to the naive whole-name arrangement.
    assert_eq!(
        render(&named_nested(
            &["S"],
            &[("A", usize::MAX), ("B", 1)],
            vec![p(Primitive::I4)]
        )),
        "int A.B"
    );
}

#[test]
fn nested_inconsistent_arity_falls_back() {
    // Corrupt-metadata shape: per-segment arities ([2, 0]) don't sum to the arg
    // count (1). The renderer must not panic — it falls back to the naive
    // whole-name arrangement (dotted name + postfix single arg).
    assert_eq!(
        render(&named_nested(
            &["S"],
            &[("A", 2), ("B", 0)],
            vec![p(Primitive::I4)]
        )),
        "int A.B"
    );
}

// ---- structural property tests --------------------------------------------

fn arb_primitive() -> impl Strategy<Value = Primitive> {
    prop_oneof![
        Just(Primitive::I4),
        Just(Primitive::String),
        Just(Primitive::Bool),
        Just(Primitive::R8),
        Just(Primitive::Object),
        Just(Primitive::Void),
    ]
}

/// Arbitrary `TypeRef`. `Var` indices stay within the fixed scope used by the
/// property below (3 type typars, 2 method typars) so no placeholder leaks.
fn arb_typeref() -> impl Strategy<Value = TypeRef> {
    let leaf = prop_oneof![
        arb_primitive().prop_map(TypeRef::Primitive),
        (0u16..3).prop_map(|index| TypeRef::Var {
            index,
            is_method: false
        }),
        (0u16..2).prop_map(|index| TypeRef::Var {
            index,
            is_method: true
        }),
        Just(TypeRef::Ptr(None)),
        Just(named(&["System"], "Console", vec![])),
        // A non-generic nested type, stored CLR-style with a `/` separator.
        Just(named_nested(
            &["System"],
            &[("Environment", 0), ("SpecialFolder", 0)],
            vec![]
        )),
    ];
    leaf.prop_recursive(4, 48, 3, |inner| {
        prop_oneof![
            (inner.clone(), 1u8..4).prop_map(|(elem, rank)| array(elem, rank)),
            inner.clone().prop_map(|e| TypeRef::ByRef {
                inner: Box::new(e),
                readonly: false
            }),
            inner.clone().prop_map(|e| TypeRef::Ptr(Some(Box::new(e)))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| fsharp_func(a, b)),
            prop::collection::vec(inner.clone(), 2..4).prop_map(|args| named(
                &["System"],
                "Tuple",
                args
            )),
            inner.clone().prop_map(|a| named(
                &["Microsoft", "FSharp", "Collections"],
                "FSharpList",
                vec![a]
            )),
            inner
                .clone()
                .prop_map(|a| named(&["System", "Threading", "Tasks"], "Task", vec![a])),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| named(
                &["System", "Collections", "Generic"],
                "Dictionary",
                vec![a, b]
            )),
            // A nested generic `Outer`1/Inner`1` with consistent arities, so the
            // distribution path (not the fallback) is exercised.
            (inner.clone(), inner.clone()).prop_map(|(a, b)| named_nested(
                &["N"],
                &[("Outer", 1), ("Inner", 1)],
                vec![a, b]
            )),
        ]
    })
}

proptest! {
    #[test]
    fn total_and_well_formed(t in arb_typeref()) {
        let tps = vec![typar("T"), typar("U"), typar("V")];
        let mtps = vec![typar("a"), typar("b")];
        let scope = TyparScope::new(&tps, &mtps);
        let s = format_type(&t, &scope);

        prop_assert!(!s.is_empty());
        // Every typar is in range, so no placeholder may appear.
        prop_assert!(!s.contains("'?"), "placeholder leaked: {s}");
        // Arity backticks must never reach the surface.
        prop_assert!(!s.contains('`'), "backtick leaked: {s}");
        // The CLR nested-type separator must be converted, never leaked.
        prop_assert!(!s.contains('/'), "nested separator leaked: {s}");

        // Parens and brackets balance exactly.
        prop_assert_eq!(s.matches('(').count(), s.matches(')').count(), "parens: {}", &s);
        prop_assert_eq!(s.matches('[').count(), s.matches(']').count(), "brackets: {}", &s);
        // Angle brackets balance once the `>` of each `->` arrow is removed.
        let no_arrows = s.replace("->", "");
        prop_assert_eq!(
            no_arrows.matches('<').count(),
            no_arrows.matches('>').count(),
            "angles: {}", &s
        );

        // Deterministic.
        prop_assert_eq!(&s, &format_type(&t, &scope));
    }
}
